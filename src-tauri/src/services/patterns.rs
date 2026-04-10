use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossSectorPattern {
    pub source_sector: String,
    pub source_pattern: String,
    pub target_sector: String,
    pub predicted_pattern: String,
    pub current_stage: String,
    pub estimated_timeline: String,
    pub confidence: f32,
    pub source_evidence: Vec<i64>,
    pub target_evidence: Vec<i64>,
}

/// Per-sector aggregation for a single entity.
#[derive(Debug)]
struct SectorProfile {
    sector: String,
    mentions: usize,
    first_mention: String,
    last_mention: String,
    story_ids: Vec<i64>,
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Find entities/topics that appear in one sector first, then start appearing
/// in another. This detects cross-sector migration of themes.
///
/// Strategy:
/// 1. For each entity, aggregate per-sector mention counts and date ranges.
/// 2. The sector with the most mentions **and** the earliest first_mention is
///    the *source*.
/// 3. Any other sector with recent activity (within `recency_days`) and at
///    least `min_target_mentions` mentions is a *target*.
/// 4. If the source meets `min_source_mentions`, emit a `CrossSectorPattern`.
pub fn detect_cross_sector_patterns(
    conn: &Connection,
    min_source_mentions: usize,
    min_target_mentions: usize,
    recency_days: i64,
) -> Result<Vec<CrossSectorPattern>> {
    // Gather per-entity, per-sector statistics.
    //
    // We derive the sector from the story row (stories.sector), not from the
    // entity's own sector column, because the same entity can be mentioned in
    // stories belonging to different sectors.
    let mut stmt = conn.prepare(
        "SELECT e.id, e.name, e.entity_type, s.sector,
                COUNT(*) AS mentions,
                MIN(em.mentioned_at) AS first_mention,
                MAX(em.mentioned_at) AS last_mention,
                GROUP_CONCAT(DISTINCT em.story_id) AS story_ids
         FROM entities e
         JOIN entity_mentions em ON em.entity_id = e.id
         JOIN stories s ON s.id = em.story_id
         GROUP BY e.id, s.sector
         ORDER BY e.id",
    )?;

    // entity_id -> Vec<SectorProfile>
    let mut entity_profiles: HashMap<(i64, String, String), Vec<SectorProfile>> = HashMap::new();

    let rows = stmt.query_map([], |row| {
        let entity_id: i64 = row.get(0)?;
        let entity_name: String = row.get(1)?;
        let entity_type: String = row.get(2)?;
        let sector: String = row.get(3)?;
        let mentions: usize = row.get::<_, i64>(4)? as usize;
        let first_mention: String = row.get(5)?;
        let last_mention: String = row.get(6)?;
        let story_ids_csv: String = row.get(7)?;

        let story_ids: Vec<i64> = story_ids_csv
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        Ok((entity_id, entity_name, entity_type, SectorProfile {
            sector,
            mentions,
            first_mention,
            last_mention,
            story_ids,
        }))
    })?;

    for row in rows {
        let (id, name, etype, profile) = row?;
        entity_profiles
            .entry((id, name, etype))
            .or_default()
            .push(profile);
    }

    // Compute the recency cutoff date.
    let cutoff: String = conn.query_row(
        "SELECT date('now', ?1)",
        params![format!("-{recency_days} days")],
        |row| row.get(0),
    )?;

    let mut patterns = Vec::new();

    for ((_id, name, etype), profiles) in &entity_profiles {
        // Need at least 2 sectors to have a cross-sector pattern.
        if profiles.len() < 2 {
            continue;
        }

        // Identify source: sector with most mentions and earliest first_mention.
        let source = profiles
            .iter()
            .max_by(|a, b| {
                a.mentions
                    .cmp(&b.mentions)
                    .then_with(|| b.first_mention.cmp(&a.first_mention))
            });

        let source = match source {
            Some(s) if s.mentions >= min_source_mentions => s,
            _ => continue,
        };

        // Identify targets: other sectors with recent activity.
        for target in profiles {
            if target.sector == source.sector {
                continue;
            }
            if target.mentions < min_target_mentions {
                continue;
            }
            // Target must have recent activity.
            if target.first_mention < cutoff {
                continue;
            }

            // Estimate which stage the migration is in (1-5 based on target
            // mentions relative to source).
            let ratio = target.mentions as f32 / source.mentions as f32;
            let (stage, timeline) = if ratio < 0.1 {
                ("Stage 1 of 5", "6-12 months based on precedent")
            } else if ratio < 0.25 {
                ("Stage 2 of 5", "3-6 months based on precedent")
            } else if ratio < 0.5 {
                ("Stage 3 of 5", "1-3 months based on precedent")
            } else if ratio < 0.75 {
                ("Stage 4 of 5", "2-6 weeks based on precedent")
            } else {
                ("Stage 5 of 5", "Already converging")
            };

            // Confidence is higher when there's more evidence on both sides.
            let confidence = (0.3
                + 0.3 * (source.mentions as f32 / 20.0).min(1.0)
                + 0.2 * (target.mentions as f32 / 10.0).min(1.0)
                + 0.2 * ratio.min(1.0))
            .min(1.0);

            patterns.push(CrossSectorPattern {
                source_sector: source.sector.clone(),
                source_pattern: format!(
                    "{} ({}) established in {} with {} mentions",
                    name, etype, source.sector, source.mentions
                ),
                target_sector: target.sector.clone(),
                predicted_pattern: format!(
                    "{} ({}) emerging in {} with {} recent mentions",
                    name, etype, target.sector, target.mentions
                ),
                current_stage: stage.to_string(),
                estimated_timeline: timeline.to_string(),
                confidence,
                source_evidence: source.story_ids.clone(),
                target_evidence: target.story_ids.clone(),
            });
        }
    }

    // Sort by confidence descending for deterministic, useful ordering.
    patterns.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(patterns)
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Store a detected pattern as an insight in the ledger.
/// Returns the newly created `insight_id`.
pub fn store_pattern_insight(
    conn: &Connection,
    pattern: &CrossSectorPattern,
) -> Result<i64> {
    let title = format!(
        "{} pattern migrating from {} to {}",
        pattern.source_pattern.split_once(" (").map(|(n, _)| n).unwrap_or(&pattern.source_pattern),
        pattern.source_sector,
        pattern.target_sector,
    );

    let content = format!(
        "Source: {}\nTarget: {}\nStage: {}\nTimeline: {}",
        pattern.source_pattern,
        pattern.predicted_pattern,
        pattern.current_stage,
        pattern.estimated_timeline,
    );

    let evidence_json = serde_json::to_string(
        &pattern
            .source_evidence
            .iter()
            .chain(&pattern.target_evidence)
            .map(|sid| serde_json::json!({"story_id": sid, "reasoning": "pattern evidence"}))
            .collect::<Vec<_>>(),
    )?;

    conn.execute(
        "INSERT INTO insights (insight_type, title, content, confidence, evidence, sector, status)
         VALUES ('pattern', ?1, ?2, ?3, ?4, ?5, 'active')",
        params![
            title,
            content,
            pattern.confidence,
            evidence_json,
            pattern.target_sector,
        ],
    )
    .context("failed to insert pattern insight")?;

    let insight_id = conn.last_insert_rowid();

    // Link source evidence as 'trigger', target evidence as 'support'.
    for sid in &pattern.source_evidence {
        conn.execute(
            "INSERT INTO insight_evidence (insight_id, story_id, role) VALUES (?1, ?2, 'trigger')",
            params![insight_id, sid],
        )?;
    }
    for sid in &pattern.target_evidence {
        conn.execute(
            "INSERT INTO insight_evidence (insight_id, story_id, role) VALUES (?1, ?2, 'support')",
            params![insight_id, sid],
        )?;
    }

    Ok(insight_id)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;
    use crate::services::entities::{store_entities, ExtractedEntity};

    fn make_entity(name: &str, entity_type: &str, sentiment: f64) -> ExtractedEntity {
        ExtractedEntity {
            name: name.to_string(),
            entity_type: entity_type.to_string(),
            sentiment,
            context: format!("{name} mentioned in the news"),
        }
    }

    /// Seed an entity mention for a given story at a given date and sector.
    /// Uses the entity storage from the entities module.
    fn seed_mention(
        conn: &Connection,
        name: &str,
        entity_type: &str,
        story_id: i64,
        date: &str,
        sector: &str,
    ) {
        let entities = vec![make_entity(name, entity_type, 0.5)];
        store_entities(conn, &entities, story_id, date, sector).unwrap();
    }

    #[test]
    fn test_detect_cross_sector_pattern() {
        let conn = test_db();

        // Create tech stories (old — source sector)
        let tech_stories: Vec<TestStory> = (0..10)
            .map(|i| TestStory::new("tech", &format!("Tech AI Story {i}")))
            .collect();
        let (_bid1, tech_sids) = seed_briefing(&conn, "2026-01-15", &tech_stories);

        // Create AI stories (recent — target sector)
        let ai_stories: Vec<TestStory> = (0..3)
            .map(|i| TestStory::new("ai", &format!("AI Regulation Story {i}")))
            .collect();
        let (_bid2, ai_sids) = seed_briefing(&conn, "2026-03-25", &ai_stories);

        // Seed "AI regulation" with 10 mentions in tech (old dates)
        for (i, sid) in tech_sids.iter().enumerate() {
            seed_mention(
                &conn,
                "AI regulation",
                "topic",
                *sid,
                &format!("2026-01-{:02}", 10 + i),
                "tech",
            );
        }

        // Seed "AI regulation" with 3 mentions in AI (recent dates)
        for (i, sid) in ai_sids.iter().enumerate() {
            seed_mention(
                &conn,
                "AI regulation",
                "topic",
                *sid,
                &format!("2026-03-{:02}", 20 + i),
                "ai",
            );
        }

        let patterns = detect_cross_sector_patterns(&conn, 5, 2, 30).unwrap();

        assert!(
            !patterns.is_empty(),
            "should detect at least one cross-sector pattern"
        );

        let p = &patterns[0];
        assert_eq!(p.source_sector, "tech");
        assert_eq!(p.target_sector, "ai");
        assert!(p.confidence > 0.0);
        assert!(!p.source_evidence.is_empty());
        assert!(!p.target_evidence.is_empty());
        assert!(p.current_stage.contains("Stage"));
    }

    #[test]
    fn test_no_pattern_if_insufficient() {
        let conn = test_db();

        // Only create stories in one sector.
        let stories = vec![
            TestStory::new("tech", "Single Sector Story 1"),
            TestStory::new("tech", "Single Sector Story 2"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-25", &stories);

        seed_mention(&conn, "Quantum Computing", "topic", sids[0], "2026-03-25", "tech");
        seed_mention(&conn, "Quantum Computing", "topic", sids[1], "2026-03-26", "tech");

        let patterns = detect_cross_sector_patterns(&conn, 5, 2, 30).unwrap();
        assert!(
            patterns.is_empty(),
            "should not detect a pattern with entity in only one sector"
        );
    }

    #[test]
    fn test_store_pattern_insight() {
        let conn = test_db();

        // We need at least one story in DB for FK constraints.
        let stories = vec![
            TestStory::new("tech", "Evidence Story 1"),
            TestStory::new("ai", "Evidence Story 2"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-25", &stories);

        let pattern = CrossSectorPattern {
            source_sector: "tech".to_string(),
            source_pattern: "AI regulation (topic) established in tech with 10 mentions".to_string(),
            target_sector: "ai".to_string(),
            predicted_pattern: "AI regulation (topic) emerging in ai with 3 recent mentions"
                .to_string(),
            current_stage: "Stage 2 of 5".to_string(),
            estimated_timeline: "3-6 months based on precedent".to_string(),
            confidence: 0.72,
            source_evidence: vec![sids[0]],
            target_evidence: vec![sids[1]],
        };

        let insight_id = store_pattern_insight(&conn, &pattern).unwrap();
        assert!(insight_id > 0);

        // Verify the insight was stored correctly.
        let (itype, title, confidence, status): (String, String, f64, String) = conn
            .query_row(
                "SELECT insight_type, title, confidence, status FROM insights WHERE id = ?1",
                params![insight_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(itype, "pattern");
        assert!(title.contains("AI regulation"));
        assert!((confidence - 0.72).abs() < 1e-4);
        assert_eq!(status, "active");

        // Verify evidence links.
        let evidence_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM insight_evidence WHERE insight_id = ?1",
                params![insight_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(evidence_count, 2, "should have 1 trigger + 1 support");

        // Verify roles.
        let trigger_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM insight_evidence WHERE insight_id = ?1 AND role = 'trigger'",
                params![insight_id],
                |row| row.get(0),
            )
            .unwrap();
        let support_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM insight_evidence WHERE insight_id = ?1 AND role = 'support'",
                params![insight_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trigger_count, 1);
        assert_eq!(support_count, 1);
    }
}
