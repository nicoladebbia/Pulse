use anyhow::{Context, Result};
use async_trait::async_trait;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: i64,
    pub name: String,
    pub entity_type: String,
    pub sector: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub mention_count: i64,
    pub sentiment_avg: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMention {
    pub id: i64,
    pub entity_id: i64,
    pub story_id: i64,
    pub sentiment: f64,
    pub context: Option<String>,
    pub mentioned_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    pub entity_type: String,
    pub sentiment: f64,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityExtractionResult {
    pub entities: Vec<ExtractedEntity>,
}

// ---------------------------------------------------------------------------
// Extraction trait + Claude implementation
// ---------------------------------------------------------------------------

#[async_trait]
pub trait EntityExtractor: Send + Sync {
    async fn extract_entities(&self, stories_text: &str) -> Result<EntityExtractionResult>;
}

pub struct ClaudeEntityExtractor {
    api_key: String,
    http: reqwest::Client,
}

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const CLAUDE_MODEL: &str = "claude-haiku-4-5-20251001";

const EXTRACTION_SYSTEM_PROMPT: &str = r#"You are an entity extraction engine for a news intelligence system.
Extract entities from the provided news stories. For each entity, identify:
- name: The canonical name of the entity
- entity_type: One of "company", "person", "topic", "product", "regulation"
- sentiment: A score from -1.0 (very negative) to 1.0 (very positive) reflecting how the entity is portrayed
- context: A brief (1-2 sentence) description of how the entity is mentioned

Return ONLY valid JSON in this exact format:
{"entities": [{"name": "...", "entity_type": "...", "sentiment": 0.0, "context": "..."}]}

Deduplicate entities — if the same entity appears in multiple stories, return it once with an averaged sentiment and combined context. Be selective: only extract genuinely noteworthy entities, not generic terms."#;

impl ClaudeEntityExtractor {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY not set in environment")?;
        Ok(Self::new(&api_key))
    }
}

#[derive(Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContentBlock>,
}

#[derive(Deserialize)]
struct ClaudeContentBlock {
    text: Option<String>,
}

#[async_trait]
impl EntityExtractor for ClaudeEntityExtractor {
    async fn extract_entities(&self, stories_text: &str) -> Result<EntityExtractionResult> {
        let body = serde_json::json!({
            "model": CLAUDE_MODEL,
            "max_tokens": 2000,
            "system": EXTRACTION_SYSTEM_PROMPT,
            "messages": [
                {
                    "role": "user",
                    "content": stories_text
                }
            ]
        });

        let resp = self
            .http
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Claude API request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Claude API returned {status}: {text}");
        }

        let parsed: ClaudeResponse = resp
            .json()
            .await
            .context("failed to parse Claude API response")?;

        let text = parsed
            .content
            .into_iter()
            .find_map(|block| block.text)
            .unwrap_or_default();

        // The model may wrap the JSON in markdown code fences — strip them.
        let trimmed = text.trim();
        let json_str = if trimmed.starts_with("```") {
            trimmed
                .trim_start_matches("```json")
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim()
        } else {
            trimmed
        };

        let result: EntityExtractionResult =
            serde_json::from_str(json_str).context("failed to parse entity extraction JSON")?;

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Storage functions
// ---------------------------------------------------------------------------

/// Store extracted entities from stories into the knowledge graph.
///
/// For each entity:
/// 1. Normalize name (lowercase, trim)
/// 2. INSERT or UPDATE on conflict (name_normalized, entity_type)
/// 3. Insert into entity_mentions
pub fn store_entities(
    conn: &Connection,
    entities: &[ExtractedEntity],
    story_id: i64,
    date: &str,
    sector: &str,
) -> Result<()> {
    for entity in entities {
        let name_normalized = entity.name.trim().to_lowercase();

        // Upsert: try insert, on conflict update running stats
        conn.execute(
            "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6)
             ON CONFLICT(name_normalized, entity_type) DO UPDATE SET
                 last_seen = ?5,
                 sentiment_avg = (sentiment_avg * mention_count + ?6) / (mention_count + 1),
                 mention_count = mention_count + 1",
            params![
                entity.name.trim(),
                name_normalized,
                entity.entity_type,
                sector,
                date,
                entity.sentiment,
            ],
        )
        .context("failed to upsert entity")?;

        // Retrieve the entity_id
        let entity_id: i64 = conn
            .query_row(
                "SELECT id FROM entities WHERE name_normalized = ?1 AND entity_type = ?2",
                params![name_normalized, entity.entity_type],
                |row| row.get(0),
            )
            .context("failed to find inserted entity")?;

        // Insert mention
        conn.execute(
            "INSERT INTO entity_mentions (entity_id, story_id, sentiment, context, mentioned_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entity_id,
                story_id,
                entity.sentiment,
                entity.context,
                date,
            ],
        )
        .context("failed to insert entity mention")?;
    }

