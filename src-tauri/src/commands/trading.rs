use tauri::{AppHandle, State};
use crate::db::DbState;
use crate::services::paper_trading::{self, Portfolio, PaperTrade};
use crate::services::analytics::{self, PortfolioAnalytics, TradeJournal};
use crate::services::backtester::{self, BacktestConfig, BacktestResult};
use crate::services::live_prices::{self, LivePriceState, StreamStatus};
use std::sync::Arc;

/// Get full portfolio: account info + positions + trade history.
#[tauri::command]
pub async fn get_portfolio(db: State<'_, DbState>) -> Result<Portfolio, String> {
    // Get DB trades first, then drop lock before async Alpaca calls
    let (open_trades, closed_trades) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let open = paper_trading::get_trades_from_db(&conn, "open").map_err(|e| e.to_string())?;
        let closed = paper_trading::get_trades_from_db(&conn, "closed").map_err(|e| e.to_string())?;
        (open, closed)
    };
    // Lock dropped — now safe to await
    let result = paper_trading::get_portfolio_with_trades(open_trades, closed_trades)
        .await
        .map_err(|e| e.to_string());
    // Log Alpaca reads (1 for account + 1 for positions, regardless of ticker count)
    if result.is_ok() {
        if let Ok(conn) = db.0.lock() {
            crate::services::api_usage::log_usage(&conn, "alpaca", "paper_api", "portfolio", 2, 0).ok();
        }
    }
    result
}

