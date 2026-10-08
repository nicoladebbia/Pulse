//! Learning from every trade (`--mode learn`, daily; tables `trade_reviews`,
//! `learning_reports`, migration 043).
//!
//! Three parts:
//! - **Trade reviews.** Each closed trade gets one row: its return against
//!   SPY over the same days, its best and worst point while held (MFE/MAE, from
//!   daily bars), what the price did 5/10/20 trading days after the exit, and a
//!   plain-language story with lesson tags. A review is redone until its
//!   after-exit window is complete, then frozen.
//! - **Source results.** Every buy-grade signal of the last 120 days (bought or
//!   not, the scorecard's episodes) and its 10-day excess over SPY, per signal
//!   dimension. Means are shrunk toward zero (`PRIOR_N` pseudo-signals), so a
//!   handful of lucky signals cannot look like an edge.
//! - **Weights.** A dimension is judged against all buy-grade signals, not
//!   against SPY (a generally weak strategy would otherwise mark every source a
//!   loser), and its t-statistic is taken over trading days, since signals on
//!   the same day share the market's move. Only signals since the last change
//!   count, so one reading cannot move a weight twice. A clear winner or loser
//!   (|t| >= `MIN_T` on >= `MIN_SIGNALS` new signals over >= `MIN_DAYS` days)
//!   moves its weight by `STEP`, at most once a week; every weight stays within
//!   `BOUND` of its code default and a zeroed dimension stays zero. Off with
//!   `LEARNING_AUTO_APPLY=false` (proposals are still reported).
//!
//! The report is stored daily; nothing here places or changes an order.

use std::collections::HashMap;

use pulse_alpaca::DailyBar;
use rusqlite::Connection;
use serde::Serialize;

use crate::scorecard;

/// Pseudo-signals of zero excess every mean is shrunk toward.
pub const PRIOR_N: f64 = 20.0;
/// Episodes a dimension needs before its weight may move.
pub const MIN_SIGNALS: usize = 30;
/// |t-statistic| a dimension's excess needs before its weight may move.
pub const MIN_T: f64 = 2.0;
/// Distinct signal days behind a weight move.
pub const MIN_DAYS: usize = 10;
/// Relative weight change per adjustment.
pub const STEP: f64 = 0.10;
/// Weights stay within this factor of their code default (0.5x to 1.5x).
pub const BOUND: f64 = 0.5;
/// Days between weight adjustments.
pub const ADJUST_EVERY_DAYS: i64 = 7;
/// Trading days after the exit a review waits for before it is final.
pub const AFTER_DAYS: usize = 20;

/// One closed trade, as the review reads it.
#[derive(Debug, Clone)]
pub struct ClosedTrade {
    pub id: i64,
    pub ticker: String,
    pub entry_price: f64,
    pub entry_day: String,
    pub exit_price: f64,
    pub exit_day: String,
    pub exit_reason: String,
    /// Whole-trade return in %, all legs included when known.
    pub return_pct: f64,
    pub profile: serde_json::Value,
    /// Sold short: a falling price is the gain.
    pub short: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Review {
    pub trade_id: i64,
    pub hold_days: i64,
    pub return_pct: f64,
    pub spy_pct: Option<f64>,
    pub excess_pct: Option<f64>,
    pub mfe_pct: Option<f64>,
    pub mae_pct: Option<f64>,
    pub after5_pct: Option<f64>,
    pub after10_pct: Option<f64>,
    pub after20_pct: Option<f64>,
    pub exit_kind: String,
    pub lessons: Vec<&'static str>,
    pub story: String,
    pub is_final: bool,
}

/// The exit rule that fired, from the reason text the exit path writes.
pub fn exit_kind(reason: &str) -> &'static str {
    const KINDS: [&str; 9] = [
        "max_hold",
        "trailing_stop",
        "hard_stop",
        "fixed_stop",
        "broker_stop",
        "signal_decay",
        "profit_target",
        "closed_between_runs",
        "reconcile",
    ];
    KINDS.iter().find(|k| reason.starts_with(*k)).copied().unwrap_or("other")
}

fn pct(a: f64, b: f64) -> f64 {
    (a / b - 1.0) * 100.0
}

/// The signal dimensions that were above the vote line at entry.
fn drivers(profile: &serde_json::Value) -> Vec<&'static str> {
    scorecard::DIMENSIONS
        .iter()
        .map(|(_, k)| *k)
        .filter(|k| profile.get(*k).and_then(|v| v.as_f64()).unwrap_or(0.0) >= 0.3)
        .collect()
}

/// Review one trade from its own daily bars and SPY's. `bars` must be sorted
/// and may run past the exit. `None` when the bars do not cover the entry.
pub fn review(t: &ClosedTrade, bars: &[DailyBar], spy: &[DailyBar]) -> Option<Review> {
    let entry_i = bars.iter().position(|b| b.date >= t.entry_day)?;
    let exit_i = bars.iter().rposition(|b| b.date <= t.exit_day)?;
    if exit_i < entry_i || t.entry_price <= 0.0 || t.exit_price <= 0.0 {
        return None;
    }
    // The entry day's range partly comes before the buy and the exit day's
    // after the sale, so those two days count only through their known
    // prices (entry close, exit price); full ranges in between.
    let between = if exit_i > entry_i + 1 { &bars[entry_i + 1..exit_i] } else { &[][..] };
    let known = [bars[entry_i].close, t.exit_price];
    let high = between.iter().map(|b| b.high).chain(known).fold(f64::MIN, f64::max);
    let low = between.iter().map(|b| b.low).chain(known).fold(f64::MAX, f64::min);
    // Everything below is from the position's side: positive is good for it.
    let sign = if t.short { -1.0 } else { 1.0 };
    let (best, worst) = if t.short { (low, high) } else { (high, low) };
    let mfe_pct = (best.is_finite() && best > 0.0).then(|| sign * pct(best, t.entry_price));
    let mae_pct = (worst.is_finite() && worst > 0.0).then(|| sign * pct(worst, t.entry_price));
    // After the exit: positive means holding on would have paid.
    let after = |n: usize| bars.get(exit_i + n).map(|b| sign * pct(b.close, t.exit_price));

    // Buys happen intraday, so SPY is measured from the entry day's open.
    let spy_open = |day: &str| spy.iter().find(|b| b.date.as_str() >= day).map(|b| b.open);
    let spy_close = |day: &str| spy.iter().rev().find(|b| b.date.as_str() <= day).map(|b| b.close);
    let spy_pct = match (spy_open(&t.entry_day), spy_close(&t.exit_day)) {
        (Some(a), Some(b)) if a > 0.0 => Some(pct(b, a)),
        _ => None,
    };
    // A short is measured against shorting the S&P: it beat the market when
    // the stock did worse than SPY.
    let excess_pct = spy_pct.map(|s| if t.short { t.return_pct + s } else { t.return_pct - s });
    let hold_days = (exit_i - entry_i) as i64;

    let kind = exit_kind(&t.exit_reason);
    let (after5, after10, after20) = (after(5), after(10), after(AFTER_DAYS));
    let mut lessons = Vec::new();
    if let Some(e) = excess_pct {
        lessons.push(if e >= 0.0 { "beat_market" } else { "lagged_market" });
    }
    if let Some(m) = mfe_pct
        && m >= 5.0
        && t.return_pct < m / 3.0
    {
        lessons.push("gave_back_gains");
    }
    let stopped = matches!(kind, "trailing_stop" | "hard_stop" | "fixed_stop" | "broker_stop");
    if stopped && after5.is_some_and(|a| a >= 3.0) {
        lessons.push("stopped_then_recovered");
    }
    if kind == "signal_decay" && after10.is_some_and(|a| a >= 3.0) {
        lessons.push("sold_early");
    }
    if after10.is_some_and(|a| a <= -3.0) {
        lessons.push("good_exit");
    }
    if mae_pct.is_some_and(|m| m <= -10.0) {
        lessons.push("deep_drawdown");
    }
    if hold_days <= 3 && t.return_pct < 0.0 {
        lessons.push("quick_loss");
    }

    let story = story(t, hold_days, mfe_pct, mae_pct, spy_pct, after5, after20);
    Some(Review {
        trade_id: t.id,
        hold_days,
        return_pct: t.return_pct,
        spy_pct,
        excess_pct,
        mfe_pct,
        mae_pct,
        after5_pct: after5,
        after10_pct: after10,
        after20_pct: after20,
        exit_kind: kind.to_string(),
        lessons,
        story,
        is_final: after20.is_some(),
    })
}

