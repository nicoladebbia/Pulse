use tauri::State;
use crate::db::DbState;
use crate::models::{Briefing, BriefingWithStories, Story};

fn read_story(row: &rusqlite::Row) -> rusqlite::Result<Story> {
    let key_facts_json: String = row.get(5)?;
    let key_facts: Vec<String> = serde_json::from_str(&key_facts_json).unwrap_or_default();
    let is_hero_int: i32 = row.get(11)?;
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
        is_hero: is_hero_int != 0,
        display_order: row.get(12)?,
        original_url: row.get(13)?,
        source_name: row.get(14)?,
        published_at: row.get(15)?,
        created_at: row.get(16)?,
    })
}

fn read_briefing(row: &rusqlite::Row) -> rusqlite::Result<Briefing> {
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
}

const BRIEFING_SQL: &str =
    "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at
     FROM briefings WHERE date = ?1";

const STORIES_SQL: &str =
    "SELECT id, briefing_id, sector, headline, summary, key_facts,
            why_it_matters, what_to_watch, importance_score, relevance_score,
            relevance_reason, is_hero, display_order, original_url, source_name,
            published_at, created_at
     FROM stories WHERE briefing_id = ?1
     ORDER BY display_order ASC";

fn log(msg: &str) {
    use std::io::Write;
    let log_path = dirs::home_dir().unwrap_or_default().join("Library/Logs/Pulse/app-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = writeln!(f, "[{}] {}", chrono::Local::now().format("%H:%M:%S"), msg);
    }
}

fn load_briefing(conn: &rusqlite::Connection, date: &str) -> Result<Option<BriefingWithStories>, String> {
    log(&format!("Loading briefing for date: {}", date));

    let briefing: Option<Briefing> = conn
        .query_row(BRIEFING_SQL, [date], read_briefing)
        .ok();

    let Some(briefing) = briefing else {
        log(&format!("No briefing found for {}", date));
        return Ok(None);
    };

    log(&format!("Found briefing id={}, stories={}", briefing.id, briefing.story_count));

    let mut stmt = conn.prepare(STORIES_SQL).map_err(|e| {
        log(&format!("SQL prepare error: {}", e));
        e.to_string()
    })?;

    let stories: Vec<Story> = stmt
        .query_map([briefing.id], read_story)
        .map_err(|e| {
            log(&format!("query_map error: {}", e));
            e.to_string()
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            log(&format!("row read error: {}", e));
            e.to_string()
        })?;

    log(&format!("Loaded {} stories successfully", stories.len()));

    let hero_story = stories.iter().find(|s| s.is_hero).cloned();

    Ok(Some(BriefingWithStories {
        briefing,
        stories,
        hero_story,
    }))
}

#[tauri::command]
pub fn get_today_briefing(db: State<DbState>) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    load_briefing(&conn, &today)
}

#[tauri::command]
pub fn get_briefing_by_date(db: State<DbState>, date: String) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    load_briefing(&conn, &date)
}

#[tauri::command]
pub fn list_briefings(db: State<DbState>) -> Result<Vec<Briefing>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at
             FROM briefings ORDER BY date DESC",
        )
        .map_err(|e| e.to_string())?;

    let briefings = stmt
        .query_map([], read_briefing)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(briefings)
}
