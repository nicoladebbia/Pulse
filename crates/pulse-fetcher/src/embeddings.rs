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

pub async fn generate(stories: &[SummarizedStory]) -> anyhow::Result<Vec<StoryEmbedding>> {
    let api_key = std::env::var("VOYAGE_API_KEY")
        .map_err(|_| anyhow::anyhow!("VOYAGE_API_KEY not set"))?;

    let texts: Vec<String> = stories
        .iter()
        .map(|s| {
            format!(
                "{}. {}. {}",
                s.headline,
                s.summary,
                s.key_facts.join(", ")
            )
        })
        .collect();

    let client = reqwest::Client::new();
    let request = EmbeddingRequest {
        model: VOYAGE_MODEL.to_string(),
        input: texts,
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
