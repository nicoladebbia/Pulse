use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

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
    pub position_size_pct: f64, // e.g. 5.0 (% of starting equity)
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestResult {
    pub config_summary: String,
    pub total_signals: usize,
    pub trades_taken: usize,
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

// ---------------------------------------------------------------------------
// Signal candidate from cross_signals history
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct SignalCandidate {
    entity_id: i64,
    ticker: String,
    entity_name: String,
    compound_score: f64,
    signal_date: String,
    signal_profile: String,
}

// ---------------------------------------------------------------------------
// Main backtest engine
// ---------------------------------------------------------------------------

pub fn run_backtest(conn: &Connection, config: BacktestConfig) -> Result<BacktestResult, String> {
    // 1. Get all convergence signals in date range
    let candidates = get_signal_candidates(conn, &config)?;
    let total_signals = candidates.len();

    if candidates.is_empty() {
        return Ok(empty_result(&config));
    }

    // 2. Simulate trades
    let starting_equity = 100_000.0;
    let position_value = starting_equity * (config.position_size_pct / 100.0);
    let mut trades: Vec<BacktestTrade> = Vec::new();
    let mut open_tickers: std::collections::HashSet<String> = std::collections::HashSet::new();

    for candidate in &candidates {
        // Skip if already holding this ticker
        if open_tickers.contains(&candidate.ticker) {
            continue;
        }
        // Skip if at max positions
        if open_tickers.len() >= config.max_positions {
            continue;
        }

        // Get entry price on signal date
        let entry_price = match get_price_on_date(conn, &candidate.ticker, &candidate.signal_date) {
            Some(p) if p > 0.0 => p,
            _ => continue,
        };

        // Simulate forward from entry date
        let result = simulate_trade(
            conn, &candidate.ticker, &candidate.signal_date,
            entry_price, position_value, &config,
        );

        if let Some(trade) = result {
            let bt = BacktestTrade {
                ticker: candidate.ticker.clone(),
                entity_name: candidate.entity_name.clone(),
                entry_date: candidate.signal_date.clone(),
                entry_price,
                exit_date: trade.exit_date,
                exit_price: trade.exit_price,
                pnl_pct: trade.pnl_pct,
                pnl_dollars: trade.pnl_dollars,
                holding_days: trade.holding_days,
                exit_reason: trade.exit_reason,
                compound_score: candidate.compound_score,
                signal_profile: candidate.signal_profile.clone(),
            };

            // Remove from open set once we've simulated the full trade
            // (we simulate synchronously, so it's already closed)
            open_tickers.remove(&candidate.ticker);
            trades.push(bt);
        }

        // Track that we entered (even though simulation is instant, prevent duplicate entries same day)
        open_tickers.insert(candidate.ticker.clone());
    }

    // 3. Compute aggregate metrics
    let trades_taken = trades.len();
    let trades_won = trades.iter().filter(|t| t.pnl_pct > 0.0).count();
    let trades_lost = trades_taken - trades_won;
    let hit_rate = if trades_taken > 0 { trades_won as f64 / trades_taken as f64 * 100.0 } else { 0.0 };

    let returns: Vec<f64> = trades.iter().map(|t| t.pnl_pct).collect();
    let avg_return_pct = if returns.is_empty() { 0.0 }
        else { returns.iter().sum::<f64>() / returns.len() as f64 };

    let total_pnl: f64 = trades.iter().map(|t| t.pnl_dollars).sum();
    let total_return_pct = total_pnl / starting_equity * 100.0;
    let ending_equity = starting_equity + total_pnl;

    let holding_days: Vec<f64> = trades.iter().map(|t| t.holding_days as f64).collect();
    let avg_holding_days = if holding_days.is_empty() { 0.0 }
        else { holding_days.iter().sum::<f64>() / holding_days.len() as f64 };

    let sharpe_ratio = compute_sharpe(&returns);
    let max_drawdown_pct = compute_max_drawdown(&trades, starting_equity);

    let config_summary = format!(
        "Score>{:.0}% | SL {:.0}% | TP {:.0}% | Hold {}d | {} pos | {:.0}% size | {} to {}",
        config.min_score * 100.0, config.stop_loss_pct, config.take_profit_pct,
        config.max_hold_days, config.max_positions, config.position_size_pct,
        config.start_date, config.end_date,
    );

    // 4. Save to backtest_results
    save_result(conn, &config_summary, total_signals, &trades, hit_rate, avg_return_pct, max_drawdown_pct, sharpe_ratio, avg_holding_days);

    Ok(BacktestResult {
        config_summary,
        total_signals,
        trades_taken,
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
    })
}

/// Get past backtest results.
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
        let max_drawdown: f64 = row.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
        let sharpe: f64 = row.get::<_, Option<f64>>(7)?.unwrap_or(0.0);
        let avg_hold: f64 = row.get::<_, Option<f64>>(8)?.unwrap_or(0.0);
        let details_json: Option<String> = row.get(9)?;

        let trades: Vec<BacktestTrade> = details_json
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or_default();

        let trades_won = trades.iter().filter(|t| t.pnl_pct > 0.0).count();

        Ok(BacktestResult {
            config_summary,
            total_signals: total_signals as usize,
            trades_taken: trades.len(),
            trades_won,
            trades_lost: trades.len() - trades_won,
            hit_rate,
            avg_return_pct: avg_return,
            total_return_pct: trades.iter().map(|t| t.pnl_dollars).sum::<f64>() / 100_000.0 * 100.0,
            max_drawdown_pct: max_drawdown,
            sharpe_ratio: sharpe,
            avg_holding_days: avg_hold,
            starting_equity: 100_000.0,
            ending_equity: 100_000.0 + trades.iter().map(|t| t.pnl_dollars).sum::<f64>(),
            trades,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_signal_candidates(conn: &Connection, config: &BacktestConfig) -> Result<Vec<SignalCandidate>, String> {
    let mut stmt = conn.prepare(
        "SELECT cs.entity_id, COALESCE(et.ticker, cs.ticker) as ticker,
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
                "insider": row.get::<_, f64>(5).unwrap_or(0.0),
                "institutional": row.get::<_, f64>(6).unwrap_or(0.0),
                "news": row.get::<_, f64>(7).unwrap_or(0.0),
                "government": row.get::<_, f64>(8).unwrap_or(0.0),
                "search": row.get::<_, f64>(9).unwrap_or(0.0),
                "patent": row.get::<_, f64>(10).unwrap_or(0.0),
                "supply_chain": row.get::<_, f64>(11).unwrap_or(0.0),
                "political": row.get::<_, f64>(12).unwrap_or(0.0),
            });

            Ok(SignalCandidate {
                entity_id: row.get(0)?,
                ticker: row.get(1)?,
                entity_name: row.get(2)?,
                compound_score: row.get(3)?,
                signal_date: row.get(4)?,
                signal_profile: profile.to_string(),
            })
        },
    ).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();

    Ok(candidates)
}

