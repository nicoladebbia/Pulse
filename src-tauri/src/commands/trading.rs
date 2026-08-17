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

/// Manually close an open paper position. Places a full sell on Alpaca and
/// writes exit fields to the DB. Used by the Close button in the portfolio tab.
#[tauri::command]
pub async fn close_position(db: State<'_, DbState>, trade_id: i64) -> Result<PaperTrade, String> {
    // Gather trade info, drop lock before async Alpaca calls.
    let (ticker, entry_price) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT ticker, entry_price FROM paper_trades
             WHERE id = ?1 AND status = 'open'",
            [trade_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
        )
        .map_err(|_| format!("No open trade with id {}", trade_id))?
    };

    // Get the real held qty from Alpaca (DB stores dollar notional, not shares).
    let positions = paper_trading::get_positions().await.map_err(|e| e.to_string())?;
    let held_qty: f64 = positions
        .iter()
        .find(|p| p.symbol == ticker)
        .and_then(|p| p.qty.parse().ok())
        .unwrap_or(0.0);

    if held_qty <= 0.0 {
        // Alpaca has no position — reconcile the DB row to closed without an order.
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            "UPDATE paper_trades SET status='closed', exit_date=?1 WHERE id=?2",
            rusqlite::params![today, trade_id],
        ).map_err(|e| e.to_string())?;
        return Err(format!("{} not held on Alpaca — marked closed in DB", ticker));
    }

    // Place the sell.
    let order = paper_trading::place_order(&ticker, held_qty, "sell")
        .await
        .map_err(|e| e.to_string())?;
    let exit_price = order
        .filled_avg_price
        .as_deref()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0.0);
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();

    // If we didn't get a fill price back synchronously, don't fabricate one;
    // the order is live and the next pipeline run will reconcile it.
    if exit_price <= 0.0 {
        return Err(format!(
            "Sell order {} placed for {} but not yet filled — it will reconcile on the next run",
            order.id, ticker
        ));
    }

    let pnl_pct = ((exit_price - entry_price) / entry_price) * 100.0;
    let pnl = (exit_price - entry_price) * held_qty;

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE paper_trades SET status='closed', exit_price=?1, exit_date=?2, pnl=?3, pnl_pct=?4
         WHERE id=?5",
        rusqlite::params![exit_price, now, pnl, pnl_pct, trade_id],
    ).map_err(|e| e.to_string())?;
    crate::services::api_usage::log_usage(&conn, "alpaca", "trading", "close_position", 0, 0).ok();

    paper_trading::get_trades_from_db(&conn, "closed")
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|t| t.id == trade_id)
        .ok_or_else(|| "Closed but could not re-read trade".to_string())
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
/// The entry side mirrors the production trading path (compound score > 0.3,
/// 5% position sizing). The EXIT side does not, and cannot: the simulator only
/// knows fixed percentages, while the live engine exits on a 3x ATR trailing
/// stop from the high-water mark and closes half the position at a 3x ATR
/// profit target. Neither is expressible here. Read this report as "did the
/// entry signal pick names that moved", not "this is what the auto-trader
/// would have returned".
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
        // -10% and +15% are an APPROXIMATION of a two-part exit the simulator
        // cannot express, not a match to any live constant. Live, the binding
        // exit is a 3x ATR trailing stop off the high-water mark; the -15% hard
        // stop is only the floor beneath it, and for most tickers the trail
        // fires well before -15% ever prints. -10% sits closer to where the
        // trail actually bites than the floor would, so it stays. The +15%
        // take-profit stands in for the 50%-of-position close at the 3x ATR
        // profit target — the simulator has no partial exits, so it closes all.
        stop_loss_pct: -10.0,
        take_profit_pct: 15.0,
        // The live engine has NO calendar expiry — the 90-day hard expiry this
        // used to cite was deleted from position_management on 2026-07-15. 90
        // is kept because max_hold_days also extends the price window the walk
        // loads (start_date .. end_date + max_hold_days), so it must stay large
        // enough to let late trades resolve. Positions it force-closes at the
        // limit are counted separately from genuine strategy exits.
        max_hold_days: 90,
        max_positions: 10,
        position_size_pct: 5.0, // matches the high-score tier in position_sizing
    };

    let result = backtester::run_backtest(&conn, config)
        .map_err(|e| format!("auto-backtest failed: {}", e))?;

    // Say what the hit rate is a hit rate OF. It is computed over closed exits
    // only, and on this data most of the walk's positions are still open when
    // the price history runs out — a bare "36% hit rate" invites reading it as a
    // verdict on a much larger sample than it is.
    let still_open = if result.open_at_end > 0 {
        format!(" ({} still open at data end)", result.open_at_end)
    } else {
        String::new()
    };
    // And say which names it could see at all. A ticker with no price bar on a
    // day it signalled is invisible to the walk, so the result covers only part
    // of the signal's universe — the live trader prices off Alpaca and has no
    // such gap, which makes this a limit of the measurement, not of the strategy.
    let coverage = if result.tickers_tradable < result.tickers_signalled {
        format!(
            ". Covers {} of {} signalled tickers — the rest had no price bar on a day they signalled",
            result.tickers_tradable, result.tickers_signalled
        )
    } else {
        String::new()
    };
    let summary = format!(
        "Auto-backtest ran: {} closed trades{}, {:.1}% hit rate, {:+.2}% return, Sharpe {:.2}, max DD {:.1}%{}",
        result.trades_taken,
        still_open,
        result.hit_rate,
        result.total_return_pct,
        result.sharpe_ratio,
        result.max_drawdown_pct,
        coverage,
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

/// Bars older than this are not evidence about today's volatility. Must match
/// `position_management::ATR_MAX_STALENESS_DAYS` — see the rationale there.
const ATR_MAX_STALENESS_DAYS: i64 = 45;

/// Mirror of pulse-fetcher's `position_management::compute_atr` — SMA of true
/// ranges over `period` days from entity_prices. The fetcher crate is a binary
/// and cannot be imported from here, so this copy must be kept in sync with
/// crates/pulse-fetcher/src/position_management.rs.
///
/// This copy only *describes* the exit plan in the UI; the fetcher's copy makes
/// the decision. They must agree exactly, or the Portfolio screen shows a
/// trailing stop the engine will not act on.
fn compute_atr(conn: &rusqlite::Connection, ticker: &str, period: usize) -> f64 {
    let cutoff = (chrono::Local::now() - chrono::Duration::days(ATR_MAX_STALENESS_DAYS))
        .format("%Y-%m-%d")
        .to_string();

    let mut stmt = match conn.prepare(
        "SELECT high, low, close FROM entity_prices
         WHERE ticker = ?1 AND date >= ?2 AND high IS NOT NULL AND low IS NOT NULL
         ORDER BY date DESC LIMIT ?3",
    ) {
        Ok(s) => s,
        Err(_) => return 0.0,
    };

    let candles: Vec<(f64, f64, f64)> = stmt
        .query_map(rusqlite::params![ticker, cutoff, period + 1], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    if candles.len() < period / 2 + 1 {
        return 0.0;
    }

    let candles: Vec<_> = candles.into_iter().rev().collect();
    let mut true_ranges = Vec::with_capacity(candles.len() - 1);
    for i in 1..candles.len() {
        let (high, low, _) = candles[i];
        let prev_close = candles[i - 1].2;
        let tr = (high - low)
            .max((high - prev_close).abs())
            .max((low - prev_close).abs());
        true_ranges.push(tr);
    }
    if true_ranges.is_empty() {
        return 0.0;
    }
    true_ranges.iter().sum::<f64>() / true_ranges.len() as f64
}

/// Days between entry_date and today, tolerating both date and datetime strings.
fn detail_days_held(entry_date: &str, today: &str) -> Option<i64> {
    let entry_str = entry_date.split('T').next().unwrap_or(entry_date);
    let today_str = today.split('T').next().unwrap_or(today);
    let entry = chrono::NaiveDate::parse_from_str(entry_str, "%Y-%m-%d").ok()?;
    let now = chrono::NaiveDate::parse_from_str(today_str, "%Y-%m-%d").ok()?;
    Some((now - entry).num_days())
}

/// A cross_signals row snapshot for the trade detail page.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TradeSignalSnapshot {
    pub computed_at: Option<String>,
    pub compound_score: f64,
    pub insider: f64,
    pub institutional: f64,
    pub news: f64,
    pub government: f64,
    pub search: f64,
    pub patent: f64,
    pub supply_chain: f64,
    pub political: f64,
    pub source_diversity: i64,
    pub convergence_detected: bool,
}

/// The live exit plan for an open trade — every level the exit engine
/// (pulse-fetcher Phase 13.6) will act on, mirrored read-only.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TradeExitPlan {
    pub current_price: f64,
    pub price_date: Option<String>,
    pub hard_stop_price: f64,
    pub fixed_stop_price: f64,
    pub no_atr_fallback: bool,
    pub atr: f64,
    pub atr_mult: f64,
    pub high_water_mark: Option<f64>,
    pub stored_trailing_stop: Option<f64>,
    pub live_trailing_stop: Option<f64>,
    pub profit_target_price: Option<f64>,
    pub half_closed_at: Option<String>,
    pub days_held: i64,
    pub max_hold_date: Option<String>,
    pub days_remaining: Option<i64>,
    pub decay_original_score: f64,
    pub decay_current_score: f64,
    pub decay_threshold: f64,
    pub decay_triggered: bool,
}

