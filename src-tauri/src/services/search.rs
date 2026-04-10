use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MatchType {
    Fts,
    Semantic,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum StorySource {
    Daily,
    Freedom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredStory {
    pub story_id: i64,
    pub headline: String,
    pub summary: String,
    pub key_facts: String,
    pub why_it_matters: String,
    pub sector: String,
    pub date: String,
    pub source_name: String,
    pub score: f32,
    pub match_type: MatchType,
    pub source: StorySource,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const FTS_WEIGHT: f32 = 0.4;
const SEMANTIC_WEIGHT: f32 = 0.6;
const OVERLAP_BONUS: f32 = 1.2;

// Recency decay: final = ALPHA * relevance + (1 - ALPHA) * 0.5^(age_days / HALF_LIFE)
const RECENCY_ALPHA: f32 = 0.7;
const RECENCY_HALF_LIFE: f32 = 14.0;

// ---------------------------------------------------------------------------
// Query rewriting (Haiku-powered expansion + HyDE)
// ---------------------------------------------------------------------------

/// Expanded query with richer FTS keywords and a hypothetical answer for
/// semantic search (HyDE technique).
#[derive(Debug, Clone)]
pub struct ExpandedQuery {
    pub fts_keywords: String,
    pub semantic_text: String,
    pub original: String,
}

impl ExpandedQuery {
    fn from_original(message: &str) -> Self {
        Self {
            fts_keywords: message.to_string(),
            semantic_text: message.to_string(),
            original: message.to_string(),
        }
    }
}

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const HAIKU_API_URL: &str = "https://api.anthropic.com/v1/messages";
const REWRITE_TIMEOUT_SECS: u64 = 3;

/// Use Haiku to expand a user query into richer FTS keywords and a hypothetical
/// answer (HyDE) for better semantic matching. Non-fatal: returns the original
/// message on any failure.
pub async fn rewrite_query(api_key: &str, message: &str) -> ExpandedQuery {
    if api_key.is_empty() || message.trim().is_empty() {
        return ExpandedQuery::from_original(message);
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(REWRITE_TIMEOUT_SECS),
        rewrite_query_inner(api_key, message),
    )
    .await;

    match result {
        Ok(Ok(expanded)) => expanded,
        Ok(Err(e)) => {
            tracing::warn!("Query rewrite failed (non-fatal): {}", e);
            ExpandedQuery::from_original(message)
        }
        Err(_) => {
            tracing::warn!("Query rewrite timed out after {}s", REWRITE_TIMEOUT_SECS);
            ExpandedQuery::from_original(message)
        }
    }
}

async fn rewrite_query_inner(api_key: &str, message: &str) -> anyhow::Result<ExpandedQuery> {
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "model": HAIKU_MODEL,
        "max_tokens": 300,
        "system": "You expand search queries for a news intelligence archive covering AI/LLMs, Miami Beach, Italian politics, and tech/innovation.\nGiven a user question, return ONLY valid JSON:\n{\"keywords\": \"expanded search keywords with entity names synonyms and related terms\", \"hypothetical_answer\": \"A 2-3 sentence hypothetical answer to this question as if you had the relevant news stories\"}\nBe specific: add company names, people, acronyms, related technologies. No explanation, just JSON.",
        "messages": [{"role": "user", "content": message}]
    });

    let resp = client
        .post(HAIKU_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Haiku rewrite request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("Haiku rewrite returned {}", resp.status());
    }

    let response: serde_json::Value = resp.json().await?;
    let text = response["content"][0]["text"]
        .as_str()
        .unwrap_or("{}");

    // Extract JSON from response (may have markdown wrapping)
    let json_str = if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            &text[start..=end]
        } else {
            text
        }
    } else {
        text
    };

    #[derive(Deserialize)]
    struct RewriteResponse {
        keywords: Option<String>,
        hypothetical_answer: Option<String>,
    }

    let parsed: RewriteResponse = serde_json::from_str(json_str)
        .context("failed to parse rewrite response")?;

    Ok(ExpandedQuery {
        fts_keywords: parsed.keywords.unwrap_or_else(|| message.to_string()),
        semantic_text: parsed.hypothetical_answer.unwrap_or_else(|| message.to_string()),
        original: message.to_string(),
    })
}

