use super::{SummarizedStory, AnalysisResult, Connection, RelevanceScore, TrendDetection};
use crate::sources::RawArticle;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// Groq models — fast inference, OpenAI-compatible API
const FAST_MODEL: &str = "llama-3.1-8b-instant";
const STRONG_MODEL_DEFAULT: &str = "llama-3.3-70b-versatile";
const API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODELS_URL: &str = "https://api.groq.com/openai/v1/models";

/// Latched "Groq is blocked for this whole run" flag.
///
/// The block is a PRE-AUTH 403 tied to the source IP, and it persists for HOURS — so
/// retrying per call is worthless: 4 attempts with backoff burn ~150s, and at ~140
/// summarize calls a day that is over five hours of pure sleeping before the run gives up.
/// Detect once, latch, and send everything else straight to Anthropic.
///
/// This is what turns a week-long outage into a slightly more expensive briefing.
/// Measured impact of NOT having it: 10 of 45 days with no briefing at all (2026-07-03..12
/// and 2026-08-08..13), every one of them a Groq 403 while Anthropic answered 200 on the
/// same network.
static GROQ_BLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True once any call has seen a Groq 403, or the preflight probe reported blocked.
pub fn groq_is_blocked() -> bool {
    GROQ_BLOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Force the blocked state from the environment (`PULSE_FORCE_GROQ_BLOCKED=1`).
///
/// This exists so the fallback can be EXERCISED on demand. The path it guards only runs
/// on days Groq happens to block the source IP, which is precisely when nobody is at a
/// keyboard to watch it — an untested failover is not a failover. Called once at startup.
pub fn apply_forced_block_from_env() {
    if std::env::var("PULSE_FORCE_GROQ_BLOCKED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        mark_groq_blocked("PULSE_FORCE_GROQ_BLOCKED=1 (forced for testing)");
    }
}

/// Latch the block. Idempotent; logs only on the transition so the line means something.
pub fn mark_groq_blocked(reason: &str) {
    if !GROQ_BLOCKED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            "GROQ BLOCKED ({}). Routing the rest of this run to Anthropic — the briefing \
             still lands, at a higher per-call cost. The daily cost cap still applies.",
            reason
        );
    }
}

/// Anthropic stand-in for a Groq model.
///
/// Both Groq models map to Haiku 4.5: it is the cheapest Anthropic model that reliably
/// produces the structured output these prompts ask for, and the fallback is a
/// availability measure, not a quality upgrade. Deliberately NOT Sonnet/Opus — a blocked
/// day would then cost several times the daily cap and abort halfway through.
pub fn anthropic_equivalent(groq_model: &str) -> &'static str {
    match groq_model {
        FAST_MODEL => "claude-haiku-4-5-20251001",
        _ => "claude-haiku-4-5-20251001",
    }
}

/// Does this Groq HTTP status mean "your IP is blocked", as opposed to a transient blip?
/// 403 is the pre-auth block signature; everything else retries normally.
pub fn is_groq_block_status(code: u16) -> bool {
    code == 403
}

/// Preflight reachability probe. Groq blocks certain source IPs (NordVPN exit
/// nodes when in Italy, the school network on Tuesdays) with a PRE-AUTH
/// `403 "Access denied. Please check your network settings."`. That 403 persists
/// for the whole retry window, so the per-call retries in `call()` cannot save a
/// run started into a block — they just burn ~90 min then abort and fire the
/// daily "not working" alert. This probe lets a scheduled run detect the block
/// cheaply and exit silently instead, so a later slot (7am/12pm/6pm/10pm) can
/// catch a reachable window.
///
/// Unauthenticated GET to `/models`: 403 = blocked (IP is on Groq's list),
/// anything else (401 unauthorized / 200 / etc.) = reachable. No API key needed —
/// the block is applied before auth, so an invalid/absent key still reproduces it.
/// Returns `true` if Groq is reachable, `false` if blocked or the probe errored
/// (fail-closed: a network error at probe time means don't start the pipeline).
pub async fn groq_reachable() -> bool {
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        // Local client-build failure says nothing about the blocklist — fail open.
        Err(_) => return true,
    };
    // Only an HTTP 403 is evidence of Groq's IP blocklist — that's what this
    // probe exists to detect, and only that should skip the slot. A TRANSPORT
    // error (DNS, connect, TLS) proves nothing about the blocklist, and
    // fail-closed on it caused the 2026-08-07→09 outage: launchd-context runs
    // hit "error sending request" for six consecutive slots and skipped
    // SILENTLY while the shell reached Groq fine. Fail OPEN on transport
    // errors (after one retry): if the network is genuinely dead the pipeline
    // fails fast and LOUDLY via notify_failure instead of vanishing.
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        match http.get(MODELS_URL).send().await {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if code == 403 {
                    tracing::warn!("Groq preflight: HTTP 403 — source IP blocked (VPN/network). Skipping run.");
                    return false;
                }
                tracing::info!("Groq preflight: HTTP {} — reachable.", code);
                return true;
            }
            Err(e) => {
                tracing::warn!("Groq preflight: probe transport error (attempt {}): {}", attempt + 1, e);
            }
        }
    }
    tracing::warn!("Groq preflight: probe never connected — proceeding anyway (transport error is not a 403; a dead network will fail loudly in the pipeline).");
    true
}

