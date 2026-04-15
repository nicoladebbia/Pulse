use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id: i64,
    pub topic: String,
    pub sector: Option<String>,
    pub window_7d: i64,
    pub window_30d: i64,
    pub window_90d: i64,
    pub acceleration: f64,
    pub trajectory: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// Pure computation
// ---------------------------------------------------------------------------

/// Compute acceleration from window counts.
///
/// Acceleration = (7d mention rate) / (30d mention rate).
/// A value > 1 means the topic is accelerating; < 1 means decelerating.
pub fn compute_acceleration(window_7d: i64, window_30d: i64) -> f64 {
    if window_30d == 0 {
        return if window_7d > 0 { 10.0 } else { 0.0 };
    }
    let rate_7d = window_7d as f64 / 7.0;
    let rate_30d = window_30d as f64 / 30.0;
    if rate_30d < 0.001 {
        return if window_7d > 0 { 10.0 } else { 0.0 };
    }
    rate_7d / rate_30d
}

/// Classify trajectory based on volume, consistency, and momentum.
///
/// Volume-based tiers:
/// - **dominant**: 14+ mentions AND 10+ active days — this entity dominates coverage
/// - **hot**: 7+ mentions AND 5+ active days — significant and sustained attention
/// - **rising**: 3+ mentions with positive acceleration — gaining momentum
/// - **fading**: mentions exist but acceleration < 0.8 — losing steam
/// - **dormant**: no recent mentions
///
/// `days_active` is the count of distinct days with mentions in the window.
pub fn classify_trajectory(window_7d: i64, window_30d: i64, acceleration: f64, days_active: i64) -> &'static str {
    if window_7d == 0 && window_30d == 0 {
        return "dormant";
    }

    let total = window_30d.max(window_7d);

    // Dominant: heavy coverage, sustained over many days
    if total >= 14 && days_active >= 10 {
        return "dominant";
    }

    // Hot: solid coverage across multiple days
    if total >= 7 && days_active >= 5 {
        return "hot";
    }

    // Fading: has mentions but decelerating
    if acceleration < 0.8 && total >= 3 {
        return "fading";
    }

    // Rising: has some mentions and accelerating
    if total >= 3 || days_active >= 2 {
        return "rising";
    }

    // Single mention — too thin to classify
    if window_7d > 0 {
        return "rising";
    }

    "dormant"
}

// ---------------------------------------------------------------------------
// Database operations
// ---------------------------------------------------------------------------

