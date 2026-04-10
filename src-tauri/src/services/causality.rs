use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalChain {
    pub trigger_event: String,
    pub consequence: String,
    pub avg_delay_days: f32,
    pub occurrences: usize,
    pub confidence: f32,
    pub evidence: Vec<CausalEvidence>,
    pub last_observed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalEvidence {
    pub trigger_story_id: i64,
    pub consequence_story_id: i64,
    pub trigger_date: String,
    pub consequence_date: String,
    pub delay_days: i32,
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

/// Find causal chains for a given entity/topic.
///
/// Looks for patterns where stories mentioning `trigger_topic` are followed
/// by stories mentioning OTHER entities within `max_delay_days`. Groups by
/// consequence entity and filters by `min_occurrences`.
///
/// Returns chains ordered by confidence descending.
pub fn find_causal_chains(
    conn: &Connection,
    topic: &str,
    max_delay_days: i32,
    min_occurrences: usize,
) -> Result<Vec<CausalChain>> {
    let normalized = topic.trim().to_lowercase();

    // Count total trigger events for confidence calculation
    let total_triggers: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT em.story_id)
             FROM entity_mentions em
             JOIN entities e ON e.id = em.entity_id
             WHERE e.name_normalized = ?1",
            params![normalized],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if total_triggers == 0 {
        return Ok(vec![]);
    }

    // Find all (trigger mention, consequence mention) pairs where:
    // - trigger entity matches the topic
    // - consequence entity is different
    // - consequence story date is 1..max_delay_days after trigger story date
    let mut stmt = conn.prepare(
        "SELECT e2.name AS consequence_name,
                s1.id AS trigger_sid,
                s2.id AS consequence_sid,
                s1.published_at AS trigger_date,
                s2.published_at AS consequence_date,
                CAST(julianday(s2.published_at) - julianday(s1.published_at) AS INTEGER) AS delay
         FROM entity_mentions em1
         JOIN entities e1 ON e1.id = em1.entity_id
         JOIN stories s1 ON s1.id = em1.story_id
         JOIN entity_mentions em2 ON em2.entity_id != em1.entity_id
         JOIN entities e2 ON e2.id = em2.entity_id
         JOIN stories s2 ON s2.id = em2.story_id
         WHERE e1.name_normalized = ?1
           AND s2.published_at > s1.published_at
           AND julianday(s2.published_at) - julianday(s1.published_at) BETWEEN 1 AND ?2",
    )?;

    // Group by consequence entity
    let mut chains_map: HashMap<String, Vec<CausalEvidence>> = HashMap::new();

    let rows = stmt.query_map(params![normalized, max_delay_days], |row| {
        Ok((
            row.get::<_, String>(0)?,       // consequence_name
            row.get::<_, i64>(1)?,          // trigger_sid
            row.get::<_, i64>(2)?,          // consequence_sid
            row.get::<_, String>(3)?,       // trigger_date
            row.get::<_, String>(4)?,       // consequence_date
            row.get::<_, i32>(5)?,          // delay
        ))
    })?;

    for row in rows {
        let (consequence_name, trigger_sid, consequence_sid, trigger_date, consequence_date, delay) =
            row.context("failed to read causal pair row")?;

        chains_map
            .entry(consequence_name)
            .or_default()
            .push(CausalEvidence {
                trigger_story_id: trigger_sid,
                consequence_story_id: consequence_sid,
                trigger_date,
                consequence_date,
                delay_days: delay,
            });
    }

    // Build CausalChain structs, filtering by min_occurrences
    let today_str = conn
        .query_row("SELECT date('now')", [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|_| String::from("2026-03-31"));

    let mut chains: Vec<CausalChain> = chains_map
        .into_iter()
        .filter(|(_, evidence)| evidence.len() >= min_occurrences)
        .map(|(consequence, evidence)| {
            let occurrences = evidence.len();
            let avg_delay: f32 =
                evidence.iter().map(|e| e.delay_days as f32).sum::<f32>() / occurrences as f32;

            let last_observed = evidence
                .iter()
                .map(|e| e.consequence_date.as_str())
                .max()
                .unwrap_or("")
                .to_string();

            let last_observed_days_ago = conn
                .query_row(
                    "SELECT CAST(julianday(?1) - julianday(?2) AS INTEGER)",
                    params![today_str, last_observed],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0)
                .max(0);

            let confidence =
                compute_confidence(occurrences, total_triggers as usize, last_observed_days_ago);

            CausalChain {
                trigger_event: topic.to_string(),
                consequence,
                avg_delay_days: avg_delay,
                occurrences,
                confidence,
                evidence,
                last_observed,
            }
        })
        .collect();

    // Sort by confidence descending
    chains.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));

    Ok(chains)
}

