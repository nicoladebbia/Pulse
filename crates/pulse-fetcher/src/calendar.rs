//! Upcoming earnings reports for the tickers Pulse follows (table
//! `earnings_calendar`, shown on the Calendar page).
//!
//! Finnhub's earnings calendar covers every company but returns at most
//! `ROW_CAP` rows per request and does not say when it cut the list (a busy
//! week has more), so the range is fetched in windows and any window that comes
//! back full is split until it fits (a full response is missing its earliest
//! days). Only tickers Pulse tracks or holds are kept.
//! Future rows of a ticker are replaced each time because report dates move.
//! Refreshed at most every `REFRESH_HOURS`, so every scheduled run can call
//! `refresh_if_stale` without spending the rate limit.

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::future::Future;

/// How far ahead to look.
pub const WINDOW_DAYS: i64 = 60;
/// Rows Finnhub returns per request at most. Measured 2026-10: a 60-day range
/// and each busy 7-day range all came back as exactly this many. A full
/// response is missing its EARLIEST days (10-28..11-07 came back as
/// 11-02..11-07), which is the end the earnings blackout cares about.
pub const ROW_CAP: usize = 1500;
/// First window size; a window that comes back full is halved.
const FIRST_WINDOW_DAYS: i64 = 15;
/// Requests per refresh at most (the free tier allows 60 a minute).
const MAX_CALLS: usize = 40;
/// Pause between requests so the fetcher's other Finnhub calls keep their share.
const CALL_GAP: std::time::Duration = std::time::Duration::from_millis(400);
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

/// One response: the rows that parsed, and how many entries the feed sent. The
/// cap check needs the second number, since an entry without a usable symbol or
/// date is dropped from the first.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub rows: Vec<EarningsRow>,
    pub raw: usize,
}

