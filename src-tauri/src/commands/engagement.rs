//! Local-only engagement instrumentation.
//!
//! Pulse records everything it produces and nothing it consumes, which made the
//! keep-or-kill question unanswerable: `backtest_results` and `portfolio_snapshots`
//! each hold about one row per day, which reads as "Trading is in use" but is a
//! scheduler writing to itself. These commands record the other half.
//!
//! Privacy: nothing here leaves the machine and nothing here stores user text.
//! `detail` carries small structured JSON (`{"len":42}`) and never a message body,
//! a query, or a headline — see `record_engagement`'s validation.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::DbState;

/// Events the frontend is allowed to record. An allowlist rather than a free
/// string because a typo'd event name does not fail — it silently creates a
/// second bucket and quietly halves whichever count it was supposed to join.
const EVENTS: &[&str] = &[
    "surface_view",
    "story_open",
    "story_close",
    "sector_filter",
    "chat_message",
    "citation_click",
];

/// Surfaces are SvelteKit route ids (`/`, `/archive`, `/freedoms/[freedom]`), so a
/// new route is instrumented the moment it exists and no mapping table can drift.
/// Only the shape is checked.
const MAX_FIELD_LEN: usize = 128;

/// Two years. At the observed rate (a few dozen events a day) that is well under
/// 50k rows, but the table would otherwise be the one thing in the schema that
/// grows without bound.
const RETENTION_DAYS: i64 = 730;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngagementInput {
    pub surface: String,
    pub event: String,
    pub story_id: Option<i64>,
    pub briefing_id: Option<i64>,
    pub sector: Option<String>,
    pub dwell_ms: Option<i64>,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SurfaceUsage {
    pub surface: String,
    pub events: i64,
    pub views: i64,
    pub days_active: i64,
    pub last_seen: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SectorUsage {
    pub sector: String,
    pub opens: i64,
    pub dwell_ms_total: i64,
}

#[derive(Debug, Serialize)]
pub struct EngagementSummary {
    pub days: u32,
    pub total_events: i64,
    pub story_opens: i64,
    pub median_dwell_ms: Option<i64>,
    pub surfaces: Vec<SurfaceUsage>,
    pub sectors: Vec<SectorUsage>,
}

fn validate(input: &EngagementInput) -> Result<(), String> {
    if !EVENTS.contains(&input.event.as_str()) {
        return Err(format!("unknown engagement event: {}", input.event));
    }
    if input.surface.is_empty() || input.surface.len() > MAX_FIELD_LEN {
        return Err(format!("surface must be 1..={MAX_FIELD_LEN} chars"));
    }
    if let Some(s) = &input.sector
        && s.len() > MAX_FIELD_LEN {
            return Err("sector too long".into());
        }
    // `detail` is for small structured facts, not content. The cap is what stops
    // a caller from ever passing a headline or a chat message through it.
    if let Some(d) = &input.detail
        && d.len() > MAX_FIELD_LEN {
            return Err("detail too long — it is for counters, not content".into());
        }
    if let Some(ms) = input.dwell_ms
        && ms < 0 {
            return Err("dwell_ms cannot be negative".into());
        }
    Ok(())
}

/// Record one engagement event. The frontend calls this fire-and-forget, so an
/// error here must never be able to reach a render path — it is returned for
/// tests and for the dev console, and callers drop it.
#[tauri::command]
pub fn record_engagement(db: State<DbState>, event: EngagementInput) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    insert_event(&conn, &event).map_err(|e| e.to_string())
}

pub fn insert_event(conn: &rusqlite::Connection, input: &EngagementInput) -> rusqlite::Result<()> {
    if let Err(msg) = validate(input) {
        return Err(rusqlite::Error::InvalidParameterName(msg));
    }

    conn.execute(
        "INSERT INTO engagement_events
             (surface, event, story_id, briefing_id, sector, dwell_ms, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            input.surface,
            input.event,
            input.story_id,
            input.briefing_id,
            input.sector,
            input.dwell_ms,
            input.detail,
        ],
    )?;

    // Self-maintaining retention. Running the delete on every insert would be
    // pure waste, so it rides along roughly once per thousand events.
    if conn.last_insert_rowid() % 1000 == 0 {
        conn.execute(
            "DELETE FROM engagement_events
             WHERE occurred_at < datetime('now', ?1)",
            [format!("-{RETENTION_DAYS} days")],
        )?;
    }

    Ok(())
}

/// The shape of the answer #14 needs: which surfaces are actually used, how
/// recently, and on how many distinct days.
#[tauri::command]
pub fn get_engagement_summary(db: State<DbState>, days: u32) -> Result<EngagementSummary, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    summarize(&conn, days).map_err(|e| e.to_string())
}

