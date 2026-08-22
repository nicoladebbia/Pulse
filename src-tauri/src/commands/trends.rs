use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{Emitter, Manager, State};
use serde::{Deserialize, Serialize};

use crate::db::DbState;
use crate::services::{predictions, relationships, signals};

/// Guards against overlapping background signal recomputes when Trends is
/// opened repeatedly while one is already running.
static SIGNAL_RECOMPUTE_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendThread {
    pub id: i64,
    pub title: String,
    pub sector: String,
    pub trajectory: String,
    pub acceleration: f64,
    pub mention_count: i32,
    pub days_active: i32,
    pub sparkline: Vec<i32>,
    pub points: Vec<TrendPoint>,
    pub sentiment_avg: f64,
    pub related_entities: Vec<RelatedEntity>,
    pub sectors: Vec<String>,
    pub causal_consequence: Option<String>,
    pub prediction: Option<TrendPrediction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendPoint {
    pub story_id: i64,
    pub date: String,
    pub headline: String,
    pub significance: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedEntity {
    pub name: String,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendPrediction {
    pub title: String,
    pub confidence: f32,
}

/// One ranked signal row as pulled from the trends query, in rank order:
/// (signal_id, topic, signal_sector, trajectory, acceleration, days_active, mention_count, sentiment_avg).
type RankedRow = (i64, String, Option<String>, String, f64, i32, i32, f64);

/// Bug 1 dedup. `signals` is keyed `(topic, sector)`, so an entity trending in two sectors
/// (e.g. "china" as both ai and tech) produces two rows → two cards for the same subject.
/// Collapse by `LOWER(topic)`: keep the FIRST (strongest-ranked, since input is rank-ordered)
/// row and return every later duplicate's signal-sector as `extra_sectors`, to be folded into
/// the surviving card's `sectors` vec so the UI's "⊕ N sectors" badge reflects all of them.
/// Running this BEFORE per-thread enrichment also avoids wasted DB queries on dropped dups.
fn collapse_ranked_by_topic(ranked: Vec<RankedRow>) -> Vec<(RankedRow, Vec<String>)> {
    let mut survivors: Vec<(RankedRow, Vec<String>)> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for row in ranked {
        let topic_lower = row.1.to_lowercase();
        if let Some(&idx) = seen.get(&topic_lower) {
            // Duplicate topic: fold this row's signal-sector into the survivor's extra list,
            // unless it's already the survivor's own sector or already recorded as an extra.
            if let Some(sec) = &row.2 {
                let (survivor, extras) = &mut survivors[idx];
                let is_own_sector = survivor.2.as_deref() == Some(sec.as_str());
                if !is_own_sector && !extras.contains(sec) {
                    extras.push(sec.clone());
                }
            }
            continue;
        }
        seen.insert(topic_lower, survivors.len());
        survivors.push((row, Vec::new()));
    }
    survivors
}

/// Get story badges (entity tags) for a batch of stories.
#[derive(Debug, Clone, Serialize)]
pub struct StoryTrendBadge {
    pub story_id: i64,
    pub entity: String,
    pub trajectory: String,
    pub mention_count: i64,
}

#[tauri::command]
pub fn get_story_trend_badges(db: State<'_, DbState>, story_ids: Vec<i64>) -> Result<Vec<StoryTrendBadge>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut badges = Vec::new();
    for sid in &story_ids {
        let mut stmt = conn.prepare(
            "SELECT e.name, COALESCE(sig.trajectory, 'unknown'),
                    (SELECT COUNT(*) FROM entity_mentions em2 WHERE em2.entity_id = e.id)
             FROM entity_mentions em
             JOIN entities e ON e.id = em.entity_id
             LEFT JOIN signals sig ON sig.topic = e.name_normalized
             WHERE em.story_id = ?1
             ORDER BY sig.window_30d DESC NULLS LAST
             LIMIT 3",
        ).map_err(|e| e.to_string())?;

        let rows: Vec<StoryTrendBadge> = stmt.query_map([sid], |row| {
            Ok(StoryTrendBadge {
                story_id: *sid,
                entity: row.get(0)?,
                trajectory: row.get(1)?,
                mention_count: row.get(2)?,
            })
        }).map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

        // Return the top badge per story (most mentioned entity)
        if let Some(best) = rows.into_iter().max_by_key(|b| b.mention_count) {
            badges.push(best);
        }
    }

    Ok(badges)
}

#[derive(Debug, Clone, Serialize)]
pub struct StoryEntityContext {
    pub name: String,
    pub entity_type: Option<String>,
    pub trajectory: String,
    pub acceleration: f64,
    pub sentiment: f64,
}

#[tauri::command]
pub fn get_story_entities(db: State<'_, DbState>, story_id: i64) -> Result<Vec<StoryEntityContext>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn.prepare(
        "SELECT e.name, e.entity_type, COALESCE(sig.trajectory, 'unknown'), COALESCE(sig.acceleration, 0.0), em.sentiment
         FROM entity_mentions em
         JOIN entities e ON e.id = em.entity_id
         LEFT JOIN signals sig ON sig.topic = e.name_normalized
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
        "SELECT COUNT(*) FROM signals WHERE trajectory IN ('hot', 'dominant')",
        [], |r| r.get(0),
    ).unwrap_or(0);

    Ok(IntelligenceCounts { entity_count, active_prediction_count, hot_signal_count })
}

/// Get trending entities for the Trends page.
/// Optimized: fetches everything in minimal queries to avoid blocking the main thread.
#[tauri::command]
pub fn get_trends(app: tauri::AppHandle, db: State<'_, DbState>) -> Result<Vec<TrendThread>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // If signals are stale (>1 hour since last update), recompute in the BACKGROUND
    // instead of blocking page open behind a full re-rank. We return trends built
    // from the current (possibly stale) signals immediately; when the recompute
    // finishes, a 'trends-recomputed' event tells the frontend to re-fetch.
    let needs_recompute: bool = conn.query_row(
        "SELECT COALESCE(MAX(updated_at) < datetime('now', '-1 hour'), 1) FROM signals",
        [], |row| row.get(0),
    ).unwrap_or(true);

    if needs_recompute && !SIGNAL_RECOMPUTE_RUNNING.swap(true, Ordering::SeqCst) {
        let bg_app = app.clone();
        std::thread::spawn(move || {
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let db = bg_app.state::<DbState>();
            let result = match db.0.lock() {
                Ok(conn) => signals::recompute_signals(&conn, &today),
                Err(e) => Err(anyhow::anyhow!("db lock poisoned: {}", e)),
            };
            SIGNAL_RECOMPUTE_RUNNING.store(false, Ordering::SeqCst);
            match result {
                // Only notify when rows were actually upserted: a zero-count recompute
                // leaves MAX(updated_at) stale, so emitting would make the frontend
                // re-fetch → re-spawn → re-emit in an infinite loop on an empty DB.
                Ok(count) if count > 0 => {
                    tracing::debug!("Background signal recompute done ({} signals)", count);
                    let _ = bg_app.emit("trends-recomputed", ());
                }
                Ok(_) => tracing::debug!("Background signal recompute: nothing to update"),
                Err(e) => tracing::warn!("Background signal recompute failed: {}", e),
            }
        });
    }

    // Trends are scoped to the 4 freedom-news sectors. Financial-source entities
    // (SEC filings, USASpending recipients, FedReg agencies) live under sector='finance'
    // and feed the Signals page — they are NOT stories and must never reach Trends.
    // NOTE on the two counting windows. `mention_count` is displayed as the signal's own
    // `window_30d` — the EXACT number the trajectory ladder was tested against (the ladder
    // uses total = max(window_30d, window_7d); 7d ⊂ 30d so total == window_30d). Displaying
    // it makes the badge/count contradiction impossible by construction: a "dominant" badge
    // (needs ≥14) always sits next to a ≥14 count. `days_active` is NOT persisted on signals,
    // so it's JOIN-counted over the same 30-day window to stay consistent with that number.
    // The 30d JOIN over eligible stories is also the eligibility gate (HAVING) and the ranking
    // input (ORDER BY). The 14-day sparkline below is deliberately a separate short-window
    // shape viz, not a total — it does not need to match window_30d.
    let mut stmt = conn
        .prepare(
            "SELECT sig.id, sig.topic, sig.sector, sig.trajectory, sig.acceleration,
                    COUNT(DISTINCT em.mentioned_at) AS days_active,
                    sig.window_30d AS mention_count,
                    COALESCE(AVG(em.sentiment), 0.0) AS sentiment_avg
             FROM signals sig
             JOIN entities e ON e.name_normalized = LOWER(sig.topic)
             JOIN entity_mentions em ON em.entity_id = e.id
             JOIN stories s ON s.id = em.story_id
             WHERE sig.trajectory != 'dormant'
               AND sig.sector IN ('ai', 'miami', 'italy', 'tech')
               AND s.sector IN ('ai', 'miami', 'italy', 'tech')
               AND em.mentioned_at >= date('now', '-30 days')
             GROUP BY sig.id
             HAVING COUNT(em.id) >= 3 OR COUNT(DISTINCT em.mentioned_at) >= 2
             ORDER BY (COUNT(DISTINCT em.mentioned_at) * COUNT(em.id) * sig.acceleration) DESC
             LIMIT 15",
        )
        .map_err(|e| e.to_string())?;

    let ranked: Vec<RankedRow> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        // Drop federal-filing / PAC / LLC / ALL-CAPS gov-payload noise. Shared filter —
        // see services::entity_noise (also applied to the Signals convergence watchlist).
        .filter(|r: &RankedRow| {
            !crate::services::entity_noise::is_noise_entity(&r.1)
        })
        .collect();

    // Collapse duplicate-topic rows (Bug 1) BEFORE enrichment so we neither fetch story points
    // for dropped dups nor build duplicate cards. Each survivor carries the extra signal-sectors
    // merged in from its duplicates.
    let survivors = collapse_ranked_by_topic(ranked);

    // Batch: get ALL story points for all trending topics in one query
    let topic_list: Vec<String> = survivors.iter().map(|(r, _)| r.1.to_lowercase()).collect();
    let mut all_points: std::collections::HashMap<String, Vec<TrendPoint>> = std::collections::HashMap::new();
    let mut all_sparklines: std::collections::HashMap<String, std::collections::HashMap<String, i32>> = std::collections::HashMap::new();
    let mut all_sectors: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    if !topic_list.is_empty() {
        // Batch story points — one query for all topics
        let placeholders: String = topic_list.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        // Same 30-day window and sector filter as the ranking query above — a trend
        // that qualified on 30d of mentions must always render with its evidence.
        // (This was '-14 days' without a sector filter, so a topic trending on
        // days 15-30 showed an empty story timeline.) The 14-day sparkline keeps
        // its short window: it reads only the last 14 days of these day-counts.
        let sql = format!(
            "SELECT e.name_normalized, s.id, em.mentioned_at, s.headline, s.importance_score, s.sector
             FROM entities e
             JOIN entity_mentions em ON em.entity_id = e.id
             JOIN stories s ON s.id = em.story_id
             WHERE e.name_normalized IN ({})
               AND s.sector IN ('ai', 'miami', 'italy', 'tech')
               AND em.mentioned_at >= date('now', '-30 days')
             ORDER BY em.mentioned_at DESC",
            placeholders
        );

        let mut batch_stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let params: Vec<&dyn rusqlite::types::ToSql> = topic_list.iter().map(|t| t as &dyn rusqlite::types::ToSql).collect();

        let rows = batch_stmt.query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,  // name_normalized
                row.get::<_, i64>(1)?,     // story_id
                row.get::<_, String>(2)?,  // date
                row.get::<_, String>(3)?,  // headline
                row.get::<_, i32>(4)?,     // importance
                row.get::<_, String>(5)?,  // sector
            ))
        }).map_err(|e| e.to_string())?;

        for row in rows.flatten() {
            let (name_norm, story_id, date, headline, significance, sector) = row;

            // Story points (limit 8 per topic, dedup by headline)
            let points = all_points.entry(name_norm.clone()).or_default();
            if points.len() < 8 && !points.iter().any(|p| p.headline == headline) {
                points.push(TrendPoint { story_id, date: date.clone(), headline, significance });
            }

            // Sparkline counts
            let day_counts = all_sparklines.entry(name_norm.clone()).or_default();
            *day_counts.entry(date).or_insert(0) += 1;

            // Sectors
            let sectors = all_sectors.entry(name_norm).or_default();
            if !sectors.contains(&sector) {
                sectors.push(sector);
            }
        }
    }

    // Build thread objects (no more per-entity queries). `survivors` is already deduped by topic
    // (Bug 1), with each duplicate's signal-sector carried in `extra_sectors`.
    let mut threads: Vec<TrendThread> = Vec::new();

    for ((sig_id, topic, sector, trajectory, acceleration, days_active, mention_count, sentiment_avg), extra_sectors) in survivors {
        let topic_lower = topic.to_lowercase();

        let points = all_points.remove(&topic_lower).unwrap_or_default();

        // Build sparkline from pre-fetched day counts
        let day_counts = all_sparklines.get(&topic_lower);
        let sparkline: Vec<i32> = (0..14)
            .map(|i| {
                let date = chrono::Local::now()
                    .date_naive()
                    .checked_sub_days(chrono::Days::new(13 - i))
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                day_counts.and_then(|dc| dc.get(&date).copied()).unwrap_or(0)
            })
            .collect();

        let mut sectors = all_sectors.remove(&topic_lower).unwrap_or_default();
        // Ensure this signal's own sector is represented (story-sector list may not include it),
        // then fold in the sectors merged from any duplicate-topic rows (Bug 1).
        for sec in sector.iter().chain(extra_sectors.iter()) {
            if !sectors.contains(sec) {
                sectors.push(sec.clone());
            }
        }

        // Related entities — fast indexed query
        let related_entities = relationships::get_related_entities(&conn, &topic, 2.0, 5)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, strength)| RelatedEntity { name, strength })
            .collect();

        // Related prediction — fast, only 9 rows in insights table
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
            causal_consequence: None,
            prediction,
        });
    }

    Ok(threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a RankedRow with just the fields the dedup helper reads (id, topic, sector);
    /// the rest are filler.
    fn row(id: i64, topic: &str, sector: Option<&str>) -> RankedRow {
        (id, topic.to_string(), sector.map(|s| s.to_string()), "rising".into(), 4.286, 5, 5, 0.0)
    }

    #[test]
    fn same_topic_two_sectors_collapses_to_one_survivor_with_merged_sectors() {
        // Rank order: the ai row is stronger (comes first), the italy row is the dup.
        let ranked = vec![
            row(1, "Inter Milan", Some("ai")),
            row(2, "Inter Milan", Some("italy")),
        ];
        let out = collapse_ranked_by_topic(ranked);

        assert_eq!(out.len(), 1, "two same-topic rows collapse to one card");
        let (survivor, extras) = &out[0];
        assert_eq!(survivor.0, 1, "keeps the FIRST (strongest-ranked) row");
        assert_eq!(extras, &vec!["italy".to_string()], "dup's sector carried as extra");
    }

    #[test]
    fn topic_match_is_case_insensitive() {
        let ranked = vec![
            row(1, "China", Some("ai")),
            row(2, "china", Some("tech")),
            row(3, "CHINA", Some("miami")),
        ];
        let out = collapse_ranked_by_topic(ranked);
        assert_eq!(out.len(), 1, "case variants are the same topic");
        assert_eq!(out[0].0.0, 1, "first row survives");
        assert_eq!(out[0].1, vec!["tech".to_string(), "miami".to_string()]);
    }

    #[test]
    fn distinct_topics_all_survive_in_rank_order() {
        let ranked = vec![
            row(1, "OpenAI", Some("ai")),
            row(2, "Meloni", Some("italy")),
            row(3, "Nvidia", Some("ai")),
        ];
        let out = collapse_ranked_by_topic(ranked);
        assert_eq!(out.len(), 3, "no dups → all survive");
        let ids: Vec<i64> = out.iter().map(|(r, _)| r.0).collect();
        assert_eq!(ids, vec![1, 2, 3], "rank order preserved");
        assert!(out.iter().all(|(_, extras)| extras.is_empty()), "no extra sectors when unique");
    }

    #[test]
    fn duplicate_with_same_sector_does_not_duplicate_the_sector() {
        // Degenerate case: both rows share the same sector — extras must stay empty.
        let ranked = vec![
            row(1, "Palantir", Some("ai")),
            row(2, "Palantir", Some("ai")),
        ];
        let out = collapse_ranked_by_topic(ranked);
        assert_eq!(out.len(), 1);
        assert!(out[0].1.is_empty(), "same sector isn't added twice");
    }

    #[test]
    fn duplicate_with_null_sector_is_dropped_without_adding_extras() {
        let ranked = vec![
            row(1, "Google", Some("ai")),
            row(2, "Google", None),
        ];
        let out = collapse_ranked_by_topic(ranked);
        assert_eq!(out.len(), 1, "dup still collapses");
        assert!(out[0].1.is_empty(), "None sector contributes no extra");
    }
}
