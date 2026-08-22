use crate::claude::SummarizedStory;
use serde::{Deserialize, Serialize};

const VOYAGE_API_URL: &str = "https://api.voyageai.com/v1/embeddings";
const VOYAGE_MODEL: &str = "voyage-3-lite";

#[derive(Serialize)]
struct EmbeddingRequest {
    model: String,
    input: Vec<String>,
    input_type: String,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

pub struct StoryEmbedding {
    pub story_index: usize,
    pub embedding: Vec<f32>,
}

pub async fn generate(
    stories: &[SummarizedStory],
    prefixes: Option<&[Option<String>]>,
    // Heartbeat: called once per batch with (detail, sub_pct 0-100) so the caller can
    // keep the progress file fresh across the 21s inter-batch rate-limit sleeps. Without
    // this, embeddings runs 3-5 min with zero progress writes and the UI would misread a
    // healthy run as interrupted. Optional so backfill/tests can pass a no-op.
    mut heartbeat: impl FnMut(&str, f64),
) -> anyhow::Result<Vec<StoryEmbedding>> {
    let api_key = std::env::var("VOYAGE_API_KEY")
        .map_err(|_| anyhow::anyhow!("VOYAGE_API_KEY not set"))?;

    let texts: Vec<(usize, String)> = stories
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let prefix = prefixes
                .and_then(|p| p.get(i))
                .and_then(|p| p.as_deref())
                .unwrap_or("");
            let text = if prefix.is_empty() {
                format!(
                    "{}. {}. {}",
                    s.headline,
                    s.summary,
                    s.key_facts.join(", ")
                )
            } else {
                format!(
                    "{}. {}. {}. {}",
                    prefix,
                    s.headline,
                    s.summary,
                    s.key_facts.join(", ")
                )
            };
            (i, text)
        })
        .collect();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Batch by TOKENS, not by a fixed count. The free tier caps 3 RPM *and* 10K TPM, and
    // the old fixed chunks(10) spent only ~2.3K TPM — it paid the 21s rate-limit pause for
    // a quarter-full request. See VOYAGE_REQUEST_TOKEN_BUDGET.
    let mut all_embeddings: Vec<StoryEmbedding> = Vec::with_capacity(texts.len());

    let batch_texts_only: Vec<String> = texts.iter().map(|(_, t)| t.clone()).collect();
    let batches = batch_by_tokens(&batch_texts_only, VOYAGE_REQUEST_TOKEN_BUDGET);
    let total_batches = batches.len().max(1);
    // Stories this run failed to embed. Surfaced loudly at the end — see the LOSS branch.
    let mut lost = 0usize;
    for (batch_idx, &(bstart, bend)) in batches.iter().enumerate() {
        let chunk = &texts[bstart..bend];
        // Heartbeat BEFORE the 21s sleep so the progress file stays fresh across it.
        heartbeat(
            &format!("Embedding batch {}/{}", batch_idx + 1, total_batches),
            (batch_idx as f64 / total_batches as f64) * 100.0,
        );
        if batch_idx > 0 {
            // Rate limit pacing. Default 21s matches the free tier's 3 RPM —
            // the pauses are 3-5 min of pure sleep per run. After upgrading the
            // Voyage account, set PULSE_VOYAGE_RPM in the launchd plist/.env
            // (e.g. 60 → 2s pauses); no rebuild needed.
            let pause = rate_limit_pause_secs();
            tracing::info!("Rate limit pause {}s before batch {}...", pause, batch_idx + 1);
            tokio::time::sleep(std::time::Duration::from_secs(pause)).await;
        }

        let batch_texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let batch_indices: Vec<usize> = chunk.iter().map(|(i, _)| *i).collect();

        let request = EmbeddingRequest {
            model: VOYAGE_MODEL.to_string(),
            input: batch_texts,
            input_type: "document".to_string(),
        };

        // Retry with exponential backoff. Was "retry once after 5s", which is inside the
        // same network blip that caused the first failure — both attempts died together.
        let mut last_err = None;
        let mut response: Option<EmbeddingResponse> = None;
        for attempt in 1..=MAX_BATCH_ATTEMPTS {
            let wait = backoff_secs(attempt);
            if wait > 0 {
                tracing::info!(
                    "Retrying Voyage batch {} (attempt {}/{}) after {}s",
                    batch_idx + 1, attempt, MAX_BATCH_ATTEMPTS, wait
                );
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
            match client
                .post(VOYAGE_API_URL)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        let body = resp.text().await.unwrap_or_default();
                        last_err = Some(format!("Voyage API error {}: {}", status, body));
                        continue;
                    }
                    match resp.json().await {
                        Ok(parsed) => { response = Some(parsed); break; }
                        Err(e) => { last_err = Some(format!("Failed to parse Voyage response: {}", e)); continue; }
                    }
                }
                Err(e) => {
                    last_err = Some(format!("Voyage API request failed: {}", e));
                    continue;
                }
            }
        }

        match response {
            Some(resp) => {
                let mut kept = 0usize;
                let mut dropped = 0usize;
                for (j, d) in resp.data.into_iter().enumerate() {
                    if d.embedding.len() != 512 {
                        tracing::warn!(
                            "Batch {}: dropping story {} — got {} dims, expected 512",
                            batch_idx + 1, batch_indices[j], d.embedding.len()
                        );
                        dropped += 1;
                        continue;
                    }
                    all_embeddings.push(StoryEmbedding {
                        story_index: batch_indices[j],
                        embedding: d.embedding,
                    });
                    kept += 1;
                }
                tracing::info!("Batch {}: embedded {} stories ({} dropped for bad dims)", batch_idx + 1, kept, dropped);
            }
            None => {
                // Continue with the remaining batches, but do NOT pretend this was fine.
                // This branch is what decayed embedding coverage from 100% (Mar 2026) to
                // 49.7% (Aug 2026): every dropped batch permanently removed ~10 stories
                // from vector search, and the only signal was a WARN nobody read. The
                // stories are recovered by `--mode backfill-embeddings`, which is why
                // that mode is now scheduled rather than manual-only.
                lost += batch_indices.len();
                tracing::error!(
                    "EMBEDDING LOSS: batch {}/{} failed after retries, {} stories left \
                     unembedded (recovered by the next backfill run): {}",
                    batch_idx + 1,
                    total_batches,
                    batch_indices.len(),
                    last_err.unwrap_or_default()
                );
            }
        }
    }

    if lost > 0 {
        tracing::error!(
            "EMBEDDING LOSS TOTAL: {} of {} stories went unembedded this run ({:.1}% of the batch)",
            lost,
            texts.len(),
            100.0 * lost as f64 / texts.len().max(1) as f64
        );
    }
    tracing::info!(
        "Generated {} embeddings total ({} lost)",
        all_embeddings.len(),
        lost
    );
    Ok(all_embeddings)
}

