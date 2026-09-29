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
    /// How exits are modelled. `FixedPct` (the default) is the historical
    /// proxy; `AtrTrail` mirrors the live trailing stop.
    #[serde(default)]
    pub exit_model: ExitModel,
    /// Size each entry by the live score tiers instead of `position_size_pct`.
    /// A candidate's own `size_pct` (the what-if path) still wins.
    #[serde(default)]
    pub use_live_tiers: bool,
    /// Size each entry by stop-out risk (`pulse_weights::risk_sizing`). Takes
    /// precedence over tiers and `position_size_pct`.
    #[serde(default)]
    pub risk_sizing: Option<pulse_weights::risk_sizing::RiskParams>,
}

/// Exit rules for the walk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub enum ExitModel {
    /// Stop at `stop_loss_pct`, take profit at `take_profit_pct`, both off
    /// the entry price.
    #[default]
    FixedPct,
    /// The live exit: a stop `atr_mult` ATRs below the highest close since
    /// entry, never looser than `hard_stop_pct` below entry, and a fixed -10%
    /// while a ticker has no ATR. No take-profit — the live half close at the
    /// profit target is not modelled, and the trail takes the whole position.
    AtrTrail { atr_mult: f64, hard_stop_pct: f64 },
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
pub(crate) struct SignalCandidate {
    pub(crate) ticker: String,
    pub(crate) entity_name: String,
    pub(crate) compound_score: f64,
    pub(crate) signal_date: String,
    pub(crate) signal_profile: String,
    /// Position size override (% of equity). None = `config.position_size_pct`.
    /// Set by the what-if backtester to model the live sizing tiers.
    pub(crate) size_pct: Option<f64>,
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
    /// Highest close since entry (AtrTrail).
    hwm: f64,
    /// Price the position is stopped at. Only ever rises.
    stop_price: f64,
}

/// Average true range over the `period` bars ending on or before `date`.
/// 0.0 with fewer than `period + 1` bars — callers treat that as "no ATR",
/// the same as the live `compute_atr`.
fn atr_at(bars: Option<&HashMap<String, (f64, f64, f64)>>, date: &str, period: usize) -> f64 {
    let Some(bars) = bars else { return 0.0 };
    let mut dates: Vec<&String> = bars.keys().filter(|d| d.as_str() <= date).collect();
    dates.sort();
    if dates.len() < period + 1 {
        return 0.0;
    }
    let window = &dates[dates.len() - period - 1..];
    let mut sum = 0.0;
    for pair in window.windows(2) {
        let (prev_close, _, _) = bars[pair[0]];
        let (_, high, low) = bars[pair[1]];
        sum += (high - low).max((high - prev_close).abs()).max((low - prev_close).abs());
    }
    sum / period as f64
}

/// Where a new position's stop starts.
fn initial_stop(model: &ExitModel, entry_price: f64, atr: f64, stop_loss_pct: f64) -> f64 {
    match model {
        ExitModel::FixedPct => entry_price * (1.0 + stop_loss_pct / 100.0),
        ExitModel::AtrTrail { atr_mult, hard_stop_pct } => trail_stop(entry_price, entry_price, atr, *atr_mult, *hard_stop_pct),
    }
}

/// The live stop rule for a high-water mark and today's ATR.
fn trail_stop(entry_price: f64, hwm: f64, atr: f64, atr_mult: f64, hard_stop_pct: f64) -> f64 {
    if atr > 0.0 {
        (hwm - atr_mult * atr).max(entry_price * (1.0 - hard_stop_pct / 100.0))
    } else {
        entry_price * 0.90
    }
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
    let result = simulate(conn, &config, candidates)?;
    if result.total_signals == 0 || result.equity_curve.is_empty() {
        return Ok(result);
    }
    save_result(
        conn,
        &result.config_summary,
        result.total_signals,
        &result,
        &SavedMetrics {
            hit_rate: result.hit_rate,
            avg_return: result.avg_return_pct,
            max_drawdown: result.max_drawdown_pct,
            sharpe: result.sharpe_ratio,
            avg_hold: result.avg_holding_days,
        },
    )?;
    Ok(result)
}

