use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRelationship {
    pub id: i64,
    pub entity_a_id: i64,
    pub entity_b_id: i64,
    pub entity_a_name: String,
    pub entity_b_name: String,
    pub relationship_type: String,
    pub strength: f64,
    pub first_seen: String,
    pub last_seen: String,
    pub mention_count: i64,
}

// ---------------------------------------------------------------------------
// Storage functions
// ---------------------------------------------------------------------------

/// Record a co-occurrence of two entities in the same story.
///
/// Always stores with the smaller ID first (a < b) to avoid duplicate pairs.
/// On conflict, increments mention_count, increases strength, and updates last_seen.
pub fn record_co_occurrence(
    conn: &Connection,
    entity_a_id: i64,
    entity_b_id: i64,
    date: &str,
) -> Result<()> {
    // Normalize order: smaller ID always goes first
    let (a, b) = if entity_a_id <= entity_b_id {
        (entity_a_id, entity_b_id)
    } else {
        (entity_b_id, entity_a_id)
    };

    conn.execute(
        "INSERT INTO entity_relationships (entity_a_id, entity_b_id, relationship_type, strength, first_seen, last_seen, mention_count)
         VALUES (?1, ?2, 'co_occurs', 1.0, ?3, ?3, 1)
         ON CONFLICT(entity_a_id, entity_b_id, relationship_type) DO UPDATE SET
             last_seen = ?3,
             mention_count = mention_count + 1,
             strength = strength + 1.0",
        params![a, b, date],
    )
    .context("failed to upsert co-occurrence relationship")?;

    Ok(())
}

