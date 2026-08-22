use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct BacktestConfig {
    pub start_date: String,
    pub end_date: String,
    pub min_score: f64,        // minimum compound_score to trigger entry
    pub stop_loss_pct: f64,    // e.g. -10.0
    pub take_profit_pct: f64,  // e.g. 15.0
    pub max_hold_days: i64,    // e.g. 90
    pub max_positions: usize,  // e.g. 10
    pub position_size_pct: f64, // e.g. 5.0 (% of *current* equity — compounds)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonthlyReturn {
    pub month: String,       // "YYYY-MM"
    pub pnl_dollars: f64,
    pub pnl_pct: f64,        // on equity at start of month
    pub trade_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestResult {
    pub config_summary: String,
    pub total_signals: usize,
    /// Distinct tickers among those signals.
    #[serde(default)]
    pub tickers_signalled: usize,
    /// Of those, how many ever had a price bar on a day they signalled — the
    /// only ones this backtest could admit. The gap is invisible in every other
    /// number here. Not a live-trading limit: the live path prices entries off
    /// Alpaca and never reads entity_prices.
    #[serde(default)]
    pub tickers_tradable: usize,
    /// Closed exits only — positions force-closed when the price data ran out
    /// are counted in `open_at_end`, not here. Every realized statistic below
    /// is computed over this sample.
    pub trades_taken: usize,
    /// Positions still open at the end of the walk, marked to their last bar.
    /// They contribute to the equity curve and total return but are not outcomes.
    #[serde(default)]
    pub open_at_end: usize,
    pub trades_won: usize,
    pub trades_lost: usize,
    pub hit_rate: f64,
    pub avg_return_pct: f64,
    pub total_return_pct: f64,
    pub max_drawdown_pct: f64,
    pub sharpe_ratio: f64,
    pub avg_holding_days: f64,
    pub starting_equity: f64,
    pub ending_equity: f64,
    pub trades: Vec<BacktestTrade>,
    #[serde(default)]
    pub equity_curve: Vec<EquityPoint>,
    #[serde(default)]
    pub monthly_returns: Vec<MonthlyReturn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub ticker: String,
    pub entity_name: String,
    pub entry_date: String,
    pub entry_price: f64,
    pub exit_date: String,
    pub exit_price: f64,
    pub pnl_pct: f64,
    pub pnl_dollars: f64,
    pub holding_days: i64,
    pub exit_reason: String,
    pub compound_score: f64,
    pub signal_profile: String,
}

// Computed metrics block that gets serialized into the `details` JSON alongside
// `trades`. Used so `get_backtest_history` can rehydrate without recomputing
// under the wrong (pre-compounding) model.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DetailsBlob {
    trades: Vec<BacktestTrade>,
    starting_equity: f64,
    ending_equity: f64,
    total_return_pct: f64,
    sharpe_ratio: f64,
    max_drawdown_pct: f64,
    equity_curve: Vec<EquityPoint>,
    monthly_returns: Vec<MonthlyReturn>,
}

// ---------------------------------------------------------------------------
// Candidate from cross_signals history
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SignalCandidate {
    ticker: String,
    entity_name: String,
    compound_score: f64,
    signal_date: String,
    signal_profile: String,
}

#[derive(Clone)]
struct OpenPosition {
    ticker: String,
    entity_name: String,
    entry_date: String,
    entry_price: f64,
    position_value: f64, // dollar value at entry (compounded off current equity)
    compound_score: f64,
    signal_profile: String,
}

// ---------------------------------------------------------------------------
// Main backtest engine — calendar walk, equity compounds.
// ---------------------------------------------------------------------------