/// How many times a single Voyage batch is attempted before it is given up on.
/// The failures observed in production are transient network errors ("error sending
/// request for url"), so the retries are spaced by an exponential backoff rather than
/// a fixed pause — two attempts 5s apart both land inside the same blip.
pub const MAX_BATCH_ATTEMPTS: u32 = 4;

/// Backoff before attempt `n` (1-indexed, so attempt 1 never waits).
pub fn backoff_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 0,
        n => 5u64 << (n - 2).min(4), // 5, 10, 20, 40, 80 (capped)
    }
}

/// Generate embeddings from raw text strings (for backfill).
///
/// Retries with exponential backoff — unlike the in-pipeline `generate`, a failure here
/// is worth fighting for because the backfill is the LAST line of defence: a story that
/// this gives up on stays unsearchable until the next backfill run picks it up again.
pub async fn generate_from_texts(texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
    let api_key = std::env::var("VOYAGE_API_KEY")
        .map_err(|_| anyhow::anyhow!("VOYAGE_API_KEY not set"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let request = EmbeddingRequest {
        model: VOYAGE_MODEL.to_string(),
        input: texts.to_vec(),
        input_type: "document".to_string(),
    };

    let mut last_err = String::from("no attempt made");
    for attempt in 1..=MAX_BATCH_ATTEMPTS {
        let wait = backoff_secs(attempt);
        if wait > 0 {
            tracing::info!("Voyage retry {}/{} after {}s", attempt, MAX_BATCH_ATTEMPTS, wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        }

        let resp = match client
            .post(VOYAGE_API_URL)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("Voyage API request failed: {}", e);
                continue;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            last_err = format!("Voyage API error {}: {}", status, body);
            // 4xx other than 429 will not fix itself — stop burning the budget on it.
            if status.is_client_error() && status.as_u16() != 429 {
                anyhow::bail!("{} (not retryable)", last_err);
            }
            continue;
        }

        match resp.json::<EmbeddingResponse>().await {
            Ok(parsed) => return Ok(parsed.data.into_iter().map(|d| d.embedding).collect()),
            Err(e) => {
                last_err = format!("Failed to parse Voyage response: {}", e);
                continue;
            }
        }
    }

    anyhow::bail!("{} (after {} attempts)", last_err, MAX_BATCH_ATTEMPTS)
}

/// Seconds to pause between Voyage batches for the configured rate limit.
/// Shared by the pipeline and the backfill so they cannot drift apart — the backfill
/// used to hardcode 21s and silently ignore `PULSE_VOYAGE_RPM`.
pub fn rate_limit_pause_secs() -> u64 {
    let rpm: u64 = std::env::var("PULSE_VOYAGE_RPM")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&r| r > 0)
        .unwrap_or(3);
    (60 / rpm) + 1
}

/// Token budget for a single Voyage request.
///
/// Measured against the live API 2026-08-15. The free tier's 429 body names BOTH limits:
/// "reduced rate limits of 3 RPM and 10K TPM". TPM is the binding one, and batching by a
/// fixed COUNT ignores it: at the measured 76-token average for a Pulse story, batches of
/// 10 spend ~760 tokens per request — about 2.3K TPM against a 10K budget, under a quarter
/// of what the tier allows. A 128-text request was accepted (HTTP 200, 128 vectors); a
/// 1000-text request was rejected 429 on TOKENS, not on count.
///
/// 3,000 keeps three requests per minute at ~9K TPM, inside the budget with headroom for
/// the estimate being approximate. Raising `PULSE_VOYAGE_RPM` only happens on a paid plan,
/// whose TPM ceiling is far higher, so this stays safe there too.
pub const VOYAGE_REQUEST_TOKEN_BUDGET: usize = 3_000;

/// Structural cap on texts per request: 128 is the largest size verified accepted.
pub const VOYAGE_MAX_BATCH_TEXTS: usize = 128;

/// Rough token count. Voyage does not expose a tokenizer, and ~4 chars/token is the
/// standard approximation; the budget above carries enough headroom to absorb its error.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4).max(1)
}

