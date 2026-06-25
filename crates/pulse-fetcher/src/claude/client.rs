use super::{SummarizedStory, AnalysisResult, Connection, RelevanceScore, TrendDetection};
use crate::sources::RawArticle;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Groq models — fast inference, OpenAI-compatible API
const FAST_MODEL: &str = "llama-3.1-8b-instant";
const STRONG_MODEL_DEFAULT: &str = "llama-3.3-70b-versatile";
const API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

/// The "strong" model used for the reasoning steps (cross-sector analysis +
/// pre-curation). Overridable via PULSE_STRONG_MODEL for cost A/B testing —
/// e.g. `meta-llama/llama-4-scout-17b-16e-instruct` is ~5x cheaper on input.
/// Defaults to llama-3.3-70b-versatile. Returned owned so callers pass &str.
fn strong_model() -> String {
    std::env::var("PULSE_STRONG_MODEL").unwrap_or_else(|_| STRONG_MODEL_DEFAULT.to_string())
}

const DAILY_PRE_CURATOR_SYSTEM: &str = r#"You are a news editor selecting the most newsworthy articles for a daily intelligence briefing covering 4 sectors: AI & LLMs, Miami Beach, Italy, and Tech & Innovation.

From the list of raw articles below, select the BEST ~90 articles (roughly 22-25 per sector). Pick articles that are:
- Substantive news (not clickbait, listicles, or opinion)
- Non-duplicate (if two articles cover the same story, pick the better source)
- High signal (major events, company news, product launches, policy changes)

Return ONLY a JSON array of article indices, like: [0, 2, 5, 7, 11, ...]
Select ~90 total. No explanation, just the JSON array."#;

const FREEDOMS_PRE_CURATOR_SYSTEM: &str = r#"You are a news editor selecting the most newsworthy articles for a daily Four Freedoms briefing. The briefing has 5 categories, each tagged in the [sector] field of the input:

- freedom_time: productivity, automation, AI agents replacing work, async/remote practices, creator economy
- freedom_wealth: investing, markets, crypto, VC dealflow, fintech infrastructure, real estate, personal finance
- freedom_location: US visa/immigration policy (especially F-1 transitions), digital-nomad/golden visas, geo-arbitrage, travel routes, remote-work connectivity tooling
- freedom_health: longevity, biohacking, fitness/sleep tech, supplements, nutrition science, wearables, product launches
- freedom_whoop: Whoop product/app/firmware/features, Whoop data/research, HRV and recovery science, Whoop-relevant competitor moves

Select the BEST articles across all 5 categories — aim for roughly equal coverage, around 25-35 per category (so ~125-175 total when input is large). Pick articles that are:
- Substantive news (not clickbait, listicles, or opinion)
- Non-duplicate (if two articles cover the same story, pick the better source)
- High signal (major events, company news, product launches, policy changes, research findings)
- For freedom_whoop specifically: keep ALL substantive Whoop coverage even when it seems niche — Whoop news is sparse and we want the downstream curator to have the choice

Return ONLY a JSON array of article indices, like: [0, 2, 5, 7, 11, ...]
No explanation, just the JSON array."#;

