use serde::{Deserialize, Serialize};
use super::story::Story;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Briefing {
    pub id: i64,
    pub date: String,
    pub story_count: i32,
    pub ai_count: i32,
    pub miami_count: i32,
    pub italy_count: i32,
    pub tech_count: i32,
    pub status: String,
    pub created_at: String,
    pub briefing_type: String,
    pub executive_summary: Option<String>,
    pub time_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefingConnection {
    pub story_id_a: i64,
    pub headline_a: String,
    pub sector_a: String,
    pub story_id_b: i64,
    pub headline_b: String,
    pub sector_b: String,
    pub connection_text: String,
    pub insight_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefingWithStories {
    pub briefing: Briefing,
    pub stories: Vec<Story>,
    pub hero_story: Option<Story>,
    pub connections: Vec<BriefingConnection>,
}