/// The calendar walk over pre-scored candidates. Pure with respect to the DB
/// (reads prices, writes nothing) so the what-if backtester can run it many
/// times without polluting `backtest_results`.
pub(crate) fn simulate(
    conn: &Connection,
    config: &BacktestConfig,
    candidates: Vec<SignalCandidate>,
) -> Result<BacktestResult, String> {
    let total_signals = candidates.len();

    if candidates.is_empty() {
        return Ok(empty_result(config));
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

    // Prices start 40 days early so ATR has history on the first entries. The
    // walk still starts at start_date.
    let warmup_start = add_days(&config.start_date, -40).unwrap_or_else(|| config.start_date.clone());
    let prices = load_prices(conn, &tickers, &warmup_start, &extended_end)?;

    // Build union of all trading dates we have prices for (any ticker).
    // We walk these in chronological order.
    let trading_dates: Vec<String> = prices
        .values()
        .flat_map(|m| m.keys().cloned())
        .filter(|d| d.as_str() >= config.start_date.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if trading_dates.is_empty() {
        return Ok(empty_result(config));
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
        let mut open_tickers: Vec<String> = open_positions.keys().cloned().collect();
        open_tickers.sort(); // deterministic exit order (P&L sums and the trade list)
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

            if let ExitModel::AtrTrail { atr_mult, hard_stop_pct } = config.exit_model {
                // Stop checked against today's low using the stop set through
                // yesterday. A bar that opened below the stop fills at its high
                // at best — slightly generous, since the open is not stored.
                if low <= pos.stop_price {
                    let exit_price = pos.stop_price.min(high);
                    let pnl_pct = ((exit_price - pos.entry_price) / pos.entry_price) * 100.0;
                    let pnl_dollars = pos.position_value * pnl_pct / 100.0;
                    realized_pnl += pnl_dollars;
                    equity += pnl_dollars;
                    trades.push(close_trade(&pos, date, exit_price, pnl_pct, pnl_dollars, days_held, "trailing_stop"));
                    open_positions.remove(&ticker);
                    continue;
                }
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
                // Survived today: raise the mark and the stop for tomorrow.
                let atr = atr_at(prices.get(&ticker), date, 14);
                if let Some(p) = open_positions.get_mut(&ticker) {
                    p.hwm = p.hwm.max(bar.0);
                    let next = trail_stop(p.entry_price, p.hwm, atr, atr_mult, hard_stop_pct);
                    p.stop_price = p.stop_price.max(next);
                }
                continue;
            }

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
                let entry_atr = atr_at(prices.get(&cand.ticker), date, 14);
                let position_value = if let Some(rp) = &config.risk_sizing {
                    // The same book the live trader reads, measured from the walk.
                    let invested: f64 = open_positions.values().map(|p| p.position_value).sum();
                    let peak = equity_curve.iter().map(|p| p.value).fold(starting_equity, f64::max).max(sizing_equity);
                    let mut open_sorted: Vec<&OpenPosition> = open_positions.values().collect();
                    open_sorted.sort_by(|a, b| a.ticker.cmp(&b.ticker));
                    let open_risk: f64 = open_sorted.iter().map(|p| {
                        let px = prices.get(&p.ticker).and_then(|m| m.get(date)).map(|b| b.0).unwrap_or(p.entry_price);
                        pulse_weights::risk_sizing::position_risk(p.position_value / p.entry_price, px, Some(p.stop_price), rp)
                    }).sum();
                    let returns: Vec<f64> = trades.iter().filter(|t| t.exit_reason != DATA_END).map(|t| t.pnl_pct).collect();
                    let book = pulse_weights::risk_sizing::Book {
                        equity: sizing_equity,
                        buying_power: (equity - invested).max(0.0),
                        drawdown: if peak > 0.0 { (peak - sizing_equity) / peak } else { 0.0 },
                        open_risk,
                    };
                    // Under fixed exits the stop is stop_loss_pct, whatever the
                    // ATR; express it as the ATR that sizing reads back as that.
                    let sizing_atr = match config.exit_model {
                        ExitModel::AtrTrail { .. } => entry_atr,
                        ExitModel::FixedPct => entry_price * (-config.stop_loss_pct / 100.0) / rp.atr_mult,
                    };
                    match pulse_weights::risk_sizing::size_order(
                        &book, entry_price, sizing_atr, 0.0,
                        &pulse_weights::risk_sizing::EdgeStats::from_returns(&returns), 1.0, rp,
                    ) {
                        Some(s) => s.notional,
                        None => continue,
                    }
                } else {
                    let size_pct = cand.size_pct.unwrap_or_else(|| {
                        if config.use_live_tiers {
                            pulse_weights::StrategyParams::live().size_pct(cand.compound_score)
                        } else {
                            config.position_size_pct
                        }
                    });
                    sizing_equity * (size_pct / 100.0)
                };
                if position_value <= 0.0 { continue; }

                open_positions.insert(cand.ticker.clone(), OpenPosition {
                    ticker: cand.ticker.clone(),
                    entity_name: cand.entity_name.clone(),
                    entry_date: date.clone(),
                    entry_price,
                    position_value,
                    compound_score: cand.compound_score,
                    signal_profile: cand.signal_profile.clone(),
                    hwm: entry_price,
                    stop_price: initial_stop(&config.exit_model, entry_price, entry_atr, config.stop_loss_pct),
                });
            }
        }

        // (3) Snapshot equity for the curve (realized + unrealized mark-to-market).
        let unrealized = mark_to_market(&open_positions, &prices, date);
        equity_curve.push(EquityPoint { date: date.clone(), value: starting_equity + realized_pnl + unrealized });
    }

    // Force-close anything still open at the last trading date with data.
    if let Some(last_date) = trading_dates.last().cloned() {
        let mut still_open: Vec<String> = open_positions.keys().cloned().collect();
        still_open.sort(); // deterministic exit order (P&L sums and the trade list)
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
        trades,
        equity_curve,
        monthly_returns,
    };

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
        if let Some(json) = &details_json
            && let Ok(blob) = serde_json::from_str::<DetailsBlob>(json) {
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
    prices: &PriceTable,
    date: &str,
) -> f64 {
    // Sorted, not HashMap order: float addition is not associative, so a random
    // order made two identical runs differ in the 14th digit (and the what-if
    // baseline-vs-noop check flaky).
    let mut positions: Vec<&OpenPosition> = open.values().collect();
    positions.sort_by(|a, b| a.ticker.cmp(&b.ticker));
    let mut total = 0.0;
    for pos in positions {
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
    prices: &PriceTable,
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
                size_pct: None,
            })
        },
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(candidates)
}

