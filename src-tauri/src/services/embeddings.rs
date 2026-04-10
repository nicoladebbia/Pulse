use anyhow::{Context, Result};
use async_trait::async_trait;
use rusqlite::Connection;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const VOYAGE_API_URL: &str = "https://api.voyageai.com/v1/embeddings";
pub const VOYAGE_MODEL: &str = "voyage-3-lite";
pub const EMBEDDING_DIM: usize = 512;

// ---------------------------------------------------------------------------
// Trait: pluggable embedding provider (real Voyage or test fake)
// ---------------------------------------------------------------------------

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[String], input_type: &str) -> Result<Vec<Vec<f32>>>;
}

// ---------------------------------------------------------------------------
// Vector serialization helpers
// ---------------------------------------------------------------------------

/// Convert an f32 slice to little-endian bytes (4 bytes per element).
pub fn serialize_embedding(v: &[f32]) -> Vec<u8> {
    bytemuck::cast_slice(v).to_vec()
}

/// Convert little-endian bytes back to an f32 vec.
pub fn deserialize_embedding(blob: &[u8]) -> Vec<f32> {
    bytemuck::cast_slice(blob).to_vec()
}

// ---------------------------------------------------------------------------
// Cosine similarity
// ---------------------------------------------------------------------------

/// Cosine similarity between two vectors. Returns 0.0 if either is zero-length
/// or has zero magnitude.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return 0.0;
    }

    (dot / denom) as f32
}

// ---------------------------------------------------------------------------
// DB similarity search
// ---------------------------------------------------------------------------

/// Scan all embeddings in the DB, return `(story_id, score)` sorted descending
/// by cosine similarity. Only returns results with `score >= min_score`, capped
/// at `limit` results.
pub fn find_similar(
    conn: &Connection,
    query_embedding: &[f32],
    limit: usize,
    min_score: f32,
) -> Result<Vec<(i64, f32)>> {
    let mut stmt = conn
        .prepare("SELECT story_id, embedding FROM story_embeddings")
        .context("failed to prepare embedding scan query")?;

    let mut results: Vec<(i64, f32)> = stmt
        .query_map([], |row| {
            let story_id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((story_id, blob))
        })?
        .filter_map(|r| r.ok())
        .map(|(id, blob)| {
            let emb = deserialize_embedding(&blob);
            let score = cosine_similarity(query_embedding, &emb);
            (id, score)
        })
        .filter(|(_, score)| *score >= min_score)
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    Ok(results)
}

/// Scan embeddings for stories OLDER than `days_ago` days (for proactive
/// insights that surface forgotten but relevant stories).
pub fn find_similar_older_than(
    conn: &Connection,
    query_embedding: &[f32],
    days_ago: i64,
    limit: usize,
    min_score: f32,
) -> Result<Vec<(i64, f32)>> {
    let mut stmt = conn
        .prepare(
            "SELECT se.story_id, se.embedding
             FROM story_embeddings se
             JOIN stories s ON s.id = se.story_id
             WHERE s.created_at <= datetime('now', ?1)",
        )
        .context("failed to prepare older-than embedding query")?;

    let offset = format!("-{days_ago} days");

    let mut results: Vec<(i64, f32)> = stmt
        .query_map([&offset], |row| {
            let story_id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((story_id, blob))
        })?
        .filter_map(|r| r.ok())
        .map(|(id, blob)| {
            let emb = deserialize_embedding(&blob);
            let score = cosine_similarity(query_embedding, &emb);
            (id, score)
        })
        .filter(|(_, score)| *score >= min_score)
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    Ok(results)
}

// ---------------------------------------------------------------------------
// VoyageProvider — real Voyage API client
// ---------------------------------------------------------------------------

pub struct VoyageProvider {
    api_key: String,
    http: reqwest::Client,
}

impl VoyageProvider {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self> {
        let api_key =
            std::env::var("VOYAGE_API_KEY").context("VOYAGE_API_KEY not set in environment")?;
        Ok(Self::new(&api_key))
    }
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<VoyageEmbedding>,
}