/// Group `texts` into consecutive `(start, end)` batches that each fit `token_budget`,
/// never exceeding [`VOYAGE_MAX_BATCH_TEXTS`] items.
///
/// A single text larger than the budget gets its own batch rather than being dropped or
/// looping forever — the longest unembedded story measured 6,420 chars (~1,605 tokens),
/// and a story that cannot be embedded at all is exactly the silent permanent loss this
/// module exists to prevent.
pub fn batch_by_tokens(texts: &[String], token_budget: usize) -> Vec<(usize, usize)> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut tokens = 0;
    for (i, t) in texts.iter().enumerate() {
        let cost = estimate_tokens(t);
        let full = i > start && (tokens + cost > token_budget || i - start >= VOYAGE_MAX_BATCH_TEXTS);
        if full {
            batches.push((start, i));
            start = i;
            tokens = 0;
        }
        tokens += cost;
    }
    if start < texts.len() {
        batches.push((start, texts.len()));
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mutation this must survive is a text LARGER than the whole budget. A naive
    /// "fill until full" loop either drops it or spins forever; it must get its own batch.
    #[test]
    fn oversized_text_gets_its_own_batch_and_nothing_is_lost() {
        let texts = vec![
            "a".repeat(40),      // ~10 tok
            "b".repeat(40_000),  // ~10_000 tok — four times the budget on its own
            "c".repeat(40),
        ];
        let batches = batch_by_tokens(&texts, 2_500);
        assert_eq!(batches, vec![(0, 1), (1, 2), (2, 3)]);
        // Every input must appear in exactly one batch — no gaps, no overlap, no drops.
        let covered: Vec<usize> = batches.iter().flat_map(|(a, b)| *a..*b).collect();
        assert_eq!(covered, (0..texts.len()).collect::<Vec<_>>());
    }

    /// Coverage is the load-bearing property: this replaced a fixed `chunks(10)`, and a
    /// batcher that silently skips inputs reintroduces the exact permanent-loss bug.
    #[test]
    fn batches_cover_every_input_exactly_once() {
        for n in [0, 1, 9, 10, 11, 137, 500] {
            let texts: Vec<String> = (0..n).map(|i| format!("story number {i} ").repeat(3)).collect();
            let batches = batch_by_tokens(&texts, VOYAGE_REQUEST_TOKEN_BUDGET);
            let covered: Vec<usize> = batches.iter().flat_map(|(a, b)| *a..*b).collect();
            assert_eq!(covered, (0..n).collect::<Vec<_>>(), "n={n} lost or duplicated inputs");
            for (a, b) in &batches {
                assert!(b > a, "n={n} produced an empty batch");
                assert!(b - a <= VOYAGE_MAX_BATCH_TEXTS, "n={n} exceeded the text cap");
            }
        }
    }

    /// No batch may exceed the budget unless it is a single oversized text.
    #[test]
    fn no_batch_exceeds_the_token_budget() {
        let texts: Vec<String> = (0..400).map(|i| format!("headline {i}. ").repeat(20)).collect();
        for (a, b) in batch_by_tokens(&texts, VOYAGE_REQUEST_TOKEN_BUDGET) {
            let cost: usize = texts[a..b].iter().map(|t| estimate_tokens(t)).sum();
            assert!(
                cost <= VOYAGE_REQUEST_TOKEN_BUDGET || b - a == 1,
                "batch {a}..{b} cost {cost} over budget with {} texts", b - a
            );
        }
    }

    /// The whole point of the change: real Pulse stories must batch far above the old
    /// fixed 10. Measured average is 306 chars (~76 tokens) for the 15,732 unembedded rows.
    #[test]
    fn realistic_stories_batch_far_above_the_old_fixed_ten() {
        let texts: Vec<String> = (0..1_000).map(|_| "x".repeat(306)).collect();
        let batches = batch_by_tokens(&texts, VOYAGE_REQUEST_TOKEN_BUDGET);
        let avg = texts.len() as f64 / batches.len() as f64;
        assert!(avg > 30.0, "expected >30 stories/request at the measured size, got {avg:.1}");
    }

    /// The bug this replaced: two attempts 5s apart both land inside the same network
    /// blip. Backoff must actually grow, and attempt 1 must not wait at all.
    #[test]
    fn backoff_grows_and_first_attempt_is_immediate() {
        assert_eq!(backoff_secs(1), 0, "first attempt must not sleep");
        let waits: Vec<u64> = (1..=MAX_BATCH_ATTEMPTS).map(backoff_secs).collect();
        assert_eq!(waits, vec![0, 5, 10, 20]);
        // Total window the retries cover, vs the old fixed 5s single retry.
        assert!(waits.iter().sum::<u64>() >= 30, "must span a real outage window");
    }

    /// The backfill hardcoded 21s and ignored PULSE_VOYAGE_RPM, so raising the Voyage
    /// tier sped up the pipeline but left the backfill crawling. One shared helper now.
    #[test]
    fn rate_limit_pause_matches_free_tier_by_default() {
        // Default (env unset in test env) is the 3 RPM free tier -> 21s.
        if std::env::var("PULSE_VOYAGE_RPM").is_err() {
            assert_eq!(rate_limit_pause_secs(), 21);
        }
    }
}