/// Recompute all signals from entity_mentions data.
///
/// For each entity with mentions in the last 90 days (relative to `today`),
/// counts mentions in 7d/30d/90d windows, computes acceleration and trajectory,
/// and upserts into the `signals` table.
///
/// Returns the number of signals upserted.
pub fn recompute_signals(conn: &Connection, today: &str) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT e.name, e.sector,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-7 days') THEN 1 ELSE 0 END) AS w7,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-30 days') THEN 1 ELSE 0 END) AS w30,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-90 days') THEN 1 ELSE 0 END) AS w90,
            COUNT(DISTINCT em.mentioned_at) AS days_active
         FROM entities e
         JOIN entity_mentions em ON em.entity_id = e.id
         WHERE em.mentioned_at >= date(?1, '-90 days')
         GROUP BY e.name, e.sector",
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, i64, i64)> = stmt
        .query_map(params![today], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to query entity mention windows")?;

    let mut count = 0usize;

    // Check if source_diversity column exists (added by migration 017)
    let has_source_diversity = {
        let mut pragma = conn.prepare("PRAGMA table_info(signals)")?;
        pragma
            .query_map([], |row| row.get::<_, String>(1))?
            .any(|r| r.as_deref() == Ok("source_diversity"))
    };

    for (topic, sector, w7, w30, w90, days_active) in &rows {
        let acc = compute_acceleration(*w7, *w30);
        let traj = classify_trajectory(*w7, *w30, acc, *days_active);

        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(topic, sector) DO UPDATE SET
                 window_7d = excluded.window_7d,
                 window_30d = excluded.window_30d,
                 window_90d = excluded.window_90d,
                 acceleration = excluded.acceleration,
                 trajectory = excluded.trajectory,
                 updated_at = datetime('now')",
            params![topic, sector, w7, w30, w90, acc, traj],
        )
        .context("failed to upsert signal")?;

        // Compute source diversity: count distinct feed_ids mentioning this entity in last 7 days
        if has_source_diversity {
            let diversity: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT s.feed_id) FROM entity_mentions em
                 JOIN stories s ON em.story_id = s.id
                 JOIN entities e ON em.entity_id = e.id
                 WHERE LOWER(e.name) = LOWER(?1) AND em.mentioned_at >= date(?2, '-7 days')",
                params![topic, today],
                |row| row.get(0),
            ).unwrap_or(0);

            // Insider buy volume — weighted by trade classification signal_weight
            let insider_vol: f64 = conn.query_row(
                "SELECT COALESCE(SUM(
                    CASE WHEN json_valid(s.financial_metadata)
                         AND json_extract(s.financial_metadata, '$.filing_type') IN ('4', 'form4')
                    THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.total_value') AS REAL), 0)
                         * COALESCE(CAST(json_extract(s.financial_metadata, '$.signal_weight') AS REAL), 0.0)
                    ELSE 0 END
                ), 0) FROM entity_mentions em
                JOIN stories s ON em.story_id = s.id
                JOIN entities e ON em.entity_id = e.id
                WHERE LOWER(e.name) = LOWER(?1) AND s.source_type = 'financial'
                  AND em.mentioned_at >= date(?2, '-30 days')",
                params![topic, today],
                |row| row.get(0),
            ).unwrap_or(0.0);

            let contract_val: f64 = conn.query_row(
                "SELECT COALESCE(SUM(
                    CASE WHEN json_valid(s.financial_metadata)
                         AND s.feed_id = 'usaspending'
                    THEN COALESCE(json_extract(s.financial_metadata, '$.amount'), 0)
                    ELSE 0 END
                ), 0) FROM entity_mentions em
                JOIN stories s ON em.story_id = s.id
                JOIN entities e ON em.entity_id = e.id
                WHERE LOWER(e.name) = LOWER(?1) AND s.source_type = 'financial'
                  AND em.mentioned_at >= date(?2, '-90 days')",
                params![topic, today],
                |row| row.get(0),
            ).unwrap_or(0.0);

            // Lobbying spend (LDA)
            let lobby_spend: f64 = conn.query_row(
                "SELECT COALESCE(SUM(
                    CASE WHEN json_valid(s.financial_metadata)
                         AND (s.source_name LIKE '%LDA%' OR s.source_name LIKE '%Senate%')
                    THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.amount') AS REAL), 0)
                    ELSE 0 END
                ), 0) FROM entity_mentions em
                JOIN stories s ON em.story_id = s.id
                JOIN entities e ON em.entity_id = e.id
                WHERE LOWER(e.name) = LOWER(?1) AND s.source_type = 'financial'
                  AND em.mentioned_at >= date(?2, '-90 days')",
                params![topic, today],
                |row| row.get(0),
            ).unwrap_or(0.0);

            // Regulatory mentions (Federal Register)
            let reg_count: f64 = conn.query_row(
                "SELECT COUNT(*) FROM entity_mentions em
                 JOIN stories s ON em.story_id = s.id
                 JOIN entities e ON em.entity_id = e.id
                 WHERE LOWER(e.name) = LOWER(?1) AND s.source_name LIKE '%Federal Register%'
                   AND em.mentioned_at >= date(?2, '-30 days')",
                params![topic, today],
                |row| row.get::<_, i64>(0),
            ).unwrap_or(0) as f64;

            // Patent filings (USPTO)
            let patent_count: f64 = conn.query_row(
                "SELECT COUNT(*) FROM entity_mentions em
                 JOIN stories s ON em.story_id = s.id
                 JOIN entities e ON em.entity_id = e.id
                 WHERE LOWER(e.name) = LOWER(?1) AND s.source_name = 'USPTO'
                   AND em.mentioned_at >= date(?2, '-30 days')",
                params![topic, today],
                |row| row.get::<_, i64>(0),
            ).unwrap_or(0) as f64;

            conn.execute(
                "UPDATE signals SET source_diversity = ?1, insider_buy_volume = ?2, contract_value = ?3,
                     lobbying_spend_delta = ?4, regulatory_sentiment = ?5, patent_filing_rate = ?6
                 WHERE topic = ?7 AND (sector = ?8 OR (?8 IS NULL AND sector IS NULL))",
                params![diversity, insider_vol, contract_val, lobby_spend, reg_count, patent_count, topic, sector],
            ).ok();
        }

        count += 1;
    }

    Ok(count)
}