/// How the position size was derived (auto-trade sizing formula).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TradeSizing {
    pub score: f64,
    pub tier_pct: f64,
    pub notional: f64,
    pub entry_floor: f64,
    pub entry_cap: f64,
    pub clamped: bool,
    /// The account figure the tier percentage was applied to, back-derived from
    /// the notional. Deliberately not named for a specific figure: entries up to
    /// 2026-08-17 were sized off buying power, later ones off portfolio value
    /// (see `position_sizing::entry_notional`), and a row does not record which.
    /// All 11 trades in the live DB predate the change.
    pub implied_sizing_base: Option<f64>,
    pub scale_in_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TradeDetail {
    pub trade: PaperTrade,
    pub entity_name: Option<String>,
    pub created_at: Option<String>,
    pub alpaca_order_id: Option<String>,
    pub entry_signals: Option<TradeSignalSnapshot>,
    pub current_signals: Option<TradeSignalSnapshot>,
    pub exit_plan: Option<TradeExitPlan>,
    pub sizing: TradeSizing,
}

fn signal_snapshot(
    conn: &rusqlite::Connection,
    ticker: &str,
    at_or_before: Option<&str>,
) -> Option<TradeSignalSnapshot> {
    let sql = if at_or_before.is_some() {
        "SELECT computed_at, compound_score, insider_signal, institutional_flow,
                news_momentum, government_signal, search_trend, patent_signal,
                supply_chain, political_signal, source_diversity, convergence_detected
         FROM cross_signals WHERE ticker = ?1 AND date(computed_at) <= date(?2)
         ORDER BY computed_at DESC LIMIT 1"
    } else {
        "SELECT computed_at, compound_score, insider_signal, institutional_flow,
                news_momentum, government_signal, search_trend, patent_signal,
                supply_chain, political_signal, source_diversity, convergence_detected
         FROM cross_signals WHERE ticker = ?1
         ORDER BY computed_at DESC LIMIT 1"
    };
    let mut stmt = conn.prepare(sql).ok()?;
    let map = |row: &rusqlite::Row| -> rusqlite::Result<TradeSignalSnapshot> {
        Ok(TradeSignalSnapshot {
            computed_at: row.get(0).ok(),
            compound_score: row.get::<_, f64>(1).unwrap_or(0.0),
            insider: row.get::<_, f64>(2).unwrap_or(0.0),
            institutional: row.get::<_, f64>(3).unwrap_or(0.0),
            news: row.get::<_, f64>(4).unwrap_or(0.0),
            government: row.get::<_, f64>(5).unwrap_or(0.0),
            search: row.get::<_, f64>(6).unwrap_or(0.0),
            patent: row.get::<_, f64>(7).unwrap_or(0.0),
            supply_chain: row.get::<_, f64>(8).unwrap_or(0.0),
            political: row.get::<_, f64>(9).unwrap_or(0.0),
            source_diversity: row.get::<_, i64>(10).unwrap_or(0),
            convergence_detected: row.get::<_, i32>(11).unwrap_or(0) != 0,
        })
    };
    match at_or_before {
        Some(date) => stmt.query_row(rusqlite::params![ticker, date], map).ok(),
        None => stmt.query_row([ticker], map).ok(),
    }
}