    Ok(())
}

/// Get entity by name (case-insensitive lookup).
pub fn get_entity(conn: &Connection, name: &str) -> Result<Option<Entity>> {
    let normalized = name.trim().to_lowercase();

    let mut stmt = conn.prepare(
        "SELECT id, name, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg
         FROM entities
         WHERE name_normalized = ?1",
    )?;

    let mut rows = stmt.query_map(params![normalized], |row| {
        Ok(Entity {
            id: row.get(0)?,
            name: row.get(1)?,
            entity_type: row.get(2)?,
            sector: row.get(3)?,
            first_seen: row.get(4)?,
            last_seen: row.get(5)?,
            mention_count: row.get(6)?,
            sentiment_avg: row.get(7)?,
        })
    })?;

    match rows.next() {
        Some(Ok(entity)) => Ok(Some(entity)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Get entity history: all mentions sorted by date.
pub fn get_entity_mentions(conn: &Connection, entity_id: i64) -> Result<Vec<EntityMention>> {
    let mut stmt = conn.prepare(
        "SELECT id, entity_id, story_id, sentiment, context, mentioned_at
         FROM entity_mentions
         WHERE entity_id = ?1
         ORDER BY mentioned_at ASC",
    )?;

    let mentions = stmt
        .query_map(params![entity_id], |row| {
            Ok(EntityMention {
                id: row.get(0)?,
                entity_id: row.get(1)?,
                story_id: row.get(2)?,
                sentiment: row.get(3)?,
                context: row.get(4)?,
                mentioned_at: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(mentions)
}

/// Get top entities by mention count, optionally filtered by sector.
pub fn get_top_entities(
    conn: &Connection,
    sector: Option<&str>,
    limit: usize,
) -> Result<Vec<Entity>> {
    let sql = if sector.is_some() {
        "SELECT id, name, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg
         FROM entities
         WHERE sector = ?1
         ORDER BY mention_count DESC
         LIMIT ?2"
    } else {
        "SELECT id, name, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg
         FROM entities
         ORDER BY mention_count DESC
         LIMIT ?1"
    };

    let mut stmt = conn.prepare(sql)?;

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<Entity> {
        Ok(Entity {
            id: row.get(0)?,
            name: row.get(1)?,
            entity_type: row.get(2)?,
            sector: row.get(3)?,
            first_seen: row.get(4)?,
            last_seen: row.get(5)?,
            mention_count: row.get(6)?,
            sentiment_avg: row.get(7)?,
        })
    };

    let entities: Vec<Entity> = if let Some(s) = sector {
        stmt.query_map(params![s, limit as i64], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        stmt.query_map(params![limit as i64], map_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };

    Ok(entities)
}

/// Find entities related to a query using multi-keyword matching with scoring.
///
/// Strategy:
/// 1. Split query into keywords (2+ chars, lowercased, punctuation stripped)
/// 2. For each keyword, find entities where name_normalized LIKE '%keyword%'
/// 3. Score each entity: keyword coverage + mention frequency boost
/// 4. Return sorted by combined score descending
///
/// This means "OpenAI regulation" matches both "OpenAI" (1 keyword) and
/// "AI regulation" (1 keyword), and an entity matching both keywords ranks highest.
pub fn search_entities(conn: &Connection, query: &str) -> Result<Vec<Entity>> {
    let keywords: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| w.len() >= 2)
        .collect();

    if keywords.is_empty() {
        return Ok(vec![]);
    }

    // Collect candidate entities with keyword match counts
    let mut entity_scores: std::collections::HashMap<i64, (Entity, usize)> =
        std::collections::HashMap::new();

    for keyword in &keywords {
        let pattern = format!("%{}%", keyword);
        let mut stmt = conn.prepare(
            "SELECT id, name, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg
             FROM entities
             WHERE name_normalized LIKE ?1",
        )?;

        let entities = stmt
            .query_map(params![pattern], |row| {
                Ok(Entity {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    entity_type: row.get(2)?,
                    sector: row.get(3)?,
                    first_seen: row.get(4)?,
                    last_seen: row.get(5)?,
                    mention_count: row.get(6)?,
                    sentiment_avg: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for entity in entities {
            let entry = entity_scores.entry(entity.id).or_insert((entity, 0));
            entry.1 += 1;
        }
    }

    // Score and sort: keyword coverage (70%) + log mention boost (30%, capped)
    let total_kw = keywords.len() as f64;
    let mut scored: Vec<(Entity, f64)> = entity_scores
        .into_values()
        .map(|(entity, kw_matches)| {
            let keyword_score = kw_matches as f64 / total_kw;
            let mention_boost = ((entity.mention_count as f64).ln_1p() / 10.0).min(0.3);
            let score = keyword_score * 0.7 + mention_boost;
            (entity, score)
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    Ok(scored.into_iter().map(|(e, _)| e).collect())
}

// ===========================================================================
// Tests
// ===========================================================================

/// Create a test entity (exposed for cross-module tests).
#[cfg(test)]
pub fn make_test_entity(name: &str, entity_type: &str, sentiment: f64) -> ExtractedEntity {
    ExtractedEntity {
        name: name.to_string(),
        entity_type: entity_type.to_string(),
        sentiment,
        context: format!("{name} was mentioned in the news"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;

    fn make_entity(name: &str, entity_type: &str, sentiment: f64) -> ExtractedEntity {
        ExtractedEntity {
            name: name.to_string(),
            entity_type: entity_type.to_string(),
            sentiment,
            context: format!("{name} was mentioned in the news"),
        }
    }

    #[test]
    fn test_store_entity_new() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "AI Breakthrough")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let entities = vec![make_entity("OpenAI", "company", 0.8)];
        store_entities(&conn, &entities, sids[0], "2026-03-31", "ai").unwrap();

        let entity = get_entity(&conn, "OpenAI").unwrap().expect("entity should exist");
        assert_eq!(entity.name, "OpenAI");
        assert_eq!(entity.entity_type, "company");
        assert_eq!(entity.mention_count, 1);
        assert!((entity.sentiment_avg - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_store_entity_updates_existing() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "Story One"),
            TestStory::new("ai", "Story Two"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let e1 = vec![make_entity("OpenAI", "company", 0.8)];
        store_entities(&conn, &e1, sids[0], "2026-03-31", "ai").unwrap();

        let e2 = vec![make_entity("OpenAI", "company", 0.4)];
        store_entities(&conn, &e2, sids[1], "2026-03-31", "ai").unwrap();

        let entity = get_entity(&conn, "OpenAI").unwrap().expect("entity should exist");
        assert_eq!(entity.mention_count, 2);
        // Running average: (0.8 * 1 + 0.4) / 2 = 0.6
        assert!(
            (entity.sentiment_avg - 0.6).abs() < 1e-6,
            "sentiment should be averaged, got {}",
            entity.sentiment_avg
        );
    }

    #[test]
    fn test_entity_first_seen_last_seen() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "Early Story"),
            TestStory::new("ai", "Later Story"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-01", &stories);

        let e1 = vec![make_entity("OpenAI", "company", 0.5)];
        store_entities(&conn, &e1, sids[0], "2026-03-01", "ai").unwrap();

        let e2 = vec![make_entity("OpenAI", "company", 0.5)];
        store_entities(&conn, &e2, sids[1], "2026-03-15", "ai").unwrap();

        let entity = get_entity(&conn, "OpenAI").unwrap().expect("entity should exist");
        assert_eq!(entity.first_seen, "2026-03-01");
        assert_eq!(entity.last_seen, "2026-03-15");
    }

    #[test]
    fn test_get_entity_case_insensitive() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "AI Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let entities = vec![make_entity("OpenAI", "company", 0.5)];
        store_entities(&conn, &entities, sids[0], "2026-03-31", "ai").unwrap();

        // Look up with different casing
        let found = get_entity(&conn, "openai").unwrap();
        assert!(found.is_some(), "should find entity with lowercase query");

        let found = get_entity(&conn, "OPENAI").unwrap();
        assert!(found.is_some(), "should find entity with uppercase query");
    }

    #[test]
    fn test_get_top_entities() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "S1"),
            TestStory::new("ai", "S2"),
            TestStory::new("ai", "S3"),
            TestStory::new("ai", "S4"),
            TestStory::new("ai", "S5"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        // Insert entities with varying mention counts (by inserting multiple times)
        let names = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
        for (i, name) in names.iter().enumerate() {
            // Each entity gets (i+1) mentions
            for j in 0..=i {
                let e = vec![make_entity(name, "company", 0.5)];
                let sid = sids[j.min(sids.len() - 1)];
                store_entities(&conn, &e, sid, "2026-03-31", "ai").unwrap();
            }
        }

        let top = get_top_entities(&conn, None, 3).unwrap();
        assert_eq!(top.len(), 3);
        // Epsilon has 5 mentions, Delta 4, Gamma 3
        assert_eq!(top[0].name, "Epsilon");
        assert_eq!(top[1].name, "Delta");
        assert_eq!(top[2].name, "Gamma");
    }

    #[test]
    fn test_search_entities_single_keyword() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Tech Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let entities = vec![
            make_entity("OpenAI", "company", 0.5),
            make_entity("Open Source Initiative", "topic", 0.3),
            make_entity("Microsoft", "company", 0.6),
        ];
        store_entities(&conn, &entities, sids[0], "2026-03-31", "ai").unwrap();

        let results = search_entities(&conn, "open").unwrap();
        assert_eq!(results.len(), 2, "should find both 'Open' entities");

        let names: Vec<&str> = results.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"OpenAI"));
        assert!(names.contains(&"Open Source Initiative"));
    }

    #[test]
    fn test_search_entities_multi_keyword() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "Tech Story")];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let entities = vec![
            make_entity("OpenAI", "company", 0.5),
            make_entity("AI regulation", "topic", 0.3),
            make_entity("Microsoft", "company", 0.6),
        ];
        store_entities(&conn, &entities, sids[0], "2026-03-31", "ai").unwrap();

        // "OpenAI regulation" should match "OpenAI" (via "openai") and "AI regulation" (via "regulation")
        let results = search_entities(&conn, "OpenAI regulation").unwrap();
        assert!(results.len() >= 2, "should find at least 2 entities, got {}", results.len());

        let names: Vec<&str> = results.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"OpenAI"));
        assert!(names.contains(&"AI regulation"));
    }

    #[test]
    fn test_search_entities_empty_query() {
        let conn = test_db();
        let results = search_entities(&conn, "  ").unwrap();
        assert!(results.is_empty(), "empty query should return no results");
    }

    #[test]
    fn test_get_entity_mentions() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "Story A"),
            TestStory::new("ai", "Story B"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let e1 = vec![make_entity("OpenAI", "company", 0.8)];
        store_entities(&conn, &e1, sids[0], "2026-03-01", "ai").unwrap();

        let e2 = vec![make_entity("OpenAI", "company", 0.4)];
        store_entities(&conn, &e2, sids[1], "2026-03-15", "ai").unwrap();

        let entity = get_entity(&conn, "OpenAI").unwrap().unwrap();
        let mentions = get_entity_mentions(&conn, entity.id).unwrap();

        assert_eq!(mentions.len(), 2);
        assert_eq!(mentions[0].mentioned_at, "2026-03-01");
        assert_eq!(mentions[1].mentioned_at, "2026-03-15");
        assert!((mentions[0].sentiment - 0.8).abs() < 1e-6);
        assert!((mentions[1].sentiment - 0.4).abs() < 1e-6);
    }
}