/// Get all relationships for an entity (where it appears as either side).
///
/// Joins entity names for display. Results sorted by strength descending.
pub fn get_entity_relationships(
    conn: &Connection,
    entity_id: i64,
) -> Result<Vec<EntityRelationship>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.entity_a_id, r.entity_b_id, ea.name, eb.name,
                r.relationship_type, r.strength, r.first_seen, r.last_seen, r.mention_count
         FROM entity_relationships r
         JOIN entities ea ON ea.id = r.entity_a_id
         JOIN entities eb ON eb.id = r.entity_b_id
         WHERE r.entity_a_id = ?1 OR r.entity_b_id = ?1
         ORDER BY r.strength DESC",
    )?;

    let relationships = stmt
        .query_map(params![entity_id], |row| {
            Ok(EntityRelationship {
                id: row.get(0)?,
                entity_a_id: row.get(1)?,
                entity_b_id: row.get(2)?,
                entity_a_name: row.get(3)?,
                entity_b_name: row.get(4)?,
                relationship_type: row.get(5)?,
                strength: row.get(6)?,
                first_seen: row.get(7)?,
                last_seen: row.get(8)?,
                mention_count: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(relationships)
}

/// Get strongest relationships across all entities.
///
/// Joins entity names for display. Results sorted by strength descending.
pub fn get_top_relationships(conn: &Connection, limit: usize) -> Result<Vec<EntityRelationship>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.entity_a_id, r.entity_b_id, ea.name, eb.name,
                r.relationship_type, r.strength, r.first_seen, r.last_seen, r.mention_count
         FROM entity_relationships r
         JOIN entities ea ON ea.id = r.entity_a_id
         JOIN entities eb ON eb.id = r.entity_b_id
         ORDER BY r.strength DESC
         LIMIT ?1",
    )?;

    let relationships = stmt
        .query_map(params![limit as i64], |row| {
            Ok(EntityRelationship {
                id: row.get(0)?,
                entity_a_id: row.get(1)?,
                entity_b_id: row.get(2)?,
                entity_a_name: row.get(3)?,
                entity_b_name: row.get(4)?,
                relationship_type: row.get(5)?,
                strength: row.get(6)?,
                first_seen: row.get(7)?,
                last_seen: row.get(8)?,
                mention_count: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(relationships)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;
    use crate::services::entities::{make_test_entity, store_entities};

    /// Helper to insert an entity and return its ID
    fn insert_entity(conn: &Connection, name: &str, story_id: i64) -> i64 {
        let entity = make_test_entity(name, "company", 0.5);
        store_entities(conn, &[entity], story_id, "2026-03-31", "ai").unwrap();
        conn.query_row(
            "SELECT id FROM entities WHERE name_normalized = ?1",
            params![name.trim().to_lowercase()],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_record_co_occurrence_new() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Joint Venture")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let id_a = insert_entity(&conn, "OpenAI", sids[0]);
        let id_b = insert_entity(&conn, "Microsoft", sids[0]);

        record_co_occurrence(&conn, id_a, id_b, "2026-03-31").unwrap();

        let rels = get_entity_relationships(&conn, id_a).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relationship_type, "co_occurs");
        assert!((rels[0].strength - 1.0).abs() < 1e-6);
        assert_eq!(rels[0].mention_count, 1);
    }

    #[test]
    fn test_record_co_occurrence_strengthens() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let id_a = insert_entity(&conn, "OpenAI", sids[0]);
        let id_b = insert_entity(&conn, "Microsoft", sids[0]);

        record_co_occurrence(&conn, id_a, id_b, "2026-03-01").unwrap();
        record_co_occurrence(&conn, id_a, id_b, "2026-03-15").unwrap();

        let rels = get_entity_relationships(&conn, id_a).unwrap();
        assert_eq!(rels.len(), 1);
        assert!((rels[0].strength - 2.0).abs() < 1e-6, "strength should be 2.0");
        assert_eq!(rels[0].mention_count, 2);
        assert_eq!(rels[0].first_seen, "2026-03-01");
        assert_eq!(rels[0].last_seen, "2026-03-15");
    }

    #[test]
    fn test_record_co_occurrence_normalizes_order() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let id_a = insert_entity(&conn, "OpenAI", sids[0]);
        let id_b = insert_entity(&conn, "Microsoft", sids[0]);

        // Record in reverse order (B, A) — should be stored as (A, B) if A < B
        record_co_occurrence(&conn, id_b, id_a, "2026-03-01").unwrap();
        record_co_occurrence(&conn, id_a, id_b, "2026-03-15").unwrap();

        // Should only be ONE relationship, not two
        let rels = get_entity_relationships(&conn, id_a).unwrap();
        assert_eq!(rels.len(), 1, "should be a single relationship regardless of insert order");
        assert_eq!(rels[0].mention_count, 2);
    }

    #[test]
    fn test_get_entity_relationships() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "S1"),
            TestStory::new("ai", "S2"),
            TestStory::new("ai", "S3"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let id_a = insert_entity(&conn, "OpenAI", sids[0]);
        let id_b = insert_entity(&conn, "Microsoft", sids[0]);
        let id_c = insert_entity(&conn, "Google", sids[1]);
        let id_d = insert_entity(&conn, "Meta", sids[2]);

        record_co_occurrence(&conn, id_a, id_b, "2026-03-31").unwrap();
        record_co_occurrence(&conn, id_a, id_c, "2026-03-31").unwrap();
        record_co_occurrence(&conn, id_a, id_d, "2026-03-31").unwrap();

        let rels = get_entity_relationships(&conn, id_a).unwrap();
        assert_eq!(rels.len(), 3, "OpenAI should have 3 relationships");

        // Verify entity B has only 1 relationship (with A)
        let rels_b = get_entity_relationships(&conn, id_b).unwrap();
        assert_eq!(rels_b.len(), 1);
    }

    #[test]
    fn test_get_top_relationships() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let id_a = insert_entity(&conn, "OpenAI", sids[0]);
        let id_b = insert_entity(&conn, "Microsoft", sids[0]);
        let id_c = insert_entity(&conn, "Google", sids[0]);

        // A-B co-occurs 3 times (strength=3), A-C once (strength=1)
        record_co_occurrence(&conn, id_a, id_b, "2026-03-31").unwrap();
        record_co_occurrence(&conn, id_a, id_b, "2026-03-31").unwrap();
        record_co_occurrence(&conn, id_a, id_b, "2026-03-31").unwrap();
        record_co_occurrence(&conn, id_a, id_c, "2026-03-31").unwrap();

        let top = get_top_relationships(&conn, 10).unwrap();
        assert_eq!(top.len(), 2);
        assert!((top[0].strength - 3.0).abs() < 1e-6, "strongest first");
        assert!((top[1].strength - 1.0).abs() < 1e-6);
    }
}
