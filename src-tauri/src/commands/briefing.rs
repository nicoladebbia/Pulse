use tauri::State;
use crate::db::DbState;
use crate::models::{Briefing, BriefingWithStories, Story};

#[tauri::command]
pub fn get_today_briefing(db: State<DbState>) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let briefing: Option<Briefing> = conn
        .query_row(
            "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at
             FROM briefings WHERE date = ?1",
            [&today],
            |row| {
                Ok(Briefing {
                    id: row.get(0)?,
                    date: row.get(1)?,
                    story_count: row.get(2)?,
                    ai_count: row.get(3)?,
                    miami_count: row.get(4)?,
                    italy_count: row.get(5)?,
                    tech_count: row.get(6)?,
                    status: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )
        .ok();

    let Some(briefing) = briefing else {
        return Ok(None);
    };

    let mut stmt = conn
        .prepare(
            "SELECT id, briefing_id, sector, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, relevance_score,
                    relevance_reason, is_hero, display_order, original_url, source_name,
                    published_at, created_at
             FROM stories WHERE briefing_id = ?1
             ORDER BY display_order ASC",
        )
        .map_err(|e| e.to_string())?;

    let stories: Vec<Story> = stmt
        .query_map([briefing.id], |row| {
            let key_facts_json: String = row.get(5)?;
            let key_facts: Vec<String> =
                serde_json::from_str(&key_facts_json).unwrap_or_default();
            Ok(Story {
                id: row.get(0)?,
                briefing_id: row.get(1)?,
                sector: row.get(2)?,
                headline: row.get(3)?,
                summary: row.get(4)?,
                key_facts,
                why_it_matters: row.get(6)?,
                what_to_watch: row.get(7)?,
                importance_score: row.get(8)?,
                relevance_score: row.get(9)?,
                relevance_reason: row.get(10)?,
                is_hero: row.get(11)?,
                display_order: row.get(12)?,
                original_url: row.get(13)?,
                source_name: row.get(14)?,
                published_at: row.get(15)?,
                created_at: row.get(16)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let hero_story = stories.iter().find(|s| s.is_hero).cloned();

    Ok(Some(BriefingWithStories {
        briefing,
        stories,
        hero_story,
    }))
}

#[tauri::command]
pub fn get_briefing_by_date(
    db: State<DbState>,
    date: String,
) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let briefing: Option<Briefing> = conn
        .query_row(
            "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at
             FROM briefings WHERE date = ?1",
            [&date],
            |row| {
                Ok(Briefing {
                    id: row.get(0)?,
                    date: row.get(1)?,
                    story_count: row.get(2)?,
                    ai_count: row.get(3)?,
                    miami_count: row.get(4)?,
                    italy_count: row.get(5)?,
                    tech_count: row.get(6)?,
                    status: row.get(7)?,
                    created_at: row.get(8)?,
                })
            },
        )
        .ok();

    let Some(briefing) = briefing else {
        return Ok(None);
    };

    let mut stmt = conn
        .prepare(
            "SELECT id, briefing_id, sector, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, relevance_score,
                    relevance_reason, is_hero, display_order, original_url, source_name,
                    published_at, created_at
             FROM stories WHERE briefing_id = ?1
             ORDER BY display_order ASC",
        )
        .map_err(|e| e.to_string())?;

    let stories: Vec<Story> = stmt
        .query_map([briefing.id], |row| {
            let key_facts_json: String = row.get(5)?;
            let key_facts: Vec<String> =
                serde_json::from_str(&key_facts_json).unwrap_or_default();
            Ok(Story {
                id: row.get(0)?,
                briefing_id: row.get(1)?,
                sector: row.get(2)?,
                headline: row.get(3)?,
                summary: row.get(4)?,
                key_facts,
                why_it_matters: row.get(6)?,
                what_to_watch: row.get(7)?,
                importance_score: row.get(8)?,
                relevance_score: row.get(9)?,
                relevance_reason: row.get(10)?,
                is_hero: row.get(11)?,
                display_order: row.get(12)?,
                original_url: row.get(13)?,
                source_name: row.get(14)?,
                published_at: row.get(15)?,
                created_at: row.get(16)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let hero_story = stories.iter().find(|s| s.is_hero).cloned();

    Ok(Some(BriefingWithStories {
        briefing,
        stories,
        hero_story,
    }))
}
