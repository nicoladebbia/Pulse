//! The prediction-market panel of the briefing page: the fetcher's latest
//! reading (`prediction_markets`, migration 049), read as stored. What is
//! listed and in what order is decided when it is fetched
//! (`crates/pulse-fetcher/src/prediction_markets.rs`).

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::Serialize;
use tauri::State;

use crate::db::DbState;

/// A reading from before yesterday is not shown: the panel speaks of the last
/// 24 hours, and odds that old are no longer what the market says. (The
/// fetcher reads every few hours, so an older one means it has not run.)
pub const MAX_AGE_DAYS: i64 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PredictionMarket {
    pub market_id: String,
    pub topic: String,
    pub question: String,
    pub event_title: Option<String>,
    pub url: Option<String>,
    /// Chance of "Yes", 0..1.
    pub probability: f64,
    /// Change of that chance in 24 hours (0.05 = five points).
    pub change_1d: Option<f64>,
    pub change_1w: Option<f64>,
    /// Dollars traded in 24 hours.
    pub volume_24h: f64,
    pub end_date: Option<String>,
    /// `mover` or `active`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PredictionMarkets {
    /// Local day of the reading, `YYYY-MM-DD`.
    pub day: String,
    /// Local time of the reading, `YYYY-MM-DDTHH:MM:SS`.
    pub fetched_at: String,
    pub source: String,
    /// In display order: the questions that moved, then the most traded.
    pub markets: Vec<PredictionMarket>,
}

/// The newest reading, if it is from `today` or the `MAX_AGE_DAYS` before it.
pub fn latest(conn: &Connection, today: NaiveDate) -> Result<Option<PredictionMarkets>, String> {
    let oldest = (today - chrono::Duration::days(MAX_AGE_DAYS)).to_string();
    let day: Option<String> = conn
        .query_row("SELECT MAX(day) FROM prediction_markets WHERE day >= ?1 AND day <= ?2", [&oldest, &today.to_string()], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let Some(day) = day else {
        return Ok(None);
    };
    let mut stmt = conn
        .prepare(
            "SELECT market_id, topic, question, event_title, url, probability, change_1d, change_1w,
                    volume_24h, end_date, kind, fetched_at, source
             FROM prediction_markets WHERE day = ?1 ORDER BY rank, market_id",
        )
        .map_err(|e| e.to_string())?;
    let mut fetched_at = String::new();
    let mut source = String::new();
    let rows = stmt
        .query_map([&day], |r| {
            Ok((
                PredictionMarket {
                    market_id: r.get(0)?,
                    topic: r.get(1)?,
                    question: r.get(2)?,
                    event_title: r.get(3)?,
                    url: r.get(4)?,
                    probability: r.get(5)?,
                    change_1d: r.get(6)?,
                    change_1w: r.get(7)?,
                    volume_24h: r.get(8)?,
                    end_date: r.get(9)?,
                    kind: r.get(10)?,
                },
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut markets = Vec::new();
    for row in rows {
        let (market, at, from) = row.map_err(|e| e.to_string())?;
        if at > fetched_at {
            fetched_at = at;
        }
        source = from;
        markets.push(market);
    }
    if markets.is_empty() {
        return Ok(None);
    }
    Ok(Some(PredictionMarkets { day, fetched_at, source, markets }))
}

/// What prediction markets said at the last reading, or None when there is no
/// recent one (the panel is then left out). Reads only the local DB.
#[tauri::command]
pub fn get_prediction_markets(db: State<'_, DbState>) -> Result<Option<PredictionMarkets>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    latest(&conn, chrono::Local::now().date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::test_db;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn add(conn: &Connection, day: &str, id: &str, kind: &str, rank: i64, change: Option<f64>) {
        conn.execute(
            "INSERT INTO prediction_markets
               (day, market_id, topic, question, event_title, url, probability, change_1d, volume_24h, kind, rank, fetched_at)
             VALUES (?1, ?2, 'economy', ?3, NULL, NULL, 0.4, ?4, 25000.0, ?5, ?6, ?7)",
            rusqlite::params![day, id, format!("Question {id}?"), change, kind, rank, format!("{day}T08:00:00")],
        )
        .unwrap();
    }

    #[test]
    fn nothing_stored_is_no_panel() {
        let conn = test_db();
        assert_eq!(latest(&conn, d("2026-11-19")).unwrap(), None);
    }

    #[test]
    fn the_newest_day_comes_back_in_stored_order() {
        let conn = test_db();
        add(&conn, "2026-11-18", "old", "mover", 1, Some(0.3));
        add(&conn, "2026-11-19", "second", "active", 2, None);
        add(&conn, "2026-11-19", "first", "mover", 1, Some(-0.12));
        let got = latest(&conn, d("2026-11-19")).unwrap().unwrap();
        assert_eq!(got.day, "2026-11-19");
        assert_eq!(got.fetched_at, "2026-11-19T08:00:00");
        assert_eq!(got.source, "polymarket");
        assert_eq!(got.markets.iter().map(|m| m.market_id.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
        let first = &got.markets[0];
        assert_eq!((first.kind.as_str(), first.change_1d, first.probability, first.volume_24h), ("mover", Some(-0.12), 0.4, 25000.0));
        // Columns that may be NULL come back as None, not as an error.
        let second = &got.markets[1];
        assert_eq!((second.change_1d, second.change_1w, &second.event_title, &second.url, &second.end_date), (None, None, &None, &None, &None));
    }

    #[test]
    fn yesterdays_reading_shows_and_an_older_one_does_not() {
        let conn = test_db();
        add(&conn, "2026-11-17", "stale", "mover", 1, Some(0.3));
        assert_eq!(latest(&conn, d("2026-11-19")).unwrap(), None);
        add(&conn, "2026-11-18", "recent", "mover", 1, Some(0.3));
        assert_eq!(latest(&conn, d("2026-11-19")).unwrap().unwrap().day, "2026-11-18");
    }

    #[test]
    fn a_reading_dated_after_today_is_ignored() {
        let conn = test_db();
        add(&conn, "2026-11-19", "today", "mover", 1, Some(0.3));
        add(&conn, "2026-11-25", "future", "mover", 1, Some(0.3));
        assert_eq!(latest(&conn, d("2026-11-19")).unwrap().unwrap().day, "2026-11-19");
    }
}
