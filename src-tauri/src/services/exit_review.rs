//! Exit review: for every closed paper trade, what the price did while it was
//! open and after it was closed — and so whether closing or holding was better.
//!
//! Per trade:
//! - **while open** — the best and worst price between entry and exit, and how
//!   much of the best gain was given back by the exit;
//! - **after the exit** — the return from the exit price 5, 10 and 20 trading
//!   days later, and the best and worst price in those 20 days;
//! - **if held** — had the trade stayed open under the live hard stop (-15%
//!   from entry) and a +15% target, which it would have reached first within
//!   20 trading days.
//!
//! Trades are then grouped by exit reason into a scorecard, which is what says
//! whether a rule exits too early (price keeps rising after it fires) or too
//! late (price keeps falling). Daily bars come from `entity_prices`; a trade
//! whose ticker has no bars reviews as empty rather than being dropped.

use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

/// Trading days after the exit the review looks at.
pub const HORIZON_DAYS: usize = 20;
/// The live hard stop, as a fraction below entry.
pub const HELD_STOP_PCT: f64 = 0.15;
/// The counterfactual profit target, as a fraction above entry.
pub const HELD_TARGET_PCT: f64 = 0.15;

/// One daily bar: date (YYYY-MM-DD), close, high, low.
pub type Bar = (String, f64, f64, f64);

