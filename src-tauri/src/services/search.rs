use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueryType {
    /// Wants latest news — heavy recency bias
    Breaking,
    /// Wants depth/trends — relevance dominates
    Analytical,
    /// Wants breadth across time — almost no recency bias
    Comparative,
    /// Wants actionable advice — strategy, how-to, planning
    Advisory,
    /// Default behavior
    General,
}

impl QueryType {
    /// Returns (alpha, half_life_days) for recency decay.
    pub fn recency_params(&self) -> (f32, f32) {
        match self {
            QueryType::Breaking => (0.4, 3.0),
            QueryType::Analytical => (0.85, 30.0),
            QueryType::Comparative => (0.95, 60.0),
            QueryType::Advisory => (RECENCY_ALPHA, RECENCY_HALF_LIFE),
            QueryType::General => (RECENCY_ALPHA, RECENCY_HALF_LIFE),
        }
    }

    /// Returns a label for the FORMAT instruction in the system prompt.
    pub fn format_label(&self) -> &'static str {
        match self {
            QueryType::Breaking => "breaking",
            QueryType::Analytical => "analytical",
            QueryType::Comparative => "comparative",
            QueryType::Advisory => "advisory",
            QueryType::General => "general",
        }
    }
}

/// Classify a user query into a QueryType using keyword heuristics.
pub fn classify_query_type(message: &str) -> QueryType {
    let lower = message.to_lowercase();

    let comparative_keywords: &[&str] = &[
        " vs ", "compare", "better", "alternatives", "difference",
        "pros ", "cons ", "versus", "compared to", "which is",
    ];
    // Advisory checked before analytical — "how do I" is advisory, "how does X work" is analytical
    let advisory_keywords: &[&str] = &[
        "how do i", "how can i", "how should i", "what should i",
        "help me", "strategy for", "plan for", "advice",
        "recommend", "make money", "build a ", "steps to",
        "guide me", "teach me", "should i ",
    ];
    let breaking_keywords: &[&str] = &[
        "today", "this week", "recent", "latest", "just", "breaking",
        "new ", "right now", "this morning", "tonight", "yesterday",
    ];
    let analytical_keywords: &[&str] = &[
        "trend", "explain", "why ", "how ", "analysis", "history",
        "evolution", "overview", "deep dive", "impact", "implications",
        "behind", "cause", "reason",
    ];

    // Check comparative first (most specific patterns)
    if comparative_keywords.iter().any(|kw| lower.contains(kw)) {
        return QueryType::Comparative;
    }
    // Advisory before analytical — "how do I" is advisory, not analytical
    if advisory_keywords.iter().any(|kw| lower.contains(kw)) {
        return QueryType::Advisory;
    }
    if breaking_keywords.iter().any(|kw| lower.contains(kw)) {
        return QueryType::Breaking;
    }
    if analytical_keywords.iter().any(|kw| lower.contains(kw)) {
        return QueryType::Analytical;
    }
    QueryType::General
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

// RRF (Reciprocal Rank Fusion) constant — industry standard k=60
const RRF_K: f32 = 60.0;

// Minimum results to keep after score cutoff (protects niche queries)
const MIN_RESULTS_AFTER_CUTOFF: usize = 3;

// Score cutoff: drop results below top_score * this ratio
const SCORE_CUTOFF_RATIO: f32 = 0.3;

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
    /// Parsed temporal filter from the query (e.g., "last quarter" → "2026-01-01")
    pub date_from: Option<String>,
    /// Sub-queries for complex multi-entity/multi-facet questions
    pub sub_queries: Vec<String>,
}

impl ExpandedQuery {
    pub fn from_original(message: &str) -> Self {
        Self {
            fts_keywords: message.to_string(),
            semantic_text: message.to_string(),
            original: message.to_string(),
            date_from: None,
            sub_queries: vec![],
        }
    }
}

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const HAIKU_API_URL: &str = "https://api.anthropic.com/v1/messages";
const REWRITE_TIMEOUT_SECS: u64 = 3;