pub struct GroqClient {
    api_key: String,
    http: reqwest::Client,
    /// When set, every successful API call is logged to `api_usage` with real
    /// `prompt_tokens` / `completion_tokens` from the Groq response.
    db_path: Option<PathBuf>,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    fmt_type: String,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<ChatChoice>>,
    error: Option<ChatError>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: i64,
    completion_tokens: i64,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatError {
    message: String,
}

#[derive(Deserialize)]
struct SummaryResponse {
    headline: String,
    summary: String,
    key_facts: Vec<String>,
    why_it_matters: String,
    what_to_watch: String,
    importance_score: i32,
    sentiment: Option<f64>,
    novelty: Option<f64>,
    event_type: Option<String>,
}

#[derive(Deserialize)]
struct TranslationResponse {
    title_en: String,
    snippet_en: String,
}

#[derive(Deserialize)]
struct AnalysisResponse {
    connections: Vec<AnalysisConnection>,
    relevance_scores: Vec<AnalysisRelevance>,
    trends: Vec<AnalysisTrend>,
    curation: CurationResult,
}

#[derive(Deserialize)]
struct AnalysisConnection {
    story_ids: Vec<usize>,
    connection: String,
    insight: String,
}

#[derive(Deserialize)]
struct AnalysisRelevance {
    story_id: usize,
    relevance: i32,
    reason: String,
}

#[derive(Deserialize)]
struct AnalysisTrend {
    trend: String,
    story_ids: Vec<usize>,
    trajectory: String,
}

#[derive(Deserialize)]
struct CurationResult {
    #[serde(default)]
    ai: Vec<usize>,
    #[serde(default)]
    miami: Vec<usize>,
    #[serde(default)]
    italy: Vec<usize>,
    #[serde(default)]
    tech: Vec<usize>,
}

impl GroqClient {
    pub fn new(api_key: &str, db_path: Option<PathBuf>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {}", e))?;
        Ok(Self {
            api_key: api_key.to_string(),
            http,
            db_path,
        })
    }

    /// Log a successful API call with REAL token counts from the Groq response.
    /// One row per actual call (not per phase) — retries and parse-fail repeats are billed
    /// separately by Groq, and this matches that.
    fn log_call(&self, model: &str, endpoint: &str, usage: &Usage) {
        if let Some(ref path) = self.db_path {
            if let Ok(conn) = rusqlite::Connection::open(path) {
                crate::db::log_api_usage(
                    &conn, "groq", model, endpoint,
                    usage.prompt_tokens, usage.completion_tokens,
                );
            }
        }
    }

    pub async fn call(&self, model: &str, endpoint: &str, system: &str, user_msg: &str, max_tokens: u32) -> anyhow::Result<String> {
        let request = ChatRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage { role: "system".to_string(), content: system.to_string() },
                ChatMessage { role: "user".to_string(), content: user_msg.to_string() },
            ],
            max_tokens,
            temperature: 0.3,
            response_format: ResponseFormat { fmt_type: "json_object".to_string() },
        };

