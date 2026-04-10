use serde::Serialize;
use tauri::State;
use crate::db::DbState;
use crate::models::{Story, StoryDetail, StorySource, CrossConnection};

#[derive(Debug, Clone, Serialize)]
pub struct StoryHeadline {
    pub id: i64,
    pub headline: String,
    pub sector: String,
    pub date: String,
}

/// Fetch headlines for a list of story IDs (used by chat sources).
#[tauri::command]
pub fn get_story_headlines(db: State<DbState>, story_ids: Vec<i64>) -> Result<Vec<StoryHeadline>, String> {
    if story_ids.is_empty() {
        return Ok(vec![]);
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let placeholders: String = story_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT s.id, s.headline, s.sector, b.date
         FROM stories s JOIN briefings b ON b.id = s.briefing_id
         WHERE s.id IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = story_ids.iter().map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter().map(|b| b.as_ref())), |row| {
            Ok(StoryHeadline {
                id: row.get(0)?,
                headline: row.get(1)?,
                sector: row.get(2)?,
                date: row.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
}

#[tauri::command]
pub fn get_stories_by_sector(
    db: State<DbState>,
    briefing_id: i64,
    sector: String,
) -> Result<Vec<Story>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, briefing_id, sector, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, relevance_score,
                    relevance_reason, is_hero, display_order, original_url, source_name,
                    published_at, created_at, summary_depth, deep_summary
             FROM stories WHERE briefing_id = ?1 AND sector = ?2
             ORDER BY display_order ASC",
        )
        .map_err(|e| e.to_string())?;

    let stories = stmt
        .query_map(rusqlite::params![briefing_id, sector], |row| {
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
                is_hero: { let v: i32 = row.get(11)?; v != 0 },
                display_order: row.get(12)?,
                original_url: row.get(13)?,
                source_name: row.get(14)?,
                published_at: row.get(15)?,
                created_at: row.get(16)?,
                summary_depth: row.get(17).ok(),
                deep_summary: row.get(18).ok(),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(stories)
}

#[tauri::command]
pub fn get_story_detail(db: State<DbState>, story_id: i64) -> Result<StoryDetail, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let story = conn
        .query_row(
            "SELECT id, briefing_id, sector, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, relevance_score,
                    relevance_reason, is_hero, display_order, original_url, source_name,
                    published_at, created_at, summary_depth, deep_summary
             FROM stories WHERE id = ?1",
            [story_id],
            |row| {
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
                    is_hero: { let v: i32 = row.get(11)?; v != 0 },
                    display_order: row.get(12)?,
                    original_url: row.get(13)?,
                    source_name: row.get(14)?,
                    published_at: row.get(15)?,
                    created_at: row.get(16)?,
                    summary_depth: row.get(17).ok(),
                    deep_summary: row.get(18).ok(),
                })
            },
        )
        .map_err(|e| e.to_string())?;

    let sources = conn
        .prepare("SELECT source_name, article_url, is_primary FROM story_sources WHERE story_id = ?1")
        .map_err(|e| e.to_string())?
        .query_map([story_id], |row| {
            Ok(StorySource {
                source_name: row.get(0)?,
                article_url: row.get(1)?,
                is_primary: row.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let connections = conn
        .prepare(
            "SELECT s.id, s.headline, s.sector, cc.connection_text, cc.insight_text
             FROM cross_connections cc
             JOIN stories s ON s.id = CASE WHEN cc.story_id_a = ?1 THEN cc.story_id_b ELSE cc.story_id_a END
             WHERE cc.story_id_a = ?1 OR cc.story_id_b = ?1",
        )
        .map_err(|e| e.to_string())?
        .query_map([story_id], |row| {
            Ok(CrossConnection {
                connected_story_id: row.get(0)?,
                connected_headline: row.get(1)?,
                connected_sector: row.get(2)?,
                connection_text: row.get(3)?,
                insight_text: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(StoryDetail {
        story,
        sources,
        connections,
    })
}
