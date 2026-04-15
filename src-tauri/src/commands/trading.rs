use tauri::State;
use crate::db::DbState;
use crate::services::paper_trading::{self, Portfolio, PaperTrade};

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
    paper_trading::get_portfolio_with_trades(open_trades, closed_trades)
        .await
        .map_err(|e| e.to_string())
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
                    signal_profile, status, pnl, pnl_pct
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
    }))
}
