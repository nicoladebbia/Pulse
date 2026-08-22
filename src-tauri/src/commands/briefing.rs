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
        source_type: row.get(19).ok(),
        financial_metadata: row.get(20).ok(),
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
        hero_headline: row.get(12).ok(),
    })
}

// A briefing "has news" when it holds at least one non-financial story. Bulk regulatory
// filings don't count: 29 daily briefings consist entirely of SEC/FEC/Federal Register
// rows and have nothing to read. The COALESCE is belt-and-braces: migration 017 defines
// source_type as NOT NULL DEFAULT 'news' with a CHECK, so no row can actually be NULL —
// it only keeps the predicate correct if that column is ever relaxed.
//
// This orders the WITHIN-DATE choice: a run WITH news beats a later run without it. On
// 2026-05-23 a 9:02 PM rerun produced 21 filings and 0 news, and because the ordering was
// purely created_at DESC that rerun replaced the day's real briefing — the user opened
// Pulse to a page of SEC "reported material event" cards. If no run that day has news the
// ordering still falls through to created_at, so a day is never left blank.
const BRIEFING_SQL: &str =
    "SELECT b.id, b.date, b.story_count, b.ai_count, b.miami_count, b.italy_count, b.tech_count, b.status, b.created_at, b.briefing_type, b.executive_summary, b.time_label,
            (SELECT s.headline FROM stories s WHERE s.briefing_id = b.id AND s.is_hero = 1 LIMIT 1)
     FROM briefings b WHERE b.date = ?1 AND b.briefing_type = 'daily'
     ORDER BY EXISTS (SELECT 1 FROM stories s
                      WHERE s.briefing_id = b.id
                        AND COALESCE(s.source_type, 'news') != 'financial') DESC,
              b.created_at DESC
     LIMIT 1";

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
            published_at, created_at, summary_depth, deep_summary,
            source_type, financial_metadata
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
            "SELECT b.id, b.date, b.story_count, b.ai_count, b.miami_count, b.italy_count, b.tech_count, b.status, b.created_at, b.briefing_type, b.executive_summary, b.time_label,
                    (SELECT s.headline FROM stories s WHERE s.briefing_id = b.id AND s.is_hero = 1 LIMIT 1)
             FROM briefings b WHERE b.id = ?1",
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
    let connections = load_connections(conn, briefing.id);

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

    // Fall back to the most recent briefing (covers overnight gap between 9 PM and 8 AM).
    // Prefer the most recent day that actually has news: 29 daily briefings are filings
    // only, and landing on one of those shows a page of SEC forms with no way to tell
    // that a real briefing exists one day earlier. If nothing in the archive has news,
    // fall back to the old behaviour rather than showing an empty app.
    let latest_date: Option<String> = conn
        .query_row(
            "SELECT b.date FROM briefings b
             WHERE b.briefing_type = 'daily' AND b.status = 'complete'
               AND EXISTS (SELECT 1 FROM stories s
                           WHERE s.briefing_id = b.id
                             AND COALESCE(s.source_type, 'news') != 'financial')
             ORDER BY b.date DESC, b.created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok()
        .or_else(|| {
            conn.query_row(
                "SELECT date FROM briefings WHERE briefing_type = 'daily' AND status = 'complete' ORDER BY date DESC, created_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok()
        });

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
            "SELECT b.id, b.date, b.story_count, b.ai_count, b.miami_count, b.italy_count, b.tech_count, b.status, b.created_at, b.briefing_type, b.executive_summary, b.time_label,
                    (SELECT s.headline FROM stories s WHERE s.briefing_id = b.id AND s.is_hero = 1 LIMIT 1)
             FROM briefings b
             WHERE b.id = (
                 SELECT b2.id FROM briefings b2
                 WHERE b2.date = b.date AND b2.briefing_type = b.briefing_type
                   AND COALESCE(b2.time_label, '') = COALESCE(b.time_label, '')
                 ORDER BY b2.created_at DESC LIMIT 1
             )
             ORDER BY b.date DESC, b.created_at DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;

    let briefings = stmt
        .query_map([], read_briefing)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(briefings)
}

#[cfg(test)]
mod briefing_selection_tests {
    //! Guards which briefing the app SHOWS when a date has more than one run.
    //!
    //! 29 daily briefings in production hold zero news stories — only bulk regulatory
    //! filings. On 2026-05-23 a 9:02 PM rerun produced 21 filings and 0 news, and the
    //! selection ordered purely by created_at DESC, so that rerun became the briefing the
    //! user opened Pulse to: a page of SEC "reported material event" cards.

    use super::*;
    use crate::db::connection::initialize_in_memory;
    use rusqlite::Connection;

    fn briefing(conn: &Connection, date: &str, created_at: &str) -> i64 {
        conn.execute(
            "INSERT INTO briefings (date, briefing_type, status, story_count, created_at)
             VALUES (?1, 'daily', 'complete', 0, ?2)",
            [date, created_at],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn story(conn: &Connection, briefing_id: i64, source_type: Option<&str>, headline: &str) {
        conn.execute(
            "INSERT INTO stories (briefing_id, sector, original_title, original_url, source_name,
                                  headline, summary, key_facts, why_it_matters, what_to_watch,
                                  url_hash, title_hash, importance_score, display_order, source_type)
             VALUES (?1, 'ai', ?2, 'https://example.com/' || ?2, 'Example',
                     ?2, 's', '[]', 'w', 'w', 'uh-' || ?2, 'th-' || ?2, 5, 0, ?3)",
            rusqlite::params![briefing_id, headline, source_type],
        )
        .unwrap();
    }

    /// Runs the real selection query and returns the chosen briefing id.
    fn selected(conn: &Connection, date: &str) -> Option<i64> {
        conn.query_row(BRIEFING_SQL, [date], |row| row.get::<_, i64>(0)).ok()
    }

    #[test]
    fn a_run_with_news_beats_a_later_filings_only_rerun() {
        // The 2026-05-23 shape, which is the whole reason this ordering exists.
        let conn = initialize_in_memory().unwrap();
        let morning = briefing(&conn, "2026-05-23", "2026-05-23 08:00:00");
        story(&conn, morning, Some("news"), "real story");

        let evening = briefing(&conn, "2026-05-23", "2026-05-23 21:02:00");
        for i in 0..21 {
            story(&conn, evening, Some("financial"), &format!("Form 8-K {i}"));
        }

        assert_eq!(
            selected(&conn, "2026-05-23"),
            Some(morning),
            "the filings-only rerun must not replace the day's real briefing"
        );
    }

    #[test]
    fn the_latest_run_still_wins_when_both_have_news() {
        // The ordinary case must be untouched: this guard only breaks ties in favour of
        // news, it does not pin the app to the earliest run of the day.
        let conn = initialize_in_memory().unwrap();
        let morning = briefing(&conn, "2026-05-24", "2026-05-24 08:00:00");
        story(&conn, morning, Some("news"), "morning story");

        let evening = briefing(&conn, "2026-05-24", "2026-05-24 21:00:00");
        story(&conn, evening, Some("news"), "evening story");

        assert_eq!(selected(&conn, "2026-05-24"), Some(evening));
    }

    #[test]
    fn a_day_with_only_filings_still_returns_a_briefing() {
        // Degrade gracefully: showing filings beats showing nothing at all.
        let conn = initialize_in_memory().unwrap();
        let early = briefing(&conn, "2026-05-25", "2026-05-25 08:00:00");
        story(&conn, early, Some("financial"), "Form 4");
        let late = briefing(&conn, "2026-05-25", "2026-05-25 21:00:00");
        story(&conn, late, Some("financial"), "Form 8-K");

        assert_eq!(selected(&conn, "2026-05-25"), Some(late));
    }

    #[test]
    fn every_story_is_classified_as_news_or_financial() {
        // The selection predicate splits the world in two, so it is only exhaustive if
        // source_type can never be NULL or some third value. Migration 017 enforces that
        // with NOT NULL DEFAULT 'news' + a CHECK; pin it, because relaxing the column
        // would silently make news-empty detection wrong rather than fail loudly.
        let conn = initialize_in_memory().unwrap();
        let b = briefing(&conn, "2026-05-26", "2026-05-26 08:00:00");

        let null_rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            story(&conn, b, None, "null type")
        }))
        .is_err();
        assert!(null_rejected, "source_type must be NOT NULL");

        let third_value_rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            story(&conn, b, Some("newsletter"), "third type")
        }))
        .is_err();
        assert!(third_value_rejected, "source_type must be constrained to news|financial");
    }

    #[test]
    fn an_empty_briefing_does_not_beat_one_with_filings() {
        // A run that stored nothing at all has no news either, so the tie falls through
        // to created_at and the later run wins — no special case needed.
        let conn = initialize_in_memory().unwrap();
        let _empty = briefing(&conn, "2026-05-27", "2026-05-27 08:00:00");
        let withfilings = briefing(&conn, "2026-05-27", "2026-05-27 21:00:00");
        story(&conn, withfilings, Some("financial"), "Form 4");

        assert_eq!(selected(&conn, "2026-05-27"), Some(withfilings));
    }
}