/// Get signal for a specific topic (case-insensitive).
pub fn get_signal(conn: &Connection, topic: &str) -> Result<Option<Signal>> {
    let normalized = topic.trim().to_lowercase();

    let mut stmt = conn.prepare(
        "SELECT id, topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at
         FROM signals
         WHERE lower(topic) = ?1",
    )?;

    let mut rows = stmt.query_map(params![normalized], |row| {
        Ok(Signal {
            id: row.get(0)?,
            topic: row.get(1)?,
            sector: row.get(2)?,
            window_7d: row.get(3)?,
            window_30d: row.get(4)?,
            window_90d: row.get(5)?,
            acceleration: row.get(6)?,
            trajectory: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;

    match rows.next() {
        Some(Ok(signal)) => Ok(Some(signal)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Get signals filtered by trajectory, ordered by acceleration descending.
pub fn get_signals_by_trajectory(
    conn: &Connection,
    trajectory: &str,
    limit: usize,
) -> Result<Vec<Signal>> {
    let mut stmt = conn.prepare(
        "SELECT id, topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at
         FROM signals
         WHERE trajectory = ?1
         ORDER BY acceleration DESC
         LIMIT ?2",
    )?;

    let signals = stmt
        .query_map(params![trajectory, limit as i64], |row| {
            Ok(Signal {
                id: row.get(0)?,
                topic: row.get(1)?,
                sector: row.get(2)?,
                window_7d: row.get(3)?,
                window_30d: row.get(4)?,
                window_90d: row.get(5)?,
                acceleration: row.get(6)?,
                trajectory: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(signals)
}

/// Get top accelerating signals across all topics, ordered by acceleration descending.
pub fn get_top_accelerating(conn: &Connection, limit: usize) -> Result<Vec<Signal>> {
    let mut stmt = conn.prepare(
        "SELECT id, topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at
         FROM signals
         WHERE trajectory != 'dormant'
         ORDER BY acceleration DESC
         LIMIT ?1",
    )?;

    let signals = stmt
        .query_map(params![limit as i64], |row| {
            Ok(Signal {
                id: row.get(0)?,
                topic: row.get(1)?,
                sector: row.get(2)?,
                window_7d: row.get(3)?,
                window_30d: row.get(4)?,
                window_90d: row.get(5)?,
                acceleration: row.get(6)?,
                trajectory: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

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

    /// Seed a briefing + N stories, returning story IDs.
    fn seed_stories(conn: &Connection, date: &str, n: usize) -> Vec<i64> {
        conn.execute(
            "INSERT INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status)
             VALUES (?1, ?2, 0, 0, 0, 0, 'complete')",
            params![date, n as i64],
        )
        .unwrap();
        let briefing_id = conn.last_insert_rowid();

        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            conn.execute(
                "INSERT INTO stories (briefing_id, sector, original_title, original_url, headline, summary, key_facts, why_it_matters, what_to_watch, importance_score, is_hero, display_order, source_name, url_hash, title_hash, published_at)
                 VALUES (?1, 'ai', ?2, ?3, ?4, 'summary', '[]', 'matters', 'watch', 5, 0, ?5, 'test', ?6, ?7, ?8)",
                params![
                    briefing_id,
                    format!("Title {i}"),
                    format!("https://example.com/{i}"),
                    format!("Headline {i}"),
                    i as i64,
                    format!("{date}_url_{i}"),
                    format!("{date}_title_{i}"),
                    date,
                ],
            )
            .unwrap();
            ids.push(conn.last_insert_rowid());
        }
        ids
    }

    /// Insert an entity and return its ID.
    fn seed_entity(conn: &Connection, name: &str, sector: &str, date: &str) -> i64 {
        let normalized = name.trim().to_lowercase();
        conn.execute(
            "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen)
             VALUES (?1, ?2, 'topic', ?3, ?4, ?4)",
            params![name, normalized, sector, date],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Insert an entity mention on a given date, referencing an existing story.
    fn seed_mention(conn: &Connection, entity_id: i64, story_id: i64, date: &str) {
        conn.execute(
            "INSERT INTO entity_mentions (entity_id, story_id, sentiment, mentioned_at)
             VALUES (?1, ?2, 0.0, ?3)",
            params![entity_id, story_id, date],
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // Pure function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_acceleration_emerging() {
        // 7d=14, 30d=15 → rate_7d = 2.0, rate_30d = 0.5 → acc = 4.0
        let acc = compute_acceleration(14, 15);
        assert!((acc - 4.0).abs() < 0.01, "expected ~4.0, got {acc}");
    }

    #[test]
    fn test_compute_acceleration_declining() {
        // 7d=1, 30d=30 → rate_7d = 1/7 ≈ 0.143, rate_30d = 1.0 → acc ≈ 0.143
        let acc = compute_acceleration(1, 30);
        assert!((acc - (1.0 / 7.0)).abs() < 0.01, "expected ~0.143, got {acc}");
    }

    #[test]
    fn test_compute_acceleration_stable() {
        // 7d=7, 30d=30 → rate_7d = 1.0, rate_30d = 1.0 → acc = 1.0
        let acc = compute_acceleration(7, 30);
        assert!((acc - 1.0).abs() < 0.01, "expected ~1.0, got {acc}");
    }

    #[test]
    fn test_compute_acceleration_zero_30d() {
        // 7d=5, 30d=0 → cap at 10.0
        let acc = compute_acceleration(5, 0);
        assert!((acc - 10.0).abs() < 0.01, "expected 10.0, got {acc}");
    }

    #[test]
    fn test_compute_acceleration_both_zero() {
        let acc = compute_acceleration(0, 0);
        assert!((acc - 0.0).abs() < 0.01, "expected 0.0, got {acc}");
    }

    #[test]
    fn test_classify_trajectory_dormant() {
        assert_eq!(classify_trajectory(0, 0, 0.0, 0), "dormant");
    }

    #[test]
    fn test_classify_trajectory_dominant() {
        // 29 mentions in 7d, 49 in 30d, 14 days active → dominant
        assert_eq!(classify_trajectory(29, 49, 2.5, 14), "dominant");
    }

    #[test]
    fn test_classify_trajectory_hot() {
        // 8 mentions, 5 days active → hot
        assert_eq!(classify_trajectory(8, 16, 2.1, 5), "hot");
        // 12 mentions, 9 days → hot
        assert_eq!(classify_trajectory(12, 18, 2.9, 9), "hot");
    }

    #[test]
    fn test_classify_trajectory_rising() {
        // 3 mentions, 2 days, accelerating → rising
        assert_eq!(classify_trajectory(3, 3, 4.3, 2), "rising");
        // 1 mention, 1 day → rising (single but recent)
        assert_eq!(classify_trajectory(1, 1, 4.3, 1), "rising");
    }

    #[test]
    fn test_classify_trajectory_fading() {
        // Has mentions but decelerating — must not satisfy hot (total>=7 && days>=5)
        assert_eq!(classify_trajectory(1, 5, 0.5, 3), "fading");
        // Also fading with more mentions but low acceleration
        assert_eq!(classify_trajectory(2, 8, 0.7, 4), "fading");
    }

    // -----------------------------------------------------------------------
    // DB tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_recompute_signals() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        // Seed stories across multiple dates
        let sids_recent = seed_stories(&conn, "2026-03-30", 3);
        let sids_older = seed_stories(&conn, "2026-03-10", 2);
        let sids_old = seed_stories(&conn, "2026-01-15", 2);

        // Entity A: heavy recent activity (3 in last 7d, 5 in 30d, 7 in 90d)
        let entity_a = seed_entity(&conn, "Rust Lang", "tech", "2026-01-15");
        for &sid in &sids_recent {
            seed_mention(&conn, entity_a, sid, "2026-03-30");
        }
        for &sid in &sids_older {
            seed_mention(&conn, entity_a, sid, "2026-03-10");
        }
        for &sid in &sids_old {
            seed_mention(&conn, entity_a, sid, "2026-01-15");
        }

        let today = "2026-03-31";
        let count = recompute_signals(&conn, today).unwrap();
        assert_eq!(count, 1, "should produce 1 signal");

        let sig = get_signal(&conn, "Rust Lang").unwrap().expect("signal should exist");
        assert_eq!(sig.window_7d, 3);
        assert_eq!(sig.window_30d, 5);
        assert_eq!(sig.window_90d, 7);
        assert!(sig.acceleration > 1.0, "should be accelerating");
    }

    #[test]
    fn test_recompute_signals_multiple_entities() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let today = "2026-03-31";

        let sids = seed_stories(&conn, "2026-03-30", 4);

        // Entity A: 2 recent mentions
        let ea = seed_entity(&conn, "Alpha", "ai", "2026-03-30");
        seed_mention(&conn, ea, sids[0], "2026-03-30");
        seed_mention(&conn, ea, sids[1], "2026-03-29");

        // Entity B: 1 older mention
        let eb = seed_entity(&conn, "Beta", "tech", "2026-03-01");
        seed_mention(&conn, eb, sids[2], "2026-03-01");

        let count = recompute_signals(&conn, today).unwrap();
        assert_eq!(count, 2, "should produce 2 signals");

        let sig_a = get_signal(&conn, "Alpha").unwrap().unwrap();
        assert_eq!(sig_a.window_7d, 2);

        let sig_b = get_signal(&conn, "Beta").unwrap().unwrap();
        assert_eq!(sig_b.window_7d, 0);
        assert_eq!(sig_b.window_30d, 1);
    }

    #[test]
    fn test_get_top_accelerating() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        // Directly insert signals with known accelerations
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('Fast', 'ai', 10, 5, 10, 5.0, 'rising', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('Medium', 'tech', 5, 5, 10, 2.0, 'hot', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('Slow', 'ai', 3, 10, 20, 0.5, 'fading', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('Dead', NULL, 0, 0, 5, 0.0, 'dormant', datetime('now'))",
            [],
        ).unwrap();

        let top = get_top_accelerating(&conn, 10).unwrap();
        assert_eq!(top.len(), 3, "dormant should be excluded");
        assert_eq!(top[0].topic, "Fast");
        assert_eq!(top[1].topic, "Medium");
        assert_eq!(top[2].topic, "Slow");
    }

    #[test]
    fn test_get_signals_by_trajectory() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('A', 'ai', 10, 5, 10, 5.0, 'rising', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('B', 'ai', 8, 4, 8, 3.0, 'rising', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('C', 'tech', 5, 5, 10, 1.0, 'dominant', datetime('now'))",
            [],
        ).unwrap();

        let rising = get_signals_by_trajectory(&conn, "rising", 10).unwrap();
        assert_eq!(rising.len(), 2);
        assert_eq!(rising[0].topic, "A");
        assert_eq!(rising[1].topic, "B");

        let dominant = get_signals_by_trajectory(&conn, "dominant", 10).unwrap();
        assert_eq!(dominant.len(), 1);
        assert_eq!(dominant[0].topic, "C");
    }

    #[test]
    fn test_get_signal_case_insensitive() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('OpenAI', 'ai', 10, 5, 10, 5.0, 'rising', datetime('now'))",
            [],
        ).unwrap();

        assert!(get_signal(&conn, "openai").unwrap().is_some());
        assert!(get_signal(&conn, "OPENAI").unwrap().is_some());
        assert!(get_signal(&conn, "OpenAI").unwrap().is_some());
    }
}
