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
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let client = client::ClaudeClient::new(&api_key);
    let mut result = articles.to_vec();

    for article in &mut result {
        if article.language == "it" {
            match client.translate(&article.title, &article.content_snippet).await {
                Ok((title, snippet)) => {
                    article.title = title;
                    article.content_snippet = snippet;
                    article.language = "en".to_string();
                }
                Err(e) => tracing::warn!("Translation failed: {}", e),
            }
        }
    }

    Ok(result)
}

pub async fn summarize_stories(articles: &[RawArticle]) -> anyhow::Result<Vec<SummarizedStory>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
    let client = client::ClaudeClient::new(&api_key);

    let mut summaries = Vec::new();
    let chunks: Vec<_> = articles.chunks(10).collect();

    for chunk in chunks {
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
    }

    Ok(summaries)
}

pub async fn analyze_cross_sector(stories: &[SummarizedStory]) -> anyhow::Result<AnalysisResult> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
    let client = client::ClaudeClient::new(&api_key);

    // Sort by importance and take top 30
    let mut sorted = stories.to_vec();
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));
    sorted.truncate(30);

    client.analyze(&sorted).await
}
