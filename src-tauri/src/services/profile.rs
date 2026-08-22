use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    /// Topic/sector interest counts: "ai" -> 15, "miami" -> 7
    pub interests: HashMap<String, i64>,
    /// Most frequently asked-about entities, ordered by count descending
    pub top_entities: Vec<String>,
    /// The sector with the most engagement, if any
    pub primary_sector: Option<String>,
}

// ---------------------------------------------------------------------------
// Interest tracking
// ---------------------------------------------------------------------------

/// Increment interest counter for a topic or sector.
///
/// Keys are stored as `interest:<topic>`, e.g., `interest:ai`, `interest:miami`.
pub fn track_interest(conn: &Connection, key: &str) -> Result<()> {
    let db_key = format!("interest:{}", key.trim().to_lowercase());

    conn.execute(
        "INSERT INTO user_profile (key, value)
         VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET
             value = CAST(value AS INTEGER) + 1,
             scanned_at = datetime('now')",
        params![db_key],
    )
    .context("failed to track interest")?;

    Ok(())
}

/// Track entity mention in user queries.
///
/// Keys are stored as `query_entity:<entity>`, e.g., `query_entity:openai`.
pub fn track_entity_query(conn: &Connection, entity: &str) -> Result<()> {
    let db_key = format!("query_entity:{}", entity.trim().to_lowercase());

    conn.execute(
        "INSERT INTO user_profile (key, value)
         VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET
             value = CAST(value AS INTEGER) + 1,
             scanned_at = datetime('now')",
        params![db_key],
    )
    .context("failed to track entity query")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Profile retrieval
// ---------------------------------------------------------------------------

/// Get the full user profile aggregated from all tracked data.
pub fn get_profile(conn: &Connection) -> Result<UserProfile> {
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM user_profile
             WHERE key LIKE 'interest:%' OR key LIKE 'query_entity:%'",
        )
        .context("failed to prepare profile query")?;

    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut interests: HashMap<String, i64> = HashMap::new();
    let mut entity_counts: Vec<(String, i64)> = Vec::new();

    for (key, value) in &rows {
        let count: i64 = value.parse().unwrap_or(0);

        if let Some(topic) = key.strip_prefix("interest:") {
            interests.insert(topic.to_string(), count);
        } else if let Some(entity) = key.strip_prefix("query_entity:") {
            entity_counts.push((entity.to_string(), count));
        }
    }

    // Sort entities by count descending
    entity_counts.sort_by_key(|e| std::cmp::Reverse(e.1));
    let top_entities: Vec<String> = entity_counts.into_iter().map(|(name, _)| name).collect();

    // Determine primary sector from interests
    let primary_sector = interests
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(sector, _)| sector.clone());

    Ok(UserProfile {
        interests,
        top_entities,
        primary_sector,
    })
}

// ---------------------------------------------------------------------------
// Sector affinity for retrieval reranking
// ---------------------------------------------------------------------------

/// Compute sector affinity boost for retrieval reranking.
///
/// Returns a multiplier: 1.0 = no boost, >1.0 = boosted.
/// Capped at 1.3 to avoid over-biasing results.
pub fn sector_affinity(conn: &Connection, sector: &str) -> Result<f32> {
    let sector_key = format!("interest:{}", sector.trim().to_lowercase());

    // Get this sector's count
    let sector_count: i64 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM user_profile WHERE key = ?1",
            params![sector_key],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if sector_count == 0 {
        return Ok(1.0);
    }

    // Get total interest count across all sectors
    let total_count: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(CAST(value AS INTEGER)), 0) FROM user_profile
             WHERE key LIKE 'interest:%'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if total_count == 0 {
        return Ok(1.0);
    }

    let ratio = sector_count as f32 / total_count as f32;
    // Scales linearly: 0% share -> 1.0, 100% share -> 1.3
    let boost = 1.0 + 0.3 * ratio;
    Ok(boost.min(1.3))
}

// ---------------------------------------------------------------------------
// Profile formatting for system prompts
// ---------------------------------------------------------------------------