/// Full forensics for one trade: entry provenance, the signal snapshot that
/// fired it, the sizing derivation, and the live exit plan. Read-only — the
/// exit numbers mirror pulse-fetcher's position_management constants (hard stop
/// -15%, ATR(14) trailing stop at 2.0/1.5/1.0x by age, profit target entry +
/// 3x ATR closing half at >= +10%, 90-day max hold, signal decay at
/// max(30% of original, 0.05)).
#[tauri::command]
pub fn get_trade_detail(db: State<'_, DbState>, trade_id: i64) -> Result<TradeDetail, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    type FullRow = (
        PaperTrade,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<i64>,
        Option<String>,
    );
    let (trade, created_at, alpaca_order_id, high_water_mark, stored_trailing_stop,
         original_compound_score, scale_in_count, half_closed_at): FullRow = conn
        .query_row(
            "SELECT id, entity_id, ticker, direction, entry_price, entry_date,
                    exit_price, exit_date, position_size, confidence,
                    signal_profile, status, pnl, pnl_pct, trade_journal,
                    created_at, alpaca_order_id, high_water_mark, trailing_stop,
                    original_compound_score, scale_in_count, half_closed_at
             FROM paper_trades WHERE id = ?1",
            [trade_id],
            |row| {
                Ok((
                    PaperTrade {
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
                    },
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                    row.get(19)?,
                    row.get(20)?,
                    row.get(21)?,
                ))
            },
        )
        .map_err(|_| format!("No trade with id {}", trade_id))?;

    let entity_name: Option<String> = conn
        .query_row(
            "SELECT name FROM entities WHERE id = ?1",
            [trade.entity_id],
            |row| row.get(0),
        )
        .ok();

    let entry_signals = signal_snapshot(&conn, &trade.ticker, Some(trade.entry_date.as_str()));
    let current_signals = signal_snapshot(&conn, &trade.ticker, None);

    // Sizing derivation — mirrors position_sizing::entry_notional tiers.
    // Long-term design (2026-07-23): doubled from 1%/2%/5%, cap $10K -> $20K.
    // The tier thresholds must stay identical to position_sizing::tier_pct: this
    // copy only EXPLAINS a size on the Portfolio screen while the fetcher's copy
    // decides it, so a drift here shows the user a rationale for a number the
    // engine did not produce.
    let score = original_compound_score.unwrap_or(trade.confidence);
    let tier_pct = if score > 0.6 { 0.10 } else if score > 0.4 { 0.05 } else { 0.02 };
    let scale_ins = scale_in_count.unwrap_or(0);
    let clamped = (trade.position_size - 50.0).abs() < 0.01
        || (trade.position_size - 20_000.0).abs() < 0.01;
    // Scale-ins add to position_size, so back-deriving buying power from the
    // final notional is only valid for never-scaled, unclamped entries.
    let implied_sizing_base = if !clamped && scale_ins == 0 && tier_pct > 0.0 {
        Some(trade.position_size / tier_pct)
    } else {
        None
    };
    let sizing = TradeSizing {
        score,
        tier_pct,
        notional: trade.position_size,
        entry_floor: 50.0,
        entry_cap: 20_000.0,
        clamped,
        implied_sizing_base,
        scale_in_count: scale_ins,
    };

    // Exit plan — open positions only.
    let exit_plan = if trade.status == "open" {
        let (current_price, price_date): (f64, Option<String>) = conn
            .query_row(
                "SELECT close, date FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
                [&trade.ticker],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or((0.0, None));

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let days = detail_days_held(&trade.entry_date, &today).unwrap_or(0);
        let atr = compute_atr(&conn, &trade.ticker, 14);
        // Long-term design (2026-07-23): flat 3x ATR, no time-based tightening,
        // no calendar-based max hold — mirrors position_management.rs.
        let atr_mult = 3.0;
        // Mirror the engine: HWM is the max of the stored mark and the latest price.
        let hwm_eff = high_water_mark
            .unwrap_or(trade.entry_price)
            .max(current_price.max(0.0));
        let live_trailing_stop = if atr > 0.0 { Some(hwm_eff - atr * atr_mult) } else { None };
        let profit_target_price = if atr > 0.0 { Some(trade.entry_price + atr * 3.0) } else { None };

        let decay_current_score = current_signals.as_ref().map(|s| s.compound_score).unwrap_or(0.0);
        let decay_threshold = (score * 0.3).max(0.05);

        Some(TradeExitPlan {
            current_price,
            price_date,
            hard_stop_price: trade.entry_price * 0.85,
            fixed_stop_price: trade.entry_price * 0.90,
            no_atr_fallback: atr <= 0.0,
            atr,
            atr_mult,
            high_water_mark,
            stored_trailing_stop,
            live_trailing_stop,
            profit_target_price,
            half_closed_at,
            days_held: days,
            max_hold_date: None,
            days_remaining: None,
            decay_original_score: score,
            decay_current_score,
            decay_threshold,
            decay_triggered: decay_current_score < decay_threshold,
        })
    } else {
        None
    };

    Ok(TradeDetail {
        trade,
        entity_name,
        created_at,
        alpaca_order_id,
        entry_signals,
        current_signals,
        exit_plan,
        sizing,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TradeRationale {
    pub text: String,
    pub generated_at: String,
    pub cached: bool,
}

/// Facts pulled from `signal_profile` JSON — same shapes `trade-reason.ts` parses.
/// Kept separate from the DB row so the async LLM call below never holds the lock.
struct RationaleFacts {
    ticker: String,
    entity_name: Option<String>,
    entry_date: String,
    entry_price: f64,
    position_size: f64,
    status: String,
    pnl_pct: Option<f64>,
    signals: Vec<(String, f64)>,
    stories: Vec<(String, Option<String>)>,
}

/// `load_calibrated_weights` (pipeline.rs) hardcodes institutional_flow and
/// supply_chain to ZERO weight in the compound score that actually gates
/// entries (institutional_flow: known substring-match bug; supply_chain:
/// market-wide constant, no per-entity signal). Their raw values sit in
/// signal_profile for audit purposes but never influenced any trade decision,
/// so they're excluded here at the source — the LLM must never be handed a
/// zero-weight number and asked to narrate it as corroborating evidence.
const ZERO_WEIGHT_SIGNAL_KEYS: [&str; 2] = ["institutional", "supply_chain"];

fn parse_signal_profile_facts(profile_json: &str) -> (Vec<(String, f64)>, Vec<(String, Option<String>)>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(profile_json) else {
        return (Vec::new(), Vec::new());
    };
    let Some(obj) = v.as_object() else {
        return (Vec::new(), Vec::new());
    };
    let mut signals: Vec<(String, f64)> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "stories" && !ZERO_WEIGHT_SIGNAL_KEYS.contains(&k.as_str()))
        .filter_map(|(k, val)| val.as_f64().map(|score| (k.replace('_', " "), score)))
        .filter(|(_, score)| *score > 0.05)
        .collect();
    signals.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let stories: Vec<(String, Option<String>)> = obj
        .get("stories")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    if let Some(title) = s.as_str() {
                        Some((title.to_string(), None))
                    } else {
                        let title = s.get("headline").or_else(|| s.get("title"))?.as_str()?;
                        let source = s.get("source").and_then(|v| v.as_str()).map(String::from);
                        Some((title.to_string(), source))
                    }
                })
                .take(3)
                .collect()
        })
        .unwrap_or_default();

    (signals, stories)
}