/// Compute confidence for a causal chain.
///
/// Base confidence = occurrences / total_triggers.
/// Recency weight decays chains not observed recently (half-life ~90 days).
fn compute_confidence(occurrences: usize, total_triggers: usize, last_observed_days_ago: i64) -> f32 {
    let base = occurrences as f32 / total_triggers.max(1) as f32;
    let recency = 1.0 / (1.0 + (last_observed_days_ago as f32 / 90.0));
    (base * recency).min(1.0)
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
                    format!("causal_{date}_url_{i}"),
                    format!("causal_{date}_title_{i}"),
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

    fn seed_mention(conn: &Connection, entity_id: i64, story_id: i64, date: &str) {
        conn.execute(
            "INSERT INTO entity_mentions (entity_id, story_id, sentiment, mentioned_at)
             VALUES (?1, ?2, 0.0, ?3)",
            params![entity_id, story_id, date],
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_causal_chain() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        // Create stories on days 1, 5, 30, 35
        let sids_d1 = seed_stories(&conn, "2026-03-01", 1);
        let sids_d5 = seed_stories(&conn, "2026-03-05", 1);
        let sids_d20 = seed_stories(&conn, "2026-03-20", 1);
        let sids_d25 = seed_stories(&conn, "2026-03-25", 1);

        let entity_a = seed_entity(&conn, "Trigger Corp", "2026-03-01");
        let entity_b = seed_entity(&conn, "Consequence Inc", "2026-03-05");

        // Pattern: A mentioned on day 1 → B mentioned on day 5 (delay=4)
        seed_mention(&conn, entity_a, sids_d1[0], "2026-03-01");
        seed_mention(&conn, entity_b, sids_d5[0], "2026-03-05");

        // Pattern repeats: A mentioned on day 20 → B mentioned on day 25 (delay=5)
        seed_mention(&conn, entity_a, sids_d20[0], "2026-03-20");
        seed_mention(&conn, entity_b, sids_d25[0], "2026-03-25");

        let chains = find_causal_chains(&conn, "Trigger Corp", 30, 2).unwrap();
        assert_eq!(chains.len(), 1, "should find 1 causal chain");

        let chain = &chains[0];
        assert_eq!(chain.trigger_event, "Trigger Corp");
        assert_eq!(chain.consequence, "Consequence Inc");
        // 3 pairs: A(day1)->B(day5), A(day1)->B(day25), A(day20)->B(day25)
        assert_eq!(chain.occurrences, 3);
        assert!(chain.confidence > 0.0);
        assert_eq!(chain.evidence.len(), 3);
    }

    #[test]
    fn test_no_chain_if_insufficient() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        let sids_d1 = seed_stories(&conn, "2026-03-01", 1);
        let sids_d5 = seed_stories(&conn, "2026-03-05", 1);

        let entity_a = seed_entity(&conn, "Solo Trigger", "2026-03-01");
        let entity_b = seed_entity(&conn, "Solo Follow", "2026-03-05");

        // Only 1 occurrence
        seed_mention(&conn, entity_a, sids_d1[0], "2026-03-01");
        seed_mention(&conn, entity_b, sids_d5[0], "2026-03-05");

        let chains = find_causal_chains(&conn, "Solo Trigger", 30, 2).unwrap();
        assert!(chains.is_empty(), "should find no chains with min_occurrences=2");
    }

    #[test]
    fn test_confidence_increases_with_occurrences() {
        // Pure function test
        let conf_1 = compute_confidence(1, 5, 0);
        let conf_3 = compute_confidence(3, 5, 0);
        assert!(
            conf_3 > conf_1,
            "more occurrences should yield higher confidence: {} vs {}",
            conf_3,
            conf_1
        );
    }

    #[test]
    fn test_confidence_decreases_with_age() {
        let recent = compute_confidence(3, 5, 0);
        let old = compute_confidence(3, 5, 180);
        assert!(
            recent > old,
            "recent observation should have higher confidence: {} vs {}",
            recent,
            old
        );
    }

    #[test]
    fn test_no_chain_for_unknown_topic() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let chains = find_causal_chains(&conn, "Nonexistent", 30, 1).unwrap();
        assert!(chains.is_empty());
    }

    #[test]
    fn test_chain_respects_max_delay() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        let sids_d1 = seed_stories(&conn, "2026-03-01", 1);
        let sids_d5 = seed_stories(&conn, "2026-03-05", 1);
        let sids_d10 = seed_stories(&conn, "2026-03-10", 1);
        let sids_d15 = seed_stories(&conn, "2026-03-15", 1);

        let entity_a = seed_entity(&conn, "Quick Trigger", "2026-03-01");
        let entity_b = seed_entity(&conn, "Quick Follow", "2026-03-05");

        // Two occurrences: delay=4 and delay=5
        seed_mention(&conn, entity_a, sids_d1[0], "2026-03-01");
        seed_mention(&conn, entity_b, sids_d5[0], "2026-03-05");
        seed_mention(&conn, entity_a, sids_d10[0], "2026-03-10");
        seed_mention(&conn, entity_b, sids_d15[0], "2026-03-15");

        // max_delay=3 should exclude both (delays are 4 and 5)
        let chains = find_causal_chains(&conn, "Quick Trigger", 3, 1).unwrap();
        assert!(
            chains.is_empty(),
            "delays of 4-5 days should be excluded with max_delay=3"
        );

        // max_delay=5 should include both
        let chains = find_causal_chains(&conn, "Quick Trigger", 5, 2).unwrap();
        assert_eq!(chains.len(), 1, "delays of 4-5 should pass with max_delay=5");
    }
}