/// The "strong" Groq model for pre-curation. Overridable via PULSE_STRONG_MODEL
/// for A/B testing, but ONLY with a model confirmed live on Groq's /models list —
/// the Llama-4 Scout override 404'd silently for 3 weeks after Groq
/// decommissioned it (~2026-07-17). Defaults to llama-3.3-70b-versatile.
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


#[derive(Deserialize, Default)]
struct AnalysisResponse {
    // Every field tolerant: the 70B model intermittently omits one (observed:
    // `missing field curation` on an otherwise-complete object). A missing field
    // must NOT throw away the whole briefing — we keep whatever the model DID return
    // (e.g. connections) and derive the rest. See analyze() for the recovery logic.
    #[serde(default)]
    connections: Vec<AnalysisConnection>,
    #[serde(default)]
    relevance_scores: Vec<AnalysisRelevance>,
    #[serde(default)]
    trends: Vec<AnalysisTrend>,
    #[serde(default)]
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

#[derive(Deserialize, Default)]
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
        // Groq already known blocked this run -> do not spend 150s of retries proving it
        // again for every one of ~140 stories. Straight to Anthropic.
        if groq_is_blocked() {
            return self
                .call_anthropic(anthropic_equivalent(model), endpoint, system, user_msg, max_tokens)
                .await;
        }
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
            // A 403 is the pre-auth IP block, and it lasts for HOURS — retrying it is the
            // behaviour that produced ~90-minute runs that then aborted anyway. Latch it
            // and switch this call (and every later one) to Anthropic instead.
            if is_groq_block_status(code) {
                let body = resp.text().await.unwrap_or_default();
                mark_groq_blocked(&format!("HTTP 403 on {}: {}", endpoint, body.chars().take(80).collect::<String>()));
                return self
                    .call_anthropic(anthropic_equivalent(model), endpoint, system, user_msg, max_tokens)
                    .await;
            }
            if !status.is_success() && (code == 408 || (500..600).contains(&code)) {
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
        if groq_is_blocked() {
            return self
                .call_anthropic(anthropic_equivalent(model), endpoint, system, user_msg, max_tokens)
                .await;
        }
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

            // 408/5xx are retryable — a single 8 AM network blip used to abort the whole
            // daily run. 403 is NOT retryable: it is the pre-auth IP block, it lasts for
            // hours, and it is handled above by latching and switching to Anthropic.
            // Only genuinely fatal statuses (400 bad request, 401 unauthorized) bail.
            let code = status.as_u16();
            if is_groq_block_status(code) {
                let body = resp.text().await.unwrap_or_default();
                mark_groq_blocked(&format!("HTTP 403 on {}: {}", endpoint, body.chars().take(80).collect::<String>()));
                return self
                    .call_anthropic(anthropic_equivalent(model), endpoint, system, user_msg, max_tokens)
                    .await;
            }
            if !status.is_success() && (code == 408 || (500..600).contains(&code)) {
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

    /// Call Claude (Anthropic API) for the analyze stage. Separate from `call()`
    /// because Anthropic's Messages API differs from Groq's OpenAI-compatible one
    /// (headers, body shape, usage field names). Logs real token counts under
    /// provider "anthropic". Retries on 429/5xx/network; bails on 4xx.
    async fn call_anthropic(&self, model: &str, endpoint: &str, system: &str, user_msg: &str, max_tokens: u32) -> anyhow::Result<String> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

        let body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "temperature": 0.3,
            "system": system,
            "messages": [{"role": "user", "content": user_msg}]
        });

