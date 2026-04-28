use anyhow::Result;
use rusqlite::Connection;
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
// Portfolio queries
// ---------------------------------------------------------------------------

/// Get trades from DB (synchronous — call before dropping lock).
pub fn get_trades_from_db(conn: &Connection, status: &str) -> Result<Vec<PaperTrade>> {
    get_trades_by_status(conn, status)
}

/// Build portfolio from pre-fetched trades + async Alpaca API calls.
/// Merges live Alpaca P&L data into DB open_trades for accurate display.
pub async fn get_portfolio_with_trades(
    mut open_trades: Vec<PaperTrade>,
    closed_trades: Vec<PaperTrade>,
) -> Result<Portfolio> {
    let account = get_account().await?;

    let alpaca_positions = get_positions().await.unwrap_or_default();
    let positions: Vec<Position> = alpaca_positions
        .iter()
        .map(|p| Position {
            symbol: p.symbol.clone(),
            qty: p.qty.parse().unwrap_or(0.0),
            avg_entry_price: p.avg_entry_price.parse().unwrap_or(0.0),
            current_price: p.current_price.parse().unwrap_or(0.0),
            market_value: p.market_value.parse().unwrap_or(0.0),
            unrealized_pl: p.unrealized_pl.parse().unwrap_or(0.0),
            unrealized_pl_pct: p.unrealized_plpc.parse().unwrap_or(0.0),
            side: p.side.clone(),
        })
        .collect();

    // Merge Alpaca live P&L into DB open_trades
    for trade in &mut open_trades {
        if let Some(pos) = alpaca_positions.iter().find(|p| p.symbol == trade.ticker) {
            let current_price = pos.current_price.parse::<f64>().unwrap_or(0.0);
            let unrealized_pl = pos.unrealized_pl.parse::<f64>().unwrap_or(0.0);
            let unrealized_pct = pos.unrealized_plpc.parse::<f64>().unwrap_or(0.0);
            if current_price > 0.0 {
                trade.pnl = Some(unrealized_pl);
                trade.pnl_pct = Some(unrealized_pct * 100.0); // Convert from decimal to %
            }
        }
    }

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
