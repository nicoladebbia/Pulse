use serde::Serialize;
use tauri::State;
use crate::db::DbState;
use crate::services::{causality, predictions, relationships, signals};

#[derive(Debug, Clone, Serialize)]
pub struct TrendPoint {
    pub story_id: i64,
    pub date: String,
    pub headline: String,
    pub significance: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedEntity {
    pub name: String,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendThread {
    pub id: i64,
    pub title: String,
    pub sector: String,
    pub trajectory: String,
    pub acceleration: f64,
    pub mention_count: i32,
    pub days_active: i32,
    /// Mentions per day for the last 14 days (index 0 = 13 days ago, index 13 = today)
    pub sparkline: Vec<i32>,
    pub points: Vec<TrendPoint>,
    // --- Enrichment fields ---
    pub sentiment_avg: f64,
    pub related_entities: Vec<RelatedEntity>,
    /// All sectors this entity appears in (for cross-sector badge)
    pub sectors: Vec<String>,
    /// Top causal chain consequence, if any
    pub causal_consequence: Option<String>,
    /// Related prediction, if any
    pub prediction: Option<TrendPrediction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendPrediction {
    pub title: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryTrendBadge {
    pub story_id: i64,
    pub entity: String,
    pub trajectory: String,
    pub mention_count: i32,
}

/// For a list of story IDs, find which ones mention trending entities.
/// Returns badges for stories that mention non-dormant entities.
#[tauri::command]
pub fn get_story_trend_badges(db: State<'_, DbState>, story_ids: Vec<i64>) -> Result<Vec<StoryTrendBadge>, String> {
    if story_ids.is_empty() {
        return Ok(vec![]);
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let placeholders: String = story_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT em.story_id, sig.topic, sig.trajectory, sig.window_30d
         FROM entity_mentions em
         JOIN entities e ON e.id = em.entity_id
         JOIN signals sig ON LOWER(sig.topic) = LOWER(e.name)
         WHERE em.story_id IN ({})
           AND sig.trajectory IN ('dominant', 'hot', 'rising', 'fading')
         ORDER BY sig.window_30d DESC",
        placeholders
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = story_ids.iter()
        .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>).collect();

    let mut seen = std::collections::HashSet::new();
    let badges: Vec<StoryTrendBadge> = stmt
        .query_map(rusqlite::params_from_iter(params.iter().map(|b| b.as_ref())), |row| {
            Ok(StoryTrendBadge {
                story_id: row.get(0)?,
                entity: row.get(1)?,
                trajectory: row.get(2)?,
                mention_count: row.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        // One badge per story (pick the highest-signal entity)
        .filter(|b| seen.insert(b.story_id))
        .collect();

    Ok(badges)
}

#[derive(Debug, Clone, Serialize)]
pub struct IntelligenceCounts {
    pub entity_count: i64,
    pub active_prediction_count: i64,
    pub hot_signal_count: i64,
}

#[tauri::command]
pub fn get_intelligence_counts(db: State<'_, DbState>) -> Result<IntelligenceCounts, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let entity_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM entities", [], |r| r.get(0),
    ).unwrap_or(0);

    let active_prediction_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM insights WHERE insight_type = 'prediction' AND status = 'active'",
        [], |r| r.get(0),
    ).unwrap_or(0);

    let hot_signal_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM signals WHERE trajectory IN ('dominant', 'hot')",
        [], |r| r.get(0),
    ).unwrap_or(0);

    Ok(IntelligenceCounts { entity_count, active_prediction_count, hot_signal_count })
}

/// Get entities mentioned in a specific story with their signal trajectories.
#[tauri::command]
pub fn get_story_entities(db: State<'_, DbState>, story_id: i64) -> Result<Vec<StoryEntityContext>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn.prepare(
        "SELECT e.name, e.entity_type, COALESCE(sig.trajectory, 'unknown'), COALESCE(sig.acceleration, 0.0), em.sentiment
         FROM entity_mentions em
         JOIN entities e ON e.id = em.entity_id
         LEFT JOIN signals sig ON LOWER(sig.topic) = LOWER(e.name)
         WHERE em.story_id = ?1
         ORDER BY sig.window_30d DESC NULLS LAST
         LIMIT 10",
    ).map_err(|e| e.to_string())?;

    let entities = stmt.query_map([story_id], |row| {
        Ok(StoryEntityContext {
            name: row.get(0)?,
            entity_type: row.get(1)?,
            trajectory: row.get(2)?,
            acceleration: row.get(3)?,
            sentiment: row.get(4)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(entities)
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryEntityContext {
    pub name: String,
    pub entity_type: Option<String>,
    pub trajectory: String,
    pub acceleration: f64,
    pub sentiment: f64,
}

/// Get trending entities for the Trends page.
/// Ranks by days_active * mention_count * acceleration to surface entities
/// with real momentum across multiple days, not single-mention noise.
#[tauri::command]
pub fn get_trends(db: State<'_, DbState>) -> Result<Vec<TrendThread>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // Only recompute signals if stale (>1 hour since last update)
    let needs_recompute: bool = conn.query_row(
        "SELECT COALESCE(MAX(updated_at) < datetime('now', '-1 hour'), 1) FROM signals",
        [], |row| row.get(0),
    ).unwrap_or(true);

    if needs_recompute {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        match signals::recompute_signals(&conn, &today) {
            Ok(count) => tracing::debug!("Recomputed {} signals (stale)", count),
            Err(e) => tracing::warn!("Signal recompute failed: {}", e),
        }
    }

    // Build alias map: alias → canonical entity name (for dedup)
    let mut alias_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Ok(mut alias_stmt) = conn.prepare(
        "SELECT LOWER(ea.alias), e.name FROM entity_aliases ea JOIN entities e ON e.id = ea.entity_id"
    ) {
        if let Ok(rows) = alias_stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                alias_map.insert(row.0, row.1);
            }
        }
    }

    // Find trending entities: must have ≥3 mentions or appear on ≥2 distinct days
    // Rank by days_active * mention_count * acceleration
    let mut stmt = conn
        .prepare(
            "SELECT sig.id, sig.topic, sig.sector, sig.trajectory, sig.acceleration,
                    COUNT(DISTINCT em.mentioned_at) AS days_active,
                    COUNT(em.id) AS mention_count
             FROM signals sig
             JOIN entities e ON LOWER(e.name) = LOWER(sig.topic)
             JOIN entity_mentions em ON em.entity_id = e.id
             WHERE sig.trajectory != 'dormant'
               AND em.mentioned_at >= date('now', '-14 days')
             GROUP BY sig.id
             HAVING COUNT(em.id) >= 3 OR COUNT(DISTINCT em.mentioned_at) >= 2
             ORDER BY (COUNT(DISTINCT em.mentioned_at) * COUNT(em.id) * sig.acceleration) DESC
             LIMIT 25",
        )
        .map_err(|e| e.to_string())?;

    let ranked: Vec<(i64, String, Option<String>, String, f64, i32, i32)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // Filter out alias duplicates: if topic is a known alias, skip it if the canonical is already present
    let canonical_topics: std::collections::HashSet<String> = ranked.iter()
        .map(|(_, topic, _, _, _, _, _)| topic.to_lowercase())
        .collect();

    let ranked: Vec<_> = ranked.into_iter()
        .filter(|(_, topic, _, _, _, _, _)| {
            if let Some(canonical) = alias_map.get(&topic.to_lowercase()) {
                // Skip this alias if the canonical entity is already in the list
                !canonical_topics.contains(&canonical.to_lowercase()) || canonical.to_lowercase() == topic.to_lowercase()
            } else {
                true
            }
        })
        .collect();

    // For each ranked entity, fetch story headlines and sparkline data
    let mut threads = Vec::new();

    for (sig_id, topic, sector, trajectory, acceleration, days_active, mention_count) in ranked {
        // Get story headlines (deduplicated, most recent first)
        let mut points_stmt = conn
            .prepare(
                "SELECT DISTINCT s.id, em.mentioned_at, s.headline, s.importance_score
                 FROM entities e
                 JOIN entity_mentions em ON em.entity_id = e.id
                 JOIN stories s ON s.id = em.story_id
                 WHERE LOWER(e.name) = LOWER(?1)
                   AND em.mentioned_at >= date('now', '-14 days')
                 ORDER BY em.mentioned_at DESC
                 LIMIT 10",
            )
            .map_err(|e| e.to_string())?;

        let mut seen_headlines = std::collections::HashSet::new();
        let points: Vec<TrendPoint> = points_stmt
            .query_map([&topic], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i32>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .filter(|(_, _, headline, _)| seen_headlines.insert(headline.clone()))
            .map(|(story_id, date, headline, significance)| TrendPoint {
                story_id,
                date,
                headline,
                significance,
            })
            .collect();

        // Build sparkline: mentions per day for last 14 days
        let mut spark_stmt = conn
            .prepare(
                "SELECT em.mentioned_at, COUNT(*)
                 FROM entities e
                 JOIN entity_mentions em ON em.entity_id = e.id
                 WHERE LOWER(e.name) = LOWER(?1)
                   AND em.mentioned_at >= date('now', '-13 days')
                 GROUP BY em.mentioned_at
                 ORDER BY em.mentioned_at ASC",
            )
            .map_err(|e| e.to_string())?;

        let day_counts: std::collections::HashMap<String, i32> = spark_stmt
            .query_map([&topic], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        // Build 14-element sparkline array
        let sparkline: Vec<i32> = (0..14)
            .map(|i| {
                let date = chrono::Local::now()
                    .date_naive()
                    .checked_sub_days(chrono::Days::new(13 - i))
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                *day_counts.get(&date).unwrap_or(&0)
            })
            .collect();

        // --- Enrichment: sentiment ---
        let sentiment_avg: f64 = conn
            .query_row(
                "SELECT COALESCE(AVG(em.sentiment), 0.0)
                 FROM entity_mentions em
                 JOIN entities e ON e.id = em.entity_id
                 WHERE LOWER(e.name) = LOWER(?1)
                   AND em.mentioned_at >= date('now', '-14 days')",
                [&topic],
                |row| row.get(0),
            )
            .unwrap_or(0.0);

        // --- Enrichment: related entities (top 5 by strength, min 2 co-occurrences) ---
        let related_entities = relationships::get_related_entities(&conn, &topic, 2.0, 5)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, strength)| RelatedEntity { name, strength })
            .collect();

        // --- Enrichment: cross-sector detection ---
        let mut sectors_stmt = conn
            .prepare(
                "SELECT DISTINCT s.sector
                 FROM entity_mentions em
                 JOIN entities e ON e.id = em.entity_id
                 JOIN stories s ON s.id = em.story_id
                 WHERE LOWER(e.name) = LOWER(?1)
                   AND em.mentioned_at >= date('now', '-14 days')",
            )
            .map_err(|e| e.to_string())?;

        let sectors: Vec<String> = sectors_stmt
            .query_map([&topic], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        // --- Enrichment: top causal consequence ---
        // Skip causal chains — the cross-join query is too expensive for interactive use
        let causal_consequence: Option<String> = None;

        // --- Enrichment: related prediction ---
        let prediction: Option<TrendPrediction> = predictions::get_predictions_for_topic(&conn, &topic)
            .ok()
            .and_then(|preds| preds.into_iter().find(|p| p.status == "active"))
            .map(|p| TrendPrediction { title: p.title, confidence: p.confidence });

        threads.push(TrendThread {
            id: sig_id,
            title: topic,
            sector: sector.unwrap_or_else(|| "general".to_string()),
            trajectory,
            acceleration,
            mention_count,
            days_active,
            sparkline,
            points,
            sentiment_avg,
            related_entities,
            sectors,
            causal_consequence,
            prediction,
        });
    }

    Ok(threads)
}