/// Get paper trades by status.
#[tauri::command]
pub fn get_paper_trades(db: State<'_, DbState>, status: Option<String>) -> Result<Vec<PaperTrade>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let status = status.as_deref().unwrap_or("open");

    let mut stmt = conn
        .prepare(
            "SELECT id, entity_id, ticker, direction, entry_price, entry_date,
                    exit_price, exit_date, position_size, confidence,
                    signal_profile, status, pnl, pnl_pct, trade_journal
             FROM paper_trades WHERE status = ?1
             ORDER BY entry_date DESC LIMIT 50"
        )
        .map_err(|e| e.to_string())?;

    let trades: Vec<PaperTrade> = stmt
        .query_map([status], |row| {
            Ok(PaperTrade {
                id: row.get(0)?,
                entity_id: row.get(1)?,
                ticker: row.get(2)?,
                direction: row.get(3)?,
                entry_price: row.get(4)?,
                entry_date: row.get(5)?,
                exit_price: row.get(6)?,
                exit_date: row.get(7)?,
                position_size: row.get(8)?,
                confidence: row.get(9)?,
                signal_profile: row.get(10)?,
                status: row.get(11)?,
                pnl: row.get(12)?,
                pnl_pct: row.get(13)?,
                trade_journal: row.get(14)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(trades)
}

/// Manually trigger a paper trade for a specific ticker.
#[tauri::command]
pub async fn execute_trade(
    db: State<'_, DbState>,
    ticker: String,
    confidence: f64,
) -> Result<Option<PaperTrade>, String> {
    // Gather DB info first, drop lock before async calls
    let (entity_id, signal_profile, current_price, already_open, open_count) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let eid: i64 = conn
            .query_row("SELECT entity_id FROM entity_tickers WHERE ticker = ?1", [&ticker], |row| row.get(0))
            .unwrap_or(0);
        let profile = serde_json::json!({"source": "manual", "confidence": confidence}).to_string();
        let price: f64 = conn
            .query_row("SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1", [&ticker], |row| row.get(0))
            .unwrap_or(0.0);
        let already: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM paper_trades WHERE ticker = ?1 AND status = 'open')", [&ticker], |row| row.get(0))
            .unwrap_or(false);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM paper_trades WHERE status = 'open'", [], |row| row.get(0))
            .unwrap_or(0);
        (eid, profile, price, already, count)
    };

    if already_open {
        return Err(format!("Already have open position for {}", ticker));
    }
    if open_count >= 15 {
        return Err("Max 15 open positions reached".to_string());
    }
    if current_price <= 0.0 {
        return Err(format!("No price data for {}", ticker));
    }

    // Get account + place order (async, no lock held)
    let account = paper_trading::get_account().await.map_err(|e| e.to_string())?;
    let portfolio_value: f64 = account.portfolio_value.parse().unwrap_or(100_000.0);

    let position_pct = if confidence > 0.75 { 0.05 } else if confidence > 0.60 { 0.02 } else { 0.01 };
    let position_value = portfolio_value * position_pct;
    let qty = (position_value / current_price).floor().max(1.0);

    let order = paper_trading::place_order(&ticker, qty, "buy").await.map_err(|e| e.to_string())?;
    let filled_price = order.filled_avg_price.as_deref().and_then(|p| p.parse().ok()).unwrap_or(current_price);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Write trade to DB (re-acquire lock)
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO paper_trades (entity_id, ticker, direction, entry_price, entry_date,
            position_size, confidence, signal_profile, alpaca_order_id, status)
         VALUES (?1, ?2, 'long', ?3, ?4, ?5, ?6, ?7, ?8, 'open')",
        rusqlite::params![entity_id, ticker, filled_price, today, position_value, confidence, signal_profile, order.id],
    ).map_err(|e| e.to_string())?;

    let trade_id = conn.last_insert_rowid();
    crate::services::api_usage::log_usage(&conn, "alpaca", "trading", "place_order", 0, 0).ok();

    Ok(Some(PaperTrade {
        id: trade_id, entity_id, ticker, direction: "long".to_string(),
        entry_price: filled_price, entry_date: today,
        exit_price: None, exit_date: None,
        position_size: position_value, confidence,
        signal_profile, status: "open".to_string(),
        pnl: None, pnl_pct: None,
        trade_journal: None,
    }))
}

/// Get portfolio analytics: win rate, Sharpe, attribution, etc.
#[tauri::command]
pub fn get_portfolio_analytics(db: State<'_, DbState>) -> Result<PortfolioAnalytics, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    analytics::compute_analytics(&conn)
}

/// Get or generate a trade journal for a specific trade.
#[tauri::command]
pub fn get_trade_journal(db: State<'_, DbState>, trade_id: i64) -> Result<TradeJournal, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    analytics::get_or_generate_journal(&conn, trade_id)
}

/// Run a backtest against historical cross-signals.
#[tauri::command]
pub fn run_backtest(db: State<'_, DbState>, config: BacktestConfig) -> Result<BacktestResult, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    backtester::run_backtest(&conn, config)
}

/// Get past backtest results.
#[tauri::command]
pub fn get_backtest_history(db: State<'_, DbState>) -> Result<Vec<BacktestResult>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    backtester::get_backtest_history(&conn, 10)
}


/// Daily auto-backtest. Triggered on app startup; runs at most once per day.
///
/// Gates:
/// - Requires at least 90 days of cross_signals data (the day-90 threshold the
///   user agreed to — fewer days produces statistical noise)
/// - Skips if a backtest_results row already exists for today
///
/// Uses an expanding window: every day it runs, it tests on ALL data from the
/// first cross_signals row to today. So day 91 has 91 days, day 92 has 92, etc.
///
/// Default parameters mirror the production trading path (compound score > 0.3,
/// 5% position sizing, 90-day max hold) so the daily report measures what the
/// auto-trader WOULD have done, had it been on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoBacktestStatus {
    pub ran_today: bool,
    pub days_of_data: i64,
    pub threshold_days: i64,
    pub last_result: Option<BacktestResult>,
    pub message: String,
}