#[derive(Debug, Clone, PartialEq)]
pub struct ClosedTrade {
    pub id: i64,
    pub ticker: String,
    pub status: String,
    pub exit_reason: Option<String>,
    pub entry_date: String,
    pub exit_date: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl_pct: f64,
    /// Sold short: every figure is read from the position's side, so a
    /// falling price is a gain.
    pub short: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TradeReview {
    pub id: i64,
    pub ticker: String,
    pub reason: String,
    pub entry_date: String,
    pub exit_date: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl_pct: f64,
    /// Best price while open, % from entry.
    pub max_gain_pct: Option<f64>,
    /// Worst price while open, % from entry.
    pub max_loss_pct: Option<f64>,
    /// Points of the best gain not kept: max_gain_pct - pnl_pct, never negative.
    pub gave_back_pct: Option<f64>,
    /// Close N trading days after the exit, % from the exit price.
    pub after_5d_pct: Option<f64>,
    pub after_10d_pct: Option<f64>,
    pub after_20d_pct: Option<f64>,
    /// Best and worst price in the 20 days after the exit, % from the exit price.
    pub best_after_pct: Option<f64>,
    pub worst_after_pct: Option<f64>,
    /// `stop_first`, `target_first`, `neither` (20 days passed without either),
    /// `pending` (fewer than 20 days of bars so far, neither hit yet), or
    /// `no_data`.
    pub if_held: String,
    /// Whether the price was higher 10 trading days after the exit. `None`
    /// until 10 days of bars exist.
    pub holding_was_better: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReasonScore {
    pub reason: String,
    pub trades: usize,
    pub avg_pnl_pct: f64,
    /// Mean 10-day post-exit return over trades that have one.
    pub avg_after_10d_pct: Option<f64>,
    /// Share (0-100) of trades with a 10-day reading where holding was better.
    pub holding_better_pct: Option<f64>,
    pub stop_first: usize,
    pub target_first: usize,
    pub neither: usize,
    pub pending: usize,
    pub avg_gave_back_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExitReview {
    pub horizon_days: usize,
    pub held_stop_pct: f64,
    pub held_target_pct: f64,
    pub by_reason: Vec<ReasonScore>,
    pub trades: Vec<TradeReview>,
}

/// The rule that closed a trade, grouped. Live exit reasons carry detail after
/// the rule name ("trailing_stop (-13.0%, ATR=1.60 …)"); only the name groups.
/// Trades closed before `exit_reason` existed (2026-09-29) fall back to their
/// status, as do `rebuilt_from_fills` rows: that tag says the 2026-09 ledger
/// repair recomputed the trade's P&L from Alpaca fills, not which rule closed it.
pub fn reason_group(exit_reason: Option<&str>, status: &str) -> String {
    match exit_reason
        .map(str::trim)
        .filter(|r| !r.is_empty() && *r != "rebuilt_from_fills")
    {
        Some(r) => r
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or(r)
            .to_string(),
        None if status == "stopped_out" => "stop (before reasons were recorded)".to_string(),
        None => "unrecorded".to_string(),
    }
}

fn pct(from: f64, to: f64) -> f64 {
    (to - from) / from * 100.0
}

/// Review one trade against its ticker's bars (ascending by date).
pub fn review_trade(t: &ClosedTrade, bars: &[Bar]) -> TradeReview {
    let entry_day = &t.entry_date[..t.entry_date.len().min(10)];
    let exit_day = &t.exit_date[..t.exit_date.len().min(10)];

    let during: Vec<&Bar> = bars
        .iter()
        .filter(|b| b.0.as_str() >= entry_day && b.0.as_str() <= exit_day)
        .collect();
    let sign = if t.short { -1.0 } else { 1.0 };
    fn highest(bs: &[&Bar]) -> Option<f64> {
        bs.iter().map(|b| b.2).fold(None, |m: Option<f64>, h| Some(m.map_or(h, |m| m.max(h))))
    }
    fn lowest(bs: &[&Bar]) -> Option<f64> {
        bs.iter().map(|b| b.3).fold(None, |m: Option<f64>, l| Some(m.map_or(l, |m| m.min(l))))
    }
    // The best and worst prices for the position: highs and lows, swapped for a short.
    type Pick = fn(&[&Bar]) -> Option<f64>;
    let (best, worst): (Pick, Pick) = if t.short { (lowest, highest) } else { (highest, lowest) };
    let max_gain_pct = best(&during).map(|p| sign * pct(t.entry_price, p));
    let max_loss_pct = worst(&during).map(|p| sign * pct(t.entry_price, p));
    let gave_back_pct = max_gain_pct.map(|g| (g - t.pnl_pct).max(0.0));

    let after: Vec<&Bar> = bars
        .iter()
        .filter(|b| b.0.as_str() > exit_day)
        .take(HORIZON_DAYS)
        .collect();
    let after_n = |n: usize| after.get(n - 1).map(|b| sign * pct(t.exit_price, b.1));
    let best_after_pct = best(&after).map(|p| sign * pct(t.exit_price, p));
    let worst_after_pct = worst(&after).map(|p| sign * pct(t.exit_price, p));

    let stop = t.entry_price * (1.0 - sign * HELD_STOP_PCT);
    let target = t.entry_price * (1.0 + sign * HELD_TARGET_PCT);
    let stop_hit = |b: &Bar| if t.short { b.2 >= stop } else { b.3 <= stop };
    let target_hit = |b: &Bar| if t.short { b.3 <= target } else { b.2 >= target };
    let if_held = if after.is_empty() {
        if bars.is_empty() { "no_data" } else { "pending" }
    } else {
        // Low before high on the same bar: the stop is assumed to fire first,
        // the conservative reading when only daily bars are known.
        after
            .iter()
            .find_map(|b| {
                if stop_hit(b) {
                    Some("stop_first")
                } else if target_hit(b) {
                    Some("target_first")
                } else {
                    None
                }
            })
            .unwrap_or(if after.len() >= HORIZON_DAYS { "neither" } else { "pending" })
    }
    .to_string();

    let after_10d_pct = after_n(10);
    TradeReview {
        id: t.id,
        ticker: t.ticker.clone(),
        reason: reason_group(t.exit_reason.as_deref(), &t.status),
        entry_date: entry_day.to_string(),
        exit_date: exit_day.to_string(),
        entry_price: t.entry_price,
        exit_price: t.exit_price,
        pnl_pct: t.pnl_pct,
        max_gain_pct,
        max_loss_pct,
        gave_back_pct,
        after_5d_pct: after_n(5),
        after_10d_pct,
        after_20d_pct: after_n(20),
        best_after_pct,
        worst_after_pct,
        if_held,
        holding_was_better: after_10d_pct.map(|r| r > 0.0),
    }
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Group reviews into one scorecard row per exit reason, most trades first.
pub fn score_by_reason(reviews: &[TradeReview]) -> Vec<ReasonScore> {
    let mut groups: HashMap<&str, Vec<&TradeReview>> = HashMap::new();
    for r in reviews {
        groups.entry(r.reason.as_str()).or_default().push(r);
    }
    let mut out: Vec<ReasonScore> = groups
        .into_iter()
        .map(|(reason, rs)| {
            let after10: Vec<f64> = rs.iter().filter_map(|r| r.after_10d_pct).collect();
            let better: Vec<bool> = rs.iter().filter_map(|r| r.holding_was_better).collect();
            let gave: Vec<f64> = rs.iter().filter_map(|r| r.gave_back_pct).collect();
            let count = |s: &str| rs.iter().filter(|r| r.if_held == s).count();
            ReasonScore {
                reason: reason.to_string(),
                trades: rs.len(),
                avg_pnl_pct: rs.iter().map(|r| r.pnl_pct).sum::<f64>() / rs.len() as f64,
                avg_after_10d_pct: mean(&after10),
                holding_better_pct: (!better.is_empty())
                    .then(|| 100.0 * better.iter().filter(|b| **b).count() as f64 / better.len() as f64),
                stop_first: count("stop_first"),
                target_first: count("target_first"),
                neither: count("neither"),
                pending: count("pending"),
                avg_gave_back_pct: mean(&gave),
            }
        })
        .collect();
    out.sort_by(|a, b| b.trades.cmp(&a.trades).then_with(|| a.reason.cmp(&b.reason)));
    out
}

/// Closed trades with a real exit. Rows merged away in the 2026-09 ledger
/// repair are 'expired' and have no exit price, so they never appear.
fn load_closed_trades(conn: &Connection) -> Result<Vec<ClosedTrade>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, ticker, status, exit_reason, entry_date, exit_date,
                    entry_price, exit_price, pnl_pct, COALESCE(direction, 'long') = 'short'
             FROM paper_trades
             WHERE status IN ('closed', 'stopped_out')
               AND exit_price IS NOT NULL AND exit_price > 0
               AND pnl_pct IS NOT NULL AND entry_price > 0
               AND exit_date IS NOT NULL
             ORDER BY exit_date DESC, id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ClosedTrade {
                id: row.get(0)?,
                ticker: row.get(1)?,
                status: row.get(2)?,
                exit_reason: row.get(3)?,
                entry_date: row.get(4)?,
                exit_date: row.get(5)?,
                entry_price: row.get(6)?,
                exit_price: row.get(7)?,
                pnl_pct: row.get(8)?,
                short: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

fn load_bars(conn: &Connection, ticker: &str, from: &str) -> Result<Vec<Bar>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT date, close, COALESCE(high, close), COALESCE(low, close)
             FROM entity_prices
             WHERE ticker = ?1 AND date >= ?2 AND close > 0
             ORDER BY date ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![ticker, from], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

pub fn compute_exit_review(conn: &Connection) -> Result<ExitReview, String> {
    let trades = load_closed_trades(conn)?;
    let mut bars_by_ticker: HashMap<String, Vec<Bar>> = HashMap::new();
    let mut reviews = Vec::with_capacity(trades.len());
    for t in &trades {
        if !bars_by_ticker.contains_key(&t.ticker) {
            let earliest = trades
                .iter()
                .filter(|o| o.ticker == t.ticker)
                .map(|o| &o.entry_date[..o.entry_date.len().min(10)])
                .min()
                .unwrap_or("0000-00-00")
                .to_string();
            bars_by_ticker.insert(t.ticker.clone(), load_bars(conn, &t.ticker, &earliest)?);
        }
        reviews.push(review_trade(t, &bars_by_ticker[&t.ticker]));
    }
    Ok(ExitReview {
        horizon_days: HORIZON_DAYS,
        held_stop_pct: HELD_STOP_PCT * 100.0,
        held_target_pct: HELD_TARGET_PCT * 100.0,
        by_reason: score_by_reason(&reviews),
        trades: reviews,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: i64) -> String {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
            .unwrap()
            .checked_add_signed(chrono::Duration::days(n))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string()
    }

    /// Bars from `start`, one per day, each with a $1 range around its close.
    fn bars(start: i64, closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| (day(start + i as i64), *c, c + 1.0, c - 1.0))
            .collect()
    }

    fn trade(exit_reason: Option<&str>, entry: f64, exit: f64, exit_day: i64) -> ClosedTrade {
        ClosedTrade {
            id: 1,
            ticker: "A".into(),
            status: "closed".into(),
            exit_reason: exit_reason.map(Into::into),
            entry_date: format!("{}T00:27:00", day(0)),
            exit_date: day(exit_day),
            entry_price: entry,
            exit_price: exit,
            pnl_pct: pct(entry, exit),
            short: false,
        }
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn reasons_group_by_rule_name_and_fall_back_to_status() {
        assert_eq!(reason_group(Some("trailing_stop (-13.0%, ATR=1.60)"), "closed"), "trailing_stop");
        assert_eq!(reason_group(Some("signal_decay (orig 0.34, pnl -2.0%)"), "closed"), "signal_decay");
        assert_eq!(reason_group(Some("manual"), "closed"), "manual");
        assert_eq!(reason_group(None, "stopped_out"), "stop (before reasons were recorded)");
        assert_eq!(reason_group(Some("  "), "closed"), "unrecorded");
        assert_eq!(reason_group(Some("rebuilt_from_fills"), "closed"), "unrecorded", "a repair tag, not a rule");
        assert_eq!(reason_group(Some("rebuilt_from_fills"), "stopped_out"), "stop (before reasons were recorded)");
    }

    #[test]
    fn the_best_gain_while_open_and_what_was_given_back() {
        // Entry 100, peaks at close 112 (high 113), exits at 104.
        let b = bars(0, &[100.0, 106.0, 112.0, 108.0, 104.0]);
        let r = review_trade(&trade(None, 100.0, 104.0, 4), &b);
        assert!(close(r.max_gain_pct, 13.0));
        assert!(close(r.max_loss_pct, -1.0));
        assert!(close(r.gave_back_pct, 9.0), "13% seen, 4% kept");
    }

    #[test]
    fn a_short_is_reviewed_from_its_own_side() {
        // Shorted at 100, fell to close 90 (low 89), covered at 95, then rallied.
        let mut b = bars(0, &[100.0, 90.0, 95.0]);
        b.extend(bars(3, &[100.0, 108.0, 116.0]));
        let mut t = trade(None, 100.0, 95.0, 2);
        t.short = true;
        t.pnl_pct = 5.0;
        let r = review_trade(&t, &b);
        assert!(close(r.max_gain_pct, 11.0), "{:?}", r.max_gain_pct);
        assert!(close(r.max_loss_pct, -1.0), "{:?}", r.max_loss_pct);
        assert!(close(r.gave_back_pct, 6.0));
        // The rally after the cover is a loss from the short's side.
        assert!(r.worst_after_pct.unwrap() < -20.0);
        assert_eq!(r.if_held, "stop_first", "116 + 1 crosses the +15% stop at 115");
    }

    /// Stopped out at 90, then the price recovered: holding was better, and it
    /// reached +15% on entry without first hitting -15%.
    #[test]
    fn a_stop_followed_by_a_recovery_reads_as_holding_better() {
        let mut closes = vec![100.0, 95.0, 90.0];
        closes.extend((1..=20).map(|i| 90.0 + 1.5 * i as f64)); // to 120
        let b = bars(0, &closes);
        let r = review_trade(&trade(Some("trailing_stop (x)"), 100.0, 90.0, 2), &b);
        assert!(close(r.after_10d_pct, (105.0 - 90.0) / 90.0 * 100.0));
        assert_eq!(r.holding_was_better, Some(true));
        assert_eq!(r.if_held, "target_first");
    }

    #[test]
    fn an_exit_before_a_collapse_reads_as_closing_better() {
        let mut closes = vec![100.0, 98.0];
        closes.extend((1..=20).map(|i| 98.0 - 1.5 * i as f64)); // to 68
        let b = bars(0, &closes);
        let r = review_trade(&trade(Some("signal_decay"), 100.0, 98.0, 1), &b);
        assert_eq!(r.holding_was_better, Some(false));
        assert_eq!(r.if_held, "stop_first");
        assert!(r.worst_after_pct.unwrap() < -25.0);
    }

    #[test]
    fn a_bar_touching_both_levels_counts_as_the_stop() {
        let mut b = bars(0, &[100.0, 100.0]);
        b.push((day(2), 100.0, 120.0, 80.0));
        let r = review_trade(&trade(None, 100.0, 100.0, 1), &b);
        assert_eq!(r.if_held, "stop_first");
    }

    #[test]
    fn a_recent_exit_is_pending_until_the_horizon_passes() {
        let b = bars(0, &[100.0, 101.0, 102.0, 101.0]);
        let r = review_trade(&trade(None, 100.0, 101.0, 1), &b);
        assert_eq!(r.if_held, "pending");
        assert!(r.after_5d_pct.is_none() && r.after_10d_pct.is_none());
        assert_eq!(r.holding_was_better, None);
        let r = review_trade(&trade(None, 100.0, 101.0, 3), &b);
        assert_eq!(r.if_held, "pending", "exit on the last bar: nothing after it yet");
    }

    #[test]
    fn twenty_quiet_days_read_as_neither() {
        let b = bars(0, &[100.0; 25]);
        let r = review_trade(&trade(None, 100.0, 100.0, 1), &b);
        assert_eq!(r.if_held, "neither");
        assert!(close(r.after_20d_pct, 0.0));
        assert_eq!(r.holding_was_better, Some(false), "flat is not better");
    }

    #[test]
    fn a_ticker_without_prices_reviews_as_no_data() {
        let r = review_trade(&trade(None, 100.0, 90.0, 3), &[]);
        assert_eq!(r.if_held, "no_data");
        assert!(r.max_gain_pct.is_none() && r.after_10d_pct.is_none());
    }

    #[test]
    fn the_scorecard_groups_and_averages_by_reason() {
        let mut up = vec![100.0, 90.0];
        up.extend((1..=20).map(|i| 90.0 + i as f64));
        let mut down = vec![100.0, 98.0];
        down.extend((1..=20).map(|i| 98.0 - i as f64));
        let reviews = vec![
            review_trade(&trade(Some("trailing_stop (a)"), 100.0, 90.0, 1), &bars(0, &up)),
            review_trade(&trade(Some("trailing_stop (b)"), 100.0, 98.0, 1), &bars(0, &down)),
            review_trade(&trade(Some("manual"), 100.0, 98.0, 1), &bars(0, &down)),
        ];
        let card = score_by_reason(&reviews);
        assert_eq!(card[0].reason, "trailing_stop");
        assert_eq!(card[0].trades, 2);
        assert!(close(card[0].holding_better_pct, 50.0));
        assert!(close(Some(card[0].avg_pnl_pct), (-10.0 + -2.0) / 2.0));
        assert_eq!(card[1].reason, "manual");
    }

    #[test]
    fn only_real_exits_are_reviewed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, ticker TEXT, status TEXT,
                 exit_reason TEXT, entry_date TEXT, exit_date TEXT, entry_price REAL,
                 exit_price REAL, pnl_pct REAL);
             CREATE TABLE entity_prices (ticker TEXT, date TEXT, close REAL, high REAL, low REAL);
             INSERT INTO paper_trades VALUES
                 (1, 'A', 'closed', 'manual', '2026-09-01', '2026-09-05', 100, 104, 4),
                 (2, 'A', 'expired', 'merged_into_1', '2026-09-01', '2026-09-01', 100, NULL, NULL),
                 (3, 'A', 'open', NULL, '2026-09-10', NULL, 100, NULL, 1),
                 (4, 'B', 'stopped_out', NULL, '2026-09-01', '2026-09-03', 50, 45, -10);
             INSERT INTO entity_prices VALUES ('A', '2026-09-01', 100, NULL, NULL);
             ALTER TABLE paper_trades ADD COLUMN direction TEXT;",
        )
        .unwrap();
        let review = compute_exit_review(&conn).unwrap();
        let ids: Vec<i64> = review.trades.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 4]);
        assert_eq!(review.trades[1].if_held, "no_data");
        assert_eq!(review.trades[1].reason, "stop (before reasons were recorded)");
    }
}