fn build_rationale_prompt(f: &RationaleFacts) -> String {
    let mut lines = vec![
        format!("Ticker: {}", f.ticker),
        format!("Entity: {}", f.entity_name.as_deref().unwrap_or(&f.ticker)),
        format!("Entry date: {}", f.entry_date.split('T').next().unwrap_or(&f.entry_date)),
        format!("Entry price: ${:.2}", f.entry_price),
        format!("Position size: ${:.0}", f.position_size),
        format!("Status: {}", f.status),
    ];
    if let Some(pct) = f.pnl_pct {
        lines.push(format!("P&L so far: {:+.1}%", pct));
    }
    if f.signals.is_empty() {
        lines.push("Signals: none recorded (manual or imported trade).".to_string());
    } else {
        let s = f.signals.iter().map(|(k, v)| format!("{} {:.0}%", k, v * 100.0)).collect::<Vec<_>>().join(", ");
        lines.push(format!("Signals that fired at entry (share of max strength): {}", s));
    }
    if f.stories.is_empty() {
        lines.push("Headlines captured at entry: none — the signal came from structured filings/data, not news.".to_string());
    } else {
        let s = f.stories.iter()
            .map(|(t, src)| match src {
                Some(s) => format!("\"{}\" ({})", t, s),
                None => format!("\"{}\"", t),
            })
            .collect::<Vec<_>>().join("; ");
        lines.push(format!("Headlines captured at entry: {}", s));
    }
    lines.join("\n")
}

