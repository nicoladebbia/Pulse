//! How the bot did against simply holding the S&P 500 (SPY): per trade, over
//! all closed trades, and for the whole account since its first snapshot.
//!
//! SPY prices come from the trade row when the fetcher stamped them
//! (`spy_entry_price` / `spy_exit_price`, migration 040), else from daily bars
//! by the time of day the trade happened.

use pulse_alpaca::DailyBar;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TradeVsSpy {
    pub trade_id: i64,
    /// SPY's return over the same span, in %.
    pub spy_pct: f64,
    /// The trade's return minus SPY's, in percentage points.
    pub excess_pct: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EquityPoint {
    pub date: String,
    pub equity: f64,
    /// The first snapshot's equity, held in SPY instead.
    pub spy_equity: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Benchmark {
    pub trades: Vec<TradeVsSpy>,
    pub closed_count: usize,
    pub closed_beat_spy: usize,
    /// Realized $ of the closed trades.
    pub closed_pnl: f64,
    /// What the same dollars would have made in SPY over the same days.
    pub closed_spy_pnl: f64,
    pub open_pnl: f64,
    pub open_spy_pnl: f64,
    pub equity_curve: Vec<EquityPoint>,
    /// Account return since the first snapshot, and SPY's, in %.
    pub account_return_pct: Option<f64>,
    pub spy_return_pct: Option<f64>,
    pub since: Option<String>,
}

/// SPY's price at a trade's local timestamp (`YYYY-MM-DD` or
/// `YYYY-MM-DDTHH:MM[:SS]`). Before the open (or date-only rows, which were
/// queued overnight) the order filled at that day's open; during the session
/// the open or close, whichever is nearer in time; after the close, the next
/// day's open. Trade timestamps are the Mac's local time, read here as New
/// York time: right while the Mac runs on Eastern time (as it does now), off
/// by the zone difference otherwise.
pub fn spy_at(bars: &[DailyBar], when: &str) -> Option<f64> {
    let date = when.get(..10)?;
    let minutes = when
        .get(11..16)
        .and_then(|t| Some(t.get(..2)?.parse::<u32>().ok()? * 60 + t.get(3..5)?.parse::<u32>().ok()?));
    let i = bars.partition_point(|b| b.date.as_str() < date);
    let bar = bars.get(i)?;
    let same_day = bar.date == date;
    match minutes {
        _ if !same_day => Some(bar.open), // weekend/holiday: next session's open
        None => Some(bar.open),
        Some(m) if m < 9 * 60 + 30 => Some(bar.open),
        Some(m) if m < 12 * 60 + 45 => Some(bar.open),
        Some(m) if m < 16 * 60 => Some(bar.close),
        Some(_) => bars.get(i + 1).map(|b| b.open),
    }
}

pub struct TradeRow {
    pub id: i64,
    pub entry_date: String,
    pub exit_date: Option<String>,
    pub size: f64,
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
    pub open: bool,
    pub spy_entry: Option<f64>,
    pub spy_exit: Option<f64>,
}

pub fn load_trades(conn: &Connection) -> rusqlite::Result<Vec<TradeRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, entry_date, exit_date, position_size, COALESCE(realized_pnl, pnl), pnl_pct, status = 'open',
                spy_entry_price, spy_exit_price
         FROM paper_trades
         WHERE status = 'open' OR pnl_pct IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(TradeRow {
            id: r.get(0)?,
            entry_date: r.get(1)?,
            exit_date: r.get(2)?,
            size: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            pnl: r.get(4)?,
            pnl_pct: r.get(5)?,
            open: r.get(6)?,
            spy_entry: r.get(7)?,
            spy_exit: r.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn load_equity(conn: &Connection) -> rusqlite::Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT date, total_value FROM portfolio_snapshots WHERE total_value > 0
         GROUP BY date HAVING id = MAX(id) ORDER BY date",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// `spy_now` prices open trades; without it they are left out.
pub fn compute(trades: &[TradeRow], equity: &[(String, f64)], bars: &[DailyBar], spy_now: Option<f64>) -> Benchmark {
    let mut b = Benchmark::default();
    for t in trades {
        let Some(entry) = t.spy_entry.or_else(|| spy_at(bars, &t.entry_date)).filter(|p| *p > 0.0) else { continue };
        let exit = if t.open {
            spy_now
        } else {
            t.spy_exit.or_else(|| t.exit_date.as_deref().and_then(|d| spy_at(bars, d)))
        };
        let Some(exit) = exit.filter(|p| *p > 0.0) else { continue };
        let spy_pct = (exit / entry - 1.0) * 100.0;
        let spy_pnl = t.size * spy_pct / 100.0;
        if let Some(pct) = t.pnl_pct {
            b.trades.push(TradeVsSpy { trade_id: t.id, spy_pct, excess_pct: pct - spy_pct });
        }
        let pnl = t.pnl.unwrap_or(0.0);
        if t.open {
            b.open_pnl += pnl;
            b.open_spy_pnl += spy_pnl;
        } else {
            b.closed_count += 1;
            b.closed_pnl += pnl;
            b.closed_spy_pnl += spy_pnl;
            if t.pnl_pct.is_some_and(|p| p > spy_pct) {
                b.closed_beat_spy += 1;
            }
        }
    }

    let closes: HashMap<&str, f64> = bars.iter().map(|x| (x.date.as_str(), x.close)).collect();
    // A snapshot on a non-trading day takes the latest close before it.
    let close_on = |d: &str| -> Option<f64> {
        closes.get(d).copied().or_else(|| {
            let i = bars.partition_point(|x| x.date.as_str() <= d);
            i.checked_sub(1).map(|j| bars[j].close)
        })
    };
    if let Some(((first_date, first_eq), spy0)) = equity.first().and_then(|f| close_on(&f.0).map(|s| (f, s))) {
        for (date, eq) in equity {
            if let Some(spy) = close_on(date) {
                b.equity_curve.push(EquityPoint { date: date.clone(), equity: *eq, spy_equity: first_eq * spy / spy0 });
            }
        }
        if let Some(last) = b.equity_curve.last() {
            b.account_return_pct = Some((last.equity / first_eq - 1.0) * 100.0);
            b.spy_return_pct = Some((last.spy_equity / first_eq - 1.0) * 100.0);
            b.since = Some(first_date.clone());
        }
    }
    b
}

/// SPY bars are cached for 15 minutes: the page reloads often and the bars
/// only change once a day.
static CACHE: Mutex<Option<(std::time::Instant, String, Vec<DailyBar>)>> = Mutex::new(None);

async fn spy_bars(client: &reqwest::Client, creds: &pulse_alpaca::Credentials, start: &str) -> Result<Vec<DailyBar>, String> {
    if let Ok(guard) = CACHE.lock()
        && let Some((at, s, bars)) = guard.as_ref()
        && s.as_str() <= start
        && at.elapsed() < std::time::Duration::from_secs(900)
    {
        return Ok(bars.clone());
    }
    let bars = pulse_alpaca::daily_bars(client, creds, "SPY", start).await?;
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((std::time::Instant::now(), start.to_string(), bars.clone()));
    }
    Ok(bars)
}

pub async fn benchmark(trades: Vec<TradeRow>, equity: Vec<(String, f64)>) -> Result<Benchmark, String> {
    let key = std::env::var("ALPACA_API_KEY").unwrap_or_default();
    let secret = std::env::var("ALPACA_SECRET_KEY").unwrap_or_default();
    if key.is_empty() || secret.is_empty() {
        return Err("Alpaca keys are not set".into());
    }
    let creds = pulse_alpaca::Credentials { key, secret };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let earliest = trades
        .iter()
        .filter_map(|t| t.entry_date.get(..10))
        .chain(equity.iter().filter_map(|e| e.0.get(..10)))
        .min()
        .unwrap_or("2026-01-01")
        .to_string();
    let start = chrono::NaiveDate::parse_from_str(&earliest, "%Y-%m-%d")
        .map(|d| (d - chrono::Duration::days(7)).to_string())
        .unwrap_or(earliest);
    let bars = spy_bars(&client, &creds, &start).await?;
    let spy_now = if trades.iter().any(|t| t.open) {
        pulse_alpaca::latest_price(&client, &creds, "SPY").await.ok().flatten()
    } else {
        None
    };
    Ok(compute(&trades, &equity, &bars, spy_now))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: &str, open: f64, close: f64) -> DailyBar {
        DailyBar { date: date.into(), open, high: close, low: open, close, volume: 1.0 }
    }

    fn bars() -> Vec<DailyBar> {
        // Fri 10-02, Mon 10-05, Tue 10-06
        vec![bar("2026-10-02", 100.0, 101.0), bar("2026-10-05", 102.0, 103.0), bar("2026-10-06", 104.0, 105.0)]
    }

    fn trade(id: i64, entry: &str, exit: Option<&str>, pnl_pct: f64, open: bool) -> TradeRow {
        TradeRow {
            id, entry_date: entry.into(), exit_date: exit.map(str::to_string), size: 1000.0,
            pnl: Some(pnl_pct * 10.0), pnl_pct: Some(pnl_pct), open, spy_entry: None, spy_exit: None,
        }
    }

    #[test]
    fn spy_price_follows_the_time_of_day() {
        let b = bars();
        assert_eq!(spy_at(&b, "2026-10-05T00:27:00"), Some(102.0), "overnight order fills at the open");
        assert_eq!(spy_at(&b, "2026-10-05"), Some(102.0));
        assert_eq!(spy_at(&b, "2026-10-05T10:15:00"), Some(102.0));
        assert_eq!(spy_at(&b, "2026-10-05T14:30:00"), Some(103.0));
        assert_eq!(spy_at(&b, "2026-10-05T21:00:00"), Some(104.0), "after the close: next open");
        assert_eq!(spy_at(&b, "2026-10-03T12:00:00"), Some(102.0), "Saturday: Monday's open");
        assert_eq!(spy_at(&b, "2026-10-07"), None);
    }

    #[test]
    fn trades_are_judged_against_spy_over_the_same_days() {
        let b = bars();
        let mut stamped = trade(3, "2026-10-02", Some("2026-10-06"), 1.0, false);
        stamped.spy_entry = Some(200.0);
        stamped.spy_exit = Some(190.0);
        let trades = vec![
            trade(1, "2026-10-02T09:00:00", Some("2026-10-06T15:00:00"), 10.0, false), // SPY 100 -> 105 = +5%
            trade(2, "2026-10-05T09:00:00", None, -1.0, true),                         // SPY 102 -> now 110
            stamped,                                                                     // stamped: -5%
        ];
        let r = compute(&trades, &[], &b, Some(110.0));
        let t1 = r.trades.iter().find(|t| t.trade_id == 1).unwrap();
        assert!((t1.spy_pct - 5.0).abs() < 1e-9 && (t1.excess_pct - 5.0).abs() < 1e-9);
        let t3 = r.trades.iter().find(|t| t.trade_id == 3).unwrap();
        assert!((t3.spy_pct + 5.0).abs() < 1e-9, "stamped prices win over bars");
        assert_eq!((r.closed_count, r.closed_beat_spy), (2, 2));
        assert!((r.closed_pnl - 110.0).abs() < 1e-9);
        assert!((r.closed_spy_pnl - (50.0 - 50.0)).abs() < 1e-9);
        assert!((r.open_spy_pnl - 1000.0 * (110.0 / 102.0 - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn open_trades_need_a_live_price() {
        let r = compute(&[trade(2, "2026-10-05", None, 1.0, true)], &[], &bars(), None);
        assert!(r.trades.is_empty());
        assert_eq!(r.open_spy_pnl, 0.0);
    }

    #[test]
    fn the_account_curve_is_compared_with_the_same_money_in_spy() {
        let eq = vec![("2026-10-02".to_string(), 1000.0), ("2026-10-04".to_string(), 1010.0), ("2026-10-06".to_string(), 1020.0)];
        let r = compute(&[], &eq, &bars(), None);
        assert_eq!(r.equity_curve.len(), 3);
        assert!((r.equity_curve[1].spy_equity - 1000.0).abs() < 1e-9, "Sunday uses Friday's close");
        assert!((r.equity_curve[2].spy_equity - 1000.0 * 105.0 / 101.0).abs() < 1e-9);
        assert!((r.account_return_pct.unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(r.since.as_deref(), Some("2026-10-02"));
    }
}
