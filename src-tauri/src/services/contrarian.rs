use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContrarianSignal {
    pub topic: String,
    pub consensus_sentiment: f64,
    pub consensus_strength: f64,
    pub total_stories: usize,
    pub dissenting_story_ids: Vec<i64>,
    pub dissent_count: usize,
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

/// Analyze a topic for consensus/contrarian signals.
///
/// Examines entity_mentions in the last `days` days. If the average sentiment
/// exceeds `threshold` in absolute value, a consensus exists. Stories whose
/// sentiment sign opposes the consensus are flagged as dissenting.
///
/// Returns `None` if no consensus exists (mixed sentiment or too few mentions).
pub fn detect_contrarian(
    conn: &Connection,
    topic: &str,
    days: i64,
    threshold: f64,
) -> Result<Option<ContrarianSignal>> {
    let normalized = topic.trim().to_lowercase();

    let mut stmt = conn.prepare(
        "SELECT em.story_id, em.sentiment
         FROM entity_mentions em
         JOIN entities e ON e.id = em.entity_id
         WHERE e.name_normalized = ?1
           AND em.mentioned_at >= date('now', ?2)",
    )?;

    let day_offset = format!("-{days} days");

    let rows: Vec<(i64, f64)> = stmt
        .query_map(params![normalized, day_offset], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to query entity mentions for contrarian detection")?;

    if rows.is_empty() {
        return Ok(None);
    }

    let n = rows.len() as f64;
    let sentiments: Vec<f64> = rows.iter().map(|(_, s)| *s).collect();

    // Compute average sentiment
    let avg_sentiment = sentiments.iter().sum::<f64>() / n;

    // No consensus if average is below threshold
    if avg_sentiment.abs() < threshold {
        return Ok(None);
    }

    // Compute standard deviation
    let variance = sentiments
        .iter()
        .map(|s| (s - avg_sentiment).powi(2))
        .sum::<f64>()
        / n;
    let std_dev = variance.sqrt();

    // consensus_strength: 1.0 means perfect agreement (std_dev=0),
    // 0.0 means maximum disagreement. Max possible std_dev for [-1,1] is 1.0.
    let consensus_strength = (1.0 - std_dev).max(0.0);

    // Dissenting stories: sentiment sign differs from the consensus
    let consensus_positive = avg_sentiment > 0.0;
    let dissenting_story_ids: Vec<i64> = rows
        .iter()
        .filter(|(_, s)| {
            if consensus_positive {
                *s < 0.0
            } else {
                *s > 0.0
            }
        })
        .map(|(id, _)| *id)
        .collect();

    let dissent_count = dissenting_story_ids.len();

    Ok(Some(ContrarianSignal {
        topic: topic.to_string(),
        consensus_sentiment: avg_sentiment,
        consensus_strength,
        total_stories: rows.len(),
        dissenting_story_ids,
        dissent_count,
    }))
}

/// Scan all topics for contrarian signals (proactive detection).
///
/// Examines all entities with at least `min_mentions` mentions in the last
/// `days` days, and returns contrarian signals for any that show strong
/// consensus with dissent.
pub fn scan_all_contrarian(
    conn: &Connection,
    days: i64,
    threshold: f64,
    min_mentions: usize,
) -> Result<Vec<ContrarianSignal>> {
    let day_offset = format!("-{days} days");

    // Find all entities with enough recent mentions
    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.name
         FROM entities e
         JOIN entity_mentions em ON em.entity_id = e.id
         WHERE em.mentioned_at >= date('now', ?1)
         GROUP BY e.name
         HAVING COUNT(*) >= ?2",
    )?;

    let topics: Vec<String> = stmt
        .query_map(params![day_offset, min_mentions as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to query topics for contrarian scan")?;

    let mut signals = Vec::new();
    for topic in &topics {
        if let Some(signal) = detect_contrarian(conn, topic, days, threshold)?
            && signal.dissent_count > 0 {
                signals.push(signal);
            }
    }

    // Sort by dissent_count descending (most interesting first)
    signals.sort_by_key(|s| std::cmp::Reverse(s.dissent_count));

    Ok(signals)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn seed_stories(conn: &Connection, date: &str, n: usize) -> Vec<i64> {
        conn.execute(
            "INSERT OR IGNORE INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status)
             VALUES (?1, ?2, 0, 0, 0, 0, 'complete')",
            params![date, n as i64],
        )
        .unwrap();
        let briefing_id: i64 = conn
            .query_row(
                "SELECT id FROM briefings WHERE date = ?1",
                params![date],
                |row| row.get(0),
            )
            .unwrap();

        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            conn.execute(
                "INSERT INTO stories (briefing_id, sector, original_title, original_url, headline, summary, key_facts, why_it_matters, what_to_watch, importance_score, is_hero, display_order, source_name, url_hash, title_hash, published_at)
                 VALUES (?1, 'ai', ?2, ?3, ?4, 'summary', '[]', 'matters', 'watch', 5, 0, ?5, 'test', ?6, ?7, ?8)",
                params![
                    briefing_id,
                    format!("Title {date}-{i}"),
                    format!("https://example.com/{date}/{i}"),
                    format!("Headline {date}-{i}"),
                    i as i64,
                    format!("contr_{date}_url_{i}"),
                    format!("contr_{date}_title_{i}"),
                    date,
                ],
            )
            .unwrap();
            ids.push(conn.last_insert_rowid());
        }
        ids
    }

    fn seed_entity(conn: &Connection, name: &str, date: &str) -> i64 {
        let normalized = name.trim().to_lowercase();
        conn.execute(
            "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen)
             VALUES (?1, ?2, 'topic', 'ai', ?3, ?3)",
            params![name, normalized, date],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_mention_with_sentiment(
        conn: &Connection,
        entity_id: i64,
        story_id: i64,
        sentiment: f64,
        date: &str,
    ) {
        conn.execute(
            "INSERT INTO entity_mentions (entity_id, story_id, sentiment, mentioned_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![entity_id, story_id, sentiment, date],
        )
        .unwrap();
    }

    /// The detect_contrarian function uses `date('now', ...)` which in tests
    /// returns today's actual date. To make tests deterministic, we use very
    /// recent dates and a large window. This helper overrides by inserting
    /// mentions that will always be "recent" relative to any test run date.
    ///
    /// For precise control, we use dates like "2099-01-01" and a huge window.
    /// But a simpler approach: just use `date('now')` as mention date.
    fn today_str(conn: &Connection) -> String {
        conn.query_row("SELECT date('now')", [], |row| row.get::<_, String>(0))
            .unwrap()
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_consensus_positive() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let today = today_str(&conn);

        let sids = seed_stories(&conn, &today, 5);
        let entity_id = seed_entity(&conn, "Bullish Corp", &today);

        // 5 mentions, all positive (0.7-0.9)
        let sentiments = [0.7, 0.75, 0.8, 0.85, 0.9];
        for (i, &s) in sentiments.iter().enumerate() {
            seed_mention_with_sentiment(&conn, entity_id, sids[i], s, &today);
        }

        let signal = detect_contrarian(&conn, "Bullish Corp", 30, 0.4)
            .unwrap()
            .expect("should detect consensus");

        assert!(
            (signal.consensus_sentiment - 0.8).abs() < 0.01,
            "avg sentiment should be ~0.8, got {}",
            signal.consensus_sentiment
        );
        assert_eq!(signal.total_stories, 5);
        assert_eq!(signal.dissent_count, 0, "no dissent when all positive");
        assert!(signal.consensus_strength > 0.9, "strong consensus expected");
    }

    #[test]
    fn test_detect_dissent() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let today = today_str(&conn);

        let sids = seed_stories(&conn, &today, 6);
        let entity_id = seed_entity(&conn, "Debated Topic", &today);

        // 5 positive + 1 negative
        for sid in sids.iter().take(5) {
            seed_mention_with_sentiment(&conn, entity_id, *sid, 0.7, &today);
        }
        seed_mention_with_sentiment(&conn, entity_id, sids[5], -0.5, &today);

        let signal = detect_contrarian(&conn, "Debated Topic", 30, 0.4)
            .unwrap()
            .expect("should detect consensus with dissent");

        assert!(signal.consensus_sentiment > 0.0, "consensus should be positive");
        assert_eq!(signal.dissent_count, 1, "should have 1 dissenter");
        assert_eq!(signal.dissenting_story_ids.len(), 1);
        assert_eq!(signal.dissenting_story_ids[0], sids[5]);
    }

    #[test]
    fn test_no_contrarian_if_mixed() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let today = today_str(&conn);

        let sids = seed_stories(&conn, &today, 6);
        let entity_id = seed_entity(&conn, "Mixed Corp", &today);

        // Sentiments spread from -0.5 to 0.5 → avg near 0
        let sentiments = [-0.5, -0.3, -0.1, 0.1, 0.3, 0.5];
        for (i, &s) in sentiments.iter().enumerate() {
            seed_mention_with_sentiment(&conn, entity_id, sids[i], s, &today);
        }

        let result = detect_contrarian(&conn, "Mixed Corp", 30, 0.4).unwrap();
        assert!(result.is_none(), "no consensus when sentiments are mixed");
    }

    #[test]
    fn test_no_contrarian_if_few_mentions() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        // No mentions at all
        let result = detect_contrarian(&conn, "Ghost Topic", 30, 0.4).unwrap();
        assert!(result.is_none(), "no signal for unknown topic");
    }

    #[test]
    fn test_detect_negative_consensus() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let today = today_str(&conn);

        let sids = seed_stories(&conn, &today, 4);
        let entity_id = seed_entity(&conn, "Bearish Corp", &today);

        // 3 negative + 1 positive dissenter
        seed_mention_with_sentiment(&conn, entity_id, sids[0], -0.8, &today);
        seed_mention_with_sentiment(&conn, entity_id, sids[1], -0.7, &today);
        seed_mention_with_sentiment(&conn, entity_id, sids[2], -0.9, &today);
        seed_mention_with_sentiment(&conn, entity_id, sids[3], 0.4, &today);

        let signal = detect_contrarian(&conn, "Bearish Corp", 30, 0.4)
            .unwrap()
            .expect("should detect negative consensus");

        assert!(signal.consensus_sentiment < 0.0, "consensus should be negative");
        assert_eq!(signal.dissent_count, 1, "positive story is the dissenter");
        assert_eq!(signal.dissenting_story_ids[0], sids[3]);
    }

    #[test]
    fn test_scan_all_contrarian() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let today = today_str(&conn);

        let sids = seed_stories(&conn, &today, 10);

        // Entity A: strong positive consensus with 1 dissenter
        let ea = seed_entity(&conn, "Consensus A", &today);
        for sid in sids.iter().take(4) {
            seed_mention_with_sentiment(&conn, ea, *sid, 0.8, &today);
        }
        seed_mention_with_sentiment(&conn, ea, sids[4], -0.5, &today);

        // Entity B: mixed sentiment (should NOT appear)
        let eb = seed_entity(&conn, "Mixed B", &today);
        seed_mention_with_sentiment(&conn, eb, sids[5], 0.3, &today);
        seed_mention_with_sentiment(&conn, eb, sids[6], -0.3, &today);
        seed_mention_with_sentiment(&conn, eb, sids[7], 0.1, &today);

        let signals = scan_all_contrarian(&conn, 30, 0.4, 3).unwrap();

        // Only Consensus A should appear (Mixed B has no consensus above threshold)
        assert_eq!(signals.len(), 1, "only 1 topic should have contrarian signal");
        assert_eq!(signals[0].topic, "Consensus A");
        assert_eq!(signals[0].dissent_count, 1);
    }
}