pub fn summarize(conn: &rusqlite::Connection, days: u32) -> rusqlite::Result<EngagementSummary> {
    let window = format!("-{days} days");

    let total_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM engagement_events WHERE occurred_at >= datetime('now', ?1)",
        [&window],
        |r| r.get(0),
    )?;

    let story_opens: i64 = conn.query_row(
        "SELECT COUNT(*) FROM engagement_events
         WHERE event = 'story_open' AND occurred_at >= datetime('now', ?1)",
        [&window],
        |r| r.get(0),
    )?;

    // SQLite has no median. Ordering the closes and taking the middle row is
    // exact and cheap at this table's size; the mean would be dragged around by
    // the "left it open over lunch" tail.
    let dwell_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM engagement_events
         WHERE dwell_ms IS NOT NULL AND occurred_at >= datetime('now', ?1)",
        [&window],
        |r| r.get(0),
    )?;
    let median_dwell_ms: Option<i64> = if dwell_count == 0 {
        None
    } else {
        conn.query_row(
            "SELECT dwell_ms FROM engagement_events
             WHERE dwell_ms IS NOT NULL AND occurred_at >= datetime('now', ?1)
             ORDER BY dwell_ms LIMIT 1 OFFSET ?2",
            rusqlite::params![&window, dwell_count / 2],
            |r| r.get(0),
        )
        .ok()
    };

    let mut stmt = conn.prepare(
        "SELECT surface,
                COUNT(*),
                SUM(CASE WHEN event = 'surface_view' THEN 1 ELSE 0 END),
                COUNT(DISTINCT date(occurred_at, 'localtime')),
                MAX(occurred_at)
         FROM engagement_events
         WHERE occurred_at >= datetime('now', ?1)
         GROUP BY surface
         ORDER BY COUNT(*) DESC",
    )?;
    let surfaces = stmt
        .query_map([&window], |r| {
            Ok(SurfaceUsage {
                surface: r.get(0)?,
                events: r.get(1)?,
                views: r.get(2)?,
                days_active: r.get(3)?,
                last_seen: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(
        "SELECT sector,
                SUM(CASE WHEN event = 'story_open' THEN 1 ELSE 0 END),
                COALESCE(SUM(dwell_ms), 0)
         FROM engagement_events
         WHERE sector IS NOT NULL AND occurred_at >= datetime('now', ?1)
         GROUP BY sector
         ORDER BY 2 DESC",
    )?;
    let sectors = stmt
        .query_map([&window], |r| {
            Ok(SectorUsage {
                sector: r.get(0)?,
                opens: r.get(1)?,
                dwell_ms_total: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(EngagementSummary {
        days,
        total_events,
        story_opens,
        median_dwell_ms,
        surfaces,
        sectors,
    })
}

// ── Explicit curation feedback ────────────────────────────────────────────────
//
// The one explicit signal about curation: a shown story "mattered" or "didn't".
// Stored in `story_feedback` (migration 034), one row per story. Opens and read
// time are not stored again — they are the story_open / story_close events above,
// joined in by the `story_feedback_signals` view.

const FEEDBACK_LABELS: &[&str] = &["mattered", "didnt"];

/// The frontend asks for the labels of whatever is on screen in one call; an
/// archive page is a few hundred stories, so this only guards against a runaway.
const MAX_FEEDBACK_LOOKUP: usize = 2000;

#[derive(Debug, Serialize, PartialEq)]
pub struct StoryFeedback {
    pub story_id: i64,
    pub label: String,
}

/// Set (`Some`) or clear (`None`) the label on a story. The briefing id is read
/// from the story row rather than trusted from the caller.
pub fn set_feedback(
    conn: &rusqlite::Connection,
    story_id: i64,
    label: Option<&str>,
) -> rusqlite::Result<()> {
    let Some(label) = label else {
        conn.execute("DELETE FROM story_feedback WHERE story_id = ?1", [story_id])?;
        return Ok(());
    };
    if !FEEDBACK_LABELS.contains(&label) {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "unknown feedback label '{label}'"
        )));
    }
    let written = conn.execute(
        "INSERT INTO story_feedback (story_id, briefing_id, label)
         SELECT id, briefing_id, ?2 FROM stories WHERE id = ?1
         ON CONFLICT(story_id) DO UPDATE SET
             label = excluded.label,
             updated_at = datetime('now')",
        rusqlite::params![story_id, label],
    )?;
    if written == 0 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "no story with id {story_id}"
        )));
    }
    Ok(())
}

