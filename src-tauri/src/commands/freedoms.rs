use serde::{Deserialize, Serialize};
use tauri::State;
use crate::db::DbState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreedomStory {
    pub id: i64,
    pub freedom: String,
    pub headline: String,
    pub summary: String,
    pub key_facts: Vec<String>,
    pub why_it_matters: String,
    pub what_to_watch: String,
    pub importance_score: i32,
    pub is_hero: bool,
    pub original_url: String,
    pub source_name: String,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreedomsBriefing {
    pub date: String,
    pub summary: Option<String>,
    pub time_stories: Vec<FreedomStory>,
    pub financial_stories: Vec<FreedomStory>,
    pub location_stories: Vec<FreedomStory>,
    pub health_stories: Vec<FreedomStory>,
}

/// Load all freedom stories for a briefing in one query, grouped by freedom type.
fn load_all_freedom_stories(conn: &rusqlite::Connection, briefing_id: i64) -> Result<(Vec<FreedomStory>, Vec<FreedomStory>, Vec<FreedomStory>, Vec<FreedomStory>), String> {
    let mut stmt = conn.prepare(
        "SELECT id, freedom, headline, summary, key_facts, why_it_matters, what_to_watch,
                importance_score, is_hero, original_url, source_name, published_at
         FROM freedom_stories WHERE briefing_id = ?1
         ORDER BY freedom, display_order"
    ).map_err(|e| e.to_string())?;

    let all: Vec<FreedomStory> = stmt.query_map(rusqlite::params![briefing_id], |row| {
        let key_facts_str: String = row.get(4)?;
        let key_facts: Vec<String> = serde_json::from_str(&key_facts_str).unwrap_or_default();
        Ok(FreedomStory {
            id: row.get(0)?,
            freedom: row.get(1)?,
            headline: row.get(2)?,
            summary: row.get(3)?,
            key_facts,
            why_it_matters: row.get(5)?,
            what_to_watch: row.get(6)?,
            importance_score: row.get(7)?,
            is_hero: row.get::<_, i32>(8)? != 0,
            original_url: row.get(9)?,
            source_name: row.get(10)?,
            published_at: row.get(11)?,
        })
    }).map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())?;

    let mut time = Vec::new();
    let mut financial = Vec::new();
    let mut location = Vec::new();
    let mut health = Vec::new();

    for story in all {
        match story.freedom.as_str() {
            "time" => time.push(story),
            "financial" => financial.push(story),
            "location" => location.push(story),
            "health" => health.push(story),
            _ => {}
        }
    }

    Ok((time, financial, location, health))
}

#[tauri::command]
pub fn get_today_freedoms(db: State<'_, DbState>) -> Result<Option<FreedomsBriefing>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let row: Option<(i64, Option<String>)> = conn.query_row(
        "SELECT id, executive_summary FROM briefings WHERE date = ?1 AND briefing_type = 'freedoms'",
        [&today], |row| Ok((row.get(0)?, row.get(1)?))
    ).ok();

    let Some((bid, summary)) = row else { return Ok(None) };

    let (time_stories, financial_stories, location_stories, health_stories) = load_all_freedom_stories(&conn, bid)?;
    Ok(Some(FreedomsBriefing {
        date: today,
        summary,
        time_stories,
        financial_stories,
        location_stories,
        health_stories,
    }))
}

#[tauri::command]
pub fn get_freedoms_by_date(db: State<'_, DbState>, date: String) -> Result<Option<FreedomsBriefing>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let row: Option<(i64, Option<String>)> = conn.query_row(
        "SELECT id, executive_summary FROM briefings WHERE date = ?1 AND briefing_type = 'freedoms'",
        [&date], |row| Ok((row.get(0)?, row.get(1)?))
    ).ok();

    let Some((bid, summary)) = row else { return Ok(None) };

    let (time_stories, financial_stories, location_stories, health_stories) = load_all_freedom_stories(&conn, bid)?;
    Ok(Some(FreedomsBriefing {
        date,
        summary,
        time_stories,
        financial_stories,
        location_stories,
        health_stories,
    }))
}