/// Check if a query is simple enough to skip the Haiku rewrite.
/// Simple queries: short, direct lookups that don't benefit from HyDE expansion.
fn should_skip_rewrite(message: &str, has_context: bool) -> bool {
    // Always rewrite follow-ups (need conversation context resolution)
    if has_context { return false; }

    let words: Vec<&str> = message.split_whitespace().collect();
    // Very short queries (< 6 words) are usually direct lookups
    if words.len() < 6 { return true; }

    let lower = message.to_lowercase();
    // Simple patterns that are already good search queries
    let simple_patterns = ["what happened", "news about", "tell me about", "latest on", "update on"];
    if simple_patterns.iter().any(|p| lower.starts_with(p)) { return true; }

    false
}

/// Use Haiku to expand a user query into richer FTS keywords and a hypothetical
/// answer (HyDE) for better semantic matching. Non-fatal: returns the original
/// message on any failure.
///
/// When `conversation_context` is provided, Haiku rewrites follow-up messages
/// into standalone queries (e.g., "What about the US?" → "AI regulation in the US").
pub async fn rewrite_query(api_key: &str, message: &str, conversation_context: Option<&str>) -> ExpandedQuery {
    if api_key.is_empty() || message.trim().is_empty() {
        return ExpandedQuery::from_original(message);
    }

    // Skip expensive Haiku call for simple queries
    if should_skip_rewrite(message, conversation_context.is_some()) {
        tracing::info!("Skipping query rewrite (simple query)");
        return ExpandedQuery::from_original(message);
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(REWRITE_TIMEOUT_SECS),
        rewrite_query_inner(api_key, message, conversation_context),
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

async fn rewrite_query_inner(api_key: &str, message: &str, conversation_context: Option<&str>) -> anyhow::Result<ExpandedQuery> {
    let client = reqwest::Client::new();

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let base_instruction = format!(
        "You expand search queries for a news intelligence archive covering AI/LLMs, Miami Beach, Italian politics, and tech/innovation.\n\
        Today is {}. Return ONLY valid JSON with these fields:\n\
        {{\"keywords\": \"expanded search keywords with entity names synonyms and related terms\",\n\
        \"hypothetical_answer\": \"A 2-3 sentence hypothetical answer as if you had the relevant news stories\",\n\
        \"date_from\": \"YYYY-MM-DD or null — extract temporal references: 'last quarter'→3 months ago, 'since January'→YYYY-01-01, 'this week'→Monday's date, 'yesterday'→yesterday. null if no temporal reference.\",\n\
        \"sub_queries\": [\"sub-query 1\", \"sub-query 2\"] or [] — ONLY split if the question asks about 2+ DISTINCT topics/entities that need separate searches. Most questions should return []. Max 3 sub-queries.\n\
        }}\n\
        Be specific: add company names, people, acronyms, related technologies. No explanation, just JSON.",
        today
    );

    let system = if let Some(ctx) = conversation_context {
        format!(
            "{}\n\nThe user is in an ongoing conversation. Here is the recent context:\n{}\n\n\
            Rewrite the user's latest message as a STANDALONE search query that includes all necessary context from the conversation.",
            base_instruction, ctx
        )
    } else {
        base_instruction
    };

    let body = serde_json::json!({
        "model": HAIKU_MODEL,
        "max_tokens": 300,
        "system": system,
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
        date_from: Option<String>,
        sub_queries: Option<Vec<String>>,
    }

    let parsed: RewriteResponse = serde_json::from_str(json_str)
        .context("failed to parse rewrite response")?;

    // Filter out "null" string values for date_from
    let date_from = parsed.date_from.filter(|d| d != "null" && !d.is_empty() && d.len() == 10);

    // Filter sub_queries: only keep if 2+ non-empty entries, max 3
    let sub_queries = parsed.sub_queries
        .map(|sq| sq.into_iter().filter(|s| !s.is_empty()).take(3).collect::<Vec<_>>())
        .filter(|sq| sq.len() >= 2)
        .unwrap_or_default();

    if date_from.is_some() {
        tracing::info!("Temporal filter parsed: {:?}", date_from);
    }
    if !sub_queries.is_empty() {
        tracing::info!("Query decomposed into {} sub-queries: {:?}", sub_queries.len(), sub_queries);
    }

    Ok(ExpandedQuery {
        fts_keywords: parsed.keywords.unwrap_or_else(|| message.to_string()),
        semantic_text: parsed.hypothetical_answer.unwrap_or_else(|| message.to_string()),
        original: message.to_string(),
        date_from,
        sub_queries,
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
        // BM25 with field weights: headline=50, summary=1, key_facts=5, why_it_matters=5
        let mut stmt = conn
            .prepare(
                "SELECT s.id, bm25(stories_fts, 50.0, 1.0, 5.0, 5.0) as score
                 FROM stories_fts
                 JOIN stories s ON s.id = stories_fts.rowid
                 WHERE stories_fts MATCH ?1
                 ORDER BY score
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

/// Merge FTS and semantic search results using score-weighted Reciprocal Rank
/// Fusion. Unlike standard RRF which only uses rank position, this incorporates
/// the raw similarity scores so high-confidence semantic matches can compete
/// with weak FTS hits.
///
/// Formula: score = Σ(raw_score / (k + rank)) weighted by channel multiplier.
/// Semantic gets a 1.5x boost because FTS produces more low-quality results.
pub fn merge_results(
    fts_results: &[(i64, f32)],
    semantic_results: &[(i64, f32)],
) -> Vec<(i64, f32, MatchType)> {
    if fts_results.is_empty() && semantic_results.is_empty() {
        return vec![];
    }

    // Track which lists each ID appeared in
    let mut fts_set: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut sem_set: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // Build score-weighted RRF: raw_score / (k + rank), with semantic boost
    const SEMANTIC_BOOST: f32 = 1.3;
    let mut scores: HashMap<i64, f32> = HashMap::new();

    for (rank, &(id, raw_score)) in fts_results.iter().enumerate() {
        let contribution = raw_score.max(0.01) / (RRF_K + rank as f32 + 1.0);
        *scores.entry(id).or_insert(0.0) += contribution;
        fts_set.insert(id);
    }
    for (rank, &(id, raw_score)) in semantic_results.iter().enumerate() {
        let contribution = SEMANTIC_BOOST * raw_score.max(0.01) / (RRF_K + rank as f32 + 1.0);
        *scores.entry(id).or_insert(0.0) += contribution;
        sem_set.insert(id);
    }

    let mut results: Vec<(i64, f32, MatchType)> = scores
        .into_iter()
        .map(|(id, score)| {
            let match_type = match (fts_set.contains(&id), sem_set.contains(&id)) {
                (true, true) => MatchType::Both,
                (true, false) => MatchType::Fts,
                (false, true) => MatchType::Semantic,
                (false, false) => {
                    debug_assert!(false, "score id {} present without fts/semantic membership", id);
                    MatchType::Fts
                }
            };
            (id, score, match_type)
        })
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Apply score cutoff: drop results below top_score * CUTOFF_RATIO,
    // but always keep at least MIN_RESULTS_AFTER_CUTOFF
    if let Some(&(_, top_score, _)) = results.first() {
        let threshold = top_score * SCORE_CUTOFF_RATIO;
        let cutoff_idx = results.iter()
            .position(|(_, s, _)| *s < threshold)
            .unwrap_or(results.len());
        let keep = cutoff_idx.max(MIN_RESULTS_AFTER_CUTOFF).min(results.len());
        results.truncate(keep);
    }

    // Normalize RRF scores to 0..1 (needed for recency decay compatibility)
    if let Some(&(_, max_score, _)) = results.first() {
        if max_score > 0.0 {
            for item in results.iter_mut() {
                item.1 /= max_score;
            }
        }
    }

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
/// Returns a value in [0, 1] that halves every `half_life` days.
pub fn recency_score(age_days: f32, half_life: f32) -> f32 {
    if age_days <= 0.0 {
        return 1.0;
    }
    0.5_f32.powf(age_days / half_life)
}

/// Apply time-decay to merged search results using story dates from the DB.
/// Formula: final = alpha * relevance + (1 - alpha) * recency_score(age)
/// Blend recency into the fused scores, returning the date map it had to build
/// anyway so the caller can filter by date without a second query.
fn apply_recency_decay(
    conn: &Connection,
    results: &mut Vec<(i64, f32, MatchType)>,
    alpha: f32,
    half_life: f32,
) -> Result<std::collections::HashMap<i64, String>> {
    if results.is_empty() {
        return Ok(std::collections::HashMap::new());
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
                let recency = recency_score(age_days, half_life);
                *score = alpha * *score + (1.0 - alpha) * recency;
            }
        }
        // If date lookup/parse fails, keep original score unchanged
    }

    // Re-sort by new scores
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(date_map)
}

/// Drop candidates older than `from` while the candidate pool is still full.
///
/// The date filter used to run *after* the pool had been truncated to `limit`,
/// which meant the top N were chosen by score across the entire archive and only
/// then checked against the requested window. Ask "what happened with X since
/// January" about a topic whose biggest events predate January and the in-range
/// stories — ranked below those — were already gone. The filter shrank the answer
/// instead of shaping the search.
///
/// A candidate whose date could not be resolved is kept: a missing date is a
/// lookup failure, not evidence the story falls outside the window, and silently
/// dropping it would be the same class of bug in the other direction.
fn retain_from_date(
    results: &mut Vec<(i64, f32, MatchType)>,
    date_map: &std::collections::HashMap<i64, String>,
    from: &str,
) {
    results.retain(|(id, _, _)| match date_map.get(id) {
        Some(d) => d.as_str() >= from,
        None => true,
    });
}

// ---------------------------------------------------------------------------
// Full hybrid search
// ---------------------------------------------------------------------------

/// Simple word-overlap similarity between two headlines.
/// Returns 0.0 to 1.0 (Jaccard similarity on word sets).
fn headline_word_overlap(a: &str, b: &str) -> f32 {
    let lower_a = a.to_lowercase();
    let lower_b = b.to_lowercase();
    let words_a: std::collections::HashSet<&str> = lower_a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = lower_b.split_whitespace().collect();
    if words_a.is_empty() || words_b.is_empty() { return 0.0; }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    if union == 0 { 0.0 } else { intersection as f32 / union as f32 }
}

/// Expand a query with known entity names and aliases (tickers, acronyms, etc.).
/// E.g., "NVDA earnings" → "NVDA earnings Nvidia"
pub fn expand_query_with_entities(conn: &Connection, query: &str) -> String {
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut expansions: Vec<String> = Vec::new();
    let lower_query = query.to_lowercase();

    for word in &words {
        if word.len() < 2 { continue; }
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
        if clean.is_empty() { continue; }

        // Look up entities matching this word by name
        let pattern = format!("%{}%", clean);
        if let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT name FROM entities WHERE name_normalized LIKE ?1 LIMIT 3"
        ) {
            if let Ok(rows) = stmt.query_map(params![pattern], |row| row.get::<_, String>(0)) {
                for name in rows.flatten() {
                    let lower_name = name.to_lowercase();
                    if !lower_query.contains(&lower_name) && name.len() > 2
                        && !expansions.iter().any(|e| e.to_lowercase() == lower_name)
                    {
                        expansions.push(name);
                    }
                }
            }
        }

        // Also look up via entity_aliases table (tickers, acronyms)
        if let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT e.name FROM entity_aliases ea
             JOIN entities e ON e.id = ea.entity_id
             WHERE LOWER(ea.alias) = ?1 LIMIT 3"
        ) {
            if let Ok(rows) = stmt.query_map(params![clean], |row| row.get::<_, String>(0)) {
                for name in rows.flatten() {
                    let lower_name = name.to_lowercase();
                    if !lower_query.contains(&lower_name)
                        && !expansions.iter().any(|e| e.to_lowercase() == lower_name)
                    {
                        expansions.push(name);
                    }
                }
            }
        }

        // Reverse: if query contains entity name, find its aliases/tickers
        if let Ok(mut stmt) = conn.prepare(
            "SELECT DISTINCT ea.alias FROM entity_aliases ea
             JOIN entities e ON e.id = ea.entity_id
             WHERE e.name_normalized LIKE ?1 LIMIT 3"
        ) {
            if let Ok(rows) = stmt.query_map(params![pattern], |row| row.get::<_, String>(0)) {
                for alias in rows.flatten() {
                    let lower_alias = alias.to_lowercase();
                    if !lower_query.contains(&lower_alias)
                        && !expansions.iter().any(|e| e.to_lowercase() == lower_alias)
                    {
                        expansions.push(alias);
                    }
                }
            }
        }
    }

    if expansions.is_empty() {
        query.to_string()
    } else {
        tracing::info!("Entity expansion: added {} terms: {:?}", expansions.len(), expansions);
        format!("{} {}", query, expansions.join(" "))
    }
}

/// Expand a query with graph-connected entities (1-hop co-occurrence traversal).
/// For each entity detected in the query, finds related entities with strength ≥ 3
/// and appends their names to the search query.
pub fn expand_query_with_graph(conn: &Connection, query: &str) -> (String, Vec<String>) {
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut graph_entities: Vec<String> = Vec::new();
    let lower_query = query.to_lowercase();

    for word in &words {
        if word.len() < 3 { continue; }
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        if clean.is_empty() { continue; }

        // Check if this word matches an entity name
        if let Ok(mut stmt) = conn.prepare(
            "SELECT name_normalized FROM entities WHERE name_normalized LIKE ?1 LIMIT 1"
        ) {
            let pattern = format!("%{}%", clean);
            if let Ok(Some(entity_norm)) = stmt.query_row(params![pattern], |row| row.get::<_, String>(0)).optional() {
                // Found an entity — traverse its graph
                if let Ok(related) = super::relationships::get_related_entities(conn, &entity_norm, 3.0, 5) {
                    for (name, _strength) in related {
                        let lower_name = name.to_lowercase();
                        if !lower_query.contains(&lower_name) && !graph_entities.iter().any(|g| g.to_lowercase() == lower_name) {
                            graph_entities.push(name);
                        }
                    }
                }
            }
        }
    }

    if graph_entities.is_empty() {
        (query.to_string(), vec![])
    } else {
        tracing::info!("Graph expansion: added {} related entities: {:?}", graph_entities.len(), graph_entities);
        let expanded = format!("{} {}", query, graph_entities.join(" "));
        (expanded, graph_entities)
    }
}

/// Backward-compatible hybrid search (no HyDE).
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    query_embedding: Option<&[f32]>,
    limit: usize,
    query_type: QueryType,
    date_from: Option<&str>,
) -> Result<Vec<ScoredStory>> {
    hybrid_search_with_hyde(conn, query, query_embedding, None, limit, query_type, date_from)
}

/// Full hybrid search with dual semantic embeddings (raw query + HyDE).
pub fn hybrid_search_with_hyde(
    conn: &Connection,
    query: &str,
    query_embedding: Option<&[f32]>,
    hyde_embedding: Option<&[f32]>,
    limit: usize,
    query_type: QueryType,
    date_from: Option<&str>,
) -> Result<Vec<ScoredStory>> {
    // 1. Run FTS search on daily stories
    let fts = fts_search(conn, query, limit * 2)?;

    // 1b. Also search freedom stories
    let fts_freedom = fts_search_freedoms(conn, query, limit).unwrap_or_default();

    // 2. Combine daily + freedom FTS results
    let mut all_fts = fts;
    all_fts.extend(fts_freedom);

    // 3. Run semantic search — merge raw query + HyDE embeddings
    let mut semantic: Vec<(i64, f32)> = Vec::new();
    if let Some(emb) = query_embedding {
        semantic.extend(super::embeddings::find_similar(conn, emb, limit * 3, 0.2).unwrap_or_default());
    }
    if let Some(hyde_emb) = hyde_embedding {
        let hyde_results = super::embeddings::find_similar(conn, hyde_emb, limit * 3, 0.2).unwrap_or_default();
        // Merge: keep max score per story
        for (id, score) in hyde_results {
            if let Some(existing) = semantic.iter_mut().find(|(eid, _)| *eid == id) {
                existing.1 = existing.1.max(score);
            } else {
                semantic.push((id, score));
            }
        }
        // Re-sort by score
        semantic.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }
    semantic.truncate(limit * 3);

    // 4. Merge results
    let mut merged = merge_results(&all_fts, &semantic);

    // 5. Apply recency decay (non-fatal — keeps original scores on error)
    let (alpha, half_life) = query_type.recency_params();
    let date_map = match apply_recency_decay(conn, &mut merged, alpha, half_life) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Recency decay failed (non-fatal): {}", e);
            std::collections::HashMap::new()
        }
    };

    // 5.5. Apply the date filter BEFORE truncating. Running it afterwards meant
    // the top `limit` were picked by score across the whole archive and only then
    // checked against the window, so in-range stories ranked below out-of-range
    // ones were already discarded. Uses the date map recency decay just built, so
    // this costs no extra query.
    if let Some(from) = date_from {
        let before = merged.len();
        retain_from_date(&mut merged, &date_map, from);
        tracing::info!(
            "Date filter >= {}: {} of {} candidates in range (applied pre-truncation)",
            from, merged.len(), before
        );
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

    // 6.5 Apply novelty boost — penalize stories that are near-duplicates of other
    // stories in the same briefing (same headline overlap = same event, different source)
    if stories.len() > 1 {
        // Check each story against all EARLIER stories in the briefing for same-event coverage
        for i in 1..stories.len() {
            let dominated = stories[..i].iter().any(|earlier| {
                earlier.date == stories[i].date && headline_word_overlap(&earlier.headline, &stories[i].headline) > 0.5
            });
            if dominated {
                stories[i].score *= 0.6; // 40% penalty for covering same event
            }
        }
        stories.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    }

    // 7. Date filter, second pass. Step 5.5 already did the load-bearing work on
    // the full candidate pool; this catches only the stragglers whose date could
    // not be resolved from the batch lookup there and so were deliberately kept.
    // Now that each story is loaded we have its real date, so a genuinely
    // out-of-range one still goes.
    if let Some(from) = date_from {
        stories.retain(|s| s.date.as_str() >= from);
    }

    // 7.5. Deduplicate: headline overlap + semantic similarity + source diversity
    if stories.len() > 3 {
        // Load embeddings for semantic dedup (cheap — already in DB)
        let story_embeddings: HashMap<i64, Vec<f32>> = {
            let mut embs = HashMap::new();
            for story in &stories {
                let real_id = decode_story_id(story.story_id).0;
                if let Ok(blob) = conn.query_row(
                    "SELECT embedding FROM story_embeddings WHERE story_id = ?1",
                    params![real_id],
                    |row| row.get::<_, Vec<u8>>(0),
                ) {
                    embs.insert(story.story_id, super::embeddings::deserialize_embedding(&blob));
                }
            }
            embs
        };

        let mut deduped: Vec<ScoredStory> = Vec::with_capacity(stories.len());
        let mut source_counts: HashMap<String, usize> = HashMap::new();
        for story in stories {
            // Max 2 stories from same source
            let count = source_counts.entry(story.source_name.clone()).or_insert(0);
            if *count >= 2 { continue; }
            // Check headline word overlap with already-kept stories
            let headline_dup = deduped.iter().any(|kept| {
                headline_word_overlap(&kept.headline, &story.headline) > 0.75
            });
            if headline_dup { continue; }
            // Check semantic similarity (same event, different headline) — only across sources
            if let Some(emb) = story_embeddings.get(&story.story_id) {
                let semantic_dup = deduped.iter().any(|kept| {
                    if kept.source_name == story.source_name { return false; } // same source handled above
                    if let Some(kept_emb) = story_embeddings.get(&kept.story_id) {
                        super::embeddings::cosine_similarity(emb, kept_emb) > 0.85
                    } else {
                        false
                    }
                });
                if semantic_dup { continue; }
            }
            *count += 1;
            deduped.push(story);
        }
        stories = deduped;
    }

    // 8. Apply feedback reputation boost (non-fatal)
    if !stories.is_empty() {
        let (source_boosts, sector_boosts) = super::feedback::load_reputation_cache(conn);
        if !source_boosts.is_empty() || !sector_boosts.is_empty() {
            for story in stories.iter_mut() {
                let sb = source_boosts.get(&story.source_name).copied().unwrap_or(1.0);
                let secb = sector_boosts.get(&story.sector).copied().unwrap_or(1.0);
                story.score *= sb * secb;
            }
            // Re-sort by boosted scores
            stories.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        }
    }

    Ok(stories)
}

// ---------------------------------------------------------------------------
// Voyage reranking
// ---------------------------------------------------------------------------

const VOYAGE_RERANK_URL: &str = "https://api.voyageai.com/v1/rerank";
const VOYAGE_RERANK_MODEL: &str = "rerank-2-lite";
const VOYAGE_RERANK_TIMEOUT_SECS: u64 = 8;

/// Rerank stories using Voyage rerank-2-lite. Falls back to original order on failure.
/// Returns reordered stories, keeping at most `top_k`.
pub async fn voyage_rerank(
    question: &str,
    stories: Vec<ScoredStory>,
    top_k: usize,
) -> Vec<ScoredStory> {
    if stories.len() <= top_k {
        return stories;
    }

    let api_key = match std::env::var("VOYAGE_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return stories.into_iter().take(top_k).collect(),
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(VOYAGE_RERANK_TIMEOUT_SECS),
        voyage_rerank_inner(&api_key, question, &stories, top_k),
    )
    .await;

    match result {
        Ok(Ok(reranked)) => reranked,
        Ok(Err(e)) => {
            tracing::warn!("Voyage reranking failed (non-fatal): {}", e);
            stories.into_iter().take(top_k).collect()
        }
        Err(_) => {
            tracing::warn!("Voyage reranking timed out after {}s", VOYAGE_RERANK_TIMEOUT_SECS);
            stories.into_iter().take(top_k).collect()
        }
    }
}

async fn voyage_rerank_inner(
    api_key: &str,
    question: &str,
    stories: &[ScoredStory],
    top_k: usize,
) -> anyhow::Result<Vec<ScoredStory>> {
    // Format each story as rich text for reranking
    let documents: Vec<String> = stories
        .iter()
        .map(|s| format!("{}. {}. {}", s.headline, s.summary, s.key_facts))
        .collect();

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": VOYAGE_RERANK_MODEL,
        "query": question,
        "documents": documents,
        "top_k": top_k,
    });

    let resp = client
        .post(VOYAGE_RERANK_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Voyage rerank request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Voyage rerank returned {}: {}", status, body);
    }

    let response: serde_json::Value = resp.json().await?;
    let results = response["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("No data array in Voyage rerank response"))?;

    let mut reranked = Vec::with_capacity(top_k);
    for item in results {
        let idx = item["index"].as_u64().ok_or_else(|| anyhow::anyhow!("missing index"))? as usize;
        if idx < stories.len() {
            let mut story = stories[idx].clone();
            // Update score with Voyage relevance score
            if let Some(relevance) = item["relevance_score"].as_f64() {
                story.score = relevance as f32;
            }
            reranked.push(story);
        }
    }

    Ok(reranked)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;

    // -----------------------------------------------------------------------
    // Date filtering — must happen while the candidate pool is still full
    // -----------------------------------------------------------------------

    fn dated(pairs: &[(i64, f32, &str)]) -> (Vec<(i64, f32, MatchType)>, HashMap<i64, String>) {
        let results = pairs.iter().map(|(id, s, _)| (*id, *s, MatchType::Both)).collect();
        let map = pairs.iter().map(|(id, _, d)| (*id, d.to_string())).collect();
        (results, map)
    }

    #[test]
    fn in_range_stories_ranked_below_out_of_range_ones_survive() {
        // The exact defect: "what happened with X since June" where X's biggest
        // coverage predates June. Filtering after a truncation to 2 would keep the
        // two March stories and return nothing in range.
        let (mut results, map) = dated(&[
            (1, 0.90, "2026-03-01"),
            (2, 0.80, "2026-03-02"),
            (3, 0.20, "2026-07-04"),
            (4, 0.10, "2026-07-05"),
        ]);
        retain_from_date(&mut results, &map, "2026-06-01");
        let ids: Vec<i64> = results.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec![3, 4]);

        // And the ordering that used to destroy them: truncate first, filter after.
        let (before, map2) = dated(&[
            (1, 0.90, "2026-03-01"),
            (2, 0.80, "2026-03-02"),
            (3, 0.20, "2026-07-04"),
            (4, 0.10, "2026-07-05"),
        ]);
        let mut truncated_first: Vec<_> = before.into_iter().take(2).collect();
        retain_from_date(&mut truncated_first, &map2, "2026-06-01");
        assert!(
            truncated_first.is_empty(),
            "filtering after truncation loses every in-range story — this is what was fixed"
        );
    }

    #[test]
    fn a_candidate_with_no_resolvable_date_is_kept_not_guessed() {
        // A missing date is a failed lookup, not evidence of being out of range.
        // Step 7 re-checks it once the story is loaded and its real date is known.
        let (mut results, mut map) = dated(&[(1, 0.9, "2026-07-01"), (2, 0.8, "2026-01-01")]);
        map.remove(&2);
        retain_from_date(&mut results, &map, "2026-06-01");
        let ids: Vec<i64> = results.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn the_boundary_date_is_inclusive() {
        let (mut results, map) = dated(&[(1, 0.9, "2026-06-01"), (2, 0.8, "2026-05-31")]);
        retain_from_date(&mut results, &map, "2026-06-01");
        let ids: Vec<i64> = results.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec![1]);
    }

    // -----------------------------------------------------------------------
    // Recency scoring tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_recency_score_today() {
        let score = recency_score(0.0, 14.0);
        assert!((score - 1.0).abs() < 1e-5, "today should score 1.0, got {}", score);
    }

    #[test]
    fn test_recency_score_half_life() {
        let score = recency_score(14.0, 14.0);
        assert!((score - 0.5).abs() < 1e-5, "14 days should score 0.5, got {}", score);
    }

    #[test]
    fn test_recency_score_two_half_lives() {
        let score = recency_score(28.0, 14.0);
        assert!((score - 0.25).abs() < 1e-5, "28 days should score 0.25, got {}", score);
    }

    #[test]
    fn test_recency_score_old() {
        let score = recency_score(90.0, 14.0);
        assert!(score < 0.02, "90 days should score near zero, got {}", score);
    }

    #[test]
    fn test_recency_score_negative_age() {
        let score = recency_score(-5.0, 14.0);
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
        let results = hybrid_search(&conn, "OpenAI", None, 10, QueryType::General, None).unwrap();
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
        let results = hybrid_search(&conn, "OpenAI", Some(&query_emb), 10, QueryType::General, None).unwrap();

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

        let results = hybrid_search(&conn, "OpenAI", None, 10, QueryType::General, None).unwrap();
        assert!(!results.is_empty());

        let story = &results[0];
        assert_eq!(story.headline, "OpenAI Launches GPT-5");
        assert_eq!(story.sector, "ai");
        assert_eq!(story.date, "2026-03-31");
        assert_eq!(story.source_name, "Test Source");
        assert!(!story.summary.is_empty());
    }
}
