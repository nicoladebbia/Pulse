pub mod client;
pub mod prompts;

use crate::sources::RawArticle;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizedStory {
    pub article: RawArticle,
    pub headline: String,
    pub summary: String,
    pub key_facts: Vec<String>,
    pub why_it_matters: String,
    pub what_to_watch: String,
    pub importance_score: i32,
    pub sentiment: Option<f64>,
    pub novelty: Option<f64>,
    pub event_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub curated_stories: Vec<SummarizedStory>,
    pub connections: Vec<Connection>,
    pub relevance_scores: Vec<RelevanceScore>,
    pub trends: Vec<TrendDetection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub story_idx_a: usize,
    pub story_idx_b: usize,
    pub connection: String,
    pub insight: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelevanceScore {
    pub story_idx: usize,
    pub relevance: i32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendDetection {
    pub trend: String,
    pub story_indices: Vec<usize>,
    pub trajectory: String,
}


/// What `summarize_stories` actually produced, plus what went wrong if anything did.
///
/// The `failure` line exists because the abort message used to read "likely a
/// blocked API (VPN/network)" — a GUESS, printed as a conclusion, at the top of
/// `fetch-progress.json` where it is the first thing anyone reads during an
/// outage. On 2026-08-17 it was wrong: Groq had deleted the entire Llama family
/// and every call was returning 404, nothing was blocked, and the message sent
/// the diagnosis at the network for days. The per-story errors were right there
/// and were being logged and dropped.
pub struct SummarizeOutcome {
    pub stories: Vec<SummarizedStory>,
    /// One line naming the observed dominant error. `None` iff nothing failed.
    pub failure: Option<String>,
}

/// Classify one upstream error into a short cause, or `None` if unrecognised.
///
/// Matched against the formats this crate actually emits, verified by reading
/// them rather than assumed: `client.rs` bails with `Groq API error {status}: {body}`
/// (status Displays as e.g. `404 Not Found`), `Groq API error: {message}`,
/// `Groq API: max retries exceeded (last error: ...)`, plus bare reqwest and
/// serde messages.
///
/// Order is load-bearing where an error body can match two arms: a
/// `json_validate_failed` body that also says `max completion tokens` is a
/// truncation, not a re-rollable validator failure — the same precedence
/// `client.rs` applies when deciding whether to retry.
fn failure_cause(err: &str) -> Option<&'static str> {
    let e = err.to_ascii_lowercase();
    let has = |needle: &str| e.contains(needle);

    if has("does not exist") || has("model_not_found") || has("decommissioned") {
        return Some("the model id no longer exists at the provider (404) — check GET /models, and check the WHOLE family, not just this id");
    }
    if has("max completion tokens") || has("max_tokens") {
        return Some("the response was truncated at max_tokens — for a reasoning model, thinking is spending the answer's budget");
    }
    if has("json_validate_failed") {
        return Some("the model returned invalid JSON and the re-rolls did not clear it");
    }
    if has("401") || has("invalid_api_key") || has("invalid api key") {
        return Some("the API key was rejected (401)");
    }
    if has("403") {
        return Some("HTTP 403 — the source IP is blocked (this is the VPN/network case)");
    }
    if has("429") || has("rate_limit") || has("rate limit") {
        return Some("rate limited (429)");
    }
    if has("500") || has("502") || has("503") || has("internal server error") || has("bad gateway") {
        return Some("the provider returned a 5xx — upstream fault, not ours");
    }
    if has("error sending request") || has("timed out") || has("timeout") || has("dns") || has("connect") {
        return Some("the request never got a response (transport error) — this IS a network-shaped failure");
    }
    if has("missing field") || has("invalid type") || has("expected") {
        return Some("the model answered but the JSON did not match the expected schema");
    }
    None
}

/// Turn the per-story failures into the one line the abort message should carry.
///
/// Reports the MOST COMMON observed cause with its count, and quotes one real
/// error verbatim so a cause this function has never seen is still legible.
/// Returns `None` only when nothing failed.
pub(crate) fn dominant_failure(errors: &[String], attempted: usize) -> Option<String> {
    if errors.is_empty() {
        return None;
    }

    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut unclassified = 0usize;
    for err in errors {
        match failure_cause(err) {
            Some(cause) => match counts.iter_mut().find(|(c, _)| *c == cause) {
                Some(slot) => slot.1 += 1,
                None => counts.push((cause, 1)),
            },
            None => unclassified += 1,
        }
    }

    // Quote a real error, preferring one that matched — an unclassified sample
    // is the least useful line to show when a known cause dominates.
    let sample = errors
        .iter()
        .find(|e| failure_cause(e).is_some())
        .unwrap_or(&errors[0]);
    // chars(), never truncate() — a byte-index cut mid-UTF-8 panics, which is
    // how article_text.rs killed three runs in August 2026.
    //
    // Control chars are flattened to spaces because this string ends up inside an
    // `osascript display notification "..."` argument. notify_failure escapes
    // quotes and backslashes but not newlines, and a raw newline is a syntax error
    // in an AppleScript literal — i.e. a body containing one would silently kill
    // the very notification that reports the outage.
    let sample: String = sample
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();

    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let headline = match counts.first() {
        Some((cause, n)) => format!("{n}\u{00d7} {cause}"),
        None => format!("{unclassified}\u{00d7} an error this build does not recognise"),
    };

    Some(format!(
        "{}/{} failed. Dominant cause: {}. One real error: \"{}\"",
        errors.len(),
        attempted,
        headline,
        sample
    ))
}

pub async fn summarize_stories(articles: &[RawArticle], progress: Option<&crate::pipeline::ProgressWriter>, db_path: &std::path::Path) -> anyhow::Result<SummarizeOutcome> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;

    let mut summaries = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let chunks: Vec<_> = articles.chunks(10).collect();
    let total_chunks = chunks.len();

    for (batch_idx, chunk) in chunks.into_iter().enumerate() {
        tracing::info!("Summarizing batch {}/{} ({} stories done)", batch_idx + 1, total_chunks, summaries.len());
        if let Some(pw) = progress {
            let sub_pct = (batch_idx as f64 / total_chunks as f64) * 100.0;
            pw.update_detail(&format!("Batch {}/{} ({} done)", batch_idx + 1, total_chunks, summaries.len()), sub_pct);
        }

        let futures: Vec<_> = chunk
            .iter()
            .map(|article| {
                let client = &client;
                let article = article.clone();
                async move {
                    match client.summarize_story(&article).await {
                        Ok(summary) => Ok(summary),
                        Err(e) => {
                            tracing::warn!("Summary failed for '{}': {}", article.title, e);
                            Err(format!("{e:#}"))
                        }
                    }
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        for r in results {
            match r {
                Ok(story) => summaries.push(story),
                Err(e) => errors.push(e),
            }
        }

        // Brief pause between batches to avoid burst limits
        if batch_idx < total_chunks - 1 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    let failure = dominant_failure(&errors, articles.len());
    Ok(SummarizeOutcome {
        stories: summaries,
        failure,
    })
}

pub async fn analyze_cross_sector(stories: &[SummarizedStory], db_path: &std::path::Path) -> anyhow::Result<AnalysisResult> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;

    // Sector-balanced selection: ensure each sector has at least 35 stories in the 160-story input
    let mut sorted = stories.to_vec();
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));

    let sectors = ["ai", "miami", "italy", "tech"];
    let mut balanced = Vec::with_capacity(160);
    let min_per_sector = 35;

    // First: take top stories per sector
    for sector in &sectors {
        let sector_stories: Vec<_> = sorted.iter()
            .filter(|s| s.article.sector == *sector)
            .take(min_per_sector)
            .cloned()
            .collect();
        balanced.extend(sector_stories);
    }

    // Fill remaining slots with top stories from any sector (by importance)
    let already: std::collections::HashSet<String> = balanced.iter().map(|s| s.article.url.clone()).collect();
    for s in &sorted {
        if balanced.len() >= 160 { break; }
        if !already.contains(&s.article.url) {
            balanced.push(s.clone());
        }
    }

    // Log sector distribution
    for sector in &sectors {
        let count = balanced.iter().filter(|s| s.article.sector == *sector).count();
        tracing::info!("Analysis input: {} = {} stories", sector, count);
    }

    client.analyze(&balanced).await
}

/// Degraded fallback for when `analyze_cross_sector` fails (e.g. Groq block flips
/// on mid-run, or a stochastic serde-on-200 error). Returns an `AnalysisResult`
/// carrying the SAME sector-balanced story set that analyze would have curated,
/// but WITHOUT the cross-sector enrichment (connections/relevance/trends empty).
/// This lets the pipeline persist the news it already summarized instead of
/// throwing all of it away and firing a false "FAILED" alert — the daily briefing
/// still lands, just without the cross-sector-connections feature for that day.
pub fn degraded_analysis(stories: &[SummarizedStory]) -> AnalysisResult {
    let mut sorted = stories.to_vec();
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));

    let sectors = ["ai", "miami", "italy", "tech"];
    let mut balanced = Vec::with_capacity(160);
    let min_per_sector = 35;
    for sector in &sectors {
        balanced.extend(
            sorted.iter()
                .filter(|s| s.article.sector == *sector)
                .take(min_per_sector)
                .cloned(),
        );
    }
    let already: std::collections::HashSet<String> =
        balanced.iter().map(|s| s.article.url.clone()).collect();
    for s in &sorted {
        if balanced.len() >= 160 { break; }
        if !already.contains(&s.article.url) {
            balanced.push(s.clone());
        }
    }

    AnalysisResult {
        curated_stories: balanced,
        connections: Vec::new(),
        relevance_scores: Vec::new(),
        trends: Vec::new(),
    }
}