/// Format profile as a human-readable string for inclusion in system prompts.
///
/// Example output:
/// "Primary interest: ai (60% of queries). Also follows: miami (25%), tech (15%)"
/// "Frequently asks about: OpenAI, Anthropic, Claude"
pub fn profile_summary(profile: &UserProfile) -> String {
    if profile.interests.is_empty() && profile.top_entities.is_empty() {
        return "No user profile data yet.".to_string();
    }

    let mut parts = Vec::new();

    // Interest breakdown
    if !profile.interests.is_empty() {
        let total: i64 = profile.interests.values().sum();

        // Sort interests by count descending
        let mut sorted: Vec<(&String, &i64)> = profile.interests.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));

        if let Some((primary, _count)) = sorted.first() {
            let pct = if total > 0 {
                (*sorted.first().unwrap().1 as f64 / total as f64 * 100.0).round() as i64
            } else {
                0
            };
            let mut line = format!("Primary interest: {} ({}% of queries)", primary, pct);

            if sorted.len() > 1 {
                let others: Vec<String> = sorted[1..]
                    .iter()
                    .map(|(name, count)| {
                        let p =
                            if total > 0 { (**count as f64 / total as f64 * 100.0).round() as i64 } else { 0 };
                        format!("{} ({}%)", name, p)
                    })
                    .collect();
                line.push_str(&format!(". Also follows: {}", others.join(", ")));
            }

            parts.push(line);
        }
    }

    // Entity breakdown
    if !profile.top_entities.is_empty() {
        let display: Vec<&str> = profile.top_entities.iter().take(5).map(|s| s.as_str()).collect();
        parts.push(format!("Frequently asks about: {}", display.join(", ")));
    }

    parts.join("\n")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::test_db;

    #[test]
    fn test_track_interest_new() {
        let conn = test_db();
        track_interest(&conn, "ai").unwrap();

        let value: String = conn
            .query_row(
                "SELECT value FROM user_profile WHERE key = 'interest:ai'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "1");
    }

    #[test]
    fn test_track_interest_increment() {
        let conn = test_db();
        track_interest(&conn, "ai").unwrap();
        track_interest(&conn, "ai").unwrap();
        track_interest(&conn, "ai").unwrap();

        let value: String = conn
            .query_row(
                "SELECT value FROM user_profile WHERE key = 'interest:ai'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "3");
    }

    #[test]
    fn test_track_entity_query() {
        let conn = test_db();
        track_entity_query(&conn, "OpenAI").unwrap();
        track_entity_query(&conn, "OpenAI").unwrap();

        let value: String = conn
            .query_row(
                "SELECT value FROM user_profile WHERE key = 'query_entity:openai'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "2");
    }

    #[test]
    fn test_get_profile() {
        let conn = test_db();

        track_interest(&conn, "ai").unwrap();
        track_interest(&conn, "ai").unwrap();
        track_interest(&conn, "ai").unwrap();
        track_interest(&conn, "miami").unwrap();
        track_entity_query(&conn, "OpenAI").unwrap();
        track_entity_query(&conn, "Anthropic").unwrap();
        track_entity_query(&conn, "OpenAI").unwrap();

        let profile = get_profile(&conn).unwrap();

        assert_eq!(*profile.interests.get("ai").unwrap(), 3);
        assert_eq!(*profile.interests.get("miami").unwrap(), 1);
        assert_eq!(profile.top_entities[0], "openai", "OpenAI has 2 queries, should be first");
        assert_eq!(profile.top_entities[1], "anthropic");
        assert_eq!(profile.primary_sector, Some("ai".to_string()));
    }

    #[test]
    fn test_get_profile_empty() {
        let conn = test_db();
        let profile = get_profile(&conn).unwrap();

        assert!(profile.interests.is_empty());
        assert!(profile.top_entities.is_empty());
        assert_eq!(profile.primary_sector, None);
    }

    #[test]
    fn test_sector_affinity_boost() {
        let conn = test_db();

        // ai = 20 interactions, miami = 10 → total = 30
        for _ in 0..20 {
            track_interest(&conn, "ai").unwrap();
        }
        for _ in 0..10 {
            track_interest(&conn, "miami").unwrap();
        }

        let boost = sector_affinity(&conn, "ai").unwrap();
        assert!(
            boost > 1.0,
            "AI should have affinity boost, got {}",
            boost
        );
        assert!(
            boost <= 1.3,
            "boost should be capped at 1.3, got {}",
            boost
        );

        let miami_boost = sector_affinity(&conn, "miami").unwrap();
        assert!(
            miami_boost > 1.0,
            "Miami should also have a boost, got {}",
            miami_boost
        );
        assert!(
            boost > miami_boost,
            "AI boost ({}) should be higher than Miami boost ({})",
            boost,
            miami_boost
        );
    }

    #[test]
    fn test_sector_affinity_no_data() {
        let conn = test_db();
        let boost = sector_affinity(&conn, "ai").unwrap();
        assert!(
            (boost - 1.0).abs() < 1e-6,
            "empty profile should return 1.0, got {}",
            boost
        );
    }

    #[test]
    fn test_profile_summary() {
        let profile = UserProfile {
            interests: {
                let mut m = HashMap::new();
                m.insert("ai".to_string(), 60);
                m.insert("miami".to_string(), 25);
                m.insert("tech".to_string(), 15);
                m
            },
            top_entities: vec![
                "openai".to_string(),
                "anthropic".to_string(),
                "claude".to_string(),
            ],
            primary_sector: Some("ai".to_string()),
        };

        let summary = profile_summary(&profile);
        assert!(
            summary.contains("Primary interest: ai"),
            "should mention primary interest, got: {}",
            summary
        );
        assert!(
            summary.contains("60%"),
            "should show percentage, got: {}",
            summary
        );
        assert!(
            summary.contains("Frequently asks about:"),
            "should list entities, got: {}",
            summary
        );
        assert!(
            summary.contains("openai"),
            "should include top entity, got: {}",
            summary
        );
    }

    #[test]
    fn test_profile_summary_empty() {
        let profile = UserProfile {
            interests: HashMap::new(),
            top_entities: vec![],
            primary_sector: None,
        };

        let summary = profile_summary(&profile);
        assert_eq!(summary, "No user profile data yet.");
    }
}