struct SimResult {
    exit_date: String,
    exit_price: f64,
    pnl_pct: f64,
    pnl_dollars: f64,
    holding_days: i64,
    exit_reason: String,
}

fn simulate_trade(
    conn: &Connection, ticker: &str, entry_date: &str,
    entry_price: f64, position_value: f64, config: &BacktestConfig,
) -> Option<SimResult> {
    // Get all prices after entry date
    let mut stmt = conn.prepare(
        "SELECT date, close, high, low FROM entity_prices
         WHERE ticker = ?1 AND date > ?2
         ORDER BY date ASC"
    ).ok()?;

    let prices: Vec<(String, f64, f64, f64)> = stmt.query_map(
        params![ticker, entry_date],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).ok()?
    .filter_map(|r| r.ok())
    .collect();

    if prices.is_empty() {
        // No forward price data — can't simulate
        return None;
    }

    let entry = chrono::NaiveDate::parse_from_str(entry_date, "%Y-%m-%d").ok()?;

    for (date, close, high, low) in &prices {
        let current_date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        let days_held = (current_date - entry).num_days();

        // Check intraday stop-loss (using low)
        let low_pct = ((low - entry_price) / entry_price) * 100.0;
        if low_pct <= config.stop_loss_pct {
            let exit_price = entry_price * (1.0 + config.stop_loss_pct / 100.0);
            let pnl_dollars = position_value * config.stop_loss_pct / 100.0;
            return Some(SimResult {
                exit_date: date.clone(), exit_price, pnl_pct: config.stop_loss_pct,
                pnl_dollars, holding_days: days_held, exit_reason: "stop_loss".into(),
            });
        }

        // Check intraday take-profit (using high)
        let high_pct = ((high - entry_price) / entry_price) * 100.0;
        if high_pct >= config.take_profit_pct {
            let exit_price = entry_price * (1.0 + config.take_profit_pct / 100.0);
            let pnl_dollars = position_value * config.take_profit_pct / 100.0;
            return Some(SimResult {
                exit_date: date.clone(), exit_price, pnl_pct: config.take_profit_pct,
                pnl_dollars, holding_days: days_held, exit_reason: "take_profit".into(),
            });
        }

        // Check max hold
        if days_held >= config.max_hold_days {
            let pnl_pct = ((close - entry_price) / entry_price) * 100.0;
            let pnl_dollars = position_value * pnl_pct / 100.0;
            return Some(SimResult {
                exit_date: date.clone(), exit_price: *close, pnl_pct,
                pnl_dollars, holding_days: days_held, exit_reason: "max_hold".into(),
            });
        }
    }

    // Ran out of price data — close at last available price
    let (last_date, last_close, _, _) = prices.last()?;
    let days_held = chrono::NaiveDate::parse_from_str(last_date, "%Y-%m-%d")
        .map(|d| (d - entry).num_days()).unwrap_or(0);
    let pnl_pct = ((last_close - entry_price) / entry_price) * 100.0;
    let pnl_dollars = position_value * pnl_pct / 100.0;

    Some(SimResult {
        exit_date: last_date.clone(), exit_price: *last_close, pnl_pct,
        pnl_dollars, holding_days: days_held, exit_reason: "data_end".into(),
    })
}

