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

/// Classify trajectory based on recent activity and acceleration.
pub fn classify_trajectory(window_7d: i64, acceleration: f64) -> &'static str {
    if window_7d == 0 {
        return "dormant";
    }
    if acceleration > 2.0 {
        "emerging"
    } else if acceleration > 1.3 {
        "growing"
    } else if acceleration >= 0.8 {
        "peaking"
    } else {
        "declining"
    }
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
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-90 days') THEN 1 ELSE 0 END) AS w90
         FROM entities e
         JOIN entity_mentions em ON em.entity_id = e.id
         WHERE em.mentioned_at >= date(?1, '-90 days')
         GROUP BY e.name, e.sector",
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, i64)> = stmt
        .query_map(params![today], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to query entity mention windows")?;

    let mut count = 0usize;

    for (topic, sector, w7, w30, w90) in &rows {
        let acc = compute_acceleration(*w7, *w30);
        let traj = classify_trajectory(*w7, acc);

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
        assert_eq!(classify_trajectory(0, 0.0), "dormant");
        assert_eq!(classify_trajectory(0, 5.0), "dormant");
    }

    #[test]
    fn test_classify_trajectory_emerging() {
        assert_eq!(classify_trajectory(10, 3.0), "emerging");
    }

    #[test]
    fn test_classify_trajectory_growing() {
        assert_eq!(classify_trajectory(5, 1.5), "growing");
    }

    #[test]
    fn test_classify_trajectory_peaking() {
        assert_eq!(classify_trajectory(5, 1.0), "peaking");
    }

    #[test]
    fn test_classify_trajectory_declining() {
        assert_eq!(classify_trajectory(5, 0.5), "declining");
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
             VALUES ('Fast', 'ai', 10, 5, 10, 5.0, 'emerging', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('Medium', 'tech', 5, 5, 10, 2.0, 'growing', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('Slow', 'ai', 3, 10, 20, 0.5, 'declining', datetime('now'))",
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
             VALUES ('A', 'ai', 10, 5, 10, 5.0, 'emerging', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('B', 'ai', 8, 4, 8, 3.0, 'emerging', datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('C', 'tech', 5, 5, 10, 1.0, 'peaking', datetime('now'))",
            [],
        ).unwrap();

        let emerging = get_signals_by_trajectory(&conn, "emerging", 10).unwrap();
        assert_eq!(emerging.len(), 2);
        assert_eq!(emerging[0].topic, "A");
        assert_eq!(emerging[1].topic, "B");

        let peaking = get_signals_by_trajectory(&conn, "peaking", 10).unwrap();
        assert_eq!(peaking.len(), 1);
        assert_eq!(peaking[0].topic, "C");
    }

    #[test]
    fn test_get_signal_case_insensitive() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();

        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES ('OpenAI', 'ai', 10, 5, 10, 5.0, 'emerging', datetime('now'))",
            [],
        ).unwrap();

        assert!(get_signal(&conn, "openai").unwrap().is_some());
        assert!(get_signal(&conn, "OPENAI").unwrap().is_some());
        assert!(get_signal(&conn, "OpenAI").unwrap().is_some());
    }
}