        let mut last_err = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(15 * attempt as u64)).await;
            }
            let resp = match self.http
                .post("https://api.anthropic.com/v1/messages")
                // self.http's 60s default was tuned for Groq; an 8000-max_token
                // generation over a 140-story payload can exceed it. Per-request
                // override so a slow-but-fine Haiku call isn't billed then discarded.
                .timeout(std::time::Duration::from_secs(180))
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Anthropic connection error (attempt {}): {}", attempt + 1, e);
                    last_err = Some(format!("{}", e));
                    continue;
                }
            };

            let status = resp.status();
            let code = status.as_u16();
            if code == 429 || (500..600).contains(&code) {
                let text = resp.text().await.unwrap_or_default();
                tracing::warn!("Anthropic transient error {} (attempt {}): {}", status, attempt + 1, text.chars().take(120).collect::<String>());
                last_err = Some(format!("HTTP {}: {}", status, text));
                continue;
            }
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Anthropic API error {}: {}", status, text);
            }

            let response: serde_json::Value = resp.json().await
                .map_err(|e| anyhow::anyhow!("Anthropic response parse error: {}", e))?;

            if let Some(ref path) = self.db_path {
                if let Ok(conn) = rusqlite::Connection::open(path) {
                    crate::db::log_api_usage(
                        &conn, "anthropic", model, endpoint,
                        response["usage"]["input_tokens"].as_i64().unwrap_or(0),
                        response["usage"]["output_tokens"].as_i64().unwrap_or(0),
                    );
                }
            }

            // Distinguish truncation from model non-compliance: a max_tokens stop
            // means the JSON is syntactically cut off — bail with a clear reason
            // instead of letting the downstream parse-fail log blame the model.
            if response["stop_reason"].as_str() == Some("max_tokens") {
                anyhow::bail!("Anthropic hit max_tokens ({}) for {} — output truncated", max_tokens, endpoint);
            }
            let text = response["content"][0]["text"].as_str().unwrap_or("").to_string();
            if text.is_empty() {
                anyhow::bail!("Anthropic returned empty content for {}", endpoint);
            }
            return Ok(text);
        }

        anyhow::bail!("Anthropic API: max retries exceeded (last error: {})", last_err.unwrap_or_else(|| "unknown".into()))
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

        // analyze runs on Claude Haiku 4.5 (Anthropic), falling back to Groq 70B if
        // the Anthropic call fails or the key is absent. History: this is the single
        // largest structured-output call in the pipeline — 140 stories → 140
        // relevance_scores + connections + trends + per-sector curation arrays. Scout
        // failed it twice (json_validate_failed / serde reject); 70B passed but still
        // produces dup-key and missing-field bodies (see the salvage below). Haiku 4.5
        // is the upgrade for exactly that non-compliance (~+$1.3/mo at 2026-08 volume),
        // and as a bonus is immune to the Groq VPN/IP 403 block. Set
        // PULSE_ANALYZE_PROVIDER=groq to force the old path for A/B.
        let force_groq = std::env::var("PULSE_ANALYZE_PROVIDER").map(|v| v.eq_ignore_ascii_case("groq")).unwrap_or(false);
        let text = if !force_groq && std::env::var("ANTHROPIC_API_KEY").is_ok() {
            // 16000, not the Groq path's 8000: Haiku's relevance `reason` strings run
            // longer than 70B's and a 125-story payload measured ~8k output — the 8000
            // cap truncated a live run (2026-08-06). Haiku bills only tokens produced.
            match self.call_anthropic("claude-haiku-4-5", "analyze", system, &user_msg, 16000).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("analyze via Anthropic failed ({}), falling back to Groq 70B", e);
                    self.call(STRONG_MODEL_DEFAULT, "analyze", system, &user_msg, 8000).await?
                }
            }
        } else {
            self.call(STRONG_MODEL_DEFAULT, "analyze", system, &user_msg, 8000).await?
        };
        let json_str = extract_json(&text);
        // Salvage the common serde-on-200 failure: Groq returns HTTP 200 whose body
        // has DUPLICATE keys (e.g. `"relevance"` twice in one object). serde's derived
        // struct deserializer rejects duplicates ("duplicate field `relevance`"), but
        // serde_json::Value collapses them last-wins — so parse to Value first, then
        // into the struct. Mirrors summarize_story's lenient fallback (this file).
        let parsed: AnalysisResponse = match serde_json::from_str(&json_str) {
            Ok(p) => p,
            Err(strict_err) => serde_json::from_str::<serde_json::Value>(&json_str)
                .ok()
                .and_then(|val| serde_json::from_value(val).ok())
                .ok_or_else(|| {
                    // INSTRUMENTATION (not a fix): both parses failed. Log the RAW model
                    // body — head + tail — so the NEXT free scheduled run reveals what Groq
                    // actually returned (missing `curation`? truncated? which keys present?),
                    // instead of us inferring the failure mode from serde's paraphrase.
                    // Observed modes so far: dup-key `relevance` (salvageable, above), and a
                    // clean `missing field curation` on a complete object that padded to the
                    // 8000-token cap (model non-compliance, NOT truncation — a cut-off body
                    // would be a SYNTAX error, not a clean missing-key). This log decides
                    // whether a tolerant #[serde(default)] fix would recover the connections.
                    let head: String = json_str.chars().take(800).collect();
                    let tail: String = {
                        let n = json_str.chars().count();
                        json_str.chars().skip(n.saturating_sub(800)).collect()
                    };
                    tracing::error!(
                        "analyze parse-fail — strict: {} | body_len={} chars | HEAD: {} | TAIL: {}",
                        strict_err, json_str.chars().count(), head, tail
                    );
                    anyhow::anyhow!(
                        "analyze: strict parse failed ({}) and Value salvage also failed", strict_err
                    )
                })?,
        };

        let mut parsed = parsed;
        if parsed.relevance_scores.len() < stories.len() {
            tracing::warn!(
                "Analysis returned {}/{} relevance_scores — retrying the missing ones",
                parsed.relevance_scores.len(), stories.len()
            );
            // One targeted retry: score ONLY the skipped stories. Haiku reliably
            // handles a small single-task batch even when it dropped entries from
            // the giant 4-task call. Best-effort — a failure keeps the shortfall.
            let missing = missing_relevance_ids(&parsed.relevance_scores, stories.len());
            let mut retry_msg = String::from(
                "Score each story 1-10 for personal relevance to a tech founder in Miami Beach \
                 who builds AI/ML apps, Shopify tools, and iOS apps; Italian heritage, follows Serie A.\n\
                 Return ONLY JSON: {\"relevance_scores\": [{\"story_id\": <id>, \"relevance\": <1-10>, \"reason\": \"...\"}]}\n\
                 Use the EXACT story_id values shown. Score every story listed.\n",
            );
            for &i in &missing {
                retry_msg.push_str(&format!(
                    "\n[{}] [{}] {}\n{}\n",
                    i, stories[i].article.sector, stories[i].headline, stories[i].summary
                ));
            }
            #[derive(Deserialize)]
            struct RetryScores { relevance_scores: Vec<AnalysisRelevance> }
            let retry_text = match self
                .call_anthropic("claude-haiku-4-5", "relevance_retry", "You score news stories for personal relevance. Return valid JSON only.", &retry_msg, 4000)
                .await
            {
                Ok(t) => Ok(t),
                Err(_) => self.call(STRONG_MODEL_DEFAULT, "relevance_retry", "You score news stories for personal relevance. Return valid JSON only.", &retry_msg, 4000).await,
            };
            match retry_text {
                Ok(t) => match serde_json::from_str::<RetryScores>(&extract_json(&t)) {
                    Ok(r) => {
                        let recovered = merge_retry_scores(&mut parsed.relevance_scores, r.relevance_scores, &missing);
                        tracing::info!("Relevance retry recovered {}/{} missing scores", recovered, missing.len());
                    }
                    Err(e) => tracing::warn!("Relevance retry parse failed (non-fatal): {}", e),
                },
                Err(e) => tracing::warn!("Relevance retry call failed (non-fatal): {}", e),
            }
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

        // All index remapping + curation lives in a pure function so it can be
        // unit-tested without a live Groq call (see tests below).
        Ok(build_analysis_result(parsed, stories))
    }
    /// Dedicated cross-sector connections pass over the CURATED stories.
    /// The 4-task analyze prompt reliably under-delivers connections (models
    /// pad with same-sector pairs or skip the task); a single-task call over
    /// the final curated list complies far better. Indices in the input ARE
    /// curated positions, so no URL remap is needed — just bounds+sector checks.
    /// Best-effort: callers keep analyze()'s connections on Err.
    pub async fn find_connections(&self, curated: &[SummarizedStory]) -> anyhow::Result<Vec<Connection>> {
        let system = "You find cross-sector connections between news stories for an intelligence briefing. Return valid JSON only.";
        let mut user_msg = String::from(
            "Below are today's curated stories, each tagged [index] [sector].\n\
             Find 3-6 connections. Each connection links exactly 2 stories whose [sector] tags DIFFER \
             (ai↔tech, ai↔italy, miami↔tech, ...). Same-sector pairs are invalid and will be discarded.\n\
             Look for: shared companies, shared policy/regulation, shared money flows, shared people, shared supply chains.\n\
             The insight must say something neither story says alone.\n\
             Return ONLY JSON: {\"connections\": [{\"story_ids\": [3, 87], \"connection\": \"...\", \"insight\": \"...\"}]}\n\
             FINAL CHECK: for each pair, confirm the two [sector] tags differ — replace any same-sector pair.\n",
        );
        for (i, s) in curated.iter().enumerate() {
            user_msg.push_str(&format!("\n[{}] [{}] {}", i, s.article.sector, s.headline));
        }

        #[derive(Deserialize)]
        struct ConnectionsOnly { connections: Vec<AnalysisConnection> }
        let text = match self.call_anthropic("claude-haiku-4-5", "connections", system, &user_msg, 2000).await {
            Ok(t) => Ok(t),
            Err(_) => self.call(STRONG_MODEL_DEFAULT, "connections", system, &user_msg, 2000).await,
        }?;
        let parsed: ConnectionsOnly = serde_json::from_str(&extract_json(&text))?;

        let mut dropped = 0usize;
        let connections: Vec<Connection> = parsed.connections.into_iter()
            .filter_map(|c| {
                if c.story_ids.len() < 2 { return None; }
                let (a, b) = (c.story_ids[0], c.story_ids[1]);
                if a >= curated.len() || b >= curated.len() || a == b { return None; }
                if curated[a].article.sector == curated[b].article.sector {
                    dropped += 1;
                    return None;
                }
                Some(Connection { story_idx_a: a, story_idx_b: b, connection: c.connection, insight: c.insight })
            })
            .collect();
        tracing::info!("Dedicated connections pass: {} cross-sector kept, {} same-sector dropped", connections.len(), dropped);
        Ok(connections)
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

/// Turn a parsed analyze response + the analyze INPUT `stories` array into an
/// `AnalysisResult`, remapping every model index onto the final curated array.
///
/// Two things this fixes, both load-bearing:
///  1. Empty curation (the `missing field curation` non-compliance case) → derive the
///     curated set from importance so the briefing still curates, while KEEPING the
///     connections/relevance the model returned.
///  2. The model's connection/relevance/trend indices reference the INPUT array, but
///     the persist path resolves them positionally against `curated_stories`. Each
///     index is translated to the story's position in `curated_stories` (matched by
///     URL, the stable identity). Before this, an input index like 15 was read as
///     curated position 15 — landing inside the sector-grouped block and producing the
///     same-sector "cross-sector" connections seen in the DB (ai↔ai, miami↔miami).
/// Input-story ids the model's relevance_scores skipped. Ids outside
/// `0..total` are model garbage and do NOT mark a story as scored.
fn missing_relevance_ids(scored: &[AnalysisRelevance], total: usize) -> Vec<usize> {
    let have: std::collections::HashSet<usize> = scored
        .iter()
        .map(|r| r.story_id)
        .filter(|&id| id < total)
        .collect();
    (0..total).filter(|i| !have.contains(i)).collect()
}

/// Merge retry scores into the main list: accept only ids we actually asked
/// for (the missing set), at most once each. Returns how many were recovered.
fn merge_retry_scores(
    scores: &mut Vec<AnalysisRelevance>,
    retry: Vec<AnalysisRelevance>,
    missing: &[usize],
) -> usize {
    let wanted: std::collections::HashSet<usize> = missing.iter().copied().collect();
    let mut taken: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut recovered = 0usize;
    for s in retry {
        if wanted.contains(&s.story_id) && taken.insert(s.story_id) {
            scores.push(s);
            recovered += 1;
        }
    }
    recovered
}

fn build_analysis_result(parsed: AnalysisResponse, stories: &[SummarizedStory]) -> AnalysisResult {
    let mut curated_input_indices: Vec<usize> = Vec::new();
    curated_input_indices.extend(&parsed.curation.ai);
    curated_input_indices.extend(&parsed.curation.miami);
    curated_input_indices.extend(&parsed.curation.italy);
    curated_input_indices.extend(&parsed.curation.tech);

    let curated_stories: Vec<SummarizedStory> = if curated_input_indices.is_empty() {
        tracing::warn!(
            "analyze: model returned empty curation — deriving curated set from importance \
             (connections/relevance still kept from the model's response)"
        );
        super::degraded_analysis(stories).curated_stories
    } else {
        curated_input_indices
            .iter()
            .filter_map(|&idx| stories.get(idx).cloned())
            .collect()
    };

    let curated_pos_by_url: std::collections::HashMap<&str, usize> = curated_stories
        .iter()
        .enumerate()
        .map(|(pos, s)| (s.article.url.as_str(), pos))
        .collect();
    // input index -> curated position (None if that input story wasn't curated)
    let remap = |input_idx: usize| -> Option<usize> {
        stories
            .get(input_idx)
            .and_then(|s| curated_pos_by_url.get(s.article.url.as_str()).copied())
    };

    // Count same-sector pairs the model produced so we can observe, across free scheduled
    // runs, whether it EVER emits genuine cross-sector connections. The model frequently
    // returns pairs within one sector (e.g. ai↔ai) despite the prompt asking for
    // cross-sector links; those are not valid instances of a feature literally named
    // "cross-sector connections", so they are dropped (no fallback — empty-but-honest beats
    // wrong-but-present). Making cross-sector pairs reliably APPEAR is a separate prompt-level
    // fix (costs Groq); this filter only removes the same-sector garbage.
    let mut same_sector_dropped = 0usize;
    let connections: Vec<Connection> = parsed.connections
        .into_iter()
        .filter_map(|c| {
            if c.story_ids.len() < 2 {
                return None;
            }
            let a = remap(c.story_ids[0])?;
            let b = remap(c.story_ids[1])?;
            if a == b {
                return None;
            }
            // Reject same-sector pairs: this is the honesty fix for "cross-sector connections".
            if curated_stories[a].article.sector == curated_stories[b].article.sector {
                same_sector_dropped += 1;
                // Log the model's raw claim so scheduled runs reveal WHETHER the model
                // mislabels sectors or the remap does: input ids vs the sectors we resolved.
                tracing::info!(
                    "  dropped same-sector pair: model story_ids={:?} → [{}] {} ↔ [{}] {}",
                    c.story_ids,
                    curated_stories[a].article.sector, curated_stories[a].headline.chars().take(50).collect::<String>(),
                    curated_stories[b].article.sector, curated_stories[b].headline.chars().take(50).collect::<String>()
                );
                return None;
            }
            Some(Connection {
                story_idx_a: a,
                story_idx_b: b,
                connection: c.connection,
                insight: c.insight,
            })
        })
        .collect();
    tracing::info!(
        "Connections: {} cross-sector kept, {} same-sector dropped",
        connections.len(),
        same_sector_dropped
    );

    let relevance_scores: Vec<RelevanceScore> = parsed.relevance_scores
        .into_iter()
        .filter_map(|r| {
            remap(r.story_id).map(|pos| RelevanceScore {
                story_idx: pos,
                relevance: r.relevance,
                reason: r.reason,
            })
        })
        .collect();

    let trends: Vec<TrendDetection> = parsed.trends
        .into_iter()
        .map(|t| TrendDetection {
            trend: t.trend,
            story_indices: t.story_ids.into_iter().filter_map(remap).collect(),
            trajectory: t.trajectory,
        })
        .collect();

    AnalysisResult {
        curated_stories,
        connections,
        relevance_scores,
        trends,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(id: usize, score: i32) -> AnalysisRelevance {
        AnalysisRelevance { story_id: id, relevance: score, reason: "r".into() }
    }

    /// Mid-sequence gaps + out-of-range garbage ids: garbage must not mark a
    /// story as scored, and gaps anywhere (not just the tail) must be found.
    #[test]
    fn missing_ids_finds_gaps_and_ignores_garbage() {
        let scored = vec![rel(0, 5), rel(2, 6), rel(999, 9), rel(4, 7)];
        assert_eq!(missing_relevance_ids(&scored, 5), vec![1, 3]);
        assert_eq!(missing_relevance_ids(&[], 3), vec![0, 1, 2]);
        assert_eq!(missing_relevance_ids(&[rel(0, 5), rel(1, 5), rel(2, 5)], 3), Vec::<usize>::new());
    }

    /// Merge must reject unrequested ids, garbage ids, and duplicates — the
    /// retry model echoing already-scored stories must not double-score them.
    #[test]
    fn merge_accepts_only_requested_ids_once() {
        let mut scores = vec![rel(0, 5), rel(2, 6)];
        let missing = vec![1, 3];
        let retry = vec![rel(1, 4), rel(1, 8), rel(2, 9), rel(999, 1), rel(3, 7)];
        let recovered = merge_retry_scores(&mut scores, retry, &missing);
        assert_eq!(recovered, 2);
        assert_eq!(scores.len(), 4);
        assert!(scores.iter().filter(|s| s.story_id == 1).count() == 1);
        assert!(scores.iter().filter(|s| s.story_id == 2).count() == 1); // not double-scored
        assert!(!scores.iter().any(|s| s.story_id == 999));
    }


    /// The salvage path in `analyze()`: a Groq HTTP-200 body with a DUPLICATE key
    /// (`"relevance"` twice in one object) must not throw away the whole briefing.
    /// serde's derived struct deserializer rejects duplicates; serde_json::Value
    /// collapses them last-wins; from_value then succeeds. This test reproduces the
    /// exact stochastic failure that degraded briefing 313 and asserts recovery.
    #[test]
    fn analyze_salvages_duplicate_relevance_key() {
        // Note the duplicate `"relevance"` inside the first relevance_scores entry.
        let json_str = r#"{
            "connections": [],
            "relevance_scores": [
                {"story_id": 0, "relevance": 3, "reason": "first", "relevance": 8},
                {"story_id": 1, "relevance": 5, "reason": "second"}
            ],
            "trends": [],
            "curation": {"ai": [0], "miami": [], "italy": [], "tech": [1]}
        }"#;

        // Strict parse MUST fail on the duplicate key (this is the bug).
        assert!(
            serde_json::from_str::<AnalysisResponse>(json_str).is_err(),
            "strict parse should reject the duplicate `relevance` key"
        );

        // Salvage path: Value collapses duplicates last-wins, then from_value succeeds.
        let parsed: AnalysisResponse = serde_json::from_str::<serde_json::Value>(json_str)
            .ok()
            .and_then(|val| serde_json::from_value(val).ok())
            .expect("Value salvage should recover the duplicate-key response");

        assert_eq!(parsed.relevance_scores.len(), 2);
        // last-wins: the second `relevance: 8` overrides the first `relevance: 3`.
        assert_eq!(parsed.relevance_scores[0].relevance, 8);
        assert_eq!(parsed.curation.ai, vec![0]);
        assert_eq!(parsed.curation.tech, vec![1]);
    }

    // ---- fixtures for the build_analysis_result / index-remap tests ----

    fn story(url: &str, sector: &str, importance: i32) -> SummarizedStory {
        SummarizedStory {
            article: crate::sources::RawArticle {
                title: format!("t-{url}"),
                url: url.to_string(),
                source_name: "src".into(),
                source_url: "http://src".into(),
                published_at: None,
                content_snippet: "snip".into(),
                sector: sector.to_string(),
                feed_id: "feed".into(),
                language: "en".into(),
                source_type: "news".into(),
                financial_metadata: None,
            },
            headline: format!("h-{url}"),
            summary: "s".into(),
            key_facts: vec![],
            why_it_matters: "w".into(),
            what_to_watch: "".into(),
            importance_score: importance,
            sentiment: None,
            novelty: None,
            event_type: None,
        }
    }

    /// A missing `curation` field must now parse (tolerant defaults) instead of erroring,
    /// and the connections the model DID return must survive — this is the exact
    /// non-compliance mode that degraded briefing 314.
    #[test]
    fn missing_curation_keeps_connections_and_derives_curation() {
        // 4 input stories, one per sector, with a real cross-sector connection [0,1].
        let input = vec![
            story("a", "ai", 9),
            story("b", "miami", 8),
            story("c", "italy", 7),
            story("d", "tech", 6),
        ];
        // Body OMITS curation entirely (model non-compliance). Tolerant parse must succeed.
        let body = r#"{
            "connections": [{"story_ids": [0, 1], "connection": "ai↔miami", "insight": "i"}],
            "relevance_scores": [{"story_id": 2, "relevance": 9, "reason": "r"}],
            "trends": []
        }"#;
        let parsed: AnalysisResponse = serde_json::from_str(body)
            .expect("missing `curation` must parse with tolerant defaults");

        let result = build_analysis_result(parsed, &input);

        // Curation derived from importance → all 4 stories curated (each sector < 35 cap).
        assert_eq!(result.curated_stories.len(), 4);
        // The connection is KEPT (not thrown away) and remapped to curated positions.
        assert_eq!(result.connections.len(), 1, "the model's connection must survive");
        // Relevance for input story 2 ("c") is kept and remapped to its curated position.
        assert_eq!(result.relevance_scores.len(), 1);
        let c_pos = result.curated_stories.iter().position(|s| s.article.url == "c").unwrap();
        assert_eq!(result.relevance_scores[0].story_idx, c_pos);
    }

    /// A body missing EVERYTHING (all defaults) must not crash: 0 connections, curation
    /// derived from importance, news still flows. Never worse than the old behavior.
    #[test]
    fn empty_body_derives_curation_zero_connections() {
        let input = vec![story("a", "ai", 5), story("b", "miami", 4)];
        let parsed: AnalysisResponse = serde_json::from_str("{}").expect("empty object parses");
        let result = build_analysis_result(parsed, &input);
        assert_eq!(result.curated_stories.len(), 2);
        assert_eq!(result.connections.len(), 0);
        assert_eq!(result.relevance_scores.len(), 0);
    }

    /// The pre-existing Bug B regression: model indices reference the INPUT array, but the
    /// persist path resolves positionally against curated_stories. When curation REORDERS
    /// the input, a connection's endpoints must remap to the stories the model MEANT — not
    /// collide onto whatever sits at those positions. Before the fix, connection [0,2]
    /// (ai↔italy) with curation reordering would land on the wrong, often same-sector, rows.
    #[test]
    fn connection_indices_remap_through_reordered_curation() {
        // Input order: [0]=ai, [1]=miami, [2]=italy, [3]=tech.
        let input = vec![
            story("a", "ai", 9),
            story("b", "miami", 8),
            story("c", "italy", 7),
            story("d", "tech", 6),
        ];
        // Curation drops miami and keeps ai/italy/tech. The output is concatenated in
        // fixed sector order (ai, miami, italy, tech), so with input indices
        // {ai:[0], italy:[2], tech:[3]} the curated array = [a(ai), c(italy), d(tech)].
        // The key point is that these curated POSITIONS (0,1,2) differ from the INPUT
        // indices (0,2,3), so a naive positional resolve would corrupt the connection.
        let body = r#"{
            "connections": [
                {"story_ids": [0, 2], "connection": "ai↔italy", "insight": "kept"},
                {"story_ids": [0, 1], "connection": "ai↔miami(dropped)", "insight": "x"}
            ],
            "relevance_scores": [],
            "trends": [],
            "curation": {"ai": [0], "miami": [], "italy": [2], "tech": [3]}
        }"#;
        let parsed: AnalysisResponse = serde_json::from_str(body).unwrap();
        let result = build_analysis_result(parsed, &input);

        // curated order: ai(a)=0, italy(c)=1, tech(d)=2
        assert_eq!(result.curated_stories.len(), 3);
        assert_eq!(result.curated_stories[0].article.url, "a");
        assert_eq!(result.curated_stories[1].article.url, "c");
        assert_eq!(result.curated_stories[2].article.url, "d");

        // Connection [0,2] = input ai(a)↔italy(c) → curated positions a=0, c=1.
        // Input story 2 mapped to curated position 1 — the remap did real work (2 != 1).
        // The second connection references input story 1 (miami), which wasn't curated → dropped.
        assert_eq!(result.connections.len(), 1, "connection to a dropped story must be dropped");
        let conn = &result.connections[0];
        let sec_a = &result.curated_stories[conn.story_idx_a].article.sector;
        let sec_b = &result.curated_stories[conn.story_idx_b].article.sector;
        // The whole point: endpoints land on DIFFERENT sectors (ai & italy), as the model meant.
        assert_ne!(sec_a, sec_b, "remapped connection must stay cross-sector");
        let mut sectors = [sec_a.as_str(), sec_b.as_str()];
        sectors.sort();
        assert_eq!(sectors, ["ai", "italy"]);
    }

    /// The honesty fix: the feature is literally named "cross-sector connections", yet the
    /// 70B model routinely emits SAME-sector pairs (e.g. ai↔ai). Those are not valid
    /// instances of the feature and must be dropped — empty-but-honest beats wrong-but-present.
    /// A genuinely cross-sector pair in the same body must survive.
    #[test]
    fn same_sector_connections_are_dropped_cross_sector_kept() {
        // Two AI stories + one Italy story, so a same-sector (ai↔ai) pair is possible.
        let input = vec![
            story("a", "ai", 9),
            story("b", "ai", 8),
            story("c", "italy", 7),
        ];
        let body = r#"{
            "connections": [
                {"story_ids": [0, 1], "connection": "ai↔ai(same-sector)", "insight": "drop"},
                {"story_ids": [0, 2], "connection": "ai↔italy(cross)", "insight": "keep"}
            ],
            "relevance_scores": [],
            "trends": [],
            "curation": {"ai": [0, 1], "italy": [2]}
        }"#;
        let parsed: AnalysisResponse = serde_json::from_str(body).unwrap();
        let result = build_analysis_result(parsed, &input);

        // All 3 stories curated (each sector under cap).
        assert_eq!(result.curated_stories.len(), 3);
        // Only the cross-sector connection survives; the ai↔ai pair is dropped.
        assert_eq!(
            result.connections.len(),
            1,
            "same-sector connection must be dropped, cross-sector kept"
        );
        let conn = &result.connections[0];
        let sec_a = &result.curated_stories[conn.story_idx_a].article.sector;
        let sec_b = &result.curated_stories[conn.story_idx_b].article.sector;
        assert_ne!(sec_a, sec_b, "surviving connection must be cross-sector");
        let mut sectors = [sec_a.as_str(), sec_b.as_str()];
        sectors.sort();
        assert_eq!(sectors, ["ai", "italy"]);
    }
}