        // Retry with backoff for rate limits and transient connection errors
        let mut last_err = None;
        for attempt in 0..4u32 {
            let resp = match self.http
                .post(API_URL)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let delay = 10 * (attempt + 1) as u64;
                    tracing::warn!("Groq connection error (attempt {}): {}, retrying in {}s", attempt + 1, e, delay);
                    last_err = Some(format!("{}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
            };

            let status = resp.status();
            if status.as_u16() == 429 {
                let delay = 30 * (attempt + 1) as u64;
                tracing::warn!("Rate limited (attempt {}), waiting {}s...", attempt + 1, delay);
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                continue;
            }

            // Transient/network-layer blocks (403 Access denied, 408 timeout, 5xx) are
            // retryable — a single 8 AM network blip used to abort the whole daily run.
            // Only genuinely fatal statuses (400 bad request, 401 unauthorized) bail.
            let code = status.as_u16();
            if !status.is_success() && (code == 403 || code == 408 || (500..600).contains(&code)) {
                let body = resp.text().await.unwrap_or_default();
                let delay = 15 * (attempt + 1) as u64;
                tracing::warn!("Groq transient error {} (attempt {}): {}, retrying in {}s", status, attempt + 1, body.chars().take(120).collect::<String>(), delay);
                last_err = Some(format!("HTTP {}: {}", status, body));
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await?;
                // Groq's structured-output validator returns 400 json_validate_failed when
                // the model emits a near-valid-but-broken JSON (stray escape, dropped brace).
                // This is STOCHASTIC — a re-roll at temp 0.3 usually clears it, so it must not
                // abort the whole briefing (this was the Phase-4 analyze killer on 2026-06-25).
                // Exception: "max completion tokens reached" is NOT fixable by retry (the output
                // is simply too long for max_tokens) — bail so it surfaces instead of looping.
                if body.contains("json_validate_failed")
                    && !body.contains("max completion tokens")
                    && attempt + 1 < 4
                {
                    let delay = 5 * (attempt + 1) as u64;
                    tracing::warn!("Groq json_validate_failed (attempt {}), re-rolling in {}s", attempt + 1, delay);
                    last_err = Some(format!("HTTP {}: {}", status, body.chars().take(160).collect::<String>()));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
                anyhow::bail!("Groq API error {}: {}", status, body);
            }

            let response: ChatResponse = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    let delay = 10 * (attempt + 1) as u64;
                    tracing::warn!("Groq response parse error (attempt {}): {}, retrying in {}s", attempt + 1, e, delay);
                    last_err = Some(format!("{}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
            };

            if let Some(err) = response.error {
                anyhow::bail!("Groq API error: {}", err.message);
            }

            if let Some(ref u) = response.usage {
                self.log_call(model, endpoint, u);
            }

            let text = response
                .choices
                .and_then(|c| c.into_iter().next())
                .map(|c| c.message.content)
                .unwrap_or_default();

            return Ok(text);
        }

        anyhow::bail!("Groq API: max retries exceeded (last error: {})", last_err.unwrap_or_else(|| "rate limiting".into()))
    }

    /// Like `call()` but returns plain text (no JSON response_format constraint).
    /// Used for executive summaries and other prose generation.
    pub async fn call_text(&self, model: &str, endpoint: &str, system: &str, user_msg: &str, max_tokens: u32) -> anyhow::Result<String> {
        let request = ChatRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage { role: "system".to_string(), content: system.to_string() },
                ChatMessage { role: "user".to_string(), content: user_msg.to_string() },
            ],
            max_tokens,
            temperature: 0.3,
            response_format: ResponseFormat { fmt_type: "text".to_string() },
        };

        let mut last_err = None;
        for attempt in 0..4u32 {
            let resp = match self.http
                .post(API_URL)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let delay = 10 * (attempt + 1) as u64;
                    tracing::warn!("Groq connection error (attempt {}): {}, retrying in {}s", attempt + 1, e, delay);
                    last_err = Some(format!("{}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
            };

            let status = resp.status();
            if status.as_u16() == 429 {
                let delay = 30 * (attempt + 1) as u64;
                tracing::warn!("Rate limited (attempt {}), waiting {}s...", attempt + 1, delay);
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                continue;
            }

            // Transient/network-layer blocks (403 Access denied, 408 timeout, 5xx) are
            // retryable — a single 8 AM network blip used to abort the whole daily run.
            // Only genuinely fatal statuses (400 bad request, 401 unauthorized) bail.
            let code = status.as_u16();
            if !status.is_success() && (code == 403 || code == 408 || (500..600).contains(&code)) {
                let body = resp.text().await.unwrap_or_default();
                let delay = 15 * (attempt + 1) as u64;
                tracing::warn!("Groq transient error {} (attempt {}): {}, retrying in {}s", status, attempt + 1, body.chars().take(120).collect::<String>(), delay);
                last_err = Some(format!("HTTP {}: {}", status, body));
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await?;
                // Groq's structured-output validator returns 400 json_validate_failed when
                // the model emits a near-valid-but-broken JSON (stray escape, dropped brace).
                // This is STOCHASTIC — a re-roll at temp 0.3 usually clears it, so it must not
                // abort the whole briefing (this was the Phase-4 analyze killer on 2026-06-25).
                // Exception: "max completion tokens reached" is NOT fixable by retry (the output
                // is simply too long for max_tokens) — bail so it surfaces instead of looping.
                if body.contains("json_validate_failed")
                    && !body.contains("max completion tokens")
                    && attempt + 1 < 4
                {
                    let delay = 5 * (attempt + 1) as u64;
                    tracing::warn!("Groq json_validate_failed (attempt {}), re-rolling in {}s", attempt + 1, delay);
                    last_err = Some(format!("HTTP {}: {}", status, body.chars().take(160).collect::<String>()));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
                anyhow::bail!("Groq API error {}: {}", status, body);
            }

            let response: ChatResponse = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    let delay = 10 * (attempt + 1) as u64;
                    tracing::warn!("Groq response parse error (attempt {}): {}, retrying in {}s", attempt + 1, e, delay);
                    last_err = Some(format!("{}", e));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
            };

            if let Some(err) = response.error {
                anyhow::bail!("Groq API error: {}", err.message);
            }

            if let Some(ref u) = response.usage {
                self.log_call(model, endpoint, u);
            }

            let text = response
                .choices
                .and_then(|c| c.into_iter().next())
                .map(|c| c.message.content)
                .unwrap_or_default();

            return Ok(text.trim().to_string());
        }

        anyhow::bail!("Groq API: max retries exceeded (last error: {})", last_err.unwrap_or_else(|| "rate limiting".into()))
    }

    pub async fn translate(&self, title: &str, snippet: &str) -> anyhow::Result<(String, String)> {
        let system = "You are a translator. Translate Italian news to English. Return valid JSON only.";
        let user_msg = format!(
            "Translate to English. Return JSON: {{\"title_en\": \"...\", \"snippet_en\": \"...\"}}\n\nTitle: {}\nContent: {}",
            title, snippet
        );

        let text = self.call(FAST_MODEL, "translate", system, &user_msg, 500).await?;
        let parsed: TranslationResponse = serde_json::from_str(&extract_json(&text))?;
        Ok((parsed.title_en, parsed.snippet_en))
    }

    pub async fn summarize_story(&self, article: &RawArticle) -> anyhow::Result<SummarizedStory> {
        let system = super::prompts::SUMMARY_SYSTEM;
        let user_msg = format!(
            "Source: {}\nTitle: {}\nURL: {}\nSnippet: {}\nSector: {}\n\nReturn valid JSON.",
            article.source_name, article.title, article.url, article.content_snippet, article.sector
        );

        // Retry once on transient failures (connection errors, 429, 500)
        let mut last_err = None;
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }

            let text = match self.call(FAST_MODEL, "summarize", system, &user_msg, 2000).await {
                Ok(t) => t,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };

            let json_str = extract_json(&text);
            match serde_json::from_str::<SummaryResponse>(&json_str) {
                Ok(parsed) => {
                    return Ok(SummarizedStory {
                        article: article.clone(),
                        headline: parsed.headline,
                        summary: parsed.summary,
                        key_facts: parsed.key_facts,
                        why_it_matters: parsed.why_it_matters,
                        what_to_watch: parsed.what_to_watch,
                        importance_score: parsed.importance_score,
                        sentiment: parsed.sentiment,
                        novelty: parsed.novelty,
                        event_type: parsed.event_type,
                    });
                }
                Err(e) => {
                    // Try lenient parse: fill missing fields with defaults
                    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                        if let Some(obj) = val.as_object_mut() {
                            obj.entry("importance_score").or_insert(serde_json::json!(5));
                            obj.entry("sentiment").or_insert(serde_json::json!("neutral"));
                            obj.entry("novelty").or_insert(serde_json::json!("incremental"));
                            obj.entry("event_type").or_insert(serde_json::json!("development"));
                            obj.entry("what_to_watch").or_insert(serde_json::json!(""));
                            if let Ok(parsed) = serde_json::from_value::<SummaryResponse>(val) {
                                return Ok(SummarizedStory {
                                    article: article.clone(),
                                    headline: parsed.headline,
                                    summary: parsed.summary,
                                    key_facts: parsed.key_facts,
                                    why_it_matters: parsed.why_it_matters,
                                    what_to_watch: parsed.what_to_watch,
                                    importance_score: parsed.importance_score,
                                    sentiment: parsed.sentiment,
                                    novelty: parsed.novelty,
                                    event_type: parsed.event_type,
                                });
                            }
                        }
                    }
                    last_err = Some(e.into());
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("summarize_story failed after retries")))
    }

    pub async fn analyze(&self, stories: &[SummarizedStory]) -> anyhow::Result<AnalysisResult> {
        let system = super::prompts::ANALYSIS_SYSTEM;

        let mut user_msg = String::new();
        for (i, story) in stories.iter().enumerate() {
            user_msg.push_str(&format!(
                "\n[{}] [{}] {}\nSummary: {}\nImportance: {}\n",
                i, story.article.sector, story.headline, story.summary, story.importance_score
            ));
        }
        user_msg.push_str(&format!(
            "\nReturn valid JSON with keys: connections, relevance_scores, trends, curation.\nYou MUST return exactly {} relevance_scores entries — one for EVERY story listed above. Do not skip any.",
            stories.len()
        ));

        // analyze is PINNED to 70B (not the scout env default). This is the single
        // largest structured-output call in the pipeline — 140 stories → 140
        // relevance_scores + connections + trends + per-sector curation arrays. Measured
        // A/B (2026-06-25): scout failed it twice on the same payload — once with a Groq
        // 400 json_validate_failed (stray escape), once with a 200 whose body our serde
        // rejected ("invalid type: map, expected usize" in the curation field). 70B passed
        // the identical payload first try. pre_curate stays on the cheap scout (simpler
        // index-list output it handles fine); only this call needs the stronger model.
        let text = self.call(STRONG_MODEL_DEFAULT, "analyze", system, &user_msg, 8000).await?;
        let parsed: AnalysisResponse = serde_json::from_str(&extract_json(&text))?;

        if parsed.relevance_scores.len() < stories.len() {
            tracing::warn!(
                "Analysis returned {}/{} relevance_scores — some stories will lack relevance data",
                parsed.relevance_scores.len(), stories.len()
            );
        }

        // Validate and log per-sector curation
        tracing::info!("Curation response: ai={}, miami={}, italy={}, tech={}",
            parsed.curation.ai.len(), parsed.curation.miami.len(),
            parsed.curation.italy.len(), parsed.curation.tech.len());

        if parsed.curation.miami.is_empty() {
            tracing::warn!("Curation: 0 Miami stories — check Miami source feeds");
        }
        if parsed.curation.italy.is_empty() {
            tracing::warn!("Curation: 0 Italy stories — check Italy source feeds");
        }

        let mut curated_indices: Vec<usize> = Vec::new();
        curated_indices.extend(&parsed.curation.ai);
        curated_indices.extend(&parsed.curation.miami);
        curated_indices.extend(&parsed.curation.italy);
        curated_indices.extend(&parsed.curation.tech);

        let curated_stories: Vec<SummarizedStory> = curated_indices
            .iter()
            .filter_map(|&idx| stories.get(idx).cloned())
            .collect();

        let connections = parsed.connections
            .into_iter()
            .filter_map(|c| {
                if c.story_ids.len() >= 2 {
                    Some(Connection {
                        story_idx_a: c.story_ids[0],
                        story_idx_b: c.story_ids[1],
                        connection: c.connection,
                        insight: c.insight,
                    })
                } else {
                    None
                }
            })
            .collect();

        let relevance_scores = parsed.relevance_scores
            .into_iter()
            .map(|r| RelevanceScore {
                story_idx: r.story_id,
                relevance: r.relevance,
                reason: r.reason,
            })
            .collect();

        let trends = parsed.trends
            .into_iter()
            .map(|t| TrendDetection {
                trend: t.trend,
                story_indices: t.story_ids,
                trajectory: t.trajectory,
            })
            .collect();

        Ok(AnalysisResult {
            curated_stories,
            connections,
            relevance_scores,
            trends,
        })
    }
    /// Pre-curate raw articles before summarization. Uses 70B to pick the most
    /// newsworthy articles from the raw pool, then applies a sector-balanced hard
    /// cap (max_keep), so we only pay to summarize stories that can actually make
    /// the briefing AND every sector keeps representation.
    pub async fn pre_curate(&self, articles: &[RawArticle]) -> anyhow::Result<Vec<usize>> {
        // Daily has 4 evenly-supplied sectors — scout (the cheap default) balances them fine.
        self.pre_curate_with_prompt(articles, DAILY_PRE_CURATOR_SYSTEM, "pre_curate", 140, &strong_model()).await
    }

    /// Freedoms-pipeline pre-curator. Same mechanism as daily, but the system
    /// prompt names the 5 freedom categories (Time/Wealth/Location/Health/Whoop)
    /// so freedom articles are not down-weighted as off-sector noise.
    pub async fn pre_curate_freedoms(&self, articles: &[RawArticle]) -> anyhow::Result<Vec<usize>> {
        // Cap = 200 (40/sector × 5 freedom categories), matching the downstream curator's
        // PER_SECTOR_CAP of 40. Summarizing more than the curator can consume wastes calls.
        //
        // MODEL: pinned to 70B (NOT the scout env default). Freedoms has 5 sectors with
        // uneven supply (whoop/health are sparse); measured A/B showed scout selects only
        // ~1.5% of an abundant health pool (5 of 344) and 0 whoop, while 70B selects ~20%
        // (72 of 355) and keeps whoop — i.e. scout starves minority sectors here. The daily
        // 4-symmetric-sector A/B couldn't catch this. 70B costs more but freedoms is one
        // pre_curate call/run, so the delta is tiny vs the coverage it preserves.
        self.pre_curate_with_prompt(articles, FREEDOMS_PRE_CURATOR_SYSTEM, "pre_curate_freedoms", 200, STRONG_MODEL_DEFAULT).await
    }

    async fn pre_curate_with_prompt(&self, articles: &[RawArticle], system: &str, endpoint: &str, max_keep: usize, model: &str) -> anyhow::Result<Vec<usize>> {

        let mut user_msg = String::new();
        for (i, article) in articles.iter().enumerate() {
            // Compact format: just index, sector, source, title — minimal tokens
            user_msg.push_str(&format!(
                "[{}] [{}] {} — {}\n",
                i, article.sector, article.source_name, article.title
            ));
        }
        user_msg.push_str(&format!("\nSelect the best ~{} from these {} articles, in priority order (most important first). Return JSON array of indices.", max_keep, articles.len()));

        let text = self.call_text(model, endpoint, system, &user_msg, 2000).await?;

        // Parse the JSON array — try multiple extraction strategies
        let json_str = extract_json_array(&text);
        let indices: Vec<usize> = serde_json::from_str::<Vec<usize>>(&json_str)
            .or_else(|_| {
                // Try extracting from a JSON object wrapper like {"indices": [...]}
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&extract_json(&text)) {
                    // Find the first array value in the object
                    if let Some(arr) = obj.as_object().and_then(|o| o.values().find(|v| v.is_array())) {
                        return serde_json::from_value(arr.clone());
                    }
                }
                Err(serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, "no array found")))
            })
            .map_err(|_| {
                tracing::warn!("Pre-curation JSON parse failed (response: {}...)", &text[..text.len().min(200)]);
                anyhow::anyhow!("Pre-curation response was not a valid JSON array")
            })?;

        // Validate indices (dedup preserves the LLM's priority order)
        let mut seen = std::collections::HashSet::new();
        let mut valid: Vec<usize> = indices.into_iter()
            .filter(|&i| i < articles.len())
            .filter(|&i| seen.insert(i))
            .collect();

        if valid.len() < 20 {
            tracing::warn!("Pre-curation returned too few articles ({}), falling back to all", valid.len());
            return Ok((0..articles.len()).collect());
        }

        // Sector-balanced selection. The LLM is unreliable two ways: it over-selects
        // (returns 500+ when the prompt says ~90) AND it skews toward one hot sector
        // (e.g. all AI, or all freedom_time) regardless of how rich the other sectors
        // are in the input. A naive top-N truncate inherits the skew. The earlier
        // "freedoms collapses to time-only" came from this: the LLM returned 166 time
        // / 12 wealth / 0 of everything else even though health had 351 candidates.
        //
        // Fix: always round-robin to a per-sector floor, and BACKFILL each sector from
        // the original input pool (best-first by input order) when the LLM under-picked
        // it. This guarantees representation for every sector that HAS candidates, while
        // capping the total at max_keep. A sector with no input candidates stays empty
        // (correct — e.g. whoop on a genuinely quiet news day).
        let before = valid.len();

        // What the LLM picked, grouped by sector (priority order preserved).
        let mut picked_by_sector: std::collections::BTreeMap<&str, std::collections::VecDeque<usize>> =
            std::collections::BTreeMap::new();
        for &i in &valid {
            picked_by_sector.entry(articles[i].sector.as_str()).or_default().push_back(i);
        }
        // The full input pool grouped by sector (for backfill), excluding already-picked.
        let picked_set: std::collections::HashSet<usize> = valid.iter().copied().collect();
        let mut pool_by_sector: std::collections::BTreeMap<&str, std::collections::VecDeque<usize>> =
            std::collections::BTreeMap::new();
        for (i, a) in articles.iter().enumerate() {
            if !picked_set.contains(&i) {
                pool_by_sector.entry(a.sector.as_str()).or_default().push_back(i);
            }
        }
        // Union of all sectors that have ANY candidate (picked or in pool).
        let mut all_sectors: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        all_sectors.extend(picked_by_sector.keys().copied());
        all_sectors.extend(pool_by_sector.keys().copied());

        let per_sector_floor = if all_sectors.is_empty() { max_keep } else { max_keep / all_sectors.len().max(1) };
        let mut balanced: Vec<usize> = Vec::with_capacity(max_keep);

        // Pass 1: take up to per_sector_floor from each sector — LLM picks first, then
        // backfill from the input pool so a sector the LLM ignored still gets filled.
        for sector in &all_sectors {
            let mut taken = 0;
            if let Some(q) = picked_by_sector.get_mut(*sector) {
                while taken < per_sector_floor { if let Some(idx) = q.pop_front() { balanced.push(idx); taken += 1; } else { break; } }
            }
            if let Some(q) = pool_by_sector.get_mut(*sector) {
                while taken < per_sector_floor { if let Some(idx) = q.pop_front() { balanced.push(idx); taken += 1; } else { break; } }
            }
        }
        // Pass 2: fill any remaining cap headroom round-robin from leftovers (picks then pool).
        let mut progressing = true;
        while balanced.len() < max_keep && progressing {
            progressing = false;
            for sector in &all_sectors {
                if balanced.len() >= max_keep { break; }
                let next = picked_by_sector.get_mut(*sector).and_then(|q| q.pop_front())
                    .or_else(|| pool_by_sector.get_mut(*sector).and_then(|q| q.pop_front()));
                if let Some(idx) = next { balanced.push(idx); progressing = true; }
            }
        }

        let mut hist: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for &i in &balanced { *hist.entry(articles[i].sector.as_str()).or_insert(0) += 1; }
        tracing::info!("Pre-curation: sector-balanced {} → {} (floor {}/sector, {:?})",
            before, balanced.len(), per_sector_floor, hist);
        valid = balanced;

        tracing::info!("Pre-curated {} articles from {} raw", valid.len(), articles.len());
        Ok(valid)
    }
}

/// Extract JSON array from response text, handling truncation
fn extract_json_array(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            return trimmed[start..=end].to_string();
        }
        // Truncated response — no closing bracket. Trim trailing comma/whitespace and close it.
        let partial = trimmed[start..].trim_end_matches([',', ' ', '\n', '\r']);
        tracing::warn!("JSON array truncated — closing bracket missing, attempting recovery");
        return format!("{}]", partial);
    }
    trimmed.to_string()
}

/// Extract JSON from a response that might have markdown code fences
fn extract_json(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}
