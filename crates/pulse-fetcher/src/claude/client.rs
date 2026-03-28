use super::{SummarizedStory, AnalysisResult, Connection, RelevanceScore, TrendDetection};
use crate::sources::RawArticle;
use serde::{Deserialize, Serialize};

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const SONNET_MODEL: &str = "claude-sonnet-4-6";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

pub struct ClaudeClient {
    api_key: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct MessageRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

#[derive(Deserialize)]
struct SummaryResponse {
    headline: String,
    summary: String,
    key_facts: Vec<String>,
    why_it_matters: String,
    what_to_watch: String,
    importance_score: i32,
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
    ai: Vec<usize>,
    miami: Vec<usize>,
    italy: Vec<usize>,
    tech: Vec<usize>,
}

impl ClaudeClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn call(&self, model: &str, system: &str, user_msg: &str, max_tokens: u32) -> anyhow::Result<String> {
        let request = MessageRequest {
            model: model.to_string(),
            max_tokens,
            system: system.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: user_msg.to_string(),
            }],
        };

        let resp = self.http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await?;
            anyhow::bail!("Claude API error {}: {}", status, body);
        }

        let response: MessageResponse = resp.json().await?;
        let text = response
            .content
            .first()
            .and_then(|c| c.text.clone())
            .unwrap_or_default();

        Ok(text)
    }

    pub async fn translate(&self, title: &str, snippet: &str) -> anyhow::Result<(String, String)> {
        let system = "You are a translator. Translate Italian news to English. Return valid JSON only.";
        let user_msg = format!(
            "Translate to English. Return JSON: {{\"title_en\": \"...\", \"snippet_en\": \"...\"}}\n\nTitle: {}\nContent: {}",
            title, snippet
        );

        let text = self.call(HAIKU_MODEL, system, &user_msg, 500).await?;
        let parsed: TranslationResponse = serde_json::from_str(&extract_json(&text))?;
        Ok((parsed.title_en, parsed.snippet_en))
    }

    pub async fn summarize_story(&self, article: &RawArticle) -> anyhow::Result<SummarizedStory> {
        let system = super::prompts::SUMMARY_SYSTEM;
        let user_msg = format!(
            "Source: {}\nTitle: {}\nURL: {}\nSnippet: {}\nSector: {}\n\nReturn valid JSON.",
            article.source_name, article.title, article.url, article.content_snippet, article.sector
        );

        let text = self.call(HAIKU_MODEL, system, &user_msg, 800).await?;
        let parsed: SummaryResponse = serde_json::from_str(&extract_json(&text))?;

        Ok(SummarizedStory {
            article: article.clone(),
            headline: parsed.headline,
            summary: parsed.summary,
            key_facts: parsed.key_facts,
            why_it_matters: parsed.why_it_matters,
            what_to_watch: parsed.what_to_watch,
            importance_score: parsed.importance_score,
        })
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
        user_msg.push_str("\nReturn valid JSON with keys: connections, relevance_scores, trends, curation.");

        let text = self.call(SONNET_MODEL, system, &user_msg, 3000).await?;
        let parsed: AnalysisResponse = serde_json::from_str(&extract_json(&text))?;

        // Gather curated story indices
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