#[cfg(test)]
mod fallback_tests {
    use super::*;

    /// 403 is the pre-auth IP block — the ONLY status that should abandon Groq for the
    /// run. Treating 429/5xx the same way would send a briefing to the paid provider over
    /// an ordinary rate limit that a retry would have cleared.
    #[test]
    fn only_403_counts_as_a_block() {
        assert!(is_groq_block_status(403));
        for code in [200, 400, 401, 408, 429, 500, 502, 503] {
            assert!(!is_groq_block_status(code), "{code} must not latch the block");
        }
    }

    /// The fallback exists for availability, not quality. Both Groq models must map to a
    /// CHEAP Anthropic model: a blocked day on Sonnet/Opus would blow the daily cost cap
    /// and abort the run halfway, turning a degraded briefing into no briefing.
    #[test]
    fn fallback_models_are_the_cheap_tier() {
        for m in [FAST_MODEL, STRONG_MODEL_DEFAULT, "some-future-groq-model"] {
            let target = anthropic_equivalent(m);
            assert!(target.starts_with("claude-haiku"), "{m} -> {target} is not the cheap tier");
        }
    }

    /// The latch must be idempotent and one-way within a run: once blocked, every
    /// subsequent call skips Groq rather than re-proving the block for ~140 stories.
    #[test]
    fn block_latch_is_idempotent_and_sticky() {
        // Serialised with the other latch test — these share process-global state.
        let _g = LATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        GROQ_BLOCKED.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!groq_is_blocked());

        mark_groq_blocked("first");
        assert!(groq_is_blocked());
        mark_groq_blocked("second");
        assert!(groq_is_blocked(), "latch must stay set");

        GROQ_BLOCKED.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Default state must be "not blocked" — a fresh run always tries the cheap provider
    /// first. If this ever defaulted true, every run would silently go to Anthropic.
    #[test]
    fn default_is_unblocked() {
        let _g = LATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        GROQ_BLOCKED.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!groq_is_blocked());
    }

    static LATCH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
