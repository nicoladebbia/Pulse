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

    for (batch_idx, chunk) in texts.chunks(BATCH_SIZE).enumerate() {
        if batch_idx > 0 {
            // Rate limit: free tier = 3 RPM, so wait 21s between batches
            tracing::info!("Rate limit pause before batch {}...", batch_idx + 1);
            tokio::time::sleep(std::time::Duration::from_secs(21)).await;
        }

        let batch_texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let batch_indices: Vec<usize> = chunk.iter().map(|(i, _)| *i).collect();

        let request = EmbeddingRequest {
            model: VOYAGE_MODEL.to_string(),
            input: batch_texts,
            input_type: "document".to_string(),
        };

        // Retry once on transient failure
        let mut last_err = None;
        let mut response: Option<EmbeddingResponse> = None;
        for attempt in 0..2 {
            if attempt > 0 {
                tracing::info!("Retrying Voyage API batch {} (attempt {})", batch_idx + 1, attempt + 1);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
                tracing::warn!("Batch {} failed: {}", batch_idx + 1, last_err.unwrap_or_default());
                // Continue with remaining batches — partial success is fine
            }
        }
    }

    tracing::info!("Generated {} embeddings total", all_embeddings.len());
    Ok(all_embeddings)
}

/// Generate embeddings from raw text strings (for backfill)
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

    let resp = client
        .post(VOYAGE_API_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await?;
        anyhow::bail!("Voyage API error {}: {}", status, body);
    }

    let response: EmbeddingResponse = resp.json().await?;
    Ok(response.data.into_iter().map(|d| d.embedding).collect())
}