pub fn run_backtest(conn: &Connection, config: BacktestConfig) -> Result<BacktestResult, String> {
    // 30-day hard floor: count distinct trading days in entity_prices covering
    // *any* ticker in the window. Cheap sanity check for "do we have data to work with".
    let trading_days = count_trading_days(conn, &config.start_date, &config.end_date)?;
    if trading_days < 30 {
        return Err(format!(
            "Backtest requires at least 30 trading days of price data in the window; found {}. Widen the date range.",
            trading_days
        ));
    }

    let candidates = get_signal_candidates(conn, &config)?;
    let total_signals = candidates.len();

    if candidates.is_empty() {
        return Ok(empty_result(&config));
    }

    // Preload prices for all candidate tickers across the extended window.
    // Extended end = end_date + max_hold_days (give trades room to close).
    let extended_end = add_days(&config.end_date, config.max_hold_days)
        .unwrap_or_else(|| config.end_date.clone());
    let tickers: Vec<String> = candidates
        .iter()
        .map(|c| c.ticker.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let prices = load_prices(conn, &tickers, &config.start_date, &extended_end)?;

    // Build union of all trading dates we have prices for (any ticker).
    // We walk these in chronological order.
    let trading_dates: Vec<String> = prices
        .values()
        .flat_map(|m| m.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if trading_dates.is_empty() {
        return Ok(empty_result(&config));
    }

    // A ticker can only ever be admitted on a day it both signals AND has a
    // bar, because entry takes that day's close and the walk refuses to peek
    // forward. Tickers that never line up are silently invisible to the whole
    // backtest — on the live data, eight of the twenty-three signalled names
    // have no row in entity_prices at all (NU alone signalled 44 times), and
    // three more never had a bar on a day they signalled. Without this count
    // the result reads as a verdict on the signal, when it is a verdict on the
    // half of the signal's names that happen to have price history.
    //
    // The live trader does not share this blind spot: it prices entries off
    // Alpaca and never reads entity_prices, so it can and does trade names
    // this backtest cannot see.
    let tickers_signalled = tickers.len();
    let tickers_tradable = candidates
        .iter()
        .filter(|c| prices.get(&c.ticker).is_some_and(|m| m.contains_key(&c.signal_date)))
        .map(|c| c.ticker.as_str())
        .collect::<BTreeSet<_>>()
        .len();

    // Group candidates by signal_date for O(1) admission lookup.
    let mut candidates_by_date: HashMap<String, Vec<SignalCandidate>> = HashMap::new();
    for c in &candidates {
        candidates_by_date
            .entry(c.signal_date.clone())
            .or_default()
            .push(c.clone());
    }

    let starting_equity = 100_000.0_f64;
    // `equity` tracks realized-only equity — the pool we size new positions off.
    // Unrealized P&L from still-open positions is folded in only for the curve
    // snapshot, not for sizing decisions.
    let mut equity = starting_equity;
    let mut realized_pnl = 0.0_f64;
    let mut open_positions: HashMap<String, OpenPosition> = HashMap::new();
    let mut trades: Vec<BacktestTrade> = Vec::new();
    let mut equity_curve: Vec<EquityPoint> = Vec::new();

    for date in &trading_dates {
        // (1) Process opens on *this* day — check SL, TP, then max_hold. Any
        // exit frees the slot immediately so a same-day admit can reuse it.
        let open_tickers: Vec<String> = open_positions.keys().cloned().collect();
        for ticker in open_tickers {
            let pos = match open_positions.get(&ticker) {
                Some(p) => p.clone(),
                None => continue,
            };

            let bar = prices.get(&ticker).and_then(|m| m.get(date));
            let bar = match bar {
                Some(b) => *b,
                None => continue, // no bar for this ticker today — hold
            };
            let (_close, high, low) = bar;

            let entry_naive = match chrono::NaiveDate::parse_from_str(&pos.entry_date, "%Y-%m-%d") {
                Ok(d) => d,
                Err(_) => continue,
            };
            let today_naive = match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                Ok(d) => d,
                Err(_) => continue,
            };
            let days_held = (today_naive - entry_naive).num_days();

            // Intraday stop-loss (negative pct, e.g. -10)
            let low_pct = ((low - pos.entry_price) / pos.entry_price) * 100.0;
            if low_pct <= config.stop_loss_pct {
                let exit_price = pos.entry_price * (1.0 + config.stop_loss_pct / 100.0);
                let pnl_dollars = pos.position_value * config.stop_loss_pct / 100.0;
                realized_pnl += pnl_dollars;
                equity += pnl_dollars;
                trades.push(close_trade(&pos, date, exit_price, config.stop_loss_pct, pnl_dollars, days_held, "stop_loss"));
                open_positions.remove(&ticker);
                continue;
            }

            // Intraday take-profit (positive pct, e.g. 15)
            let high_pct = ((high - pos.entry_price) / pos.entry_price) * 100.0;
            if high_pct >= config.take_profit_pct {
                let exit_price = pos.entry_price * (1.0 + config.take_profit_pct / 100.0);
                let pnl_dollars = pos.position_value * config.take_profit_pct / 100.0;
                realized_pnl += pnl_dollars;
                equity += pnl_dollars;
                trades.push(close_trade(&pos, date, exit_price, config.take_profit_pct, pnl_dollars, days_held, "take_profit"));
                open_positions.remove(&ticker);
                continue;
            }

            // Max-hold close at today's close
            if days_held >= config.max_hold_days {
                let close = bar.0;
                let pnl_pct = ((close - pos.entry_price) / pos.entry_price) * 100.0;
                let pnl_dollars = pos.position_value * pnl_pct / 100.0;
                realized_pnl += pnl_dollars;
                equity += pnl_dollars;
                trades.push(close_trade(&pos, date, close, pnl_pct, pnl_dollars, days_held, "max_hold"));
                open_positions.remove(&ticker);
                continue;
            }
        }

        // (2) Admit new candidates that signal today, in score-desc order.
        if let Some(mut day_candidates) = candidates_by_date.remove(date) {
            day_candidates.sort_by(|a, b| b.compound_score.partial_cmp(&a.compound_score).unwrap_or(std::cmp::Ordering::Equal));
            for cand in day_candidates {
                if open_positions.contains_key(&cand.ticker) { continue; }
                if open_positions.len() >= config.max_positions { break; }
                // Need today's close as entry price. If no bar for this ticker
                // today, skip — we don't peek forward.
                let bar = match prices.get(&cand.ticker).and_then(|m| m.get(date)) {
                    Some(b) => *b,
                    None => continue,
                };
                let entry_price = bar.0;
                if entry_price <= 0.0 { continue; }

                // Size off mark-to-market equity so wins compound while open.
                // `equity` here is realized; we add unrealized from still-open
                // positions to get the true "book value" we'd size against.
                let unrealized_now = mark_to_market(&open_positions, &prices, date);
                let sizing_equity = equity + unrealized_now;
                let position_value = sizing_equity * (config.position_size_pct / 100.0);
                if position_value <= 0.0 { continue; }

                open_positions.insert(cand.ticker.clone(), OpenPosition {
                    ticker: cand.ticker.clone(),
                    entity_name: cand.entity_name.clone(),
                    entry_date: date.clone(),
                    entry_price,
                    position_value,
                    compound_score: cand.compound_score,
                    signal_profile: cand.signal_profile.clone(),
                });
            }
        }

        // (3) Snapshot equity for the curve (realized + unrealized mark-to-market).
        let unrealized = mark_to_market(&open_positions, &prices, date);
        equity_curve.push(EquityPoint { date: date.clone(), value: starting_equity + realized_pnl + unrealized });
    }

    // Force-close anything still open at the last trading date with data.
    if let Some(last_date) = trading_dates.last().cloned() {
        let still_open: Vec<String> = open_positions.keys().cloned().collect();
        for ticker in still_open {
            let pos = match open_positions.remove(&ticker) {
                Some(p) => p,
                None => continue,
            };
            // Use the latest bar we have for this ticker (may be earlier than last_date).
            let (exit_date, exit_price) = latest_bar(&prices, &ticker, &last_date).unwrap_or((last_date.clone(), pos.entry_price));
            let pnl_pct = ((exit_price - pos.entry_price) / pos.entry_price) * 100.0;
            let pnl_dollars = pos.position_value * pnl_pct / 100.0;
            realized_pnl += pnl_dollars;
            let entry_naive = chrono::NaiveDate::parse_from_str(&pos.entry_date, "%Y-%m-%d").ok();
            let exit_naive = chrono::NaiveDate::parse_from_str(&exit_date, "%Y-%m-%d").ok();
            let days_held = match (entry_naive, exit_naive) {
                (Some(a), Some(b)) => (b - a).num_days(),
                _ => 0,
            };
            trades.push(close_trade(&pos, &exit_date, exit_price, pnl_pct, pnl_dollars, days_held, DATA_END));
        }
        // Don't overwrite the final curve point — the mark-to-market already
        // equals realized + unrealized at that date, and the force-close is
        // just a reclassification that preserves total value. Overwriting
        // would inject a spurious "last day return" into Sharpe.
    }

    // ---------------- Metrics ----------------

    // A `data_end` row is not an outcome — it is an open position marked to the
    // last bar we have. Counting one as a win says a trade succeeded when all we
    // know is that it was up on the day the price data stopped; a position at
    // +2% that would have hit the stop next week scores identically to one that
    // actually took profit. Realized statistics run over closed exits only.
    // They stay in `trades` (the equity curve and total return need their marks,
    // and the trade list should still show them) — they just aren't outcomes.
    let RealizedMetrics {
        trades_taken, open_at_end, trades_won, trades_lost,
        hit_rate, avg_return_pct, avg_holding_days,
    } = realized_metrics(&trades);

    let ending_equity = starting_equity + realized_pnl;
    let total_return_pct = (ending_equity - starting_equity) / starting_equity * 100.0;

    let sharpe_ratio = compute_sharpe(&equity_curve);
    let max_drawdown_pct = compute_max_drawdown(&equity_curve);
    let monthly_returns = compute_monthly_returns(&equity_curve, &trades);

    let config_summary = format!(
        "Score>{:.0}% | SL {:.0}% | TP {:.0}% | Hold {}d | {} pos | {:.0}% size | {} to {}",
        config.min_score * 100.0, config.stop_loss_pct, config.take_profit_pct,
        config.max_hold_days, config.max_positions, config.position_size_pct,
        config.start_date, config.end_date,
    );

    let result = BacktestResult {
        config_summary: config_summary.clone(),
        total_signals,
        tickers_signalled,
        tickers_tradable,
        trades_taken,
        open_at_end,
        trades_won,
        trades_lost,
        hit_rate,
        avg_return_pct,
        total_return_pct,
        max_drawdown_pct,
        sharpe_ratio,
        avg_holding_days,
        starting_equity,
        ending_equity,
        trades: trades.clone(),
        equity_curve: equity_curve.clone(),
        monthly_returns: monthly_returns.clone(),
    };

    save_result(conn, &config_summary, total_signals, &result, hit_rate, avg_return_pct, max_drawdown_pct, sharpe_ratio, avg_holding_days)?;

    Ok(result)
}

/// Get past backtest results. Reads the rich `DetailsBlob` from `details` JSON
/// when available; falls back to approximating from raw trades for legacy rows.
pub fn get_backtest_history(conn: &Connection, limit: usize) -> Result<Vec<BacktestResult>, String> {
    let mut stmt = conn.prepare(
        "SELECT signal_profile, start_date, end_date, total_signals, hit_rate,
                avg_return, max_drawdown, sharpe_ratio, avg_holding_days, details
         FROM backtest_results ORDER BY created_at DESC LIMIT ?1"
    ).map_err(|e| e.to_string())?;

    let results = stmt.query_map([limit as i64], |row| {
        let config_summary: String = row.get(0)?;
        let total_signals: i64 = row.get(3)?;
        let hit_rate: f64 = row.get(4)?;
        let avg_return: f64 = row.get::<_, Option<f64>>(5)?.unwrap_or(0.0);
        let max_drawdown_col: f64 = row.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
        let sharpe_col: f64 = row.get::<_, Option<f64>>(7)?.unwrap_or(0.0);
        let avg_hold: f64 = row.get::<_, Option<f64>>(8)?.unwrap_or(0.0);
        let details_json: Option<String> = row.get(9)?;

        // Try new shape first.
        if let Some(json) = &details_json {
            if let Ok(blob) = serde_json::from_str::<DetailsBlob>(json) {
                // Recompute the realized statistics from the stored trades rather
                // than reading the stored columns. Rows written before `data_end`
                // was excluded hold a hit rate, average return and average hold
                // computed over open marks as well as closed exits — replaying the
                // blob under the current rule is what keeps a stored result and a
                // fresh re-run of the same backtest reporting the same numbers.
                let m = realized_metrics(&blob.trades);
                return Ok(BacktestResult {
                    config_summary,
                    total_signals: total_signals as usize,
                    // Not persisted — backtest_results has no column for it and
                    // recomputing would need the signal window replayed. Zero
                    // reads as "unknown" rather than a fabricated coverage claim.
                    tickers_signalled: 0,
                    tickers_tradable: 0,
                    trades_taken: m.trades_taken,
                    open_at_end: m.open_at_end,
                    trades_won: m.trades_won,
                    trades_lost: m.trades_lost,
                    hit_rate: m.hit_rate,
                    avg_return_pct: m.avg_return_pct,
                    total_return_pct: blob.total_return_pct,
                    max_drawdown_pct: blob.max_drawdown_pct,
                    sharpe_ratio: blob.sharpe_ratio,
                    avg_holding_days: m.avg_holding_days,
                    starting_equity: blob.starting_equity,
                    ending_equity: blob.ending_equity,
                    trades: blob.trades,
                    equity_curve: blob.equity_curve,
                    monthly_returns: blob.monthly_returns,
                });
            }
        }

        // Legacy fallback: trades-only payload, no compounding, flat 100k equity.
        let legacy_trades: Vec<BacktestTrade> = details_json
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default();
        // Recompute from the trades when there are any. A row whose `details`
        // is missing or unparseable has no trades to replay, and zeroing its
        // metrics would destroy the only record of it — so those fall back to
        // the stored columns, which is all such a row has ever had.
        let m = realized_metrics(&legacy_trades);
        let have_trades = !legacy_trades.is_empty();
        let total_pnl: f64 = legacy_trades.iter().map(|t| t.pnl_dollars).sum();

        Ok(BacktestResult {
            config_summary,
            total_signals: total_signals as usize,
            tickers_signalled: 0,
            tickers_tradable: 0,
            trades_taken: m.trades_taken,
            open_at_end: m.open_at_end,
            trades_won: m.trades_won,
            trades_lost: m.trades_lost,
            hit_rate: if have_trades { m.hit_rate } else { hit_rate },
            avg_return_pct: if have_trades { m.avg_return_pct } else { avg_return },
            total_return_pct: total_pnl / 100_000.0 * 100.0,
            max_drawdown_pct: max_drawdown_col,
            sharpe_ratio: sharpe_col,
            avg_holding_days: if have_trades { m.avg_holding_days } else { avg_hold },
            starting_equity: 100_000.0,
            ending_equity: 100_000.0 + total_pnl,
            trades: legacy_trades,
            equity_curve: Vec::new(),
            monthly_returns: Vec::new(),
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn count_trading_days(conn: &Connection, start: &str, end: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(DISTINCT date) FROM entity_prices WHERE date >= ?1 AND date <= ?2",
        params![start, end],
        |row| row.get::<_, i64>(0),
    ).map_err(|e| e.to_string())
}

/// Exit reason written for a position that was still open when the price data
/// ran out. Not a strategy exit — see `split_closed`.
pub(crate) const DATA_END: &str = "data_end";

/// The realized statistics, computed over closed exits only.
pub(crate) struct RealizedMetrics {
    pub trades_taken: usize,
    pub open_at_end: usize,
    pub trades_won: usize,
    pub trades_lost: usize,
    pub hit_rate: f64,
    pub avg_return_pct: f64,
    pub avg_holding_days: f64,
}

/// Single source of truth for every count and average derived from trades.
/// `run_backtest` computes them from the live walk and `get_backtest_history`
/// replays them from the stored blob — they call this so the two cannot drift,
/// which is what would otherwise make a stored result and a re-run of the same
/// backtest disagree.
pub(crate) fn realized_metrics(trades: &[BacktestTrade]) -> RealizedMetrics {
    let (closed, open): (Vec<&BacktestTrade>, Vec<&BacktestTrade>) =
        trades.iter().partition(|t| t.exit_reason != DATA_END);
    let trades_taken = closed.len();
    let trades_won = closed.iter().filter(|t| t.pnl_pct > 0.0).count();
    let mean = |v: Vec<f64>| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    RealizedMetrics {
        trades_taken,
        open_at_end: open.len(),
        trades_won,
        trades_lost: trades_taken - trades_won,
        hit_rate: if trades_taken > 0 { trades_won as f64 / trades_taken as f64 * 100.0 } else { 0.0 },
        avg_return_pct: mean(closed.iter().map(|t| t.pnl_pct).collect()),
        avg_holding_days: mean(closed.iter().map(|t| t.holding_days as f64).collect()),
    }
}

fn add_days(date_str: &str, days: i64) -> Option<String> {
    let d = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
    Some(d.checked_add_signed(chrono::Duration::days(days))?.format("%Y-%m-%d").to_string())
}

fn close_trade(pos: &OpenPosition, exit_date: &str, exit_price: f64, pnl_pct: f64, pnl_dollars: f64, days_held: i64, reason: &str) -> BacktestTrade {
    BacktestTrade {
        ticker: pos.ticker.clone(),
        entity_name: pos.entity_name.clone(),
        entry_date: pos.entry_date.clone(),
        entry_price: pos.entry_price,
        exit_date: exit_date.to_string(),
        exit_price,
        pnl_pct,
        pnl_dollars,
        holding_days: days_held,
        exit_reason: reason.to_string(),
        compound_score: pos.compound_score,
        signal_profile: pos.signal_profile.clone(),
    }
}

// Sum of unrealized P&L across all open positions on a given date.
fn mark_to_market(
    open: &HashMap<String, OpenPosition>,
    prices: &HashMap<String, HashMap<String, (f64, f64, f64)>>,
    date: &str,
) -> f64 {
    let mut total = 0.0;
    for pos in open.values() {
        let close = prices
            .get(&pos.ticker)
            .and_then(|m| m.get(date))
            .map(|b| b.0)
            .unwrap_or(pos.entry_price);
        let pnl_pct = ((close - pos.entry_price) / pos.entry_price) * 100.0;
        total += pos.position_value * pnl_pct / 100.0;
    }
    total
}

fn latest_bar(
    prices: &HashMap<String, HashMap<String, (f64, f64, f64)>>,
    ticker: &str,
    not_after: &str,
) -> Option<(String, f64)> {
    let map = prices.get(ticker)?;
    let mut dates: Vec<&String> = map.keys().filter(|d| d.as_str() <= not_after).collect();
    dates.sort();
    dates.last().map(|d| ((*d).clone(), map.get(*d).map(|b| b.0).unwrap_or(0.0)))
}

fn get_signal_candidates(conn: &Connection, config: &BacktestConfig) -> Result<Vec<SignalCandidate>, String> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(et.ticker, cs.ticker) as ticker,
                COALESCE(e.name, cs.ticker, '') as entity_name,
                cs.compound_score, date(cs.computed_at) as signal_date,
                cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                cs.government_signal, cs.search_trend, cs.patent_signal,
                cs.supply_chain, cs.political_signal
         FROM cross_signals cs
         LEFT JOIN entity_tickers et ON et.entity_id = cs.entity_id
         LEFT JOIN entities e ON e.id = cs.entity_id
         WHERE cs.convergence_detected = 1
           AND cs.compound_score >= ?1
           AND date(cs.computed_at) >= ?2
           AND date(cs.computed_at) <= ?3
           AND COALESCE(et.ticker, cs.ticker) IS NOT NULL
         ORDER BY date(cs.computed_at) ASC, cs.compound_score DESC"
    ).map_err(|e| e.to_string())?;

    let candidates = stmt.query_map(
        params![config.min_score, config.start_date, config.end_date],
        |row| {
            let profile = serde_json::json!({
                "insider": row.get::<_, f64>(4).unwrap_or(0.0),
                "institutional": row.get::<_, f64>(5).unwrap_or(0.0),
                "news": row.get::<_, f64>(6).unwrap_or(0.0),
                "government": row.get::<_, f64>(7).unwrap_or(0.0),
                "search": row.get::<_, f64>(8).unwrap_or(0.0),
                "patent": row.get::<_, f64>(9).unwrap_or(0.0),
                "supply_chain": row.get::<_, f64>(10).unwrap_or(0.0),
                "political": row.get::<_, f64>(11).unwrap_or(0.0),
            });

            Ok(SignalCandidate {
                ticker: row.get(0)?,
                entity_name: row.get(1)?,
                compound_score: row.get(2)?,
                signal_date: row.get(3)?,
                signal_profile: profile.to_string(),
            })
        },
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(candidates)
}

// Returns a nested map: ticker -> date -> (close, high, low).
fn load_prices(
    conn: &Connection,
    tickers: &[String],
    start: &str,
    end: &str,
) -> Result<HashMap<String, HashMap<String, (f64, f64, f64)>>, String> {
    if tickers.is_empty() {
        return Ok(HashMap::new());
    }
    // Build `?,?,?` placeholders. We cap batch size so we don't blow past
    // SQLITE_MAX_VARIABLE_NUMBER on pathological runs. 500 tickers is plenty.
    let mut out: HashMap<String, HashMap<String, (f64, f64, f64)>> = HashMap::new();
    // COALESCE high/low to close: the daily quote-only fetch writes close-only rows,
    // and the Alpaca candle backfill uses INSERT OR IGNORE (won't overwrite them) —
    // without this, row.get::<_,f64> on a NULL high/low errors and the whole day
    // silently vanishes from `prices`, which the entry logic then reads as "no bar
    // today" for a date that actually has a valid close.
    for chunk in tickers.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT ticker, date, close, COALESCE(high, close), COALESCE(low, close) FROM entity_prices
             WHERE ticker IN ({}) AND date >= ? AND date <= ?
             ORDER BY date ASC",
            placeholders
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut values: Vec<&dyn rusqlite::ToSql> = chunk.iter().map(|t| t as &dyn rusqlite::ToSql).collect();
        values.push(&start);
        values.push(&end);
        let rows = stmt.query_map(values.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        }).map_err(|e| e.to_string())?;

        for row in rows.filter_map(|r| r.ok()) {
            let (ticker, date, close, high, low) = row;
            out.entry(ticker).or_default().insert(date, (close, high, low));
        }
    }
    Ok(out)
}

// Daily-return Sharpe, annualized by sqrt(252). Skips non-finite / zero-div.
fn compute_sharpe(curve: &[EquityPoint]) -> f64 {
    if curve.len() < 2 { return 0.0; }
    let mut daily_returns: Vec<f64> = Vec::with_capacity(curve.len() - 1);
    for pair in curve.windows(2) {
        let prev = pair[0].value;
        let curr = pair[1].value;
        if prev > 0.0 {
            daily_returns.push((curr - prev) / prev);
        }
    }
    if daily_returns.len() < 2 { return 0.0; }
    let n = daily_returns.len() as f64;
    let mean = daily_returns.iter().sum::<f64>() / n;
    let variance = daily_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std_dev = variance.sqrt();
    if std_dev <= 0.0 || !std_dev.is_finite() { return 0.0; }
    (mean / std_dev) * 252.0_f64.sqrt()
}

// Walk the curve in calendar order, track peak, report worst trough.
fn compute_max_drawdown(curve: &[EquityPoint]) -> f64 {
    if curve.is_empty() { return 0.0; }
    let mut peak = curve[0].value;
    let mut max_dd = 0.0;
    for p in curve {
        if p.value > peak { peak = p.value; }
        if peak > 0.0 {
            let dd = (peak - p.value) / peak * 100.0;
            if dd > max_dd { max_dd = dd; }
        }
    }
    max_dd
}

// Derive monthly returns from the equity curve: pct change between first and
// last equity snapshot within each month. `trade_count` = trades that *exited*
// in that month (realized events), which is what the tooltip represents.
fn compute_monthly_returns(curve: &[EquityPoint], trades: &[BacktestTrade]) -> Vec<MonthlyReturn> {
    if curve.is_empty() { return Vec::new(); }
    let mut by_month: std::collections::BTreeMap<String, (f64, f64)> = std::collections::BTreeMap::new();
    for p in curve {
        let month = if p.date.len() >= 7 { p.date[..7].to_string() } else { continue };
        by_month
            .entry(month)
            .and_modify(|(_, last)| *last = p.value)
            .or_insert((p.value, p.value));
    }
    let mut exits_by_month: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for t in trades {
        if t.exit_date.len() >= 7 {
            *exits_by_month.entry(t.exit_date[..7].to_string()).or_insert(0) += 1;
        }
    }
    let mut out = Vec::with_capacity(by_month.len());
    for (month, (first, last)) in by_month {
        let pnl_dollars = last - first;
        let pnl_pct = if first > 0.0 { pnl_dollars / first * 100.0 } else { 0.0 };
        let trade_count = *exits_by_month.get(&month).unwrap_or(&0);
        out.push(MonthlyReturn { month, pnl_dollars, pnl_pct, trade_count });
    }
    out
}

fn save_result(
    conn: &Connection, config_summary: &str, total_signals: usize,
    result: &BacktestResult, hit_rate: f64, avg_return: f64,
    max_drawdown: f64, sharpe: f64, avg_hold: f64,
) -> Result<(), String> {
    let blob = DetailsBlob {
        trades: result.trades.clone(),
        starting_equity: result.starting_equity,
        ending_equity: result.ending_equity,
        total_return_pct: result.total_return_pct,
        sharpe_ratio: result.sharpe_ratio,
        max_drawdown_pct: result.max_drawdown_pct,
        equity_curve: result.equity_curve.clone(),
        monthly_returns: result.monthly_returns.clone(),
    };
    // An empty details blob is what the trade-detail view reads, so storing ""
    // would persist a backtest row that renders as having made no trades. Better
    // to fail the run than to save a result that lies about itself.
    let details = serde_json::to_string(&blob)
        .map_err(|e| format!("failed to serialize backtest details: {e}"))?;
    let parts: Vec<&str> = config_summary.split(" | ").collect();
    let date_part = parts.last().unwrap_or(&"");
    let dates: Vec<&str> = date_part.split(" to ").collect();
    let start = dates.first().unwrap_or(&"");
    let end = dates.last().unwrap_or(&"");

    conn.execute(
        "INSERT INTO backtest_results (signal_profile, start_date, end_date, total_signals,
            hit_rate, avg_return, max_drawdown, sharpe_ratio, avg_holding_days, details)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![config_summary, start, end, total_signals as i64,
            hit_rate, avg_return, max_drawdown, sharpe, avg_hold, details],
    )
    // Was `.ok()`. The whole point of this function is to persist the run; a
    // discarded error meant the backtest "succeeded" and was simply absent from
    // history afterwards, with nothing to explain the gap.
    .map_err(|e| format!("failed to save backtest result: {e}"))?;

    Ok(())
}

fn empty_result(config: &BacktestConfig) -> BacktestResult {
    BacktestResult {
        config_summary: format!("{} to {} — no convergence signals found", config.start_date, config.end_date),
        total_signals: 0, tickers_signalled: 0, tickers_tradable: 0, trades_taken: 0, open_at_end: 0, trades_won: 0, trades_lost: 0,
        hit_rate: 0.0, avg_return_pct: 0.0, total_return_pct: 0.0,
        max_drawdown_pct: 0.0, sharpe_ratio: 0.0, avg_holding_days: 0.0,
        starting_equity: 100_000.0, ending_equity: 100_000.0,
        trades: Vec::new(),
        equity_curve: Vec::new(),
        monthly_returns: Vec::new(),
    }
}

#[cfg(test)]
mod realized_metrics_tests {
    use super::*;

    fn trade(pnl_pct: f64, holding_days: i64, exit_reason: &str) -> BacktestTrade {
        BacktestTrade {
            ticker: "T".into(),
            entity_name: "T Inc".into(),
            entry_date: "2026-01-01".into(),
            entry_price: 100.0,
            exit_date: "2026-02-01".into(),
            exit_price: 100.0 * (1.0 + pnl_pct / 100.0),
            pnl_pct,
            pnl_dollars: 50.0 * pnl_pct,
            holding_days,
            exit_reason: exit_reason.into(),
            compound_score: 0.5,
            signal_profile: "{}".into(),
        }
    }

    /// The shape the auto-backtest actually produced: 22 rows, of which 6 were
    /// positions still open when the price data ran out. Counting those as
    /// outcomes is what made a 36.4% hit rate out of a sample that was never 22
    /// closed trades.
    #[test]
    fn open_marks_are_excluded_from_every_realized_statistic() {
        let trades = vec![
            trade(15.0, 10, "take_profit"),
            trade(-10.0, 20, "stop_loss"),
            // Three open marks, all green and all long-held. If they leak into
            // the statistics they drag the hit rate up and the average hold out.
            trade(2.0, 100, DATA_END),
            trade(3.0, 100, DATA_END),
            trade(4.0, 100, DATA_END),
        ];

        let m = realized_metrics(&trades);

        assert_eq!(m.trades_taken, 2, "sample is the closed exits, not every row");
        assert_eq!(m.open_at_end, 3, "the open marks are reported, not discarded");
        assert_eq!(m.trades_won, 1);
        assert_eq!(m.trades_lost, 1);
        // Contaminated, this would be 4/5 = 80%.
        assert!((m.hit_rate - 50.0).abs() < 1e-9, "hit rate was {}", m.hit_rate);
        // Contaminated, this would be (15-10+2+3+4)/5 = +2.8%.
        assert!((m.avg_return_pct - 2.5).abs() < 1e-9, "avg return was {}", m.avg_return_pct);
        // Contaminated, this would be (10+20+300)/5 = 66 days.
        assert!((m.avg_holding_days - 15.0).abs() < 1e-9, "avg hold was {}", m.avg_holding_days);
    }

    #[test]
    fn a_walk_that_closed_nothing_reports_no_hit_rate_rather_than_a_flattering_one() {
        let trades = vec![trade(8.0, 40, DATA_END), trade(12.0, 40, DATA_END)];
        let m = realized_metrics(&trades);
        // Contaminated, two green marks read as a 100% hit rate off zero closes.
        assert_eq!(m.trades_taken, 0);
        assert_eq!(m.open_at_end, 2);
        assert_eq!(m.hit_rate, 0.0);
        assert_eq!(m.avg_return_pct, 0.0);
        assert_eq!(m.avg_holding_days, 0.0);
    }

    /// `run_backtest` and `get_backtest_history` must agree. They agree only
    /// because both call `realized_metrics`; this pins that they see the same
    /// trade list the same way.
    #[test]
    fn stored_and_fresh_results_agree_on_the_same_trades() {
        let trades = vec![
            trade(15.0, 10, "take_profit"),
            trade(-10.0, 20, "stop_loss"),
            trade(1.0, 60, "max_hold"),
            trade(5.0, 90, DATA_END),
        ];
        let blob = DetailsBlob {
            trades: trades.clone(),
            starting_equity: 100_000.0,
            ending_equity: 105_500.0,
            total_return_pct: 5.5,
            sharpe_ratio: 0.9,
            max_drawdown_pct: 3.0,
            equity_curve: Vec::new(),
            monthly_returns: Vec::new(),
        };
        let json = serde_json::to_string(&blob).expect("blob serializes");
        let replayed: DetailsBlob = serde_json::from_str(&json).expect("blob round-trips");

        let fresh = realized_metrics(&trades);
        let stored = realized_metrics(&replayed.trades);

        assert_eq!(fresh.trades_taken, stored.trades_taken);
        assert_eq!(fresh.open_at_end, stored.open_at_end);
        assert_eq!(fresh.hit_rate, stored.hit_rate);
        assert_eq!(fresh.avg_return_pct, stored.avg_return_pct);
        assert_eq!(fresh.avg_holding_days, stored.avg_holding_days);
        assert_eq!(stored.trades_taken, 3);
        assert_eq!(stored.open_at_end, 1);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    /// Minimal schema: just the columns `get_signal_candidates` and
    /// `load_prices` actually touch.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE cross_signals (
                 entity_id INTEGER, ticker TEXT, compound_score REAL,
                 convergence_detected INTEGER, computed_at TEXT,
                 insider_signal REAL, institutional_flow REAL, news_momentum REAL,
                 government_signal REAL, search_trend REAL, patent_signal REAL,
                 supply_chain REAL, political_signal REAL);
             CREATE TABLE entity_tickers (entity_id INTEGER, ticker TEXT);
             CREATE TABLE entities (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE entity_prices (
                 ticker TEXT, date TEXT, open REAL, close REAL, high REAL, low REAL);
             -- The run persists its result, and this fixture omitted the table.
             -- save_result used `.ok()`, so the INSERT failed on EVERY run of this
             -- test and the test passed anyway. Making the persist fail loudly is
             -- what surfaced it.
             CREATE TABLE backtest_results (
                 id INTEGER PRIMARY KEY, signal_profile TEXT, start_date TEXT, end_date TEXT,
                 total_signals INTEGER, hit_rate REAL, avg_return REAL, max_drawdown REAL,
                 sharpe_ratio REAL, avg_holding_days REAL, details TEXT);",
        )
        .expect("schema");
        conn
    }

    fn day(n: i64) -> String {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .expect("valid date")
            .checked_add_signed(chrono::Duration::days(n))
            .expect("in range")
            .format("%Y-%m-%d")
            .to_string()
    }

    /// PRICED signals and has bars — admissible.
    /// UNPRICED signals just as often but has no row in entity_prices at all.
    /// OFFDAY has bars, but never on a day it signals.
    /// Only PRICED can ever be traded, and nothing else in the result says so.
    #[test]
    fn coverage_counts_only_tickers_that_could_actually_be_admitted() {
        let conn = fixture();
        // 40 trading days of bars for PRICED and OFFDAY.
        for d in 0..40 {
            for t in ["PRICED", "OFFDAY"] {
                conn.execute(
                    "INSERT INTO entity_prices (ticker, date, open, close, high, low)
                     VALUES (?1, ?2, 100.0, 100.0, 101.0, 99.0)",
                    params![t, day(d)],
                )
                .expect("insert bar");
            }
        }
        // PRICED and UNPRICED signal on days that have bars; OFFDAY signals on a
        // day well past the last bar, so it never lines up.
        for (i, t) in ["PRICED", "UNPRICED"].iter().enumerate() {
            conn.execute(
                "INSERT INTO cross_signals (entity_id, ticker, compound_score,
                     convergence_detected, computed_at)
                 VALUES (?1, ?2, 0.9, 1, ?3)",
                params![i as i64 + 1, t, format!("{} 12:00:00", day(5))],
            )
            .expect("insert signal");
        }
        conn.execute(
            "INSERT INTO cross_signals (entity_id, ticker, compound_score,
                 convergence_detected, computed_at)
             VALUES (3, 'OFFDAY', 0.9, 1, ?1)",
            params![format!("{} 12:00:00", day(80))],
        )
        .expect("insert offday signal");

        let result = run_backtest(
            &conn,
            BacktestConfig {
                start_date: day(0),
                end_date: day(90),
                min_score: 0.30,
                stop_loss_pct: -10.0,
                take_profit_pct: 15.0,
                max_hold_days: 90,
                max_positions: 10,
                position_size_pct: 5.0,
            },
        )
        .expect("backtest runs");

        assert_eq!(result.total_signals, 3, "all three signalled");
        assert_eq!(result.tickers_signalled, 3);
        assert_eq!(
            result.tickers_tradable, 1,
            "only PRICED had a bar on a day it signalled; UNPRICED has no price \
             data and OFFDAY never lines up"
        );
    }
}