/// ticker -> date -> `(close, high, low)`. High and low are COALESCEd to close
/// for close-only rows, so all three are always populated.
type PriceTable = HashMap<String, HashMap<String, (f64, f64, f64)>>;

fn load_prices(
    conn: &Connection,
    tickers: &[String],
    start: &str,
    end: &str,
) -> Result<PriceTable, String> {
    if tickers.is_empty() {
        return Ok(HashMap::new());
    }
    // Build `?,?,?` placeholders. We cap batch size so we don't blow past
    // SQLITE_MAX_VARIABLE_NUMBER on pathological runs. 500 tickers is plenty.
    let mut out: PriceTable = HashMap::new();
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

/// The five realized statistics persisted alongside a backtest run.
///
/// They were five adjacent `f64` parameters, so any transposition — sharpe
/// into max_drawdown, avg_return into hit_rate — compiled and silently wrote
/// a row whose summary columns disagreed with its own details blob.
struct SavedMetrics {
    hit_rate: f64,
    avg_return: f64,
    max_drawdown: f64,
    sharpe: f64,
    avg_hold: f64,
}

fn save_result(
    conn: &Connection,
    config_summary: &str,
    total_signals: usize,
    result: &BacktestResult,
    metrics: &SavedMetrics,
) -> Result<(), String> {
    let SavedMetrics { hit_rate, avg_return, max_drawdown, sharpe, avg_hold } = *metrics;
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
                exit_model: ExitModel::FixedPct,
                use_live_tiers: false,
                risk_sizing: None,
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

#[cfg(test)]
mod exit_and_sizing_tests {
    use super::*;

    fn day(n: i64) -> String {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .expect("valid date")
            .checked_add_signed(chrono::Duration::days(n))
            .expect("in range")
            .format("%Y-%m-%d")
            .to_string()
    }

    /// Bars with a constant $2 range around each close.
    fn bars(closes: &[f64]) -> HashMap<String, (f64, f64, f64)> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| (day(i as i64), (*c, c + 1.0, c - 1.0)))
            .collect()
    }

    fn prices(ticker: &str, closes: &[f64]) -> PriceTable {
        HashMap::from([(ticker.to_string(), bars(closes))])
    }

    fn config(exit_model: ExitModel, risk: Option<pulse_weights::risk_sizing::RiskParams>) -> BacktestConfig {
        BacktestConfig {
            start_date: day(20),
            end_date: day(60),
            min_score: 0.3,
            stop_loss_pct: -10.0,
            take_profit_pct: 15.0,
            max_hold_days: 90,
            max_positions: 10,
            position_size_pct: 5.0,
            exit_model,
            use_live_tiers: false,
            risk_sizing: risk,
        }
    }

    fn candidate(ticker: &str, on: i64) -> SignalCandidate {
        SignalCandidate {
            ticker: ticker.into(),
            entity_name: ticker.into(),
            compound_score: 0.5,
            signal_date: day(on),
            signal_profile: "{}".into(),
            size_pct: None,
        }
    }

    fn db_with(ticker: &str, closes: &[f64]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE entity_prices (ticker TEXT, date TEXT, close REAL, high REAL, low REAL);")
            .unwrap();
        for (i, c) in closes.iter().enumerate() {
            conn.execute(
                "INSERT INTO entity_prices VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ticker, day(i as i64), c, c + 1.0, c - 1.0],
            )
            .unwrap();
        }
        conn
    }

    const TRAIL: ExitModel = ExitModel::AtrTrail { atr_mult: 3.0, hard_stop_pct: 15.0 };

    #[test]
    fn atr_is_the_mean_true_range_and_needs_enough_history() {
        let p = prices("A", &[100.0; 20]);
        assert!((atr_at(p.get("A"), &day(19), 14) - 2.0).abs() < 1e-9);
        assert_eq!(atr_at(p.get("A"), &day(10), 14), 0.0, "11 bars is not enough for 14");
        assert_eq!(atr_at(None, &day(19), 14), 0.0);
    }

    #[test]
    fn the_trail_never_sits_below_the_hard_stop_and_falls_back_without_atr() {
        assert!((trail_stop(100.0, 110.0, 2.0, 3.0, 15.0) - 104.0).abs() < 1e-9);
        assert!((trail_stop(100.0, 100.0, 10.0, 3.0, 15.0) - 85.0).abs() < 1e-9, "3 ATR = 30 > 15% hard stop");
        assert!((trail_stop(100.0, 120.0, 0.0, 3.0, 15.0) - 90.0).abs() < 1e-9);
    }

    /// Flat at 100 for the warmup, rally to 130, then fall. The trail ratchets
    /// up with the rally and takes the exit on the way down, locking a gain the
    /// fixed -10%/+15% model would have closed at +15% instead.
    #[test]
    fn an_atr_trail_rides_a_rally_and_exits_on_the_pullback() {
        let mut closes = vec![100.0; 21];
        closes.extend((1..=15).map(|i| 100.0 + 2.0 * i as f64)); // to 130
        closes.extend((1..=15).map(|i| 130.0 - 2.0 * i as f64)); // back down
        let conn = db_with("A", &closes);
        let r = simulate(&conn, &config(TRAIL, None), vec![candidate("A", 20)]).unwrap();
        let t = &r.trades[0];
        assert_eq!(t.exit_reason, "trailing_stop");
        // Highest close 130. Stepping $2 a day, each bar's true range reaches
        // back to the prior close: 3, not the bar's own $2 range. Stop 130 - 9.
        assert!((t.exit_price - 121.0).abs() < 1e-6, "exit {}", t.exit_price);
        assert!(t.pnl_pct > 20.0);
    }

    #[test]
    fn risk_sizing_puts_the_budget_at_the_stop() {
        let conn = db_with("A", &[100.0; 61]);
        let risk = pulse_weights::risk_sizing::RiskParams::default();
        let r = simulate(&conn, &config(TRAIL, Some(risk)), vec![candidate("A", 20)]).unwrap();
        // ATR 2 at $100 -> 6% stop; 0.5% of $100k = $500 at risk -> $8,333.
        // Flat prices: marked at entry, so the trade closes at data end with no P&L.
        let t = &r.trades[0];
        assert!((t.pnl_dollars).abs() < 1e-6);
        assert_eq!(r.trades.len(), 1);
    }

    #[test]
    fn fixed_exits_with_risk_sizing_size_to_the_fixed_stop() {
        // A -10% stop and a 0.5% budget -> $5,000. Price drops 20% on day 25,
        // so the fixed stop fires and books -10% of $5,000.
        let mut closes = vec![100.0; 25];
        closes.extend(vec![80.0; 36]);
        let conn = db_with("A", &closes);
        let risk = pulse_weights::risk_sizing::RiskParams::default();
        let r = simulate(&conn, &config(ExitModel::FixedPct, Some(risk)), vec![candidate("A", 20)]).unwrap();
        let t = &r.trades[0];
        assert_eq!(t.exit_reason, "stop_loss");
        assert!((t.pnl_dollars + 500.0).abs() < 1e-6, "lost {}", t.pnl_dollars);
    }
}
