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

    // Batch in chunks of 10 to stay under Voyage free-tier limits (10K TPM, 3 RPM)
    const BATCH_SIZE: usize = 10;
    let mut all_embeddings: Vec<StoryEmbedding> = Vec::with_capacity(texts.len());

    let total_batches = texts.chunks(BATCH_SIZE).len().max(1);
    // Stories this run failed to embed. Surfaced loudly at the end — see the LOSS branch.
    let mut lost = 0usize;
    for (batch_idx, chunk) in texts.chunks(BATCH_SIZE).enumerate() {
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

#[cfg(test)]
mod tests {
    use super::*;

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