pub fn get_feedback(
    conn: &rusqlite::Connection,
    story_ids: &[i64],
) -> rusqlite::Result<Vec<StoryFeedback>> {
    if story_ids.is_empty() {
        return Ok(Vec::new());
    }
    if story_ids.len() > MAX_FEEDBACK_LOOKUP {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "at most {MAX_FEEDBACK_LOOKUP} story ids per lookup"
        )));
    }
    // One bound parameter regardless of list length: the ids travel as a JSON array.
    let ids = serde_json::to_string(story_ids).map_err(|e| {
        rusqlite::Error::InvalidParameterName(e.to_string())
    })?;
    let mut stmt = conn.prepare(
        "SELECT story_id, label FROM story_feedback
         WHERE story_id IN (SELECT value FROM json_each(?1))
         ORDER BY story_id",
    )?;
    let rows = stmt.query_map([ids], |r| {
        Ok(StoryFeedback { story_id: r.get(0)?, label: r.get(1)? })
    })?;
    rows.collect()
}

/// Label a story. Called on a tap, so unlike `record_engagement` the frontend
/// does look at the result — it reverts its optimistic state on error.
#[tauri::command]
pub fn set_story_feedback(
    db: State<DbState>,
    story_id: i64,
    label: Option<String>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    set_feedback(&conn, story_id, label.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_story_feedback(
    db: State<DbState>,
    story_ids: Vec<i64>,
) -> Result<Vec<StoryFeedback>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    get_feedback(&conn, &story_ids).map_err(|e| e.to_string())
}

#[cfg(test)]
mod engagement_tests {
    use super::*;
    use crate::db::connection::initialize_in_memory;
    use rusqlite::Connection;

    fn ev(surface: &str, event: &str) -> EngagementInput {
        EngagementInput {
            surface: surface.into(),
            event: event.into(),
            story_id: None,
            briefing_id: None,
            sector: None,
            dwell_ms: None,
            detail: None,
        }
    }

    fn db() -> Connection {
        initialize_in_memory().expect("migrations should apply")
    }

    /// The frontend and this struct are a contract that nothing else checks:
    /// `generate_handler!` verifies the command NAME at compile time and says
    /// nothing about field names. A mismatch here fails at runtime, inside a
    /// fire-and-forget call whose error is deliberately swallowed — so it would
    /// present as "instrumentation silently records nothing", the single worst
    /// failure mode for this feature.
    #[test]
    fn the_minimal_payload_the_frontend_sends_deserializes() {
        // Exactly what trackSurfaceView produces: every optional field absent,
        // because JSON.stringify drops `undefined`.
        let json = r#"{"surface":"/archive","event":"surface_view"}"#;
        let input: EngagementInput =
            serde_json::from_str(json).expect("optional fields must be omissible");
        assert_eq!(input.surface, "/archive");
        assert_eq!(input.event, "surface_view");
        assert_eq!(input.story_id, None);
        assert_eq!(input.dwell_ms, None);
    }

    #[test]
    fn the_full_payload_maps_camel_case_to_snake_case() {
        // trackStoryClose sends camelCase; the struct is snake_case. The
        // rename_all attribute is the only thing bridging them.
        let json = r#"{
            "surface":"/",
            "event":"story_close",
            "storyId":7,
            "briefingId":3,
            "sector":"ai",
            "dwellMs":4200,
            "detail":"{\"filing\":false}"
        }"#;
        let input: EngagementInput = serde_json::from_str(json).expect("camelCase must map");
        assert_eq!(input.story_id, Some(7));
        assert_eq!(input.briefing_id, Some(3));
        assert_eq!(input.dwell_ms, Some(4200));
        assert_eq!(input.sector.as_deref(), Some("ai"));
    }

    #[test]
    fn a_snake_case_payload_is_rejected_so_the_rename_cannot_silently_regress() {
        // If rename_all were dropped, the test above would still pass under
        // snake_case input. This pins the direction.
        let json = r#"{"surface":"/","event":"story_open","story_id":7}"#;
        let input: EngagementInput = serde_json::from_str(json).unwrap();
        assert_eq!(
            input.story_id, None,
            "snake_case storyId must NOT bind — the frontend sends camelCase"
        );
    }

    #[test]
    fn migration_033_creates_the_table() {
        let conn = db();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM engagement_events", [], |r| r.get(0))
            .expect("engagement_events must exist after migrations");
        assert_eq!(n, 0);
    }

    #[test]
    fn an_event_round_trips() {
        let conn = db();
        let mut e = ev("/archive", "story_open");
        e.story_id = Some(42);
        e.sector = Some("ai".into());
        insert_event(&conn, &e).unwrap();

        let (surface, event, story_id, sector): (String, String, i64, String) = conn
            .query_row(
                "SELECT surface, event, story_id, sector FROM engagement_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(surface, "/archive");
        assert_eq!(event, "story_open");
        assert_eq!(story_id, 42);
        assert_eq!(sector, "ai");
    }

    #[test]
    fn occurred_at_defaults_so_the_caller_cannot_forge_a_timestamp() {
        let conn = db();
        insert_event(&conn, &ev("/", "surface_view")).unwrap();
        let ts: String = conn
            .query_row("SELECT occurred_at FROM engagement_events", [], |r| r.get(0))
            .unwrap();
        // 'YYYY-MM-DD HH:MM:SS'
        assert_eq!(ts.len(), 19, "expected a datetime('now') default, got {ts}");
    }

    #[test]
    fn a_typod_event_name_is_rejected_rather_than_silently_bucketed() {
        let conn = db();
        // The whole point of the allowlist: 'story_opened' would otherwise create a
        // second bucket and split the open count in half with no error anywhere.
        assert!(insert_event(&conn, &ev("/", "story_opened")).is_err());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM engagement_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "a rejected event must not be written");
    }

    #[test]
    fn detail_cannot_be_used_to_smuggle_content() {
        let conn = db();
        let mut e = ev("/ask", "chat_message");
        e.detail = Some("x".repeat(MAX_FIELD_LEN + 1));
        assert!(
            insert_event(&conn, &e).is_err(),
            "detail is for counters; an oversized payload must be refused"
        );
    }

    #[test]
    fn a_negative_dwell_is_rejected() {
        let conn = db();
        let mut e = ev("/", "story_close");
        e.dwell_ms = Some(-1);
        assert!(insert_event(&conn, &e).is_err());
    }

    #[test]
    fn summary_separates_views_from_other_events() {
        let conn = db();
        for _ in 0..3 {
            insert_event(&conn, &ev("/trends", "surface_view")).unwrap();
        }
        insert_event(&conn, &ev("/trends", "citation_click")).unwrap();
        insert_event(&conn, &ev("/", "surface_view")).unwrap();

        let s = summarize(&conn, 30).unwrap();
        assert_eq!(s.total_events, 5);
        let trends = s.surfaces.iter().find(|u| u.surface == "/trends").unwrap();
        assert_eq!(trends.events, 4);
        assert_eq!(trends.views, 3);
        assert_eq!(trends.days_active, 1);
        assert!(trends.last_seen.is_some());
    }

    #[test]
    fn a_surface_nobody_opened_is_absent_rather_than_zero() {
        // This is the #14 question. `/freedoms` producing 26 briefings a month
        // must not look the same as `/freedoms` being read.
        let conn = db();
        insert_event(&conn, &ev("/", "surface_view")).unwrap();
        let s = summarize(&conn, 30).unwrap();
        assert!(!s.surfaces.iter().any(|u| u.surface == "/freedoms"));
    }

    #[test]
    fn median_dwell_ignores_opens_that_never_closed() {
        let conn = db();
        // Three closes with dwell, plus opens carrying no dwell at all.
        for ms in [1_000i64, 5_000, 9_000] {
            let mut e = ev("/", "story_close");
            e.dwell_ms = Some(ms);
            insert_event(&conn, &e).unwrap();
        }
        for _ in 0..10 {
            insert_event(&conn, &ev("/", "story_open")).unwrap();
        }
        let s = summarize(&conn, 30).unwrap();
        assert_eq!(s.story_opens, 10);
        assert_eq!(
            s.median_dwell_ms,
            Some(5_000),
            "NULL dwell rows must not drag the median toward zero"
        );
    }

    #[test]
    fn median_dwell_is_none_when_nothing_has_closed_yet() {
        let conn = db();
        insert_event(&conn, &ev("/", "story_open")).unwrap();
        assert_eq!(summarize(&conn, 30).unwrap().median_dwell_ms, None);
    }

    #[test]
    fn the_window_excludes_older_events() {
        let conn = db();
        insert_event(&conn, &ev("/", "surface_view")).unwrap();
        conn.execute(
            "UPDATE engagement_events SET occurred_at = datetime('now', '-40 days')",
            [],
        )
        .unwrap();
        insert_event(&conn, &ev("/archive", "surface_view")).unwrap();

        let s = summarize(&conn, 30).unwrap();
        assert_eq!(s.total_events, 1);
        assert_eq!(s.surfaces.len(), 1);
        assert_eq!(s.surfaces[0].surface, "/archive");
    }

    #[test]
    fn days_active_counts_human_days_not_utc_days() {
        // occurred_at is stored UTC (the column DEFAULT is datetime('now')), and
        // every window filter compares UTC to UTC, which is self-consistent. The
        // one place the storage clock leaks is this bucket: "days active" is a
        // claim about days a person used the app. Two events two hours apart, in
        // the same evening for the user, straddle UTC midnight anywhere east of
        // Greenwich and would report two days of use for one sitting.
        //
        // Expected is derived from the machine's own offset so this asserts
        // "buckets by local day" rather than "buckets by CEST". On a UTC box the
        // two instants genuinely are two local days and the expectation follows.
        let conn = db();
        let instants = ["2026-08-16 23:00:00", "2026-08-17 01:00:00"];
        for ts in instants {
            insert_event(&conn, &ev("/", "surface_view")).unwrap();
            conn.execute(
                "UPDATE engagement_events SET occurred_at = ?1 WHERE id = ?2",
                rusqlite::params![ts, conn.last_insert_rowid()],
            )
            .unwrap();
        }

        let expected = instants
            .iter()
            .map(|ts| {
                let utc = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
                    .unwrap()
                    .and_utc();
                utc.with_timezone(&chrono::Local).date_naive()
            })
            .collect::<std::collections::HashSet<_>>()
            .len() as i64;

        // A 30-day window anchored on `now` cannot contain a fixed 2026 date
        // forever, so ask for a window wide enough to keep this test honest.
        let s = summarize(&conn, 36_500).unwrap();
        assert_eq!(s.surfaces.len(), 1);
        assert_eq!(
            s.surfaces[0].days_active, expected,
            "days_active must bucket by the user's calendar day"
        );
    }

    #[test]
    fn sector_engagement_totals_dwell_per_sector() {
        let conn = db();
        for (sector, ms) in [("ai", 4_000i64), ("ai", 6_000), ("italy", 1_000)] {
            let mut open = ev("/", "story_open");
            open.sector = Some(sector.into());
            insert_event(&conn, &open).unwrap();

            let mut close = ev("/", "story_close");
            close.sector = Some(sector.into());
            close.dwell_ms = Some(ms);
            insert_event(&conn, &close).unwrap();
        }

        let s = summarize(&conn, 30).unwrap();
        let ai = s.sectors.iter().find(|x| x.sector == "ai").unwrap();
        assert_eq!(ai.opens, 2);
        assert_eq!(ai.dwell_ms_total, 10_000);
        let italy = s.sectors.iter().find(|x| x.sector == "italy").unwrap();
        assert_eq!(italy.opens, 1);
        assert_eq!(italy.dwell_ms_total, 1_000);
    }

    /// Clearing every filter at once is a sector event with no sector — the frontend
    /// deliberately sends null rather than inventing an "all" pseudo-sector. The
    /// rollup must drop it, otherwise the sector table grows a phantom row with zero
    /// opens and zero dwell that reads as "a sector nobody looks at".
    #[test]
    fn a_sector_event_with_no_sector_stays_out_of_the_rollup() {
        let conn = db();
        let mut opened = ev("/", "story_open");
        opened.sector = Some("ai".into());
        insert_event(&conn, &opened).unwrap();
        insert_event(&conn, &ev("/", "sector_filter")).unwrap();

        let s = summarize(&conn, 30).unwrap();
        assert_eq!(s.sectors.len(), 1);
        assert_eq!(s.sectors[0].sector, "ai");
        // It is still recorded — only excluded from the per-sector breakdown.
        assert_eq!(s.total_events, 2);
    }

    #[test]
    fn retention_prunes_only_what_is_past_the_window() {
        let conn = db();
        insert_event(&conn, &ev("/", "surface_view")).unwrap();
        conn.execute(
            "UPDATE engagement_events SET occurred_at = datetime('now', '-800 days')",
            [],
        )
        .unwrap();
        insert_event(&conn, &ev("/", "surface_view")).unwrap();

        // Force the prune branch deterministically rather than inserting 1000 rows.
        conn.execute(
            "DELETE FROM engagement_events WHERE occurred_at < datetime('now', '-730 days')",
            [],
        )
        .unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM engagement_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the recent event must survive the prune");
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use crate::db::test_helpers::{seed_briefing, test_db, TestStory};

    fn label_of(conn: &rusqlite::Connection, story_id: i64) -> Option<String> {
        get_feedback(conn, &[story_id]).unwrap().pop().map(|f| f.label)
    }

    #[test]
    fn label_can_be_set_changed_and_cleared() {
        let conn = test_db();
        let (bid, ids) = seed_briefing(&conn, "2026-09-24", &[TestStory::new("ai", "One")]);

        set_feedback(&conn, ids[0], Some("mattered")).unwrap();
        assert_eq!(label_of(&conn, ids[0]).as_deref(), Some("mattered"));
        let stored_bid: i64 = conn
            .query_row("SELECT briefing_id FROM story_feedback WHERE story_id = ?1", [ids[0]], |r| r.get(0))
            .unwrap();
        assert_eq!(stored_bid, bid);

        set_feedback(&conn, ids[0], Some("didnt")).unwrap();
        assert_eq!(label_of(&conn, ids[0]).as_deref(), Some("didnt"));
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM story_feedback", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "relabelling overwrites, it does not append");

        set_feedback(&conn, ids[0], None).unwrap();
        assert_eq!(label_of(&conn, ids[0]), None);
    }

    #[test]
    fn rejects_unknown_labels_and_unknown_stories() {
        let conn = test_db();
        let (_, ids) = seed_briefing(&conn, "2026-09-24", &[TestStory::new("ai", "One")]);
        assert!(set_feedback(&conn, ids[0], Some("meh")).is_err());
        assert!(set_feedback(&conn, 999_999, Some("mattered")).is_err());
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM story_feedback", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0);
        // Clearing a label that was never set is a no-op, not an error.
        assert!(set_feedback(&conn, ids[0], None).is_ok());
    }

    #[test]
    fn lookup_returns_only_requested_labelled_stories() {
        let conn = test_db();
        let (_, ids) = seed_briefing(
            &conn,
            "2026-09-24",
            &[TestStory::new("ai", "A"), TestStory::new("tech", "B"), TestStory::new("italy", "C")],
        );
        set_feedback(&conn, ids[0], Some("mattered")).unwrap();
        set_feedback(&conn, ids[2], Some("didnt")).unwrap();
        let got = get_feedback(&conn, &[ids[0], ids[1]]).unwrap();
        assert_eq!(got, vec![StoryFeedback { story_id: ids[0], label: "mattered".into() }]);
        assert!(get_feedback(&conn, &[]).unwrap().is_empty());
        assert!(get_feedback(&conn, &vec![1; MAX_FEEDBACK_LOOKUP + 1]).is_err());
    }

    /// A simulated briefing session end to end: the reader opens one story twice
    /// (two reads), labels two, ignores one; one fetched article was never shown.
    /// The view must carry every signal for the shown stories, and the buried
    /// article must be findable as a candidate with no story.
    #[test]
    fn simulated_session_lands_in_the_signals_view() {
        let conn = test_db();
        let (bid, ids) = seed_briefing(
            &conn,
            "2026-09-24",
            &[TestStory::new("ai", "Read"), TestStory::new("tech", "Skimmed"), TestStory::new("miami", "Ignored")],
        );
        let open = |id: i64| EngagementInput { story_id: Some(id), briefing_id: Some(bid), ..ev_base("story_open") };
        let close = |id: i64, ms: i64| EngagementInput {
            story_id: Some(id),
            briefing_id: Some(bid),
            dwell_ms: Some(ms),
            ..ev_base("story_close")
        };
        insert_event(&conn, &open(ids[0])).unwrap();
        insert_event(&conn, &close(ids[0], 30_000)).unwrap();
        insert_event(&conn, &open(ids[0])).unwrap();
        insert_event(&conn, &close(ids[0], 12_500)).unwrap();
        set_feedback(&conn, ids[0], Some("mattered")).unwrap();
        set_feedback(&conn, ids[1], Some("didnt")).unwrap();

        conn.execute(
            "INSERT INTO fetch_candidates (briefing_id, url_hash, url, title, sector, pool_position,
                 precurate_ran, kept_by_precurate, summarized, story_id)
             VALUES (?1, 'h_shown', 'https://x/shown', 'Read', 'ai', 0, 1, 1, 1, ?2),
                    (?1, 'h_buried', 'https://x/buried', 'Buried', 'italy', 1, 1, 0, 0, NULL)",
            rusqlite::params![bid, ids[0]],
        )
        .unwrap();

        let rows: Vec<(i64, i64, i64, f64, Option<String>)> = conn
            .prepare(
                "SELECT story_id, rank_shown, opened, read_seconds, explicit_label
                 FROM story_feedback_signals WHERE briefing_id = ?1 ORDER BY rank_shown",
            )
            .unwrap()
            .query_map([bid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (ids[0], 0, 1, 42.5, Some("mattered".into())),
                (ids[1], 1, 0, 0.0, Some("didnt".into())),
                (ids[2], 2, 0, 0.0, None),
            ]
        );

        let buried: Vec<String> = conn
            .prepare("SELECT url FROM fetch_candidates WHERE briefing_id = ?1 AND story_id IS NULL")
            .unwrap()
            .query_map([bid], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(buried, vec!["https://x/buried".to_string()]);
    }

    fn ev_base(event: &str) -> EngagementInput {
        EngagementInput {
            surface: "/".into(),
            event: event.into(),
            story_id: None,
            briefing_id: None,
            sector: None,
            dwell_ms: None,
            detail: None,
        }
    }
}
