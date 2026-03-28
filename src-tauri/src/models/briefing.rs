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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefingWithStories {
    pub briefing: Briefing,
    pub stories: Vec<Story>,
    pub hero_story: Option<Story>,
}