/// Get (or lazily generate + cache) a plain-English rationale for why an auto-trade
/// fired. Generated ONCE per trade via Claude Haiku, grounded ONLY in the facts
/// above — no invented catalysts. Cached in `paper_trades.ai_rationale` so a
/// re-view never re-calls the API; pass `regenerate: true` to force a fresh call.
#[tauri::command]
pub async fn get_trade_rationale(
    db: State<'_, DbState>,
    trade_id: i64,
    regenerate: bool,
) -> Result<TradeRationale, String> {
    let facts = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;

        if !regenerate {
            let cached: Option<(String, String)> = conn
                .query_row(
                    "SELECT ai_rationale, ai_rationale_at FROM paper_trades WHERE id = ?1",
                    [trade_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok();
            if let Some((text, generated_at)) = cached {
                return Ok(TradeRationale { text, generated_at, cached: true });
            }
        }

        let (ticker, entity_id, entry_date, entry_price, position_size, status, pnl_pct, signal_profile): (
            String, i64, String, f64, f64, String, Option<f64>, String,
        ) = conn
            .query_row(
                "SELECT ticker, entity_id, entry_date, entry_price, position_size, status, pnl_pct, signal_profile
                 FROM paper_trades WHERE id = ?1",
                [trade_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
            )
            .map_err(|_| format!("No trade with id {}", trade_id))?;
        let entity_name: Option<String> = conn
            .query_row("SELECT name FROM entities WHERE id = ?1", [entity_id], |row| row.get(0))
            .ok();
        let (signals, stories) = parse_signal_profile_facts(&signal_profile);

        RationaleFacts {
            ticker, entity_name, entry_date, entry_price, position_size, status, pnl_pct, signals, stories,
        }
    };
    // Lock dropped above — safe to await from here.

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY not set. Add it to .env".to_string())?;
    let prompt = build_rationale_prompt(&facts);
    let text = call_haiku_for_rationale(&api_key, &prompt).await?;
    let generated_at = chrono::Utc::now().to_rfc3339();

    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE paper_trades SET ai_rationale = ?1, ai_rationale_at = ?2 WHERE id = ?3",
            rusqlite::params![text, generated_at, trade_id],
        )
        .map_err(|e| e.to_string())?;
        crate::services::api_usage::log_usage(
            &conn, "anthropic", crate::services::conversation::CONVERSATION_MODEL_FAST,
            "trade_rationale", (prompt.len() / 4) as i64, (text.len() / 4) as i64,
        ).ok();
    }

    Ok(TradeRationale { text, generated_at, cached: false })
}