#[cfg(test)]
mod failure_diagnosis_tests {
    use super::{dominant_failure, failure_cause};

    /// The error Groq actually returned on every call from 2026-08-17 to 08-22,
    /// verbatim in the shape `client.rs` bails with. This is the specimen the
    /// whole change exists for: the old message called it a network block.
    const THE_AUGUST_OUTAGE: &str = r#"Groq API error 404 Not Found: {"error":{"message":"The model `llama-3.3-70b-versatile` does not exist or you do not have access to it.","type":"invalid_request_error","code":"model_not_found"}}"#;

    #[test]
    fn the_august_outage_is_not_reported_as_a_network_problem() {
        let msg = dominant_failure(&[THE_AUGUST_OUTAGE.to_string()], 150).unwrap();
        assert!(
            msg.contains("no longer exists at the provider"),
            "got: {msg}"
        );
        assert!(
            !msg.to_ascii_lowercase().contains("vpn"),
            "the guess that cost 5 days must not reappear: {msg}"
        );
    }

    /// The message must carry the real upstream text, so a cause this build has
    /// never seen is still diagnosable from fetch-progress.json alone.
    #[test]
    fn the_real_error_is_quoted_verbatim() {
        let msg = dominant_failure(&[THE_AUGUST_OUTAGE.to_string()], 150).unwrap();
        assert!(msg.contains("Groq API error 404 Not Found"), "got: {msg}");
    }

