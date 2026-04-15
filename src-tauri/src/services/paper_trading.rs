use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Alpaca paper trading integration.
/// API: https://paper-api.alpaca.markets/v2
/// Free paper trading, real-time simulation.
/// Env vars: ALPACA_API_KEY, ALPACA_SECRET_KEY

const PAPER_BASE_URL: &str = "https://paper-api.alpaca.markets/v2";

// ---------------------------------------------------------------------------
// Alpaca API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AlpacaAccount {
    pub id: String,
    pub account_number: String,
    pub equity: String,
    pub cash: String,
    pub buying_power: String,
    pub portfolio_value: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct AlpacaPosition {
    pub symbol: String,
    pub qty: String,
    pub avg_entry_price: String,
    pub current_price: String,
    pub market_value: String,
    pub unrealized_pl: String,
    pub unrealized_plpc: String,
    pub side: String,
}

#[derive(Debug, Deserialize)]
pub struct AlpacaOrder {
    pub id: String,
    pub symbol: String,
    pub qty: Option<String>,
    pub filled_qty: Option<String>,
    pub filled_avg_price: Option<String>,
    pub side: String,
    pub status: String,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Public API types (for frontend)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portfolio {
    pub equity: f64,
    pub cash: f64,
    pub buying_power: f64,
    pub portfolio_value: f64,
    pub positions: Vec<Position>,
    pub open_trades: Vec<PaperTrade>,
    pub closed_trades: Vec<PaperTrade>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub qty: f64,
    pub avg_entry_price: f64,
    pub current_price: f64,
    pub market_value: f64,
    pub unrealized_pl: f64,
    pub unrealized_pl_pct: f64,
    pub side: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperTrade {
    pub id: i64,
    pub entity_id: i64,
    pub ticker: String,
    pub direction: String,
    pub entry_price: f64,
    pub entry_date: String,
    pub exit_price: Option<f64>,
    pub exit_date: Option<String>,
    pub position_size: f64,
    pub confidence: f64,
    pub signal_profile: String,
    pub status: String,
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
}

// ---------------------------------------------------------------------------
// Alpaca API client
// ---------------------------------------------------------------------------

fn get_credentials() -> Result<(String, String)> {
    let key = std::env::var("ALPACA_API_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_API_KEY not set"))?;
    let secret = std::env::var("ALPACA_SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY not set"))?;
    Ok((key, secret))
}

fn build_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?)
}

/// Get Alpaca paper trading account info.
pub async fn get_account() -> Result<AlpacaAccount> {
    let (key, secret) = get_credentials()?;
    let client = build_client()?;

    let resp = client
        .get(format!("{}/account", PAPER_BASE_URL))
        .header("APCA-API-KEY-ID", &key)
        .header("APCA-API-SECRET-KEY", &secret)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Alpaca account API returned {}: {}", status, body);
    }

    Ok(resp.json().await?)
}

/// Get all open positions from Alpaca.
pub async fn get_positions() -> Result<Vec<AlpacaPosition>> {
    let (key, secret) = get_credentials()?;
    let client = build_client()?;

    let resp = client
        .get(format!("{}/positions", PAPER_BASE_URL))
        .header("APCA-API-KEY-ID", &key)
        .header("APCA-API-SECRET-KEY", &secret)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Alpaca positions API returned {}", resp.status());
    }

    Ok(resp.json().await?)
}

/// Place a market order on Alpaca paper trading.
pub async fn place_order(
    symbol: &str,
    qty: f64,
    side: &str, // "buy" or "sell"
) -> Result<AlpacaOrder> {
    let (key, secret) = get_credentials()?;
    let client = build_client()?;

    let body = serde_json::json!({
        "symbol": symbol,
        "qty": qty.to_string(),
        "side": side,
        "type": "market",
        "time_in_force": "day"
    });

    let resp = client
        .post(format!("{}/orders", PAPER_BASE_URL))
        .header("APCA-API-KEY-ID", &key)
        .header("APCA-API-SECRET-KEY", &secret)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Alpaca order API returned {}: {}", status, body);
    }

    Ok(resp.json().await?)
}