async fn call_haiku_for_rationale(api_key: &str, facts: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let system = "You explain a paper-trading system's past automated decisions to the \
        person reviewing their own portfolio. Write 3-4 plain-English sentences: \
        (1) why THIS trade fired — the specific signal categories and their strength; \
        (2) the algorithmic thesis — why the system treats that signal combination as \
        bullish in general (e.g. lobbying/political activity and government contract \
        signals are treated as leading indicators of future revenue for a small-cap; \
        news momentum signals attention/flow). Use ONLY the facts given — never invent \
        news, catalysts, financials, or company details not listed, and never claim this \
        specific company will go up. If headlines were captured, summarize what they were \
        about; if not, say plainly the signal came from structured filings/data rather \
        than news. (3) End with one blunt sentence: this strategy's historical backtest \
        showed NO measured statistical edge (overfit, failed out-of-sample) — it is \
        running live only as an unproven forward paper-test with fake money, not vetted \
        investment advice. Do not soften or omit this caveat. No markdown, no preamble, \
        no quotes around the output, no disclaimers about being an AI.";
    let body = serde_json::json!({
        "model": crate::services::conversation::CONVERSATION_MODEL_FAST,
        "max_tokens": 280,
        "system": system,
        "messages": [{"role": "user", "content": facts}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Anthropic request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Anthropic API {}: {}", status, body));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let text = json["content"][0]["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err("Anthropic returned an empty rationale".to_string());
    }
    Ok(text)
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingCalibrationRow {
    pub id: i64,
    pub batch_id: String,
    pub computed_at: String,
    pub dimension: String,
    pub old_weight: f64,
    pub new_weight: f64,
    pub hit_rate: Option<f64>,
    pub sample_size: Option<i64>,
    pub total_resolved: i64,
    pub status: String,
}

/// List calibration proposals (default: only 'pending', newest batch first).
/// Task 3.3 — calibration.rs::adjust_weights writes here instead of touching
/// live weights directly; this is the review surface before an explicit apply.
#[tauri::command]
pub fn get_pending_calibration(
    db: State<'_, DbState>,
    status: Option<String>,
) -> Result<Vec<PendingCalibrationRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let status = status.unwrap_or_else(|| "pending".to_string());
    let mut stmt = conn.prepare(
        "SELECT id, batch_id, computed_at, dimension, old_weight, new_weight,
                hit_rate, sample_size, total_resolved, status
         FROM pending_calibration
         WHERE status = ?1
         ORDER BY computed_at DESC, dimension ASC"
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([&status], |row| {
        Ok(PendingCalibrationRow {
            id: row.get(0)?,
            batch_id: row.get(1)?,
            computed_at: row.get(2)?,
            dimension: row.get(3)?,
            old_weight: row.get(4)?,
            new_weight: row.get(5)?,
            hit_rate: row.get(6)?,
            sample_size: row.get(7)?,
            total_resolved: row.get(8)?,
            status: row.get(9)?,
        })
    }).map_err(|e| e.to_string())?
    .filter_map(|r| r.ok())
    .collect();
    Ok(rows)
}

/// Apply a pending calibration batch to live weights (explicit, manual —
/// Nicola triggers this; nothing here fires automatically). Writes the
/// batch's (dimension, new_weight) pairs to `user_profile.calibrated_weights`
/// as the same JSON shape `pipeline.rs::load_calibrated_weights` reads, then
/// marks every row in the batch 'applied'. Passing `batch_id: None` applies
/// the most recent pending batch.
#[tauri::command]
pub fn apply_pending_calibration(
    db: State<'_, DbState>,
    batch_id: Option<String>,
) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let batch_id = match batch_id {
        Some(b) => b,
        None => conn
            .query_row(
                "SELECT batch_id FROM pending_calibration WHERE status = 'pending' ORDER BY computed_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(|_| "No pending calibration proposal to apply".to_string())?,
    };

    let rows: Vec<(String, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT dimension, new_weight FROM pending_calibration WHERE batch_id = ?1 AND status = 'pending'"
        ).map_err(|e| e.to_string())?;
        stmt.query_map([&batch_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };

    if rows.is_empty() {
        return Err(format!("No pending rows found for batch '{}'", batch_id));
    }

    let weights_json = serde_json::to_string(&rows).map_err(|e| e.to_string())?;
    let applied_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();

    conn.execute(
        "INSERT OR REPLACE INTO user_profile (key, value) VALUES ('calibrated_weights', ?1)",
        [&weights_json],
    ).map_err(|e| e.to_string())?;

    let updated = conn.execute(
        "UPDATE pending_calibration SET status = 'applied', applied_at = ?1 WHERE batch_id = ?2 AND status = 'pending'",
        rusqlite::params![applied_at, batch_id],
    ).map_err(|e| e.to_string())?;

    tracing::warn!("Calibration: batch {} APPLIED to live weights ({} dimensions)", batch_id, updated);

    Ok(updated)
}

/// Reject a pending calibration batch without touching live weights (explicit,
/// manual — the review counterpart to apply). Passing `batch_id: None` rejects
/// the most recent pending batch.
#[tauri::command]
pub fn reject_pending_calibration(
    db: State<'_, DbState>,
    batch_id: Option<String>,
) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let batch_id = match batch_id {
        Some(b) => b,
        None => conn
            .query_row(
                "SELECT batch_id FROM pending_calibration WHERE status = 'pending' ORDER BY computed_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(|_| "No pending calibration proposal to reject".to_string())?,
    };

    let applied_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let updated = conn.execute(
        "UPDATE pending_calibration SET status = 'rejected', applied_at = ?1 WHERE batch_id = ?2 AND status = 'pending'",
        rusqlite::params![applied_at, batch_id],
    ).map_err(|e| e.to_string())?;

    if updated == 0 {
        return Err(format!("No pending rows found for batch '{}'", batch_id));
    }

    tracing::warn!("Calibration: batch {} REJECTED, live weights untouched ({} dimensions)", batch_id, updated);

    Ok(updated)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrationGateStatus {
    pub total_resolved: i64,
    pub threshold: i64,
}

/// How close the live paper-trading loop is to the calibration gate
/// (calibration.rs proposes a reweight once total_resolved >= threshold).
/// Same WHERE clause as calibration.rs::analyze_signal_performance so this
/// count always matches what actually gates a proposal. `threshold: 10` is
/// duplicated from calibration.rs's `>= 10` check — keep both in sync if
/// that changes, or this display silently drifts from the real gate.
#[tauri::command]
pub fn get_calibration_gate_status(db: State<'_, DbState>) -> Result<CalibrationGateStatus, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let total_resolved: i64 = conn.query_row(
        "SELECT COUNT(*) FROM paper_trades WHERE status IN ('closed', 'stopped_out', 'expired') AND pnl_pct IS NOT NULL",
        [],
        |row| row.get(0),
    ).map_err(|e| e.to_string())?;
    Ok(CalibrationGateStatus { total_resolved, threshold: 10 })
}