    /// The genuine network case still reads as one — the fix removes a false
    /// positive, it must not remove the true one.
    #[test]
    fn a_real_ip_block_is_still_called_a_block() {
        let cause = failure_cause("Groq API error 403 Forbidden: error code: 1010").unwrap();
        assert!(cause.contains("403") && cause.contains("blocked"));
    }

    #[test]
    fn a_transport_error_is_named_as_one() {
        let cause = failure_cause(
            "error sending request for url (https://api.groq.com/openai/v1/chat/completions)",
        )
        .unwrap();
        assert!(cause.contains("transport"), "got: {cause}");
    }

    /// Order matters where a body matches two arms: Groq's 400 for a truncated
    /// reasoning-model response says BOTH `json_validate_failed` and `max
    /// completion tokens`, and it is a truncation — the same precedence
    /// `client.rs` uses to decide the re-roll is pointless.
    #[test]
    fn a_truncated_reasoning_response_beats_the_validator_arm() {
        let cause = failure_cause(
            r#"Groq API error 400 Bad Request: {"error":{"code":"json_validate_failed","message":"max completion tokens reached"}}"#,
        )
        .unwrap();
        assert!(cause.contains("truncated"), "got: {cause}");
    }

    /// The count decides, not the order of arrival.
    #[test]
    fn the_most_common_cause_wins() {
        let mut errors = vec!["Groq API error 403 Forbidden: blocked".to_string()];
        for _ in 0..9 {
            errors.push(THE_AUGUST_OUTAGE.to_string());
        }
        let msg = dominant_failure(&errors, 10).unwrap();
        assert!(msg.contains("9\u{d7} the model id no longer exists"), "got: {msg}");
        assert!(msg.starts_with("10/10 failed"), "got: {msg}");
    }

    /// An unrecognised error must still produce a usable line rather than
    /// falling back to a guess.
    #[test]
    fn an_unrecognised_error_is_still_reported_with_its_text() {
        let msg = dominant_failure(&["something nobody predicted".to_string()], 3).unwrap();
        assert!(msg.contains("does not recognise"), "got: {msg}");
        assert!(msg.contains("something nobody predicted"), "got: {msg}");
    }

    /// When a known cause and an unknown one both appear, quote the known one —
    /// the unknown sample is the least useful line to show.
    #[test]
    fn a_recognised_error_is_preferred_as_the_quoted_sample() {
        let msg = dominant_failure(
            &["mystery".to_string(), THE_AUGUST_OUTAGE.to_string()],
            2,
        )
        .unwrap();
        assert!(msg.contains("Groq API error 404"), "got: {msg}");
    }

    /// The message is embedded in an `osascript display notification "..."`
    /// literal, where a raw newline is a syntax error — it would kill the
    /// notification that reports the outage.
    #[test]
    fn the_message_is_a_single_line() {
        let msg = dominant_failure(
            &["Groq API error 403 Forbidden:\n{\n  \"error\": \"blocked\"\n}".to_string()],
            1,
        )
        .unwrap();
        assert!(!msg.contains('\n'), "got: {msg:?}");
        assert!(!msg.chars().any(|c| c.is_control()), "got: {msg:?}");
    }

    #[test]
    fn no_failures_means_no_line() {
        assert!(dominant_failure(&[], 10).is_none());
    }

    /// The truncation must be char-based. A byte-index cut lands mid-UTF-8 and
    /// panics — that is exactly how `article_text.rs` killed three runs in
    /// August 2026, inside this same pipeline.
    #[test]
    fn a_multibyte_error_does_not_panic_on_truncation() {
        let long = format!("Groq API error 403 Forbidden: {}", "è".repeat(500));
        let msg = dominant_failure(&[long], 1).unwrap();
        assert!(msg.contains("403"));
    }
}
