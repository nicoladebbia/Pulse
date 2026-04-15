use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Story {
    pub id: i64,
    pub briefing_id: i64,
    pub sector: String,
    pub headline: String,
    pub summary: String,
    pub key_facts: Vec<String>,
    pub why_it_matters: String,
    pub what_to_watch: String,
    pub importance_score: i32,
    pub relevance_score: Option<i32>,
    pub relevance_reason: Option<String>,
    pub is_hero: bool,
    pub display_order: i32,
    pub original_url: String,
    pub source_name: String,
    pub published_at: Option<String>,
    pub created_at: String,
    pub summary_depth: Option<String>,
    pub deep_summary: Option<String>,
    pub source_type: Option<String>,
    pub financial_metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorySource {
    pub source_name: String,
    pub article_url: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryDetail {
    #[serde(flatten)]
    pub story: Story,
    pub sources: Vec<StorySource>,
    pub connections: Vec<CrossConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossConnection {
    pub connected_story_id: i64,
    pub connected_headline: String,
    pub connected_sector: String,
    pub connection_text: String,
    pub insight_text: String,
}