fn signed(v: f64) -> String {
    format!("{}{:.1}%", if v >= 0.0 { "+" } else { "" }, v)
}

fn story(
    t: &ClosedTrade,
    hold_days: i64,
    mfe: Option<f64>,
    mae: Option<f64>,
    spy: Option<f64>,
    after5: Option<f64>,
    after20: Option<f64>,
) -> String {
    let why = match drivers(&t.profile).as_slice() {
        [] => "a weak combined score".to_string(),
        d => d.join(" + "),
    };
    let headline = t
        .profile
        .get("stories")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.get("headline"))
        .and_then(|h| h.as_str())
        .map(|h| format!(" (latest story: \"{}\")", h.chars().take(90).collect::<String>()))
        .unwrap_or_default();
    let mut out = format!(
        "{} {} on {} at ${:.2} on {} signals{}. Held {} trading day{}.",
        if t.short { "Shorted" } else { "Bought" },
        t.ticker,
        t.entry_day,
        t.entry_price,
        why,
        headline,
        hold_days,
        if hold_days == 1 { "" } else { "s" }
    );
    if let (Some(up), Some(down)) = (mfe, mae) {
        out.push_str(&format!(" Best point {}, worst {}.", signed(up), signed(down)));
    }
    out.push_str(&format!(
        " {} at ${:.2} ({}) because {}.",
        if t.short { "Bought back" } else { "Sold" },
        t.exit_price,
        signed(t.return_pct),
        crate::position_management::describe_exit(&t.exit_reason)
    ));
    if let Some(s) = spy {
        let e = if t.short { t.return_pct + s } else { t.return_pct - s };
        out.push_str(&format!(
            " The S&P 500 did {} over the same days, so the trade {} {} by {:.1} points.",
            signed(s),
            if e >= 0.0 { "beat" } else { "trailed" },
            if t.short { "shorting it" } else { "it" },
            e.abs()
        ));
    }
    match (after5, after20) {
        (Some(a5), Some(a20)) => out.push_str(&format!(
            " After the exit, staying in would have made {} in 5 days and {} in 20.",
            signed(a5),
            signed(a20)
        )),
        (Some(a5), None) => out.push_str(&format!(" After the exit, staying in would have made {} in 5 days.", signed(a5))),
        _ => {}
    }
    out
}

/// Closed trades whose review is missing or not final yet.
pub fn trades_to_review(conn: &Connection) -> rusqlite::Result<Vec<ClosedTrade>> {
    let mut stmt = conn.prepare(
        "SELECT pt.id, pt.ticker, pt.entry_price, substr(pt.entry_date, 1, 10), pt.exit_price,
                substr(pt.exit_date, 1, 10), COALESCE(pt.exit_reason, ''), pt.pnl_pct, pt.pnl,
                json_extract(pt.entry_context, '$.notional')
                    + COALESCE((SELECT SUM(COALESCE(e.price * e.qty, json_extract(e.detail, '$.notional')))
                                FROM trade_events e WHERE e.trade_id = pt.id AND e.kind = 'scale_in'), 0),
                pt.signal_profile, COALESCE(pt.direction, 'long') = 'short'
         FROM paper_trades pt
         LEFT JOIN trade_reviews r ON r.trade_id = pt.id
         WHERE pt.status IN ('closed', 'stopped_out')
           AND pt.exit_price > 0 AND pt.entry_price > 0 AND pt.exit_date IS NOT NULL
           AND COALESCE(pt.exit_reason, '') NOT LIKE 'merged_into%'
           AND COALESCE(r.is_final, 0) = 0
         ORDER BY pt.exit_date",
    )?;
    let rows = stmt.query_map([], |r| {
        let entry: f64 = r.get(2)?;
        let exit: f64 = r.get(4)?;
        let pnl_pct: Option<f64> = r.get(7)?;
        let pnl: Option<f64> = r.get(8)?;
        let notional: Option<f64> = r.get(9)?;
        // Whole-trade return when the money put in is known (entry plus
        // scale-ins; half closes book their legs into `pnl`); otherwise the
        // last leg's.
        let short: bool = r.get(11)?;
        let return_pct = match (pnl, notional) {
            (Some(p), Some(n)) if n > 0.0 => p / n * 100.0,
            _ => pnl_pct.unwrap_or_else(|| if short { -pct(exit, entry) } else { pct(exit, entry) }),
        };
        let profile: Option<String> = r.get(10)?;
        Ok(ClosedTrade {
            id: r.get(0)?,
            ticker: r.get(1)?,
            entry_price: entry,
            entry_day: r.get(3)?,
            exit_price: exit,
            exit_day: r.get(5)?,
            exit_reason: r.get(6)?,
            return_pct,
            profile: profile.as_deref().and_then(|p| serde_json::from_str(p).ok()).unwrap_or_default(),
            short,
        })
    })?;
    rows.collect()
}

pub fn store_review(conn: &Connection, r: &Review) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO trade_reviews (trade_id, computed_at, hold_days, return_pct, spy_pct, excess_pct, mfe_pct, mae_pct,
             after5_pct, after10_pct, after20_pct, exit_kind, lessons, story, is_final)
         VALUES (?1, datetime('now', 'localtime'), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(trade_id) DO UPDATE SET computed_at = excluded.computed_at, hold_days = excluded.hold_days,
             return_pct = excluded.return_pct, spy_pct = excluded.spy_pct, excess_pct = excluded.excess_pct,
             mfe_pct = excluded.mfe_pct, mae_pct = excluded.mae_pct, after5_pct = excluded.after5_pct,
             after10_pct = excluded.after10_pct, after20_pct = excluded.after20_pct, exit_kind = excluded.exit_kind,
             lessons = excluded.lessons, story = excluded.story, is_final = excluded.is_final",
        rusqlite::params![
            r.trade_id,
            r.hold_days,
            r.return_pct,
            r.spy_pct,
            r.excess_pct,
            r.mfe_pct,
            r.mae_pct,
            r.after5_pct,
            r.after10_pct,
            r.after20_pct,
            r.exit_kind,
            serde_json::to_string(&r.lessons).unwrap_or_default(),
            r.story,
            r.is_final,
        ],
    )?;
    Ok(())
}

