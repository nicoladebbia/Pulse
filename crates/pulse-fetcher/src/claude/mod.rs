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

pub async fn translate_italian(articles: &[RawArticle]) -> anyhow::Result<Vec<RawArticle>> {
    let italian: Vec<_> = articles.iter().filter(|a| a.language == "it").collect();
    if italian.is_empty() {
        return Ok(articles.to_vec());
    }

    tracing::info!("Translating {} Italian articles...", italian.len());
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;

    let client = client::GroqClient::new(&api_key)?;
    let mut result = articles.to_vec();

    let mut translated_count = 0;
    for article in &mut result {
        if article.language == "it" {
            match client.translate(&article.title, &article.content_snippet).await {
                Ok((title, snippet)) => {
                    article.title = title;
                    article.content_snippet = snippet;
                    article.language = "en".to_string();
                    translated_count += 1;
                }
                Err(e) => tracing::warn!("Translation failed: {}", e),
            }
            // Throttle to stay under Gemini free tier (15 RPM)
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
    tracing::info!("Translated {}/{} Italian articles", translated_count, italian.len());

    Ok(result)
}

pub async fn summarize_stories(articles: &[RawArticle], progress: Option<&crate::pipeline::ProgressWriter>) -> anyhow::Result<Vec<SummarizedStory>> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = client::GroqClient::new(&api_key)?;

    let mut summaries = Vec::new();
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
                        Ok(summary) => Some(summary),
                        Err(e) => {
                            tracing::warn!("Summary failed for '{}': {}", article.title, e);
                            None
                        }
                    }
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        summaries.extend(results.into_iter().flatten());

        // Brief pause between batches to avoid burst limits
        if batch_idx < total_chunks - 1 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    Ok(summaries)
}

pub async fn analyze_cross_sector(stories: &[SummarizedStory]) -> anyhow::Result<AnalysisResult> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = client::GroqClient::new(&api_key)?;

    // Sector-balanced selection: ensure each sector has at least 25 stories in the 120-story input
    let mut sorted = stories.to_vec();
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));

    let sectors = ["ai", "miami", "italy", "tech"];
    let mut balanced = Vec::with_capacity(120);
    let min_per_sector = 25;

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
        if balanced.len() >= 120 { break; }
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
