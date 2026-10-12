//! The Calendar page: upcoming earnings for the stocks Pulse follows and the
//! macro dates that move the whole market. Earnings come from the
//! `earnings_calendar` table the fetcher fills; macro dates from `pulse-calendar`.

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use tauri::State;

use crate::db::DbState;

/// Reports from this many days back stay listed ("reported 2 days ago").
const LOOKBACK_DAYS: i64 = 2;
const DEFAULT_DAYS: i64 = 30;
const MAX_DAYS: i64 = 90;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EarningsEvent {
    pub ticker: String,
    pub name: Option<String>,
    pub date: String,
    /// `bmo` before the open, `amc` after the close, `dmh` during hours.
    pub hour: Option<String>,
    pub eps_estimate: Option<f64>,
    pub revenue_estimate: Option<f64>,
    /// Negative once reported.
    pub days_until: i64,
    pub held: bool,
    /// Inside the window where the auto-trader will not open a position.
    pub blackout: bool,
    /// Latest cross-signal score over the last 3 days, if the ticker has one.
    pub signal_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MacroEventView {
    #[serde(flatten)]
    pub event: pulse_calendar::MacroEvent,
    pub days_until: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalendarData {
    pub today: String,
    pub days: i64,
    pub earnings: Vec<EarningsEvent>,
    pub macro_events: Vec<MacroEventView>,
    /// False when the fetcher has never filled the table (no Finnhub key, or
    /// it has not run yet): the page then says so instead of showing nothing.
    pub has_earnings_data: bool,
    pub earnings_updated_at: Option<String>,
    /// Held tickers reporting inside the blackout window.
    pub held_in_blackout: Vec<String>,
}

fn names(conn: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare(
        "SELECT UPPER(t.ticker), MIN(e.name) FROM entity_tickers t
         JOIN entities e ON e.id = t.entity_id WHERE t.is_public = 1 GROUP BY UPPER(t.ticker)",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

fn held(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT UPPER(ticker) FROM paper_trades WHERE status = 'open'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

fn scores(conn: &Connection, today: NaiveDate) -> rusqlite::Result<HashMap<String, f64>> {
    let mut stmt = conn.prepare(
        // Most rows carry only an entity id; the ticker comes from `entity_tickers`.
        "SELECT UPPER(COALESCE(et.ticker, cs.ticker)), MAX(cs.compound_score) FROM cross_signals cs
         LEFT JOIN entity_tickers et ON et.entity_id = cs.entity_id
         WHERE COALESCE(et.ticker, cs.ticker) IS NOT NULL AND date(cs.computed_at) >= date(?1, '-3 days')
         GROUP BY UPPER(COALESCE(et.ticker, cs.ticker))",
    )?;
    let rows = stmt.query_map([today.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (t, s) = row?;
        if let Some(s) = s.filter(|s| s.is_finite()) {
            out.insert(t, s);
        }
    }
    Ok(out)
}

pub fn build_calendar(conn: &Connection, today: NaiveDate, days: i64) -> Result<CalendarData, String> {
    let days = days.clamp(1, MAX_DAYS);
    let from = today - chrono::Duration::days(LOOKBACK_DAYS);
    let to = today + chrono::Duration::days(days);
    let names = names(conn).map_err(|e| e.to_string())?;
    let held = held(conn).map_err(|e| e.to_string())?;
    let scores = scores(conn, today).map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT ticker, report_date, hour, eps_estimate, revenue_estimate FROM earnings_calendar
             WHERE report_date >= ?1 AND report_date <= ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([from.to_string(), to.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut earnings = Vec::new();
    for row in rows {
        let (ticker, date, hour, eps, revenue) = row.map_err(|e| e.to_string())?;
        let Ok(report_date) = date.parse::<NaiveDate>() else { continue };
        let key = ticker.to_uppercase();
        earnings.push(EarningsEvent {
            name: names.get(&key).cloned(),
            held: held.contains(&key),
            blackout: pulse_calendar::in_earnings_blackout(report_date, today),
            signal_score: scores.get(&key).copied(),
            days_until: (report_date - today).num_days(),
            hour: hour.filter(|h| !h.is_empty()),
            eps_estimate: eps.filter(|v| v.is_finite()),
            revenue_estimate: revenue.filter(|v| v.is_finite()),
            date,
            ticker,
        });
    }
    // Soonest first; within a day, held names, then stronger signals, then A-Z.
    earnings.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(b.held.cmp(&a.held))
            .then(
                b.signal_score
                    .unwrap_or(f64::NEG_INFINITY)
                    .partial_cmp(&a.signal_score.unwrap_or(f64::NEG_INFINITY))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.ticker.cmp(&b.ticker))
    });

    let mut held_in_blackout: Vec<String> =
        earnings.iter().filter(|e| e.held && e.blackout).map(|e| e.ticker.clone()).collect();
    held_in_blackout.dedup();

    let (count, updated): (i64, Option<String>) = conn
        .query_row("SELECT COUNT(*), MAX(fetched_at) FROM earnings_calendar", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?;

    let macro_events = pulse_calendar::macro_events(today - chrono::Duration::days(LOOKBACK_DAYS), to)
        .into_iter()
        .map(|event| {
            let days_until = event.date.parse::<NaiveDate>().map(|d| (d - today).num_days()).unwrap_or(0);
            MacroEventView { event, days_until }
        })
        .collect();

    Ok(CalendarData {
        today: today.to_string(),
        days,
        earnings,
        macro_events,
        has_earnings_data: count > 0,
        earnings_updated_at: updated,
        held_in_blackout,
    })
}

/// Earnings and macro dates from two days ago through the next `days` (default
/// 30, at most 90). Reads only the local DB.
#[tauri::command]
pub fn get_calendar(db: State<'_, DbState>, days: Option<i64>) -> Result<CalendarData, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    build_calendar(&conn, chrono::Local::now().date_naive(), days.unwrap_or(DEFAULT_DAYS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::initialize_in_memory;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn setup() -> Connection {
        let conn = initialize_in_memory().unwrap();
        conn.execute_batch(
            "INSERT INTO entities (id, name, name_normalized, entity_type, first_seen, last_seen) VALUES
               (1, 'Apple Inc', 'apple inc', 'company', '2026-01-01', '2026-10-01'),
               (2, 'Microsoft', 'microsoft', 'company', '2026-01-01', '2026-10-01');
             INSERT INTO entity_tickers (entity_id, ticker) VALUES (1, 'AAPL'), (2, 'MSFT');
             INSERT INTO paper_trades (entity_id, ticker, direction, entry_price, entry_date, position_size, confidence, signal_profile, status)
               VALUES (1, 'AAPL', 'long', 200, '2026-10-01', 1000, 0.5, '{}', 'open');
             INSERT INTO earnings_calendar (ticker, report_date, hour, eps_estimate) VALUES
               ('MSFT', '2026-10-13', 'amc', 3.1), ('AAPL', '2026-10-13', 'bmo', 1.6),
               ('AAPL', '2026-10-09', NULL, NULL), ('AAPL', '2026-12-30', NULL, NULL),
               ('AAPL', '2026-10-30', 'amc', 1.8);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn lists_the_window_soonest_first_with_held_names_ahead() {
        let cal = build_calendar(&setup(), d("2026-10-11"), 30).unwrap();
        let got: Vec<(&str, &str)> = cal.earnings.iter().map(|e| (e.date.as_str(), e.ticker.as_str())).collect();
        // 10-09 is two days back (kept), 12-30 is beyond 30 days (dropped); AAPL is held so it leads on 10-13.
        assert_eq!(got, vec![("2026-10-09", "AAPL"), ("2026-10-13", "AAPL"), ("2026-10-13", "MSFT"), ("2026-10-30", "AAPL")]);
        assert_eq!(cal.earnings[0].days_until, -2);
        assert_eq!(cal.earnings[1].name.as_deref(), Some("Apple Inc"));
        assert!(cal.has_earnings_data);
    }

    #[test]
    fn flags_held_names_inside_the_blackout() {
        let cal = build_calendar(&setup(), d("2026-10-11"), 30).unwrap();
        let aapl = cal.earnings.iter().find(|e| e.ticker == "AAPL" && e.date == "2026-10-13").unwrap();
        assert!(aapl.held && aapl.blackout);
        let msft = cal.earnings.iter().find(|e| e.ticker == "MSFT").unwrap();
        assert!(!msft.held && msft.blackout, "blackout is about the date, held is separate");
        assert_eq!(cal.held_in_blackout, vec!["AAPL"]);
        let later = cal.earnings.iter().find(|e| e.date == "2026-10-30").unwrap();
        assert!(!later.blackout);
        let past = cal.earnings.iter().find(|e| e.date == "2026-10-09").unwrap();
        assert!(!past.blackout, "already reported");
    }

    #[test]
    fn macro_events_come_from_the_table_and_know_how_far_away_they_are() {
        let cal = build_calendar(&setup(), d("2026-10-11"), 30).unwrap();
        let cpi = cal.macro_events.iter().find(|e| e.event.date == "2026-10-14").unwrap();
        assert_eq!(cpi.days_until, 3);
        assert!(cal.macro_events.iter().any(|e| e.event.date == "2026-10-28"), "FOMC decision");
        let json = serde_json::to_value(cpi).unwrap();
        assert_eq!(json["date"], "2026-10-14", "event fields are flattened");
        assert_eq!(json["kind"], "cpi");
    }

    #[test]
    fn an_empty_table_says_so_but_still_lists_macro_dates() {
        let conn = initialize_in_memory().unwrap();
        let cal = build_calendar(&conn, d("2026-10-11"), 30).unwrap();
        assert!(!cal.has_earnings_data);
        assert!(cal.earnings_updated_at.is_none());
        assert!(!cal.macro_events.is_empty());
    }

    #[test]
    fn days_are_clamped() {
        let conn = setup();
        assert_eq!(build_calendar(&conn, d("2026-10-11"), 9999).unwrap().days, MAX_DAYS);
        assert_eq!(build_calendar(&conn, d("2026-10-11"), -5).unwrap().days, 1);
    }

    #[test]
    fn signal_score_finds_tickers_through_the_entity_and_counts_from_today() {
        let conn = setup();
        conn.execute_batch(
            "INSERT INTO cross_signals (entity_id, ticker, compound_score, computed_at) VALUES
               (2, NULL, 0.45, '2026-10-10 07:00:00'), (1, 'AAPL', 0.3, '2026-10-11 07:00:00'),
               (2, NULL, 0.9, '2026-10-01 07:00:00');",
        )
        .unwrap();
        let cal = build_calendar(&conn, d("2026-10-11"), 30).unwrap();
        let score = |t: &str| cal.earnings.iter().find(|e| e.ticker == t).and_then(|e| e.signal_score);
        assert_eq!(score("MSFT"), Some(0.45), "entity-only row, and the 10-01 row is outside the 3 days");
        assert_eq!(score("AAPL"), Some(0.3));
    }
}