// ---------------------------------------------------------------------------
// Signal-to-trade execution
// ---------------------------------------------------------------------------

/// Execute a paper trade based on cross-signal convergence.
/// Position sizing based on confidence:
///   HIGH (>0.75): 5% of portfolio
///   MODERATE (0.6-0.75): 2% of portfolio
///   LOW (0.5-0.6): 1% of portfolio
pub async fn execute_signal_trade(
    conn: &Connection,
    entity_id: i64,
    ticker: &str,
    confidence: f64,
    signal_profile: &str,
) -> Result<Option<PaperTrade>> {
    // Check: not already in a position for this ticker
    let already_open: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM paper_trades WHERE ticker = ?1 AND status = 'open')",
            [ticker],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if already_open {
        tracing::info!("Skipping {} — already have open position", ticker);
        return Ok(None);
    }

    // Check: total open positions < 15
    let open_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM paper_trades WHERE status = 'open'", [], |row| row.get(0))
        .unwrap_or(0);

    if open_count >= 15 {
        tracing::info!("Skipping {} — already at max 15 open positions", ticker);
        return Ok(None);
    }

    // Get account to determine position size
    let account = get_account().await?;
    let portfolio_value: f64 = account.portfolio_value.parse().unwrap_or(100_000.0);

    let position_pct = if confidence > 0.75 {
        0.05 // 5% for high confidence
    } else if confidence > 0.60 {
        0.02 // 2% for moderate
    } else {
        0.01 // 1% for low
    };

    let position_value = portfolio_value * position_pct;

    // Get current price to calculate qty
    let current_price: f64 = conn
        .query_row(
            "SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
            [ticker],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    if current_price <= 0.0 {
        tracing::warn!("No price data for {} — skipping trade", ticker);
        return Ok(None);
    }

    let qty = (position_value / current_price).floor().max(1.0);

    // Place the order
    let order = place_order(ticker, qty, "buy").await?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let filled_price: f64 = order
        .filled_avg_price
        .as_deref()
        .and_then(|p| p.parse().ok())
        .unwrap_or(current_price);

    // Record in paper_trades
    conn.execute(
        "INSERT INTO paper_trades (entity_id, ticker, direction, entry_price, entry_date,
            position_size, confidence, signal_profile, alpaca_order_id, status)
         VALUES (?1, ?2, 'long', ?3, ?4, ?5, ?6, ?7, ?8, 'open')",
        params![
            entity_id, ticker, filled_price, today,
            position_value, confidence, signal_profile, order.id,
        ],
    )?;

    let trade_id = conn.last_insert_rowid();

    tracing::info!(
        "Paper trade executed: BUY {} {} shares @ ${:.2} (confidence: {:.0}%, size: ${:.0})",
        ticker, qty as i64, filled_price, confidence * 100.0, position_value
    );

    // Log API usage
    crate::services::api_usage::log_usage(conn, "alpaca", "trading", "place_order", 0, 0).ok();

    Ok(Some(PaperTrade {
        id: trade_id,
        entity_id,
        ticker: ticker.to_string(),
        direction: "long".to_string(),
        entry_price: filled_price,
        entry_date: today,
        exit_price: None,
        exit_date: None,
        position_size: position_value,
        confidence,
        signal_profile: signal_profile.to_string(),
        status: "open".to_string(),
        pnl: None,
        pnl_pct: None,
    }))
}

// ---------------------------------------------------------------------------
// Portfolio queries
// ---------------------------------------------------------------------------

/// Get trades from DB (synchronous — call before dropping lock).
pub fn get_trades_from_db(conn: &Connection, status: &str) -> Result<Vec<PaperTrade>> {
    get_trades_by_status(conn, status)
}