fn get_price_on_date(conn: &Connection, ticker: &str, date: &str) -> Option<f64> {
    // Get closest price on or just before the signal date
    conn.query_row(
        "SELECT close FROM entity_prices WHERE ticker = ?1 AND date <= ?2 ORDER BY date DESC LIMIT 1",
        params![ticker, date],
        |row| row.get(0),
    ).ok()
}

fn compute_sharpe(returns: &[f64]) -> f64 {
    if returns.len() < 2 { return 0.0; }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 { return 0.0; }
    let trades_per_year = 50.0_f64.min(returns.len() as f64);
    (mean / std_dev) * trades_per_year.sqrt()
}

fn compute_max_drawdown(trades: &[BacktestTrade], starting_equity: f64) -> f64 {
    let mut equity = starting_equity;
    let mut peak = starting_equity;
    let mut max_dd = 0.0;

    for t in trades {
        equity += t.pnl_dollars;
        if equity > peak { peak = equity; }
        let dd = if peak > 0.0 { (peak - equity) / peak * 100.0 } else { 0.0 };
        if dd > max_dd { max_dd = dd; }
    }

    max_dd
}

fn save_result(
    conn: &Connection, config_summary: &str, total_signals: usize,
    trades: &[BacktestTrade], hit_rate: f64, avg_return: f64,
    max_drawdown: f64, sharpe: f64, avg_hold: f64,
) {
    let details = serde_json::to_string(trades).unwrap_or_default();
    // Extract dates from config summary (last two space-separated tokens)
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
    ).ok();
}

fn empty_result(config: &BacktestConfig) -> BacktestResult {
    BacktestResult {
        config_summary: format!("{} to {} — no convergence signals found", config.start_date, config.end_date),
        total_signals: 0, trades_taken: 0, trades_won: 0, trades_lost: 0,
        hit_rate: 0.0, avg_return_pct: 0.0, total_return_pct: 0.0,
        max_drawdown_pct: 0.0, sharpe_ratio: 0.0, avg_holding_days: 0.0,
        starting_equity: 100_000.0, ending_equity: 100_000.0, trades: Vec::new(),
    }
}
