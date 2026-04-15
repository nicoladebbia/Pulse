use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    pub id: Option<i64>,
    pub title: String,
    pub prediction: String,
    pub confidence: f32,
    pub reasoning: String,
    pub evidence_types: Vec<String>,
    pub evidence_story_ids: Vec<i64>,
    pub predicted_timeframe: String,
    pub sector: Option<String>,
    pub status: String,
    pub probability_history: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PredictionStats {
    pub total: usize,
    pub active: usize,
    pub validated: usize,
    pub partially_validated: usize,
    pub invalidated: usize,
    pub expired: usize,
    /// (validated + partially_validated) / (validated + partially_validated + invalidated + expired)
    pub accuracy_rate: f64,
    /// Average Brier score (0=perfect, 0.25=random, 1=always wrong). None if no scored predictions.
    pub avg_brier_score: Option<f64>,
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Store a prediction in the insights ledger.
/// Returns the newly created `insight_id`.
pub fn store_prediction(conn: &Connection, prediction: &Prediction) -> Result<i64> {
    let evidence_json = serde_json::to_string(
        &prediction
            .evidence_story_ids
            .iter()
            .zip(prediction.evidence_types.iter().cycle())
            .map(|(sid, etype)| {
                serde_json::json!({
                    "story_id": sid,
                    "reasoning": format!("Evidence type: {}", etype),
                })
            })
            .collect::<Vec<_>>(),
    )?;

    let content = format!(
        "{}\n\nReasoning: {}\n\nEvidence types: {}",
        prediction.prediction,
        prediction.reasoning,
        prediction.evidence_types.join(", "),
    );

    conn.execute(
        "INSERT INTO insights (
            insight_type, title, content, confidence, evidence,
            sector, status, predicted_date
        ) VALUES ('prediction', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            prediction.title,
            content,
            prediction.confidence,
            evidence_json,
            prediction.sector,
            prediction.status,
            prediction.predicted_timeframe,
        ],
    )
    .context("failed to insert prediction insight")?;

    let insight_id = conn.last_insert_rowid();

    // Link evidence stories.
    for sid in &prediction.evidence_story_ids {
        conn.execute(
            "INSERT INTO insight_evidence (insight_id, story_id, role) VALUES (?1, ?2, 'support')",
            params![insight_id, sid],
        )?;
    }

    Ok(insight_id)
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// Get all predictions regardless of status.
pub fn get_all_predictions(conn: &Connection) -> Result<Vec<Prediction>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, content, confidence, evidence, sector, status, predicted_date, probability_history
         FROM insights
         WHERE insight_type = 'prediction'
         ORDER BY
           CASE status
             WHEN 'active' THEN 0
             WHEN 'partially_validated' THEN 1
             WHEN 'validated' THEN 2
             WHEN 'invalidated' THEN 3
             WHEN 'expired' THEN 4
           END,
           confidence DESC",
    )?;

    let predictions = stmt
        .query_map([], |row| {
            Ok(RawInsightRow {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                confidence: row.get(3)?,
                evidence: row.get(4)?,
                sector: row.get(5)?,
                status: row.get(6)?,
                predicted_date: row.get(7)?,
                probability_history: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .map(|r| row_to_prediction(r))
        .collect();

    Ok(predictions)
}

/// Get all active predictions.
pub fn get_active_predictions(conn: &Connection) -> Result<Vec<Prediction>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, content, confidence, evidence, sector, status, predicted_date, probability_history
         FROM insights
         WHERE insight_type = 'prediction' AND status = 'active'
         ORDER BY confidence DESC",
    )?;

    let predictions = stmt
        .query_map([], |row| {
            Ok(RawInsightRow {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                confidence: row.get(3)?,
                evidence: row.get(4)?,
                sector: row.get(5)?,
                status: row.get(6)?,
                predicted_date: row.get(7)?,
                probability_history: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .map(|r| row_to_prediction(r))
        .collect();

    Ok(predictions)
}

/// Get predictions matching a topic (searches title and content).
pub fn get_predictions_for_topic(conn: &Connection, topic: &str) -> Result<Vec<Prediction>> {
    let pattern = format!("%{}%", topic.to_lowercase());

    let mut stmt = conn.prepare(
        "SELECT id, title, content, confidence, evidence, sector, status, predicted_date, probability_history
         FROM insights
         WHERE insight_type = 'prediction'
           AND (LOWER(title) LIKE ?1 OR LOWER(content) LIKE ?1)
         ORDER BY confidence DESC",
    )?;

    let predictions = stmt
        .query_map(params![pattern], |row| {
            Ok(RawInsightRow {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                confidence: row.get(3)?,
                evidence: row.get(4)?,
                sector: row.get(5)?,
                status: row.get(6)?,
                predicted_date: row.get(7)?,
                probability_history: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .map(|r| row_to_prediction(r))
        .collect();

    Ok(predictions)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate active predictions against newly ingested stories.
///
/// For each active prediction, extract key words from its title and check if
/// any of the new stories mention those terms. Returns a list of
/// `(insight_id, new_status)` for predictions whose status changed.
pub fn validate_predictions(
    conn: &Connection,
    new_story_ids: &[i64],
) -> Result<Vec<(i64, String)>> {
    if new_story_ids.is_empty() {
        return Ok(Vec::new());
    }

    let active = get_active_predictions(conn)?;
    if active.is_empty() {
        return Ok(Vec::new());
    }

    // Build a set of headlines + summaries from the new stories for matching.
    let placeholders: String = new_story_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, LOWER(headline), LOWER(summary) FROM stories WHERE id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;

    let story_texts: Vec<(i64, String, String)> = stmt
        .query_map(
            rusqlite::params_from_iter(new_story_ids.iter()),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
        .filter_map(|r| r.ok())
        .collect();

    let mut updates = Vec::new();

    for pred in &active {
        let pred_id = match pred.id {
            Some(id) => id,
            None => continue,
        };

        // Extract significant words from the prediction title (3+ chars).
        let keywords: Vec<String> = pred
            .title
            .to_lowercase()
            .split_whitespace()
            .filter(|w| w.len() >= 3)
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|w| !w.is_empty())
            .collect();

        if keywords.is_empty() {
            continue;
        }

        // Count how many keywords match in each new story.
        let mut best_match_ratio = 0.0_f64;
        for (_sid, headline, summary) in &story_texts {
            let combined = format!("{headline} {summary}");
            let matched = keywords
                .iter()
                .filter(|kw| combined.contains(kw.as_str()))
                .count();
            let ratio = matched as f64 / keywords.len() as f64;
            if ratio > best_match_ratio {
                best_match_ratio = ratio;
            }
        }

        let new_status = if best_match_ratio >= 0.6 {
            "validated"
        } else if best_match_ratio >= 0.3 {
            "partially_validated"
        } else {
            continue; // no change
        };

        conn.execute(
            "UPDATE insights SET status = ?1, updated_at = datetime('now')
             WHERE id = ?2",
            params![new_status, pred_id],
        )?;

        updates.push((pred_id, new_status.to_string()));
    }

    Ok(updates)
}

/// Manually validate or invalidate a prediction.
pub fn manually_validate_prediction(conn: &Connection, id: i64, outcome: &str) -> Result<()> {
    let valid_outcomes = ["validated", "partially_validated", "invalidated"];
    if !valid_outcomes.contains(&outcome) {
        anyhow::bail!("Invalid outcome '{}'. Must be one of: {:?}", outcome, valid_outcomes);
    }

    conn.execute(
        "UPDATE insights SET status = ?1, actual_outcome = ?2, updated_at = datetime('now')
         WHERE id = ?3 AND insight_type = 'prediction'",
        params![outcome, outcome, id],
    )
    .context("failed to update prediction")?;

    Ok(())
}

/// Expire predictions whose `predicted_date` has passed without validation.
/// Returns the number of expired predictions.
pub fn expire_stale_predictions(conn: &Connection) -> Result<usize> {
    let count = conn.execute(
        "UPDATE insights SET status = 'expired', updated_at = datetime('now')
         WHERE insight_type = 'prediction'
           AND status = 'active'
           AND predicted_date IS NOT NULL
           AND predicted_date < date('now')",
        [],
    )?;

    Ok(count)
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Compute prediction accuracy statistics across all predictions.
pub fn get_prediction_stats(conn: &Connection) -> Result<PredictionStats> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) FROM insights
         WHERE insight_type = 'prediction'
         GROUP BY status",
    )?;

    let mut stats = PredictionStats::default();

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
    })?;

    for row in rows {
        let (status, count) = row?;
        match status.as_str() {
            "active" => stats.active = count,
            "validated" => stats.validated = count,
            "partially_validated" => stats.partially_validated = count,
            "invalidated" => stats.invalidated = count,
            "expired" => stats.expired = count,
            _ => {}
        }
    }

    stats.total = stats.active + stats.validated + stats.partially_validated
        + stats.invalidated + stats.expired;

    let resolved = (stats.validated + stats.partially_validated
        + stats.invalidated + stats.expired) as f64;
    if resolved > 0.0 {
        stats.accuracy_rate =
            (stats.validated + stats.partially_validated) as f64 / resolved;
    }

    // Compute average Brier score for scored predictions
    stats.avg_brier_score = conn.query_row(
        "SELECT AVG(brier_score) FROM insights WHERE insight_type = 'prediction' AND brier_score IS NOT NULL",
        [], |row| row.get::<_, Option<f64>>(0),
    ).ok().flatten();

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

struct RawInsightRow {
    id: i64,
    title: String,
    content: String,
    confidence: f64,
    evidence: Option<String>,
    sector: Option<String>,
    status: String,
    predicted_date: Option<String>,
    probability_history: Option<String>,
}

fn row_to_prediction(row: RawInsightRow) -> Prediction {
    // Parse evidence JSON to extract story IDs and evidence types.
    let (story_ids, etypes) = row
        .evidence
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(json).ok())
        .map(|arr| {
            let ids: Vec<i64> = arr
                .iter()
                .filter_map(|v| v.get("story_id").and_then(|s| s.as_i64()))
                .collect();
            let types: Vec<String> = arr
                .iter()
                .filter_map(|v| {
                    v.get("reasoning")
                        .and_then(|s| s.as_str())
                        .map(|s| s.replace("Evidence type: ", ""))
                })
                .collect();
            (ids, types)
        })
        .unwrap_or_default();

    // Split content back into prediction text and reasoning.
    let (prediction_text, reasoning) = row
        .content
        .split_once("\n\nReasoning: ")
        .map(|(p, rest)| {
            let r = rest
                .split_once("\n\nEvidence types: ")
                .map(|(r, _)| r)
                .unwrap_or(rest);
            (p.to_string(), r.to_string())
        })
        .unwrap_or((row.content.clone(), String::new()));

    Prediction {
        id: Some(row.id),
        title: row.title,
        prediction: prediction_text,
        confidence: row.confidence as f32,
        reasoning,
        evidence_types: etypes,
        evidence_story_ids: story_ids,
        predicted_timeframe: row.predicted_date.unwrap_or_default(),
        sector: row.sector,
        status: row.status,
        probability_history: row
            .probability_history
            .as_deref()
            .and_then(|json| serde_json::from_str::<Vec<f64>>(json).ok())
            .unwrap_or_default(),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;

    fn make_prediction(sector: &str, status: &str, predicted_date: Option<&str>) -> Prediction {
        Prediction {
            id: None,
            title: format!("Prediction about {sector}"),
            prediction: "Something will happen".to_string(),
            confidence: 0.75,
            reasoning: "Based on strong signals".to_string(),
            evidence_types: vec!["signal".to_string(), "causal_chain".to_string()],
            evidence_story_ids: Vec::new(),
            predicted_timeframe: predicted_date.unwrap_or("2026-06-01").to_string(),
            sector: Some(sector.to_string()),
            status: status.to_string(),
            probability_history: Vec::new(),
        }
    }

    #[test]
    fn test_store_prediction() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Evidence Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-25", &stories);

        let mut pred = make_prediction("ai", "active", Some("2026-06-01"));
        pred.evidence_story_ids = vec![sids[0]];

        let insight_id = store_prediction(&conn, &pred).unwrap();
        assert!(insight_id > 0);

        // Verify stored fields.
        let (itype, title, confidence, status, sector): (String, String, f64, String, Option<String>) =
            conn.query_row(
                "SELECT insight_type, title, confidence, status, sector
                 FROM insights WHERE id = ?1",
                params![insight_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();

        assert_eq!(itype, "prediction");
        assert_eq!(title, "Prediction about ai");
        assert!((confidence - 0.75).abs() < 1e-4);
        assert_eq!(status, "active");
        assert_eq!(sector.as_deref(), Some("ai"));

        // Verify evidence link.
        let ev_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM insight_evidence WHERE insight_id = ?1",
                params![insight_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ev_count, 1);
    }

    #[test]
    fn test_get_active_predictions() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "S1")];
        let (_bid, _sids) = seed_briefing(&conn, "2026-03-25", &stories);

        // Store 2 active + 1 expired.
        store_prediction(&conn, &make_prediction("ai", "active", None)).unwrap();
        store_prediction(&conn, &make_prediction("tech", "active", None)).unwrap();
        store_prediction(&conn, &make_prediction("miami", "expired", None)).unwrap();

        let active = get_active_predictions(&conn).unwrap();
        assert_eq!(active.len(), 2, "should return only 2 active predictions");

        for p in &active {
            assert_eq!(p.status, "active");
        }
    }

    #[test]
    fn test_expire_stale_predictions() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "S1")];
        let (_bid, _sids) = seed_briefing(&conn, "2026-03-25", &stories);

        // Store a prediction with a past date.
        let past_pred = make_prediction("ai", "active", Some("2025-01-01"));
        let past_id = store_prediction(&conn, &past_pred).unwrap();

        // Store a prediction with a future date.
        let future_pred = make_prediction("tech", "active", Some("2099-12-31"));
        let future_id = store_prediction(&conn, &future_pred).unwrap();

        let expired_count = expire_stale_predictions(&conn).unwrap();
        assert_eq!(expired_count, 1, "should expire exactly 1 prediction");

        // Verify the past prediction is now expired.
        let status: String = conn
            .query_row(
                "SELECT status FROM insights WHERE id = ?1",
                params![past_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "expired");

        // Verify the future prediction is still active.
        let status: String = conn
            .query_row(
                "SELECT status FROM insights WHERE id = ?1",
                params![future_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
    }

    #[test]
    fn test_prediction_stats() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "S1")];
        let (_bid, _sids) = seed_briefing(&conn, "2026-03-25", &stories);

        // Create a mix of statuses.
        store_prediction(&conn, &make_prediction("ai", "active", None)).unwrap();
        store_prediction(&conn, &make_prediction("ai", "active", None)).unwrap();
        store_prediction(&conn, &make_prediction("ai", "validated", None)).unwrap();
        store_prediction(&conn, &make_prediction("ai", "partially_validated", None)).unwrap();
        store_prediction(&conn, &make_prediction("ai", "invalidated", None)).unwrap();
        store_prediction(&conn, &make_prediction("ai", "expired", None)).unwrap();

        let stats = get_prediction_stats(&conn).unwrap();
        assert_eq!(stats.total, 6);
        assert_eq!(stats.active, 2);
        assert_eq!(stats.validated, 1);
        assert_eq!(stats.partially_validated, 1);
        assert_eq!(stats.invalidated, 1);
        assert_eq!(stats.expired, 1);

        // accuracy = (1 + 1) / (1 + 1 + 1 + 1) = 0.5
        assert!(
            (stats.accuracy_rate - 0.5).abs() < 1e-6,
            "accuracy_rate should be 0.5, got {}",
            stats.accuracy_rate
        );
    }
}