/// Mean, standard deviation and count of a sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Stats {
    pub n: usize,
    pub mean: f64,
    pub sd: f64,
}

impl Stats {
    pub fn of(xs: &[f64]) -> Option<Self> {
        let n = xs.len();
        if n == 0 {
            return None;
        }
        let mean = xs.iter().sum::<f64>() / n as f64;
        let sd = if n > 1 { (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt() } else { 0.0 };
        Some(Self { n, mean, sd })
    }
    /// The mean pulled toward zero by `PRIOR_N` pseudo-observations.
    pub fn shrunk(&self) -> f64 {
        self.mean * self.n as f64 / (self.n as f64 + PRIOR_N)
    }
    pub fn t(&self) -> f64 {
        if self.sd <= 0.0 || self.n < 2 { 0.0 } else { self.mean / (self.sd / (self.n as f64).sqrt()) }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceResult {
    pub dimension: String,
    pub signals: usize,
    pub avg_excess: f64,
    pub shrunk_excess: f64,
    pub t: f64,
    /// Average excess minus that of all buy-grade signals, shrunk.
    pub vs_all: f64,
    /// t-statistic of `vs_all` over signal days.
    pub vs_all_t: f64,
    pub days: usize,
    /// Share of signals beating SPY, shrunk toward 50% (Beta(10,10) prior).
    pub win_rate: f64,
    pub trades: usize,
    pub trade_avg_excess: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExitResult {
    pub exit_kind: String,
    pub trades: usize,
    pub avg_return: f64,
    pub avg_after5: Option<f64>,
    pub avg_after10: Option<f64>,
    pub avg_gave_back: Option<f64>,
}

/// What the candidates each auto-trade decision touched did over the next 10
/// trading days, by decision: bought, or skipped for which reason. A filter
/// whose skips beat the market is costing money.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateResult {
    pub reason: String,
    pub outcome: String,
    pub signals: usize,
    pub avg_excess: f64,
    pub t: f64,
}

/// Every recorded event signal of a kind and direction, held from the close
/// of the day it became known for the kind's maximum hold, vs SPY. For shorts the sign is flipped,
/// so positive always means the trade would have made money.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventResult {
    pub kind: String,
    pub direction: String,
    pub hold_days: usize,
    pub signals: usize,
    pub avg_excess: f64,
    pub t: f64,
    pub trades: usize,
    pub trade_avg_excess: Option<f64>,
}

/// From the close of the day an event became known (or the next trading
/// day) to the close `h` bars later: `(entry day, exit day, return)`. The bot
/// buys that day after the open, so this leaves out part of the first day's
/// move rather than counting a move it could not have caught.
fn forward_n(bars: &[DailyBar], date: &str, h: usize) -> Option<(String, String, f64)> {
    let i = bars.partition_point(|b| b.date.as_str() < date);
    let entry = bars.get(i)?;
    let exit = bars.get(i + h.max(1))?;
    (entry.close > 0.0).then(|| (entry.date.clone(), exit.date.clone(), exit.close / entry.close - 1.0))
}

/// SPY close to close over the same days.
fn spy_close_return(spy: &[DailyBar], entry: &str, exit: &str) -> Option<f64> {
    let e = spy.iter().find(|b| b.date == entry)?;
    let x = spy.iter().find(|b| b.date == exit)?;
    (e.close > 0.0).then(|| x.close / e.close - 1.0)
}

/// Group `(kind, direction, day, signed excess %)` into results, and attach
/// the bot's own trades `(kind, direction, excess %)`.
pub fn event_results(rows: &[(String, String, String, f64)], trades: &[(String, String, f64)]) -> Vec<EventResult> {
    let mut by: std::collections::BTreeMap<(&str, &str), Vec<(&str, f64)>> = Default::default();
    for (k, d, day, x) in rows {
        by.entry((k.as_str(), d.as_str())).or_default().push((day.as_str(), *x));
    }
    by.into_iter()
        .filter_map(|((k, d), xs)| {
            let s = Stats::of(&xs.iter().map(|(_, x)| *x).collect::<Vec<_>>())?;
            let (_, t, _) = by_day(&xs);
            let tx: Vec<f64> = trades.iter().filter(|(tk, td, _)| tk == k && td == d).map(|(_, _, x)| *x).collect();
            Some(EventResult {
                kind: k.to_string(),
                direction: d.to_string(),
                hold_days: crate::event_signals::max_hold_days(k).unwrap_or(10) as usize,
                signals: s.n,
                avg_excess: s.mean,
                t,
                trades: tx.len(),
                trade_avg_excess: mean(&tx),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WeightChange {
    pub dimension: String,
    pub from: f64,
    pub to: f64,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub computed_at: String,
    pub headline: Vec<String>,
    pub signals_total: Option<Stats>,
    pub sources: Vec<SourceResult>,
    pub exits: Vec<ExitResult>,
    pub gates: Vec<GateResult>,
    pub events: Vec<EventResult>,
    pub lessons: Vec<(String, usize)>,
    pub weight_changes: Vec<WeightChange>,
    pub weights_applied: bool,
    pub trades_reviewed: usize,
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

/// One measured signal: its day, the dimensions that fired, its 10-day
/// excess over SPY in points.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub date: String,
    pub fired: Vec<&'static str>,
    pub excess: f64,
}

/// Mean and t-statistic of `(day, value)` pairs, with each day's values
/// averaged first: signals of one day share the market's move and are not
/// independent. Returns `(mean of days, t, days)`.
pub fn by_day(xs: &[(&str, f64)]) -> (f64, f64, usize) {
    let mut days: std::collections::BTreeMap<&str, (f64, usize)> = Default::default();
    for (d, x) in xs {
        let e = days.entry(d).or_default();
        e.0 += x;
        e.1 += 1;
    }
    let means: Vec<f64> = days.values().map(|(s, n)| s / *n as f64).collect();
    match Stats::of(&means) {
        Some(s) => (s.mean, s.t(), s.n),
        None => (0.0, 0.0, 0),
    }
}

/// Per-dimension results from the signal episodes and the reviewed trades
/// `(fired dims, excess %)`.
pub fn source_results(episodes: &[Episode], trades: &[(Vec<&'static str>, f64)]) -> Vec<SourceResult> {
    let all = mean(&episodes.iter().map(|e| e.excess).collect::<Vec<_>>()).unwrap_or(0.0);
    let mut out = Vec::new();
    for (_, dim) in scorecard::DIMENSIONS {
        let mine: Vec<&Episode> = episodes.iter().filter(|e| e.fired.contains(&dim)).collect();
        let xs: Vec<f64> = mine.iter().map(|e| e.excess).collect();
        let Some(s) = Stats::of(&xs) else { continue };
        let rel: Vec<(&str, f64)> = mine.iter().map(|e| (e.date.as_str(), e.excess - all)).collect();
        let (_, vs_all_t, days) = by_day(&rel);
        let vs_all = (s.mean - all) * s.n as f64 / (s.n as f64 + PRIOR_N);
        let wins = xs.iter().filter(|x| **x > 0.0).count() as f64;
        let tx: Vec<f64> = trades.iter().filter(|(f, _)| f.contains(&dim)).map(|(_, x)| *x).collect();
        out.push(SourceResult {
            dimension: dim.to_string(),
            signals: s.n,
            avg_excess: s.mean,
            shrunk_excess: s.shrunk(),
            t: s.t(),
            vs_all,
            vs_all_t,
            days,
            win_rate: (wins + 10.0) / (s.n as f64 + 20.0),
            trades: tx.len(),
            trade_avg_excess: mean(&tx),
        });
    }
    out.sort_by(|a, b| b.shrunk_excess.partial_cmp(&a.shrunk_excess).unwrap_or(std::cmp::Ordering::Equal));
    out
}

pub fn exit_results(reviews: &[Review]) -> Vec<ExitResult> {
    let mut by: HashMap<&str, Vec<&Review>> = HashMap::new();
    for r in reviews {
        by.entry(r.exit_kind.as_str()).or_default().push(r);
    }
    let mut out: Vec<ExitResult> = by
        .into_iter()
        .map(|(k, rs)| {
            let col = |f: fn(&Review) -> Option<f64>| mean(&rs.iter().filter_map(|r| f(r)).collect::<Vec<_>>());
            ExitResult {
                exit_kind: k.to_string(),
                trades: rs.len(),
                avg_return: rs.iter().map(|r| r.return_pct).sum::<f64>() / rs.len() as f64,
                avg_after5: col(|r| r.after5_pct),
                avg_after10: col(|r| r.after10_pct),
                avg_gave_back: col(|r| r.mfe_pct.map(|m| m - r.return_pct)),
            }
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.trades));
    out
}

/// One decision per ticker, day and outcome from `trade_decisions` since
/// `since`: `(ticker, day, outcome, reason)`. Later runs of the same day
/// repeat the first one's verdict and would count the signal twice.
pub fn load_decisions(conn: &Connection, since: &str) -> rusqlite::Result<Vec<(String, String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT ticker, substr(run_at, 1, 10), outcome, reason FROM trade_decisions
         WHERE id IN (SELECT MIN(id) FROM trade_decisions
                      WHERE ticker != '' AND outcome IN ('bought', 'skipped') AND run_at >= ?1
                      GROUP BY ticker, substr(run_at, 1, 10), outcome)
         ORDER BY run_at, ticker",
    )?;
    let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

/// One decision per ticker, outcome and reason every `scorecard::HORIZON`
/// trading days: a name skipped as too calm 20 days running is one signal
/// with overlapping windows, not 20.
pub fn dedup_decisions(
    rows: &[(String, String, String, String)],
    trading_days: &[String],
) -> Vec<(String, String, String, String)> {
    let index = |d: &str| trading_days.partition_point(|t| t.as_str() < d);
    let mut last: HashMap<(&str, &str, &str), usize> = HashMap::new();
    let mut out = Vec::new();
    for r in rows {
        let i = index(&r.1);
        let key = (r.0.as_str(), r.2.as_str(), r.3.as_str());
        if last.get(&key).is_some_and(|prev| i < prev + scorecard::HORIZON) {
            continue;
        }
        last.insert(key, i);
        out.push(r.clone());
    }
    out
}

/// Group decisions `(outcome, reason, excess %)` into results, biggest first.
pub fn gate_results(rows: &[(String, String, f64)]) -> Vec<GateResult> {
    let mut by: HashMap<(&str, &str), Vec<f64>> = HashMap::new();
    for (outcome, reason, x) in rows {
        by.entry((outcome.as_str(), reason.as_str())).or_default().push(*x);
    }
    let mut out: Vec<GateResult> = by
        .into_iter()
        .filter_map(|((outcome, reason), xs)| {
            let s = Stats::of(&xs)?;
            Some(GateResult { reason: reason.to_string(), outcome: outcome.to_string(), signals: s.n, avg_excess: s.mean, t: s.t() })
        })
        .collect();
    out.sort_by(|a, b| b.signals.cmp(&a.signals).then(a.reason.cmp(&b.reason)));
    out
}

/// Weight moves the source results justify. `current` and `defaults` are in
/// `pulse_weights::DIMENSIONS` order. Returns the new vector (summing to 1)
/// and what changed; an empty change list means "keep the current weights".
pub fn propose_weights(current: &[f64; 8], defaults: &[f64; 8], sources: &[SourceResult]) -> ([f64; 8], Vec<WeightChange>) {
    let mut next = *current;
    let mut why: Vec<(usize, String)> = Vec::new();
    for s in sources {
        let Some(i) = dimension_index(&s.dimension) else { continue };
        if defaults[i] <= 0.0 || s.signals < MIN_SIGNALS || s.days < MIN_DAYS || s.vs_all_t.abs() < MIN_T {
            continue;
        }
        let factor = if s.vs_all_t > 0.0 { 1.0 + STEP } else { 1.0 - STEP };
        let lo = defaults[i] * (1.0 - BOUND);
        let hi = defaults[i] * (1.0 + BOUND);
        let moved = (current[i] * factor).clamp(lo, hi);
        if (moved - current[i]).abs() > 1e-6 {
            next[i] = moved;
            why.push((
                i,
                format!(
                    "{} new signals did {:+.1} points vs all buy-grade signals over 10 days (t = {:.1} over {} days)",
                    s.signals, s.vs_all, s.vs_all_t, s.days
                ),
            ));
        }
    }
    if why.is_empty() {
        return (*current, Vec::new());
    }
    // Renormalize to 1 while keeping EVERY weight inside its band: pin the
    // ones that would leave it and share the rest among the others. A plain
    // rescale lets dimensions without evidence drift a little every week.
    let lo: Vec<f64> = defaults.iter().map(|d| d * (1.0 - BOUND)).collect();
    let hi: Vec<f64> = defaults.iter().map(|d| d * (1.0 + BOUND)).collect();
    let mut pinned = [false; 8];
    for _ in 0..16 {
        let fixed: f64 = (0..8).filter(|i| pinned[*i]).map(|i| next[i]).sum();
        let free: f64 = (0..8).filter(|i| !pinned[*i]).map(|i| next[i]).sum();
        if free <= 0.0 {
            break;
        }
        let k = (1.0 - fixed) / free;
        let mut changed = false;
        let was = pinned;
        for i in (0..8).filter(|i| !was[*i]) {
            next[i] *= k;
            if defaults[i] > 0.0 && (next[i] < lo[i] || next[i] > hi[i]) {
                next[i] = next[i].clamp(lo[i], hi[i]);
                pinned[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    if (next.iter().sum::<f64>() - 1.0).abs() > 1e-6 {
        // No vector inside every band sums to 1 with this move: keep the old one.
        return (*current, Vec::new());
    }
    for w in next.iter_mut() {
        *w = (*w * 10_000.0).round() / 10_000.0;
    }
    // Rounding can leave the sum a hair off 1; give the remainder to the
    // largest weight so the vector passes the sum check (a 1e-4 nudge, well
    // inside its band).
    let drift = 1.0 - next.iter().sum::<f64>();
    if let Some(i) = (0..8).max_by(|a, b| next[*a].partial_cmp(&next[*b]).unwrap_or(std::cmp::Ordering::Equal)) {
        next[i] += drift;
    }
    let changes = (0..8)
        .filter(|i| (next[*i] - current[*i]).abs() > 1e-6)
        .map(|i| WeightChange {
            dimension: pulse_weights::DIMENSIONS[i].to_string(),
            from: current[i],
            to: next[i],
            why: why.iter().find(|(j, _)| *j == i).map(|(_, w)| w.clone()).unwrap_or_else(|| "rebalanced so weights sum to 1".into()),
        })
        .collect();
    (next, changes)
}

/// `scorecard` dimension key ("insider") to its index in `pulse_weights::DIMENSIONS`.
fn dimension_index(key: &str) -> Option<usize> {
    let column = scorecard::DIMENSIONS.iter().find(|(_, k)| *k == key).map(|(c, _)| *c)?;
    pulse_weights::DIMENSIONS.iter().position(|d| *d == column)
}

fn auto_apply() -> bool {
    std::env::var("LEARNING_AUTO_APPLY").map(|v| !(v.eq_ignore_ascii_case("false") || v == "0")).unwrap_or(true)
}

/// The day the learner last changed the weights, if ever. An unreadable
/// table is an error, not "never": it must not let a change through.
fn last_change(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT MAX(computed_at) FROM learning_reports WHERE weights_applied = 1", [], |r| r.get(0))
}

pub fn headline(total: Option<&Stats>, sources: &[SourceResult], reviews: &[Review], gates: &[GateResult]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = total {
        out.push(format!(
            "Buy-grade signals of the last {} days: {} measured, averaging {:+.1} points vs the S&P 500 over the next 10 trading days{}.",
            scorecard::LOOKBACK_DAYS,
            s.n,
            s.mean,
            if s.n >= MIN_SIGNALS && s.t() <= -MIN_T {
                " — clearly worse than just holding the index"
            } else if s.n >= MIN_SIGNALS && s.t() >= MIN_T {
                " — a real edge so far"
            } else {
                " — not distinguishable from luck yet"
            }
        ));
    }
    let rated: Vec<&SourceResult> = sources.iter().filter(|s| s.signals >= 10).collect();
    if let Some(best) = rated.first() {
        out.push(format!(
            "Best source so far: {} ({} signals, {:+.1} points after shrinking for sample size).",
            best.dimension, best.signals, best.shrunk_excess
        ));
    }
    if let Some(worst) = rated.last().filter(|_| rated.len() >= 2) {
        out.push(format!(
            "Worst source: {} ({} signals, {:+.1} points).",
            worst.dimension, worst.signals, worst.shrunk_excess
        ));
    }
    if !reviews.is_empty() {
        let beat = reviews.iter().filter(|r| r.excess_pct.is_some_and(|e| e >= 0.0)).count();
        out.push(format!("{} of {} reviewed trades beat the S&P 500 over the days they were held.", beat, reviews.len()));
    }
    for g in gates.iter().filter(|g| g.outcome == "skipped" && g.signals >= 10 && g.t >= MIN_T) {
        out.push(format!(
            "Signals skipped as \"{}\" went on to beat the S&P 500 by {:+.1} points ({} of them) — that filter may be costing money.",
            g.reason, g.avg_excess, g.signals
        ));
    }
    out
}

/// Every review on record, for the report.
fn load_reviews(conn: &Connection) -> rusqlite::Result<Vec<Review>> {
    let mut stmt = conn.prepare(
        "SELECT trade_id, hold_days, return_pct, spy_pct, excess_pct, mfe_pct, mae_pct, after5_pct, after10_pct,
                after20_pct, exit_kind, lessons, story, is_final FROM trade_reviews",
    )?;
    let rows = stmt.query_map([], |r| {
        let lessons: String = r.get(11)?;
        let tags: Vec<String> = serde_json::from_str(&lessons).unwrap_or_default();
        Ok(Review {
            trade_id: r.get(0)?,
            hold_days: r.get(1)?,
            return_pct: r.get(2)?,
            spy_pct: r.get(3)?,
            excess_pct: r.get(4)?,
            mfe_pct: r.get(5)?,
            mae_pct: r.get(6)?,
            after5_pct: r.get(7)?,
            after10_pct: r.get(8)?,
            after20_pct: r.get(9)?,
            exit_kind: r.get(10)?,
            lessons: tags.iter().filter_map(|t| LESSONS.iter().find(|l| **l == t.as_str()).copied()).collect(),
            story: r.get(12)?,
            is_final: r.get(13)?,
        })
    })?;
    rows.collect()
}

const LESSONS: [&str; 8] = [
    "beat_market",
    "lagged_market",
    "gave_back_gains",
    "stopped_then_recovered",
    "sold_early",
    "good_exit",
    "deep_drawdown",
    "quick_loss",
];

/// Trades' fired dimensions and excess %, from their reviews.
fn reviewed_trades(conn: &Connection) -> rusqlite::Result<Vec<(Vec<&'static str>, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT pt.signal_profile, r.excess_pct FROM trade_reviews r JOIN paper_trades pt ON pt.id = r.trade_id
         WHERE r.excess_pct IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        let p: Option<String> = r.get(0)?;
        let json: serde_json::Value = p.as_deref().and_then(|p| serde_json::from_str(p).ok()).unwrap_or_default();
        let fired = scorecard::DIMENSIONS
            .iter()
            .map(|(_, k)| *k)
            .filter(|k| json.get(*k).and_then(|v| v.as_f64()).unwrap_or(0.0) >= scorecard::FIRED)
            .collect();
        Ok((fired, r.get(1)?))
    })?;
    rows.collect()
}

/// Review trades, build the report, maybe move weights, store it all.
pub async fn run(db_path: &std::path::Path) -> anyhow::Result<Report> {
    let creds = pulse_alpaca::credentials().ok_or_else(|| anyhow::anyhow!("ALPACA_API_KEY / ALPACA_SECRET_KEY not set"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let conn = Connection::open(db_path)?;
    crate::db::run_migrations(&conn)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let pending = trades_to_review(&conn)?;
    let samples = scorecard::load_samples(&conn)?;
    let lookback = (chrono::Local::now().date_naive() - chrono::Duration::days(scorecard::LOOKBACK_DAYS + 10)).to_string();
    let decisions = load_decisions(&conn, &lookback)?;
    // Record today's event signals even when auto-trade is off.
    crate::event_signals::detect(&conn);
    let events: Vec<(String, String, String, String)> = conn
        // Measured from when it became known: an old Form 4 found late counts
        // from the day it was found.
        .prepare(
            "SELECT kind, direction, ticker, MAX(day, substr(detected_at, 1, 10)) FROM event_signals
             WHERE day >= ?1 ORDER BY day",
        )?
        .query_map([&lookback], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut symbols: Vec<String> = pending
        .iter()
        .map(|t| t.ticker.clone())
        .chain(samples.iter().map(|s| s.ticker.clone()))
        .chain(decisions.iter().map(|d| d.0.clone()))
        .chain(events.iter().map(|e| e.2.clone()))
        .filter(|t| pulse_alpaca::plain_symbol(t))
        .collect();
    symbols.sort();
    symbols.dedup();
    symbols.push("SPY".into());
    let oldest_trade = pending.iter().map(|t| t.entry_day.clone()).min();
    let start = oldest_trade.map(|d| d.min(lookback.clone())).unwrap_or(lookback);
    let mut bars = pulse_alpaca::daily_bars_multi(&client, &creds, &symbols, &start).await.map_err(|e| anyhow::anyhow!(e))?;
    // Today's bar is still moving.
    for list in bars.values_mut() {
        list.retain(|b| b.date < today);
    }
    let spy = bars.remove("SPY").unwrap_or_default();
    if spy.is_empty() {
        anyhow::bail!("no SPY bars");
    }

    let mut reviewed = 0;
    for t in &pending {
        let Some(r) = bars.get(&t.ticker).and_then(|b| review(t, b, &spy)) else { continue };
        store_review(&conn, &r)?;
        reviewed += 1;
    }

    let calendar: Vec<String> = spy.iter().map(|b| b.date.clone()).collect();
    // Buy-grade only before the 10-day dedup, so a 0.25 near miss cannot
    // hide a real signal on the same ticker.
    let buy_grade: Vec<scorecard::Sample> = samples.into_iter().filter(|s| s.score >= 0.30).collect();
    let episodes: Vec<Episode> = scorecard::episodes(buy_grade, &calendar)
        .into_iter()
        .filter_map(|s| {
            let (entry, exit, ret) = scorecard::forward(bars.get(&s.ticker)?, &s.date)?;
            let spy_ret = scorecard::spy_return(&spy, &entry, &exit)?;
            Some(Episode { date: s.date, fired: s.fired, excess: (ret - spy_ret) * 100.0 })
        })
        .collect();
    let gate_rows: Vec<(String, String, f64)> = dedup_decisions(&decisions, &calendar)
        .iter()
        .filter_map(|(ticker, day, outcome, reason)| {
            let (entry, exit, ret) = scorecard::forward(bars.get(ticker)?, day)?;
            let spy_ret = scorecard::spy_return(&spy, &entry, &exit)?;
            Some((outcome.clone(), reason.clone(), (ret - spy_ret) * 100.0))
        })
        .collect();
    let event_rows: Vec<(String, String, String, f64)> = events
        .iter()
        .filter_map(|(kind, dir, ticker, day)| {
            let h = crate::event_signals::max_hold_days(kind).unwrap_or(10) as usize;
            let (entry, exit, ret) = forward_n(bars.get(ticker)?, day, h)?;
            let spy_ret = spy_close_return(&spy, &entry, &exit)?;
            let x = (ret - spy_ret) * 100.0;
            Some((kind.clone(), dir.clone(), day.clone(), if dir == "short" { -x } else { x }))
        })
        .collect();
    let event_trades: Vec<(String, String, f64)> = conn
        .prepare(
            "SELECT pt.entry_trigger, COALESCE(pt.direction, 'long'), r.excess_pct
             FROM trade_reviews r JOIN paper_trades pt ON pt.id = r.trade_id
             WHERE r.excess_pct IS NOT NULL AND pt.entry_trigger != 'convergence'",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let total = Stats::of(&episodes.iter().map(|e| e.excess).collect::<Vec<_>>());
    let sources = source_results(&episodes, &reviewed_trades(&conn)?);
    let reviews = load_reviews(&conn)?;
    let mut lesson_counts: HashMap<&str, usize> = HashMap::new();
    for r in &reviews {
        for l in &r.lessons {
            *lesson_counts.entry(l).or_default() += 1;
        }
    }
    let mut lessons: Vec<(String, usize)> = lesson_counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    lessons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Weights move only on signals since the last change.
    let last = last_change(&conn)?;
    let fresh: Vec<Episode> =
        episodes.iter().filter(|e| last.as_deref().is_none_or(|d| e.date.as_str() > d)).cloned().collect();
    let current = crate::pipeline::signals::load_calibrated_weights(&conn);
    let (next, weight_changes) =
        propose_weights(&current, &pulse_weights::default_vector(), &source_results(&fresh, &[]));
    let due = match last.as_deref().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
        None if last.is_some() => false,
        None => true,
        Some(d) => (chrono::Local::now().date_naive() - d).num_days() >= ADJUST_EVERY_DAYS,
    };
    let weights_applied = !weight_changes.is_empty() && due && auto_apply();
    if weights_applied {
        let pairs: Vec<(String, f64)> =
            pulse_weights::DIMENSIONS.iter().zip(next.iter()).map(|(d, w)| (d.to_string(), *w)).collect();
        conn.execute(
            "INSERT INTO user_profile (key, value) VALUES ('calibrated_weights', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(&pairs)?],
        )?;
        tracing::warn!("Learning: weights changed — {:?}", weight_changes);
    }

    let gates = gate_results(&gate_rows);
    let report = Report {
        computed_at: today.clone(),
        headline: headline(total.as_ref(), &sources, &reviews, &gates),
        signals_total: total,
        sources,
        exits: exit_results(&reviews),
        gates,
        events: event_results(&event_rows, &event_trades),
        lessons,
        weight_changes,
        weights_applied,
        trades_reviewed: reviews.len(),
    };
    conn.execute(
        "INSERT INTO learning_reports (computed_at, weights_applied, body) VALUES (?1, ?2, ?3)
         ON CONFLICT(computed_at) DO UPDATE SET weights_applied = MAX(weights_applied, excluded.weights_applied),
             body = CASE WHEN weights_applied = 1 AND excluded.weights_applied = 0 THEN body ELSE excluded.body END",
        rusqlite::params![today, weights_applied, serde_json::to_string(&report)?],
    )?;
    tracing::info!(
        "Learning: {} trade(s) reviewed this run, {} on record, {} signal episodes, {} weight change(s){}",
        reviewed,
        report.trades_reviewed,
        report.signals_total.map(|s| s.n).unwrap_or(0),
        report.weight_changes.len(),
        if weights_applied { " applied" } else { " (not applied)" }
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: &str, low: f64, high: f64, close: f64) -> DailyBar {
        DailyBar { date: date.into(), open: close, high, low, close, volume: 1e6 }
    }

    /// 30 weekdays from 2026-09-01, one price per day.
    fn days(prices: &[f64]) -> Vec<DailyBar> {
        let mut d = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let mut out = Vec::new();
        for p in prices {
            while matches!(chrono::Datelike::weekday(&d), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                d = d.succ_opt().unwrap();
            }
            out.push(bar(&d.to_string(), p * 0.98, p * 1.02, *p));
            d = d.succ_opt().unwrap();
        }
        out
    }

    fn trade(entry_day: &str, exit_day: &str, entry: f64, exit: f64, reason: &str) -> ClosedTrade {
        ClosedTrade {
            id: 1,
            ticker: "AAA".into(),
            entry_price: entry,
            entry_day: entry_day.into(),
            exit_price: exit,
            exit_day: exit_day.into(),
            exit_reason: reason.into(),
            return_pct: pct(exit, entry),
            profile: serde_json::json!({"news": 0.5, "search": 0.1, "stories": [{"headline": "AAA wins contract"}]}),
            short: false,
        }
    }

    #[test]
    fn exit_kinds_come_from_the_reason_prefix() {
        assert_eq!(exit_kind("trailing_stop (hwm 20.00)"), "trailing_stop");
        assert_eq!(exit_kind("signal_decay: 0.10 < 0.12"), "signal_decay");
        assert_eq!(exit_kind("something new"), "other");
    }

    #[test]
    fn a_stopped_trade_that_recovered_is_reviewed_as_such() {
        // Bought at 100 on day 0, ran to 110, stopped at 95 on day 6, then the
        // stock went back to 105 within five days.
        let prices = [100.0, 104.0, 110.0, 105.0, 100.0, 97.0, 95.0, 98.0, 100.0, 102.0, 104.0, 105.0];
        let bars = days(&prices);
        let spy = days(&[500.0; 12]);
        let t = trade(&bars[0].date, &bars[6].date, 100.0, 95.0, "trailing_stop");
        let r = review(&t, &bars, &spy).unwrap();
        assert_eq!(r.hold_days, 6);
        assert_eq!(r.exit_kind, "trailing_stop");
        assert!((r.mfe_pct.unwrap() - 12.2).abs() < 0.01, "{:?}", r.mfe_pct);
        assert!((r.after5_pct.unwrap() - pct(105.0, 95.0)).abs() < 1e-9);
        assert_eq!(r.spy_pct, Some(0.0));
        assert!(r.lessons.contains(&"stopped_then_recovered"));
        assert!(r.lessons.contains(&"gave_back_gains"));
        assert!(r.lessons.contains(&"lagged_market"));
        assert!(!r.is_final, "20 days after the exit have not passed");
        assert!(r.story.contains("news"), "{}", r.story);
        assert!(r.story.contains("AAA wins contract"));
        assert!(r.story.contains("trailed it by 5.0 points"), "{}", r.story);
    }

    #[test]
    fn a_short_is_reviewed_from_its_own_side() {
        // Shorted at 10.0, the stock fell to 8.0 and was covered at 9.0, then rose.
        let bars = days(&[10.0, 8.0, 9.0, 9.0, 9.5, 10.0, 11.0, 12.0, 12.5, 13.0]);
        let mut t = trade(&bars[0].date, &bars[3].date, 10.0, 9.0, "trailing_stop (short)");
        t.short = true;
        t.return_pct = 10.0;
        let spy = days(&[100.0; 10]);
        let r = review(&t, &bars, &spy).unwrap();
        // Best point: the low between entry and exit (8.0 * 0.98), from the short's side.
        assert!((r.mfe_pct.unwrap() - (100.0 - 8.0 * 0.98 * 10.0)).abs() < 1e-6, "{:?}", r.mfe_pct);
        assert!(r.mae_pct.unwrap() <= 0.0, "worst point is a rise: {:?}", r.mae_pct);
        // The price rose after the cover: staying short would have lost.
        assert!(r.after5_pct.unwrap() < 0.0);
        assert!(r.lessons.contains(&"good_exit") || r.after10_pct.is_none());
        assert!(r.story.starts_with("Shorted AAA"), "{}", r.story);
        assert!(r.story.contains("Bought back at $9.00"), "{}", r.story);
    }

    #[test]
    fn a_review_is_final_once_twenty_days_have_passed() {
        let bars = days(&[10.0; 30]);
        let spy = days(&[500.0; 30]);
        let t = trade(&bars[0].date, &bars[3].date, 10.0, 10.0, "signal_decay");
        assert!(review(&t, &bars, &spy).unwrap().is_final);
    }

    #[test]
    fn no_review_without_bars_covering_the_trade() {
        let bars = days(&[10.0; 5]);
        let t = trade("2026-12-01", "2026-12-05", 10.0, 11.0, "signal_decay");
        assert!(review(&t, &bars, &bars).is_none());
    }

    fn source(dim: &str, signals: usize, mean: f64, t: f64) -> SourceResult {
        SourceResult {
            dimension: dim.into(),
            signals,
            avg_excess: mean,
            shrunk_excess: mean,
            t,
            vs_all: mean,
            vs_all_t: t,
            days: signals / 2,
            win_rate: 0.5,
            trades: 0,
            trade_avg_excess: None,
        }
    }

    #[test]
    fn weights_move_one_step_only_on_strong_evidence_and_sum_to_one() {
        let d = pulse_weights::default_vector();
        let sources = [
            source("news", 200, -3.0, -3.5),      // clear loser: down a step
            source("insider", 20, 9.0, 4.0),      // too few signals: untouched
            source("government", 200, 1.0, 1.2), // not significant: untouched
            source("patent", 200, 5.0, 5.0),      // zeroed in code: never revived
        ];
        let (next, changes) = propose_weights(&d, &d, &sources);
        assert!((next.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert_eq!(next[5], 0.0, "patent stays off");
        let news = changes.iter().find(|c| c.dimension == "news_momentum").unwrap();
        assert!(news.to < news.from);
        assert!(news.why.contains("t = -3.5"), "{}", news.why);
        // The others only rescale to make room.
        assert!(next[0] > d[0] && next[0] < d[0] * 1.1);
    }

    #[test]
    fn weights_never_leave_their_bounds() {
        let d = pulse_weights::default_vector();
        let mut w = d;
        let sources = [source("news", 500, -5.0, -6.0)];
        for _ in 0..30 {
            w = propose_weights(&w, &d, &sources).0;
        }
        // Renormalizing lifts it a little above half its default, never below.
        assert!(w[2] >= d[2] * (1.0 - BOUND) - 1e-9, "news fell to {}", w[2]);
        assert!(w[2] < d[2] * 0.7);
    }

    #[test]
    fn every_weight_stays_in_its_band_over_many_weeks() {
        // The reviewer's case: three sources keep losing, one has no
        // evidence. Renormalizing must not pile the freed weight onto it.
        let d = pulse_weights::default_vector();
        for signs in [[-1.0, -1.0, -1.0], [1.0, -1.0, -1.0], [1.0, 1.0, -1.0], [-1.0, 1.0, 1.0]] {
            let sources = [
                source("insider", 200, signs[0] * 3.0, signs[0] * 4.0),
                source("news", 200, signs[1] * 3.0, signs[1] * 4.0),
                source("government", 200, signs[2] * 3.0, signs[2] * 4.0),
            ];
            let mut w = d;
            for _ in 0..30 {
                w = propose_weights(&w, &d, &sources).0;
                assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-9);
                for i in 0..8 {
                    if d[i] == 0.0 {
                        assert_eq!(w[i], 0.0);
                    } else {
                        assert!(w[i] >= d[i] * 0.5 - 2e-4 && w[i] <= d[i] * 1.5 + 2e-4, "{signs:?}: dim {i} = {} (default {})", w[i], d[i]);
                    }
                }
            }
        }
    }

    #[test]
    fn same_day_signals_count_as_one_day() {
        // 20 signals on one day are one draw of the market, not 20.
        let xs: Vec<(&str, f64)> = (0..20).map(|i| ("2026-09-01", 1.0 + i as f64 * 0.01)).collect();
        let (_, t, days) = by_day(&xs);
        assert_eq!((days, t), (1, 0.0));
        let (m, t, days) = by_day(&[("a", 1.0), ("a", 3.0), ("b", 2.0), ("c", 2.5)]);
        assert_eq!(days, 3);
        assert!((m - 2.1666).abs() < 1e-3 && t > 5.0);
    }

    #[test]
    fn repeated_skips_count_once_per_ten_trading_days() {
        let cal: Vec<String> = days(&[1.0; 30]).into_iter().map(|b| b.date).collect();
        let row = |i: usize, reason: &str| ("AAA".to_string(), cal[i].clone(), "skipped".to_string(), reason.to_string());
        let rows: Vec<_> = (0..25).map(|i| row(i, "too_calm")).chain([row(3, "earnings")]).collect();
        let kept = dedup_decisions(&rows, &cal);
        let calm: Vec<&String> = kept.iter().filter(|r| r.3 == "too_calm").map(|r| &r.1).collect();
        assert_eq!(calm, vec![&cal[0], &cal[10], &cal[20]]);
        assert_eq!(kept.len(), 4);
    }

    #[test]
    fn event_results_flip_shorts_and_attach_trades() {
        let r = |k: &str, d: &str, day: &str, x: f64| (k.to_string(), d.to_string(), day.to_string(), x);
        let rows = [
            r("news_surprise", "long", "2026-09-01", 2.0),
            r("news_surprise", "long", "2026-09-02", 4.0),
            r("news_surprise", "short", "2026-09-01", -1.0),
            r("news_surprise", "short", "2026-09-03", 3.0),
        ];
        let out = event_results(&rows, &[("news_surprise".into(), "short".into(), 1.5)]);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].direction.as_str(), out[0].signals, out[0].hold_days), ("long", 2, 5));
        assert!((out[0].avg_excess - 3.0).abs() < 1e-9);
        assert_eq!((out[0].trades, out[1].trades), (0, 1), "a trade shows on its own direction only");
        let bars = days(&[10.0, 11.0, 12.0, 13.0]);
        let (entry, exit, ret) = forward_n(&bars, &bars[0].date, 2).unwrap();
        assert_eq!((entry, exit), (bars[0].date.clone(), bars[2].date.clone()));
        let c = |i: usize| bars[i].close;
        assert!((ret - (c(2) / c(0) - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn one_rated_source_is_not_both_best_and_worst() {
        let h = headline(None, &[source("news", 40, 1.0, 1.0)], &[], &[]);
        assert_eq!(h.len(), 1, "{h:?}");
    }

    #[test]
    fn no_evidence_means_no_change() {
        let d = pulse_weights::default_vector();
        let (next, changes) = propose_weights(&d, &d, &[]);
        assert!(changes.is_empty());
        assert_eq!(next, d);
    }

    #[test]
    fn small_samples_are_shrunk_toward_zero() {
        let lucky = Stats::of(&[10.0, 12.0]).unwrap();
        assert!(lucky.shrunk() < 1.2);
        let many = Stats::of(&vec![2.0; 200]).unwrap();
        assert!(many.shrunk() > 1.8);
        let ep = |d: &str, f: Vec<&'static str>, x: f64| Episode { date: d.into(), fired: f, excess: x };
        let r = source_results(
            &[ep("2026-09-01", vec!["news"], 4.0), ep("2026-09-01", vec!["news", "search"], -2.0), ep("2026-09-02", vec!["insider"], -5.0)],
            &[(vec!["news"], 1.0)],
        );
        let news = r.iter().find(|s| s.dimension == "news").unwrap();
        assert_eq!((news.signals, news.trades, news.days), (2, 1, 1));
        assert!((news.win_rate - 11.0 / 22.0).abs() < 1e-9);
        // All signals averaged -1; news averaged +1, so +2 before shrinking.
        assert!((news.vs_all - 2.0 * 2.0 / 22.0).abs() < 1e-9);
    }

    #[test]
    fn gates_group_by_outcome_and_reason() {
        let rows = vec![
            ("skipped".to_string(), "too_calm".to_string(), 2.0),
            ("skipped".to_string(), "too_calm".to_string(), 4.0),
            ("bought".to_string(), "bought".to_string(), -1.0),
        ];
        let g = gate_results(&rows);
        assert_eq!(g[0].reason, "too_calm");
        assert_eq!(g[0].signals, 2);
        assert!((g[0].avg_excess - 3.0).abs() < 1e-9);
        assert_eq!(g[1].outcome, "bought");
    }

    #[test]
    fn decisions_count_once_per_ticker_day_and_outcome() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO trade_decisions (run_at, ticker, outcome, reason) VALUES
                 ('2026-10-01T10:00:00', 'AAA', 'skipped', 'too_calm'),
                 ('2026-10-01T13:00:00', 'AAA', 'skipped', 'earnings'),
                 ('2026-10-01T13:00:00', '', 'run_stopped', 'max_positions'),
                 ('2026-10-02T10:00:00', 'AAA', 'bought', 'bought'),
                 ('2026-09-01T10:00:00', 'BBB', 'skipped', 'too_thin');",
        )
        .unwrap();
        let d = load_decisions(&conn, "2026-09-15").unwrap();
        assert_eq!(
            d,
            vec![
                ("AAA".into(), "2026-10-01".into(), "skipped".into(), "too_calm".into()),
                ("AAA".into(), "2026-10-02".into(), "bought".into(), "bought".into()),
            ]
        );
    }

    #[test]
    fn whole_trade_return_counts_half_closes_and_scale_ins() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        // $1,000 in at $10, $500 more at $10, half sold at $12 (+$150), the
        // rest at $11 (+$75): $225 on $1,500 = +15%, though the last leg alone
        // was +10%.
        conn.execute(
            "INSERT INTO paper_trades (ticker, direction, entry_price, entry_date, position_size, confidence,
                 signal_profile, status, exit_price, exit_date, exit_reason, pnl, pnl_pct, entry_context)
             VALUES ('AAA', 'long', 10, '2026-09-01T10:00:00', 750, 0.4, '{\"news\":0.5}', 'closed', 11,
                 '2026-09-10T10:00:00', 'signal_decay', 225, 10, '{\"notional\":1000}')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO trade_events (trade_id, at, kind, price, qty) VALUES (?1, '2026-09-02T10:00:00', 'scale_in', 10, 50)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO paper_trades (ticker, direction, entry_price, entry_date, position_size, confidence,
                 signal_profile, status, exit_price, exit_date, exit_reason, pnl, pnl_pct)
             VALUES ('BBB', 'long', 10, '2026-09-01', 1000, 0.4, '{}', 'closed', 9, '2026-09-05', 'hard_stop', -100, -10),
                    ('CCC', 'long', 10, '2026-09-01', 1000, 0.4, '{}', 'expired', 9, '2026-09-05', 'merged_into_1', 0, 0)",
            [],
        )
        .unwrap();
        let t = trades_to_review(&conn).unwrap();
        assert_eq!(t.len(), 2, "merged placeholders are not trades");
        let (bbb, aaa) = (&t[0], &t[1]);
        assert_eq!((aaa.ticker.as_str(), bbb.ticker.as_str()), ("AAA", "BBB"), "oldest exit first");
        assert!((aaa.return_pct - 15.0).abs() < 1e-9, "{}", aaa.return_pct);
        assert!((bbb.return_pct + 10.0).abs() < 1e-9, "no context: the last leg's return");

        // A final review takes the trade off the list.
        let mut r = review(&trade("2026-09-01", "2026-09-05", 10.0, 9.0, "hard_stop"), &days(&[10.0; 30]), &days(&[1.0; 30])).unwrap();
        r.trade_id = bbb.id;
        store_review(&conn, &r).unwrap();
        assert_eq!(trades_to_review(&conn).unwrap().len(), 1);
        assert_eq!(load_reviews(&conn).unwrap()[0].lessons, r.lessons);
    }

    #[test]
    fn headline_flags_a_filter_that_skips_winners() {
        let gates = [GateResult { reason: "too_calm".into(), outcome: "skipped".into(), signals: 40, avg_excess: 2.5, t: 2.4 }];
        let h = headline(None, &[], &[], &gates);
        assert!(h[0].contains("too_calm") && h[0].contains("+2.5"), "{:?}", h);
    }
}