// ---------------------------------------------------------------------------
// FTS5 keyword search
// ---------------------------------------------------------------------------

/// Search stories using FTS5 keyword matching.
/// Returns up to `limit` results as `(story_id, normalized_score)` sorted by
/// FTS5 rank.
pub fn fts_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<(i64, f32)>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(vec![]);
    }

    // Build FTS5 query: split words, add * suffix to each, join with OR.
    // e.g., "AI regulation" -> "AI* OR regulation*"
    let fts_query: String = trimmed
        .split_whitespace()
        .map(|w| {
            // Strip characters that are special in FTS5 query syntax
            let clean: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            format!("{clean}*")
        })
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() {
        return Ok(vec![]);
    }

    let result = (|| -> Result<Vec<(i64, f32)>> {
        let mut stmt = conn
            .prepare(
                "SELECT s.id, rank
                 FROM stories_fts
                 JOIN stories s ON s.id = stories_fts.rowid
                 WHERE stories_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .context("failed to prepare FTS5 query")?;

        let rows: Vec<(i64, f64)> = stmt
            .query_map(params![fts_query, limit as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if rows.is_empty() {
            return Ok(vec![]);
        }

        // FTS5 rank is negative (more negative = better). Normalize to 0..1.
        let min_rank = rows
            .iter()
            .map(|(_, r)| *r)
            .fold(f64::INFINITY, f64::min);
        let max_rank = rows
            .iter()
            .map(|(_, r)| *r)
            .fold(f64::NEG_INFINITY, f64::max);

        let range = max_rank - min_rank;

        let results = rows
            .into_iter()
            .map(|(id, rank)| {
                let score = if range.abs() < 1e-9 {
                    1.0f32
                } else {
                    // min_rank is the best (most negative), map it to 1.0
                    ((max_rank - rank) / range) as f32
                };
                (id, score)
            })
            .collect();

        Ok(results)
    })();

    match result {
        Ok(r) => Ok(r),
        Err(_) => like_search(conn, query, limit),
    }
}

/// Fallback LIKE-based search when FTS fails (e.g., malformed query).
fn like_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<(i64, f32)>> {
    let pattern = format!("%{}%", query.trim().to_lowercase());

    let mut stmt = conn
        .prepare(
            "SELECT id FROM stories
             WHERE lower(headline) LIKE ?1 OR lower(summary) LIKE ?1
             ORDER BY importance_score DESC
             LIMIT ?2",
        )
        .context("failed to prepare LIKE search")?;

    let results: Vec<(i64, f32)> = stmt
        .query_map(params![pattern, limit as i64], |row| {
            Ok(row.get::<_, i64>(0)?)
        })?
        .filter_map(|r| r.ok())
        .enumerate()
        .map(|(i, id)| {
            // Simple decaying score: first result = 1.0, decays linearly
            let score = 1.0 - (i as f32 * 0.1).min(0.9);
            (id, score)
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Result merging
// ---------------------------------------------------------------------------

/// Merge FTS and semantic search results using weighted scoring.
///
/// - `fts_weight` = 0.4, `semantic_weight` = 0.6
/// - Results appearing in both sets get a 1.2x bonus
/// - Scores are normalized to 0..1 within each set before merging
pub fn merge_results(
    fts_results: &[(i64, f32)],
    semantic_results: &[(i64, f32)],
) -> Vec<(i64, f32, MatchType)> {
    if fts_results.is_empty() && semantic_results.is_empty() {
        return vec![];
    }

    // Normalize helper: divide all scores by the max in the set
    let normalize = |results: &[(i64, f32)]| -> Vec<(i64, f32)> {
        let max_score = results
            .iter()
            .map(|(_, s)| *s)
            .fold(0.0f32, f32::max);
        if max_score <= 0.0 {
            return results.iter().map(|(id, _)| (*id, 1.0)).collect();
        }
        results.iter().map(|(id, s)| (*id, s / max_score)).collect()
    };

    let fts_norm = normalize(fts_results);
    let sem_norm = normalize(semantic_results);

    // Build combined map: story_id -> (Option<fts_score>, Option<sem_score>)
    let mut combined: HashMap<i64, (Option<f32>, Option<f32>)> = HashMap::new();

    for &(id, score) in &fts_norm {
        combined.entry(id).or_insert((None, None)).0 = Some(score);
    }
    for &(id, score) in &sem_norm {
        combined.entry(id).or_insert((None, None)).1 = Some(score);
    }

    let mut results: Vec<(i64, f32, MatchType)> = combined
        .into_iter()
        .map(|(id, (fts_score, sem_score))| {
            let fts = fts_score.unwrap_or(0.0);
            let sem = sem_score.unwrap_or(0.0);
            let mut final_score = FTS_WEIGHT * fts + SEMANTIC_WEIGHT * sem;

            let match_type = match (fts_score.is_some(), sem_score.is_some()) {
                (true, true) => {
                    final_score *= OVERLAP_BONUS;
                    MatchType::Both
                }
                (true, false) => MatchType::Fts,
                (false, true) => MatchType::Semantic,
                (false, false) => unreachable!(),
            };

            (id, final_score, match_type)
        })
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results
}

// ---------------------------------------------------------------------------
// FTS5 freedom stories search
// ---------------------------------------------------------------------------

/// Search freedom stories using FTS5. Returns results with negative IDs
/// (convention: freedom story ID N is represented as -(N + 100_000) to avoid
/// collision with daily story IDs).
pub fn fts_search_freedoms(conn: &Connection, query: &str, limit: usize) -> Result<Vec<(i64, f32)>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(vec![]);
    }

    let fts_query: String = trimmed
        .split_whitespace()
        .map(|w| {
            let clean: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            format!("{clean}*")
        })
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() {
        return Ok(vec![]);
    }

    let mut stmt = match conn.prepare(
        "SELECT fs.id, rank
         FROM freedom_stories_fts
         JOIN freedom_stories fs ON fs.id = freedom_stories_fts.rowid
         WHERE freedom_stories_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Ok(vec![]), // table may not exist yet
    };

    let rows: Vec<(i64, f64)> = stmt
        .query_map(params![fts_query, limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        return Ok(vec![]);
    }

    let min_rank = rows.iter().map(|(_, r)| *r).fold(f64::INFINITY, f64::min);
    let max_rank = rows.iter().map(|(_, r)| *r).fold(f64::NEG_INFINITY, f64::max);
    let range = max_rank - min_rank;

    Ok(rows
        .into_iter()
        .map(|(id, rank)| {
            let score = if range.abs() < 1e-9 {
                1.0f32
            } else {
                ((max_rank - rank) / range) as f32
            };
            (encode_freedom_id(id), score)
        })
        .collect())
}

/// Encode a freedom_stories ID to distinguish from daily stories.
fn encode_freedom_id(id: i64) -> i64 {
    -(id + 100_000)
}

/// Decode a freedom story ID. Returns (original_id, is_freedom).
pub fn decode_story_id(encoded: i64) -> (i64, bool) {
    if encoded < 0 {
        (-(encoded) - 100_000, true)
    } else {
        (encoded, false)
    }
}

// ---------------------------------------------------------------------------
// Recency decay
// ---------------------------------------------------------------------------

/// Pure recency score for a given age in days.
/// Returns a value in [0, 1] that halves every `RECENCY_HALF_LIFE` days.
pub fn recency_score(age_days: f32) -> f32 {
    if age_days <= 0.0 {
        return 1.0;
    }
    0.5_f32.powf(age_days / RECENCY_HALF_LIFE)
}

/// Apply time-decay to merged search results using story dates from the DB.
/// Formula: final = ALPHA * relevance + (1 - ALPHA) * recency_score(age)
fn apply_recency_decay(
    conn: &Connection,
    results: &mut Vec<(i64, f32, MatchType)>,
) -> Result<()> {
    if results.is_empty() {
        return Ok(());
    }

    // Batch-query dates for all story IDs
    let ids: Vec<i64> = results.iter().map(|(id, _, _)| *id).collect();
    let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT s.id, b.date FROM stories s JOIN briefings b ON b.id = s.briefing_id WHERE s.id IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&sql).context("failed to prepare recency query")?;
    let param_values: Vec<rusqlite::types::Value> = ids
        .iter()
        .map(|id| rusqlite::types::Value::Integer(*id))
        .collect();
    let params = rusqlite::params_from_iter(param_values.iter());

    let mut date_map: std::collections::HashMap<i64, String> = stmt
        .query_map(params, |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Also query dates for freedom stories (encoded as negative IDs)
    let freedom_encoded: Vec<i64> = results.iter()
        .filter(|(id, _, _)| *id < 0)
        .map(|(id, _, _)| *id)
        .collect();
    if !freedom_encoded.is_empty() {
        let real_ids: Vec<i64> = freedom_encoded.iter()
            .map(|id| { let (real, _) = decode_story_id(*id); real })
            .collect();
        let f_placeholders: String = real_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let f_sql = format!(
            "SELECT fs.id, b.date FROM freedom_stories fs JOIN briefings b ON b.id = fs.briefing_id WHERE fs.id IN ({})",
            f_placeholders
        );
        if let Ok(mut f_stmt) = conn.prepare(&f_sql) {
            let f_params: Vec<rusqlite::types::Value> = real_ids.iter()
                .map(|id| rusqlite::types::Value::Integer(*id))
                .collect();
            if let Ok(rows) = f_stmt.query_map(rusqlite::params_from_iter(f_params.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            }) {
                for row in rows.flatten() {
                    // Map back to encoded ID so the lookup below works
                    date_map.insert(encode_freedom_id(row.0), row.1);
                }
            }
        }
    }

    let today = chrono::Local::now().date_naive();

    for (id, score, _) in results.iter_mut() {
        if let Some(date_str) = date_map.get(id) {
            if let Ok(story_date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                let age_days = (today - story_date).num_days().max(0) as f32;
                let recency = recency_score(age_days);
                *score = RECENCY_ALPHA * *score + (1.0 - RECENCY_ALPHA) * recency;
            }
        }
        // If date lookup/parse fails, keep original score unchanged
    }

    // Re-sort by new scores
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(())
}

// ---------------------------------------------------------------------------
// Full hybrid search
// ---------------------------------------------------------------------------

/// Full hybrid search: FTS5 + semantic (if embeddings available).
/// Falls back to FTS-only if no embeddings or embedding query fails.
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    query_embedding: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<ScoredStory>> {
    // 1. Run FTS search on daily stories
    let fts = fts_search(conn, query, limit * 2)?;

    // 1b. Also search freedom stories (non-fatal if table doesn't exist yet)
    let fts_freedom = fts_search_freedoms(conn, query, limit).unwrap_or_default();

    // 2. Combine daily + freedom FTS results
    let mut all_fts = fts;
    all_fts.extend(fts_freedom);

    // 3. Run semantic search if embedding provided (daily stories only — freedoms don't have embeddings yet)
    let semantic = if let Some(emb) = query_embedding {
        super::embeddings::find_similar(conn, emb, limit * 2, 0.3).unwrap_or_default()
    } else {
        vec![]
    };

    // 4. Merge results
    let mut merged = merge_results(&all_fts, &semantic);

    // 5. Apply recency decay (non-fatal — keeps original scores on error)
    if let Err(e) = apply_recency_decay(conn, &mut merged) {
        tracing::warn!("Recency decay failed (non-fatal): {}", e);
    }

    // 6. Load full story data for top results
    let top: Vec<_> = merged.into_iter().take(limit).collect();
    if top.is_empty() {
        return Ok(vec![]);
    }

    let mut stories = Vec::with_capacity(top.len());
    for (story_id, score, match_type) in &top {
        let (real_id, is_freedom) = decode_story_id(*story_id);

        let result = if is_freedom {
            // Load from freedom_stories table
            conn.query_row(
                "SELECT fs.headline, fs.summary, fs.key_facts, fs.why_it_matters,
                        fs.freedom, fs.source_name, b.date
                 FROM freedom_stories fs
                 JOIN briefings b ON b.id = fs.briefing_id
                 WHERE fs.id = ?1",
                params![real_id],
                |row| {
                    Ok(ScoredStory {
                        story_id: *story_id,
                        headline: row.get(0)?,
                        summary: row.get(1)?,
                        key_facts: row.get(2)?,
                        why_it_matters: row.get(3)?,
                        sector: row.get(4)?,
                        source_name: row.get(5)?,
                        date: row.get(6)?,
                        score: *score,
                        match_type: *match_type,
                        source: StorySource::Freedom,
                    })
                },
            )
        } else {
            // Load from stories table
            conn.query_row(
                "SELECT s.headline, s.summary, s.key_facts, s.why_it_matters,
                        s.sector, s.source_name, b.date
                 FROM stories s
                 JOIN briefings b ON b.id = s.briefing_id
                 WHERE s.id = ?1",
                params![story_id],
                |row| {
                    Ok(ScoredStory {
                        story_id: *story_id,
                        headline: row.get(0)?,
                        summary: row.get(1)?,
                        key_facts: row.get(2)?,
                        why_it_matters: row.get(3)?,
                        sector: row.get(4)?,
                        source_name: row.get(5)?,
                        date: row.get(6)?,
                        score: *score,
                        match_type: *match_type,
                        source: StorySource::Daily,
                    })
                },
            )
        };

        if let Ok(story) = result {
            stories.push(story);
        }
    }

    Ok(stories)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;

    // -----------------------------------------------------------------------
    // Recency scoring tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_recency_score_today() {
        let score = recency_score(0.0);
        assert!((score - 1.0).abs() < 1e-5, "today should score 1.0, got {}", score);
    }

    #[test]
    fn test_recency_score_half_life() {
        let score = recency_score(14.0);
        assert!((score - 0.5).abs() < 1e-5, "14 days should score 0.5, got {}", score);
    }

    #[test]
    fn test_recency_score_two_half_lives() {
        let score = recency_score(28.0);
        assert!((score - 0.25).abs() < 1e-5, "28 days should score 0.25, got {}", score);
    }

    #[test]
    fn test_recency_score_old() {
        let score = recency_score(90.0);
        assert!(score < 0.02, "90 days should score near zero, got {}", score);
    }

    #[test]
    fn test_recency_score_negative_age() {
        let score = recency_score(-5.0);
        assert!((score - 1.0).abs() < 1e-5, "negative age should score 1.0");
    }

    // -----------------------------------------------------------------------
    // FTS search tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_fts_search_finds_keyword() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "OpenAI Launches GPT-5"),
            TestStory::new("ai", "Anthropic Raises Funding"),
            TestStory::new("tech", "Apple Releases New Chip"),
        ];
        let (_bid, _sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let results = fts_search(&conn, "OpenAI", 10).unwrap();
        assert!(!results.is_empty(), "should find stories matching 'OpenAI'");

        // The OpenAI story should be in the results
        let story_ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
        assert!(
            story_ids.contains(&_sids[0]),
            "should find the OpenAI story"
        );
    }

    #[test]
    fn test_fts_search_no_results() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "AI Breakthrough")];
        let (_bid, _sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let results = fts_search(&conn, "xyznonexistent", 10).unwrap();
        assert!(results.is_empty(), "should return empty for nonsense query");
    }

    // -----------------------------------------------------------------------
    // Merge results tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_results_fts_only() {
        let fts = vec![(1, 0.9f32), (2, 0.7), (3, 0.5)];
        let semantic: Vec<(i64, f32)> = vec![];

        let merged = merge_results(&fts, &semantic);
        assert_eq!(merged.len(), 3);
        for (_, _, mt) in &merged {
            assert_eq!(*mt, MatchType::Fts, "all should be FTS-only");
        }
    }

    #[test]
    fn test_merge_results_semantic_only() {
        let fts: Vec<(i64, f32)> = vec![];
        let semantic = vec![(10, 0.95f32), (20, 0.80)];

        let merged = merge_results(&fts, &semantic);
        assert_eq!(merged.len(), 2);
        for (_, _, mt) in &merged {
            assert_eq!(*mt, MatchType::Semantic, "all should be Semantic-only");
        }
    }

    #[test]
    fn test_merge_results_overlap() {
        let fts = vec![(1, 0.9f32), (2, 0.5)];
        let semantic = vec![(1, 0.8f32), (3, 0.6)];

        let merged = merge_results(&fts, &semantic);

        // Story 1 appears in both
        let story_1 = merged.iter().find(|(id, _, _)| *id == 1).unwrap();
        assert_eq!(story_1.2, MatchType::Both);

        // Story 2 is FTS only
        let story_2 = merged.iter().find(|(id, _, _)| *id == 2).unwrap();
        assert_eq!(story_2.2, MatchType::Fts);

        // Story 3 is Semantic only
        let story_3 = merged.iter().find(|(id, _, _)| *id == 3).unwrap();
        assert_eq!(story_3.2, MatchType::Semantic);

        // Both-match story should have overlap bonus applied
        // Normalized: fts: 1=1.0, 2=0.556; sem: 1=1.0, 3=0.75
        // Story 1: (0.4*1.0 + 0.6*1.0) * 1.2 = 1.2
        // Story 3: 0.6*0.75 = 0.45
        // Story 2: 0.4*0.556 = 0.222
        assert!(
            story_1.1 > story_3.1,
            "overlap story should score higher than semantic-only"
        );
    }

    #[test]
    fn test_merge_results_ordering() {
        let fts = vec![(1, 0.9f32), (2, 0.3)];
        let semantic = vec![(1, 0.9f32), (3, 0.95)];

        let merged = merge_results(&fts, &semantic);

        // Verify descending order
        for w in merged.windows(2) {
            assert!(
                w[0].1 >= w[1].1,
                "results should be sorted descending: {} >= {}",
                w[0].1,
                w[1].1
            );
        }
    }

    // -----------------------------------------------------------------------
    // Hybrid search tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_hybrid_search_fts_fallback() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "OpenAI Launches New Model"),
            TestStory::new("tech", "Apple Releases iPhone"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        // No embeddings in DB — should gracefully fall back to FTS-only
        let results = hybrid_search(&conn, "OpenAI", None, 10).unwrap();
        assert!(
            !results.is_empty(),
            "should return FTS results even without embeddings"
        );

        let found_ids: Vec<i64> = results.iter().map(|s| s.story_id).collect();
        assert!(
            found_ids.contains(&sids[0]),
            "should find the OpenAI story via FTS"
        );

        // All results should be FTS match type
        for story in &results {
            assert_eq!(
                story.match_type,
                MatchType::Fts,
                "without embeddings, all matches should be FTS"
            );
        }
    }

    #[test]
    fn test_hybrid_search_with_embeddings() {
        let conn = test_db();
        let stories = vec![
            TestStory::new("ai", "OpenAI Launches New Model"),
            TestStory::new("ai", "Anthropic Releases Claude"),
            TestStory::new("tech", "Apple Stock Rises"),
        ];
        let (_bid, sids) = seed_briefing(&conn, "2026-03-31", &stories);

        // Seed embeddings
        let emb_0 = fake_embedding(100);
        let emb_1 = fake_embedding(200);
        let emb_2 = fake_embedding(300);
        seed_embedding(&conn, sids[0], &emb_0);
        seed_embedding(&conn, sids[1], &emb_1);
        seed_embedding(&conn, sids[2], &emb_2);

        // Search with a query embedding close to story 0
        let query_emb = fake_embedding(100);
        let results = hybrid_search(&conn, "OpenAI", Some(&query_emb), 10).unwrap();

        assert!(!results.is_empty(), "should return results");
        // Story 0 should rank highly (matches both FTS keyword and embedding)
        assert_eq!(
            results[0].story_id, sids[0],
            "story matching both FTS and semantic should rank first"
        );
    }

    #[test]
    fn test_hybrid_search_loads_full_story() {
        let conn = test_db();
        let stories = vec![TestStory::new("ai", "OpenAI Launches GPT-5")];
        let (_bid, _sids) = seed_briefing(&conn, "2026-03-31", &stories);

        let results = hybrid_search(&conn, "OpenAI", None, 10).unwrap();
        assert!(!results.is_empty());

        let story = &results[0];
        assert_eq!(story.headline, "OpenAI Launches GPT-5");
        assert_eq!(story.sector, "ai");
        assert_eq!(story.date, "2026-03-31");
        assert_eq!(story.source_name, "Test Source");
        assert!(!story.summary.is_empty());
    }
}
