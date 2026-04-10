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

    let texts: Vec<String> = stories
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let prefix = prefixes
                .and_then(|p| p.get(i))
                .and_then(|p| p.as_deref())
                .unwrap_or("");
            if prefix.is_empty() {
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
            }
        })
        .collect();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let request = EmbeddingRequest {
        model: VOYAGE_MODEL.to_string(),
        input: texts,
        input_type: "document".to_string(),
    };

    // Retry once on transient failure
    let mut last_err = None;
    let mut response: Option<EmbeddingResponse> = None;
    for attempt in 0..2 {
        if attempt > 0 {
            tracing::info!("Retrying Voyage API (attempt {})", attempt + 1);
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
                response = Some(resp.json().await?);
                break;
            }
            Err(e) => {
                last_err = Some(format!("Voyage API request failed: {}", e));
                continue;
            }
        }
    }
    let response = response.ok_or_else(|| anyhow::anyhow!("{}", last_err.unwrap_or_default()))?;

    let embeddings: Vec<StoryEmbedding> = response
        .data
        .into_iter()
        .enumerate()
        .map(|(i, d)| StoryEmbedding {
            story_index: i,
            embedding: d.embedding,
        })
        .collect();

    tracing::info!("Generated {} embeddings", embeddings.len());
    Ok(embeddings)
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
