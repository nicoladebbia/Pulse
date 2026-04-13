use tauri::State;
use crate::db::DbState;
use crate::models::{Briefing, BriefingConnection, BriefingWithStories, Story};

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
        summary_depth: row.get(17).ok(),
        deep_summary: row.get(18).ok(),
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
        briefing_type: row.get::<_, String>(9).unwrap_or_else(|_| "daily".to_string()),
        executive_summary: row.get(10).ok(),
        time_label: row.get(11).ok(),
    })
}

const BRIEFING_SQL: &str =
    "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at, briefing_type, executive_summary, time_label
     FROM briefings WHERE date = ?1 AND briefing_type = 'daily'
     ORDER BY created_at DESC LIMIT 1";

fn load_connections(conn: &rusqlite::Connection, briefing_id: i64) -> Vec<BriefingConnection> {
    let mut stmt = match conn.prepare(
        "SELECT cc.story_id_a, sa.headline, sa.sector,
                cc.story_id_b, sb.headline, sb.sector,
                cc.connection_text, cc.insight_text
         FROM cross_connections cc
         JOIN stories sa ON sa.id = cc.story_id_a
         JOIN stories sb ON sb.id = cc.story_id_b
         WHERE cc.briefing_id = ?1"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    stmt.query_map([briefing_id], |row| {
        Ok(BriefingConnection {
            story_id_a: row.get(0)?,
            headline_a: row.get(1)?,
            sector_a: row.get(2)?,
            story_id_b: row.get(3)?,
            headline_b: row.get(4)?,
            sector_b: row.get(5)?,
            connection_text: row.get(6)?,
            insight_text: row.get(7)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

const STORIES_SQL: &str =
    "SELECT id, briefing_id, sector, headline, summary, key_facts,
            why_it_matters, what_to_watch, importance_score, relevance_score,
            relevance_reason, is_hero, display_order, original_url, source_name,
            published_at, created_at, summary_depth, deep_summary
     FROM stories WHERE briefing_id = ?1
     ORDER BY display_order ASC";

fn log(msg: &str) {
    use std::io::Write;
    let log_path = dirs::home_dir().unwrap_or_default().join("Library/Logs/Pulse/app-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = writeln!(f, "[{}] {}", chrono::Local::now().format("%H:%M:%S"), msg);
    }
}

fn load_briefing_by_id(conn: &rusqlite::Connection, briefing_id: i64) -> Result<Option<BriefingWithStories>, String> {
    log(&format!("Loading briefing by id: {}", briefing_id));

    let briefing: Option<Briefing> = conn
        .query_row(
            "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at, briefing_type, executive_summary, time_label
             FROM briefings WHERE id = ?1",
            [briefing_id],
            read_briefing,
        )
        .ok();

    let Some(briefing) = briefing else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(STORIES_SQL).map_err(|e| e.to_string())?;
    let stories: Vec<Story> = stmt
        .query_map([briefing.id], read_story)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let hero_story = stories.iter().find(|s| s.is_hero).cloned();
    let connections = load_connections(conn, briefing.id);

    Ok(Some(BriefingWithStories { briefing, stories, hero_story, connections }))
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
    let connections = load_connections(&conn, briefing.id);

    Ok(Some(BriefingWithStories {
        briefing,
        stories,
        hero_story,
        connections,
    }))
}

#[tauri::command]
pub fn get_today_briefing(db: State<DbState>) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Try today first
    if let Some(briefing) = load_briefing(&conn, &today)? {
        return Ok(Some(briefing));
    }

    // Fall back to the most recent briefing (covers overnight gap between 9 PM and 8 AM)
    let latest_date: Option<String> = conn
        .query_row(
            "SELECT date FROM briefings WHERE briefing_type = 'daily' AND status = 'complete' ORDER BY date DESC, created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();

    if let Some(date) = latest_date {
        log(&format!("No briefing for {}, falling back to latest: {}", today, date));
        return load_briefing(&conn, &date);
    }

    Ok(None)
}

#[tauri::command]
pub fn get_briefing_by_date(db: State<DbState>, date: String) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    load_briefing(&conn, &date)
}

#[tauri::command]
pub fn get_briefing_by_id(db: State<DbState>, briefing_id: i64) -> Result<Option<BriefingWithStories>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    load_briefing_by_id(&conn, briefing_id)
}

#[tauri::command]
pub fn list_briefings(db: State<DbState>) -> Result<Vec<Briefing>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, date, story_count, ai_count, miami_count, italy_count, tech_count, status, created_at, briefing_type, executive_summary, time_label
             FROM briefings ORDER BY date DESC, created_at DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;

    let briefings = stmt
        .query_map([], read_briefing)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(briefings)
}