/// Build portfolio from pre-fetched trades + async Alpaca API calls.
pub async fn get_portfolio_with_trades(
    open_trades: Vec<PaperTrade>,
    closed_trades: Vec<PaperTrade>,
) -> Result<Portfolio> {
    let account = get_account().await?;

    let alpaca_positions = get_positions().await.unwrap_or_default();
    let positions: Vec<Position> = alpaca_positions
        .into_iter()
        .map(|p| Position {
            symbol: p.symbol,
            qty: p.qty.parse().unwrap_or(0.0),
            avg_entry_price: p.avg_entry_price.parse().unwrap_or(0.0),
            current_price: p.current_price.parse().unwrap_or(0.0),
            market_value: p.market_value.parse().unwrap_or(0.0),
            unrealized_pl: p.unrealized_pl.parse().unwrap_or(0.0),
            unrealized_pl_pct: p.unrealized_plpc.parse().unwrap_or(0.0),
            side: p.side,
        })
        .collect();

    Ok(Portfolio {
        equity: account.equity.parse().unwrap_or(0.0),
        cash: account.cash.parse().unwrap_or(0.0),
        buying_power: account.buying_power.parse().unwrap_or(0.0),
        portfolio_value: account.portfolio_value.parse().unwrap_or(0.0),
        positions,
        open_trades,
        closed_trades,
    })
}

fn get_trades_by_status(conn: &Connection, status: &str) -> Result<Vec<PaperTrade>> {
    let mut stmt = conn.prepare(
        "SELECT id, entity_id, ticker, direction, entry_price, entry_date,
                exit_price, exit_date, position_size, confidence,
                signal_profile, status, pnl, pnl_pct
         FROM paper_trades WHERE status = ?1
         ORDER BY entry_date DESC LIMIT 50"
    )?;

    let trades = stmt
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
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(trades)
}

/// Evaluate and close positions that have hit their time limit (90 days) or stop-loss (-10%).
pub async fn evaluate_positions(conn: &Connection) -> Result<(usize, usize)> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut closed = 0;
    let mut stopped = 0;

    let open_trades: Vec<(i64, String, f64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, ticker, entry_price, entry_date FROM paper_trades WHERE status = 'open'"
        )?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    };

    for (trade_id, ticker, entry_price, entry_date) in &open_trades {
        // Get current price
        let current_price: f64 = conn
            .query_row(
                "SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
                [ticker],
                |row| row.get(0),
            )
            .unwrap_or(0.0);

        if current_price <= 0.0 {
            continue;
        }

        let pnl_pct = ((current_price - entry_price) / entry_price) * 100.0;
        let pnl = current_price - entry_price;

        // Check stop-loss: -10%
        if pnl_pct <= -10.0 {
            conn.execute(
                "UPDATE paper_trades SET status = 'stopped_out', exit_price = ?1, exit_date = ?2, pnl = ?3, pnl_pct = ?4 WHERE id = ?5",
                params![current_price, today, pnl, pnl_pct, trade_id],
            )?;
            tracing::info!("Stop-loss triggered: {} at {:.1}%", ticker, pnl_pct);
            stopped += 1;
            continue;
        }

        // Check time limit: 90 days
        if let Ok(entry) = chrono::NaiveDate::parse_from_str(entry_date, "%Y-%m-%d") {
            let today_date = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d").unwrap_or(entry);
            let days_held = (today_date - entry).num_days();

            if days_held >= 90 {
                conn.execute(
                    "UPDATE paper_trades SET status = 'expired', exit_price = ?1, exit_date = ?2, pnl = ?3, pnl_pct = ?4 WHERE id = ?5",
                    params![current_price, today, pnl, pnl_pct, trade_id],
                )?;
                tracing::info!("Position expired: {} after {} days ({:.1}%)", ticker, days_held, pnl_pct);
                closed += 1;
            }
        }
    }

    Ok((closed, stopped))
}