#[derive(Deserialize)]
struct VoyageEmbedding {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for VoyageProvider {
    async fn embed(&self, texts: &[String], input_type: &str) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "model": VOYAGE_MODEL,
            "input": texts,
            "input_type": input_type,
        });

        let resp = self
            .http
            .post(VOYAGE_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Voyage API request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Voyage API returned {status}: {text}");
        }

        let parsed: VoyageResponse = resp
            .json()
            .await
            .context("failed to parse Voyage API response")?;

        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Pure function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let v: Vec<f32> = (0..512).map(|i| i as f32 * 0.01).collect();
        let blob = serialize_embedding(&v);
        assert_eq!(blob.len(), 512 * 4);
        let restored = deserialize_embedding(&blob);
        assert_eq!(v, restored);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v: Vec<f32> = (0..512).map(|i| (i as f32 + 1.0).sqrt()).collect();
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let mut a = vec![0.0f32; 512];
        let mut b = vec![0.0f32; 512];
        a[0] = 1.0;
        b[1] = 1.0;
        assert!((cosine_similarity(&a, &b)).abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let v: Vec<f32> = (0..512).map(|i| (i as f32 + 1.0).sqrt()).collect();
        let neg: Vec<f32> = v.iter().map(|x| -x).collect();
        let sim = cosine_similarity(&v, &neg);
        assert!((sim + 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_cosine_similarity_known_value() {
        // Simple 3-dim for hand calculation: a=[1,2,3], b=[4,5,6]
        // dot=32, |a|=sqrt(14), |b|=sqrt(77), cos=32/sqrt(1078)≈0.9746
        let mut a = vec![0.0f32; 512];
        let mut b = vec![0.0f32; 512];
        a[0] = 1.0;
        a[1] = 2.0;
        a[2] = 3.0;
        b[0] = 4.0;
        b[1] = 5.0;
        b[2] = 6.0;
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.9746).abs() < 0.001, "got {}", sim);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        let empty: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&empty, &empty), 0.0);
    }

    // -----------------------------------------------------------------------
    // DB test helpers
    // -----------------------------------------------------------------------

    /// Create a deterministic, normalized 512-dim vector from a seed.
    fn fake_embedding(seed: u64) -> Vec<f32> {
        // Simple LCG-style PRNG to get deterministic values per seed.
        let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let mut v = Vec::with_capacity(EMBEDDING_DIM);
        for _ in 0..EMBEDDING_DIM {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            // Map to [-1, 1]
            let val = ((state >> 33) as f32) / (u32::MAX as f32) * 2.0 - 1.0;
            v.push(val);
        }
        // Normalize
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    /// Seed a briefing and `n` stories, returning the story IDs.
    fn seed_stories(conn: &Connection, n: usize) -> Vec<i64> {
        conn.execute(
            "INSERT INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status)
             VALUES ('2026-03-31', ?1, 0, 0, 0, 0, 'complete')",
            [n as i64],
        )
        .unwrap();
        let briefing_id = conn.last_insert_rowid();

        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            conn.execute(
                "INSERT INTO stories (briefing_id, sector, original_title, original_url, headline, summary, key_facts, why_it_matters, what_to_watch, importance_score, is_hero, display_order, source_name, url_hash, title_hash)
                 VALUES (?1, 'ai', ?2, ?3, ?4, 'summary', 'facts', 'matters', 'watch', 5, 0, ?5, 'test', ?6, ?7)",
                rusqlite::params![
                    briefing_id,
                    format!("Title {i}"),
                    format!("https://example.com/{i}"),
                    format!("Headline {i}"),
                    i as i64,
                    format!("url_hash_{i}"),
                    format!("title_hash_{i}"),
                ],
            )
            .unwrap();
            ids.push(conn.last_insert_rowid());
        }
        ids
    }

    /// Insert an embedding blob for a story.
    fn insert_embedding(conn: &Connection, story_id: i64, embedding: &[f32]) {
        let blob = serialize_embedding(embedding);
        conn.execute(
            "INSERT INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![story_id, blob],
        )
        .unwrap();
    }

    // -----------------------------------------------------------------------
    // DB-hitting tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_similar_returns_sorted() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let ids = seed_stories(&conn, 5);

        // Give each story a distinct embedding
        for (i, &sid) in ids.iter().enumerate() {
            insert_embedding(&conn, sid, &fake_embedding(i as u64));
        }

        // Build a query vector very close to story index 2's embedding
        let target = fake_embedding(2);
        let results = find_similar(&conn, &target, 5, 0.0).unwrap();

        assert!(!results.is_empty(), "should have results");
        assert_eq!(results[0].0, ids[2], "story 2 should be first");
        assert!(
            (results[0].1 - 1.0).abs() < 1e-4,
            "identical vector should score ~1.0"
        );

        // Verify descending sort
        for w in results.windows(2) {
            assert!(
                w[0].1 >= w[1].1,
                "results should be sorted descending by score"
            );
        }
    }

    #[test]
    fn test_find_similar_respects_min_score() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let ids = seed_stories(&conn, 5);

        for (i, &sid) in ids.iter().enumerate() {
            insert_embedding(&conn, sid, &fake_embedding(i as u64));
        }

        // Query with the exact embedding of story 0 and a very high threshold
        let query = fake_embedding(0);
        let results = find_similar(&conn, &query, 10, 0.99).unwrap();

        // Only the near-identical story should pass the threshold
        assert_eq!(results.len(), 1, "only 1 story should pass min_score=0.99");
        assert_eq!(results[0].0, ids[0]);
    }

    #[test]
    fn test_find_similar_respects_limit() {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let ids = seed_stories(&conn, 10);

        for (i, &sid) in ids.iter().enumerate() {
            insert_embedding(&conn, sid, &fake_embedding(i as u64));
        }

        let query = fake_embedding(0);
        let results = find_similar(&conn, &query, 3, 0.0).unwrap();

        assert_eq!(results.len(), 3, "limit=3 should return at most 3 results");
    }
}