#[tauri::command]
pub fn auto_backtest_if_due(db: State<'_, DbState>) -> Result<AutoBacktestStatus, String> {
    const THRESHOLD_DAYS: i64 = 90;

    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // How many distinct days of cross_signals do we have?
    let (min_date, max_date, days_of_data): (Option<String>, Option<String>, i64) = conn
        .query_row(
            "SELECT MIN(date(computed_at)), MAX(date(computed_at)), COUNT(DISTINCT date(computed_at))
             FROM cross_signals WHERE ticker IS NOT NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or((None, None, 0));

    // Already ran today?
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let ran_today: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM backtest_results WHERE date(created_at) = ?1)",
            [&today],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let last_result: Option<BacktestResult> = backtester::get_backtest_history(&conn, 1)
        .ok()
        .and_then(|mut h| h.pop());

    if days_of_data < THRESHOLD_DAYS {
        return Ok(AutoBacktestStatus {
            ran_today: false,
            days_of_data,
            threshold_days: THRESHOLD_DAYS,
            last_result,
            message: format!(
                "Auto-backtest waiting: {} of {} days of data collected ({} days remaining).",
                days_of_data, THRESHOLD_DAYS, THRESHOLD_DAYS - days_of_data
            ),
        });
    }

    if ran_today {
        return Ok(AutoBacktestStatus {
            ran_today: true,
            days_of_data,
            threshold_days: THRESHOLD_DAYS,
            last_result,
            message: "Already ran today.".to_string(),
        });
    }

    // Build expanding-window config covering all available data.
    let start_date = min_date.unwrap_or_else(|| {
        // Fall back to "90 days ago" if for some reason min_date is null.
        chrono::Local::now()
            .date_naive()
            .checked_sub_days(chrono::Days::new(THRESHOLD_DAYS as u64))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| today.clone())
    });
    let end_date = max_date.unwrap_or_else(|| today.clone());

    let config = backtester::BacktestConfig {
        start_date,
        end_date,
        min_score: 0.30,        // matches auto_trade_on_convergence threshold
        stop_loss_pct: -10.0,   // realistic safety stop
        take_profit_pct: 15.0,
        max_hold_days: 90,      // matches position_management hard expiry
        max_positions: 10,
        position_size_pct: 5.0, // matches the high-score tier in position_sizing
    };

    let result = backtester::run_backtest(&conn, config)
        .map_err(|e| format!("auto-backtest failed: {}", e))?;

    let summary = format!(
        "Auto-backtest ran: {} trades, {:.1}% hit rate, {:+.2}% return, Sharpe {:.2}, max DD {:.1}%",
        result.trades_taken,
        result.hit_rate,
        result.total_return_pct,
        result.sharpe_ratio,
        result.max_drawdown_pct,
    );

    Ok(AutoBacktestStatus {
        ran_today: true,
        days_of_data,
        threshold_days: THRESHOLD_DAYS,
        last_result: Some(result),
        message: summary,
    })
}

/// Read-only kill-switch state. The auto-trader is disabled by default; the UI
/// uses this to display a banner so the user can see at a glance whether
/// trading automation is on.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AutoTradeStatus {
    pub enabled: bool,
    pub reason: String,
}

#[tauri::command]
pub fn get_auto_trade_status() -> Result<AutoTradeStatus, String> {
    let enabled = std::env::var("AUTO_TRADE_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    let reason = if enabled {
        "AUTO_TRADE_ENABLED=true — auto-trader will place orders on convergence signals.".to_string()
    } else {
        "Auto-trader is OFF (default). Set AUTO_TRADE_ENABLED=true in .env to re-enable, but only after the daily auto-backtest shows a durable edge.".to_string()
    };
    Ok(AutoTradeStatus { enabled, reason })
}

/// Start live price streaming for open positions.
#[tauri::command]
pub async fn start_price_stream(
    app: AppHandle,
    db: State<'_, DbState>,
    state: State<'_, Arc<LivePriceState>>,
) -> Result<(), String> {
    // Get tickers of open positions
    let symbols: Vec<String> = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT ticker FROM paper_trades WHERE status = 'open'"
        ).map_err(|e| e.to_string())?;
        stmt.query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };

    if symbols.is_empty() {
        return Err("No open positions to stream".into());
    }

    live_prices::start_stream(app, state.inner().clone(), symbols).await
}

/// Stop live price streaming.
#[tauri::command]
pub async fn stop_price_stream(
    state: State<'_, Arc<LivePriceState>>,
) -> Result<(), String> {
    live_prices::stop_stream(state.inner().clone()).await
}

/// Get current stream status.
#[tauri::command]
pub async fn get_stream_status(
    state: State<'_, Arc<LivePriceState>>,
) -> Result<StreamStatus, String> {
    Ok(live_prices::get_status(state.inner().clone()).await)
}
