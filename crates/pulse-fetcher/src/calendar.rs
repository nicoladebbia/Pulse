//! Upcoming earnings reports for the tickers Pulse follows (table
//! `earnings_calendar`, shown on the Calendar page).
//!
//! One call to Finnhub's earnings calendar covers every company, so the whole
//! refresh costs a single request. Only tickers Pulse tracks or holds are kept.
//! Future rows of a ticker are replaced each time because report dates move.
//! Refreshed at most every `REFRESH_HOURS`, so every scheduled run can call
//! `refresh_if_stale` without spending the rate limit.

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use std::collections::HashSet;

/// How far ahead to look.
pub const WINDOW_DAYS: i64 = 60;
/// Skip the call when the table was filled more recently than this.
pub const REFRESH_HOURS: i64 = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct EarningsRow {
    pub ticker: String,
    pub date: NaiveDate,
    /// `bmo` (before open), `amc` (after close), `dmh` (during hours), or empty.
    pub hour: Option<String>,
    pub eps_estimate: Option<f64>,
    pub revenue_estimate: Option<f64>,
}

pub fn parse_rows(body: &serde_json::Value) -> Vec<EarningsRow> {
    body.get("earningsCalendar")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let ticker = e.get("symbol")?.as_str()?.trim().to_uppercase();
            let date = e.get("date")?.as_str()?.parse::<NaiveDate>().ok()?;
            if ticker.is_empty() {
                return None;
            }
            let hour = e.get("hour").and_then(|v| v.as_str()).filter(|h| !h.is_empty()).map(str::to_string);
            let num = |k: &str| e.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite());
            Some(EarningsRow { ticker, date, hour, eps_estimate: num("epsEstimate"), revenue_estimate: num("revenueEstimate") })
        })
        .collect()
}

/// Tickers worth keeping: every public entity Pulse maps plus anything held.
pub fn followed_tickers(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT UPPER(ticker) FROM entity_tickers WHERE is_public = 1
         UNION SELECT UPPER(ticker) FROM paper_trades WHERE status = 'open'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Replace the future rows of the followed tickers with `rows`. Returns the
/// number of rows stored.
pub fn store(conn: &Connection, rows: &[EarningsRow], followed: &HashSet<String>, today: NaiveDate) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM earnings_calendar WHERE report_date >= ?1", params![today.to_string()])?;
    // Past reports stay for a while so the page can say "reported 2 days ago".
    tx.execute("DELETE FROM earnings_calendar WHERE report_date < ?1", params![(today - chrono::Duration::days(30)).to_string()])?;
    let mut n = 0;
    for r in rows.iter().filter(|r| followed.contains(&r.ticker)) {
        tx.execute(
            "INSERT INTO earnings_calendar (ticker, report_date, hour, eps_estimate, revenue_estimate, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
             ON CONFLICT(ticker, report_date) DO UPDATE SET
               hour = excluded.hour, eps_estimate = excluded.eps_estimate,
               revenue_estimate = excluded.revenue_estimate, fetched_at = excluded.fetched_at",
            params![r.ticker, r.date.to_string(), r.hour, r.eps_estimate, r.revenue_estimate],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

fn fresh(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM earnings_calendar WHERE fetched_at >= datetime('now', ?1)",
        [format!("-{REFRESH_HOURS} hours")],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Fetch and store the calendar unless it was refreshed in the last
/// `REFRESH_HOURS`. Without `FINNHUB_API_KEY` this does nothing (the page then
/// shows macro events only). Returns the rows stored, or None when skipped.
pub async fn refresh_if_stale(db_path: &std::path::Path) -> anyhow::Result<Option<usize>> {
    let key = std::env::var("FINNHUB_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        return Ok(None);
    }
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    if fresh(&conn) {
        return Ok(None);
    }
    let today = chrono::Local::now().date_naive();
    let to = today + chrono::Duration::days(WINDOW_DAYS);
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let resp = client
        .get("https://finnhub.io/api/v1/calendar/earnings")
        .query(&[("from", today.to_string()), ("to", to.to_string()), ("token", key.trim().to_string())])
        .send()
        .await?;
    crate::db::log_fetch_calls(&conn, "finnhub", "earnings_calendar", 1);
    if !resp.status().is_success() {
        anyhow::bail!("earnings calendar returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let rows = parse_rows(&body);
    // A reply with no rows at all is a failed fetch, not an empty calendar:
    // keep what we have instead of wiping it.
    if rows.is_empty() {
        anyhow::bail!("earnings calendar came back empty");
    }
    let followed = followed_tickers(&conn)?;
    let n = store(&conn, &rows, &followed, today)?;
    tracing::info!("Earnings calendar: {n} reports stored ({} in the feed)", rows.len());
    Ok(Some(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../../migrations/047_earnings_calendar.sql")).unwrap();
        c
    }

    #[test]
    fn parses_hour_and_estimates_and_skips_junk() {
        let body = serde_json::json!({"earningsCalendar": [
            {"symbol": "aapl", "date": "2026-10-29", "hour": "amc", "epsEstimate": 1.5, "revenueEstimate": 9.0e10},
            {"symbol": "MSFT", "date": "2026-10-28", "hour": "", "epsEstimate": null},
            {"symbol": "", "date": "2026-10-28"},
            {"symbol": "BAD", "date": "not-a-date"},
            {"date": "2026-10-28"}
        ]});
        let rows = parse_rows(&body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ticker, "AAPL");
        assert_eq!(rows[0].hour.as_deref(), Some("amc"));
        assert_eq!(rows[0].eps_estimate, Some(1.5));
        assert_eq!(rows[1].hour, None);
        assert_eq!(rows[1].eps_estimate, None);
        assert!(parse_rows(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn keeps_only_followed_tickers_and_replaces_future_rows() {
        let c = conn();
        let followed: HashSet<String> = ["AAPL".to_string()].into();
        let row = |t: &str, day: &str| EarningsRow { ticker: t.into(), date: d(day), hour: None, eps_estimate: None, revenue_estimate: None };
        let today = d("2026-10-11");
        assert_eq!(store(&c, &[row("AAPL", "2026-10-29"), row("ZZZ", "2026-10-29")], &followed, today).unwrap(), 1);
        // The date moved: the old row must not linger.
        store(&c, &[row("AAPL", "2026-11-02")], &followed, today).unwrap();
        let dates: Vec<String> = c.prepare("SELECT report_date FROM earnings_calendar").unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(dates, vec!["2026-11-02"]);
    }

    #[test]
    fn old_reports_are_pruned_after_30_days_but_recent_ones_stay() {
        let c = conn();
        let today = d("2026-10-11");
        c.execute("INSERT INTO earnings_calendar (ticker, report_date) VALUES ('OLD', '2026-08-01'), ('NEW', '2026-10-08')", []).unwrap();
        store(&c, &[], &HashSet::new(), today).unwrap();
        let left: Vec<String> = c.prepare("SELECT ticker FROM earnings_calendar").unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(left, vec!["NEW"]);
    }

    #[test]
    fn freshness_gate() {
        let c = conn();
        assert!(!fresh(&c));
        c.execute("INSERT INTO earnings_calendar (ticker, report_date) VALUES ('A', '2026-10-29')", []).unwrap();
        assert!(fresh(&c));
        c.execute("UPDATE earnings_calendar SET fetched_at = datetime('now', '-7 hours')", []).unwrap();
        assert!(!fresh(&c));
    }
}