pub fn parse_page(body: &serde_json::Value) -> Page {
    let raw = body.get("earningsCalendar").and_then(|v| v.as_array()).map_or(0, Vec::len);
    Page { rows: parse_rows(body), raw }
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

/// What `collect` got: the rows, the days that were still full at a single day
/// (rows for that day may be missing), and why it stopped early, if it did.
pub struct Collected {
    pub rows: Vec<EarningsRow>,
    pub full_days: Vec<NaiveDate>,
    pub error: Option<anyhow::Error>,
}

/// Fetch `from..=to` through `fetch`, splitting any window that returns `cap`
/// entries, since the feed may have cut it short. Windows go earliest first, so
/// when a request fails the rows already in `rows` cover the days up to the
/// failure; the caller decides whether a partial calendar is good enough (the
/// stored calendar is not replaced by one, the trader's blackout uses it).
pub async fn collect<F, Fut>(from: NaiveDate, to: NaiveDate, cap: usize, mut fetch: F) -> Collected
where
    F: FnMut(NaiveDate, NaiveDate) -> Fut,
    Fut: Future<Output = anyhow::Result<Page>>,
{
    let mut windows: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    let mut start = from;
    while start <= to {
        let end = (start + chrono::Duration::days(FIRST_WINDOW_DAYS - 1)).min(to);
        windows.push((start, end));
        start = end + chrono::Duration::days(1);
    }
    windows.reverse();

    let mut out = Collected { rows: Vec::new(), full_days: Vec::new(), error: None };
    let mut calls = 0;
    while let Some((a, b)) = windows.pop() {
        if calls >= MAX_CALLS {
            out.error = Some(anyhow::anyhow!("earnings calendar needed more than {MAX_CALLS} requests"));
            break;
        }
        calls += 1;
        let page = match fetch(a, b).await {
            Ok(p) => p,
            Err(e) => {
                out.error = Some(e);
                break;
            }
        };
        if page.raw >= cap && a < b {
            let mid = a + chrono::Duration::days((b - a).num_days() / 2);
            windows.push((mid + chrono::Duration::days(1), b));
            windows.push((a, mid));
            continue;
        }
        if page.raw >= cap {
            out.full_days.push(a);
        }
        out.rows.extend(page.rows);
    }
    out
}

pub(crate) async fn fetch_window(client: &reqwest::Client, key: &str, from: NaiveDate, to: NaiveDate) -> anyhow::Result<Page> {
    tokio::time::sleep(CALL_GAP).await;
    let resp = client
        .get("https://finnhub.io/api/v1/calendar/earnings")
        .query(&[("from", from.to_string()), ("to", to.to_string()), ("token", key.to_string())])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("earnings calendar returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    Ok(parse_page(&body))
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
    // Nothing to keep (no tracked tickers yet): leave the table alone.
    let followed = followed_tickers(&conn)?;
    if followed.is_empty() {
        return Ok(None);
    }
    let today = chrono::Local::now().date_naive();
    let to = today + chrono::Duration::days(WINDOW_DAYS);
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let calls = std::cell::Cell::new(0u32);
    let result = collect(today, to, ROW_CAP, |a, b| {
        calls.set(calls.get() + 1);
        fetch_window(&client, key.trim(), a, b)
    })
    .await;
    crate::db::log_fetch_calls(&conn, "finnhub", "earnings_calendar", calls.get() as _);
    // The stored calendar is replaced as a whole, so a partial fetch keeps it.
    let Collected { rows, full_days, error } = result;
    if let Some(e) = error {
        return Err(e);
    }
    // A reply with no rows at all is a failed fetch, not an empty calendar:
    // keep what we have instead of wiping it.
    if rows.is_empty() {
        anyhow::bail!("earnings calendar came back empty");
    }
    if !full_days.is_empty() {
        tracing::warn!("Earnings calendar: {} day(s) returned a full page, some reports may be missing: {:?}", full_days.len(), full_days);
    }
    let n = store(&conn, &rows, &followed, today)?;
    tracing::info!("Earnings calendar: {n} reports stored ({} in the feed, {} request(s))", rows.len(), calls.get());
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

    fn row(t: &str, day: NaiveDate) -> EarningsRow {
        EarningsRow { ticker: t.into(), date: day, hour: None, eps_estimate: None, revenue_estimate: None }
    }

    /// A feed with `per_day(date)` reports a day that returns the first `cap` rows of a window, as Finnhub does.
    fn feed(per_day: impl Fn(NaiveDate) -> usize, cap: usize) -> impl FnMut(NaiveDate, NaiveDate) -> std::future::Ready<anyhow::Result<Page>> {
        move |a, b| {
            let mut rows = Vec::new();
            let mut day = a;
            while day <= b {
                rows.extend((0..per_day(day)).map(|i| row(&format!("T{i}"), day)));
                day += chrono::Duration::days(1);
            }
            rows.truncate(cap);
            let raw = rows.len();
            std::future::ready(Ok(Page { rows, raw }))
        }
    }

    #[tokio::test]
    async fn a_full_window_is_split_until_nothing_is_cut() {
        let busy = d("2026-10-25");
        // 1 report a day, 4 on the busy day; the cap of 5 cuts any window holding it plus a couple of days.
        let per_day = move |day: NaiveDate| if day == busy { 4 } else { 1 };
        let Collected { rows, full_days: full, error } = collect(d("2026-10-11"), d("2026-12-09"), 5, feed(per_day, 5)).await;
        assert!(error.is_none());
        assert_eq!(rows.len(), 59 + 4, "60 days: 59 with one report, the busy day with four");
        assert!(full.is_empty());
        let on_busy = rows.iter().filter(|r| r.date == busy).count();
        assert_eq!(on_busy, 4);
    }

    #[tokio::test]
    async fn a_single_day_that_is_still_full_is_reported_not_lost() {
        let busy = d("2026-10-25");
        let per_day = move |day: NaiveDate| if day == busy { 9 } else { 1 };
        let Collected { rows, full_days: full, .. } = collect(d("2026-10-11"), d("2026-11-09"), 5, feed(per_day, 5)).await;
        assert_eq!(full, vec![busy]);
        assert_eq!(rows.iter().filter(|r| r.date == busy).count(), 5, "what the feed gave for that day is kept");
    }

    #[tokio::test]
    async fn no_duplicates_across_windows_and_one_request_per_quiet_window() {
        let calls = std::cell::Cell::new(0);
        let mut inner = feed(|_| 1, 1500);
        let Collected { rows, .. } = collect(d("2026-10-11"), d("2026-12-09"), 1500, |a, b| {
            calls.set(calls.get() + 1);
            inner(a, b)
        })
        .await;
        assert_eq!(rows.len(), 60);
        assert_eq!(calls.get(), 4, "60 days in 15-day windows");
        let dates: HashSet<NaiveDate> = rows.iter().map(|r| r.date).collect();
        assert_eq!(dates.len(), 60);
    }

    #[tokio::test]
    async fn a_failed_request_stops_the_fetch_but_keeps_the_earlier_days() {
        let mut inner = feed(|_| 1, 1500);
        let got = collect(d("2026-10-11"), d("2026-12-09"), 1500, |a, b| {
            if a > d("2026-11-09") {
                return std::future::ready(Err(anyhow::anyhow!("429")));
            }
            inner(a, b)
        })
        .await;
        assert!(got.error.is_some());
        assert_eq!(got.rows.len(), 30, "the first two 15-day windows arrived before the failure");
        assert!(got.rows.iter().all(|r| r.date <= d("2026-11-09")));
    }

    #[tokio::test]
    async fn too_many_requests_stop_it() {
        // Every day over the cap: it would split down to single days, far past the request limit.
        let got = collect(d("2026-10-11"), d("2026-12-09"), 1, feed(|_| 3, 1)).await;
        let err = got.error.expect("stopped").to_string();
        assert!(err.contains("more than"), "{err}");
    }

    #[tokio::test]
    async fn the_cap_counts_what_the_feed_sent_not_what_parsed() {
        // 5 entries came back (the cap), but one has no symbol and is dropped: the window was still cut.
        let body = serde_json::json!({"earningsCalendar": [
            {"symbol": "A", "date": "2026-10-20"}, {"symbol": "B", "date": "2026-10-20"},
            {"symbol": "C", "date": "2026-10-20"}, {"symbol": "D", "date": "2026-10-20"},
            {"symbol": "", "date": "2026-10-20"}]});
        let page = parse_page(&body);
        assert_eq!((page.rows.len(), page.raw), (4, 5));
        let calls = std::cell::Cell::new(0);
        let got = collect(d("2026-10-11"), d("2026-10-12"), 5, |_, _| {
            calls.set(calls.get() + 1);
            std::future::ready(Ok(page.clone()))
        })
        .await;
        assert!(calls.get() > 1, "a full window is split even though only 4 rows parsed");
        assert!(got.error.is_none());
    }
}
