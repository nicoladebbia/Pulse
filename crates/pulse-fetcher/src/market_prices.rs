use rusqlite::Connection;
use serde::Deserialize;

/// Finnhub market price fetcher.
/// Fetches daily quotes for all entities with ticker mappings.
/// Stores in entity_prices table — NOT as stories (prices aren't content).
///
/// API: https://finnhub.io/api/v1/quote
/// Free tier: 60 calls/min, 30 calls/sec
/// Env var: FINNHUB_API_KEY

#[derive(Debug, Deserialize)]
struct FinnhubQuote {
    c: f64,  // current price
    h: f64,  // high
    l: f64,  // low
    o: f64,  // open
    pc: f64, // previous close
    t: i64,  // timestamp
}

/// Fetch and store prices for all entities with ticker mappings.
/// Returns the number of prices stored.
pub async fn fetch_prices(db_path: &std::path::Path) -> anyhow::Result<usize> {
    let api_key = match std::env::var("FINNHUB_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("Finnhub: skipping (FINNHUB_API_KEY not set)");
            return Ok(0);
        }
    };

    let conn = Connection::open(db_path)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Get all tickers that need price updates today. Open-position tickers are
    // pinned to the FRONT of the list: the exit engine's ATR levels (trailing
    // stop, profit target) read entity_prices candles, and with ~4,100 public
    // tickers competing for 200 slots on a confidence tie-break, a held ticker
    // can otherwise never get a single candle (ARM: 0 rows while holding a
    // $4k position — found 2026-07-19). Front position also keeps them ahead
    // of any Finnhub 429s that hit later in the list. GROUP BY dedups tickers
    // mapped by multiple entity rows so they don't burn extra API slots.
    let tickers: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT MIN(et.entity_id), et.ticker FROM entity_tickers et
             WHERE et.is_public = 1
             AND et.ticker NOT IN (
                 SELECT ticker FROM entity_prices WHERE date = ?1
             )
             GROUP BY et.ticker
             ORDER BY (et.ticker IN (SELECT ticker FROM paper_trades WHERE status = 'open')) DESC,
                      MAX(et.confidence) DESC
             LIMIT 200"
        )?;
        stmt.query_map([&today], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if tickers.is_empty() {
        tracing::info!("Finnhub: no tickers need price updates today");
        return Ok(0);
    }

    tracing::info!("Finnhub: fetching prices for {} tickers", tickers.len());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut stored = 0;
    let mut errors = 0;

    for (entity_id, ticker) in &tickers {
        match fetch_quote(&client, &api_key, ticker).await {
            Ok(Some(quote)) => {
                // Compute change percentages from historical data
                let change_1d = if quote.pc > 0.0 {
                    Some(((quote.c - quote.pc) / quote.pc) * 100.0)
                } else {
                    None
                };

                let change_7d = compute_change(&conn, ticker, quote.c, 7);
                let change_30d = compute_change(&conn, ticker, quote.c, 30);

                conn.execute(
                    "INSERT OR REPLACE INTO entity_prices
                     (entity_id, ticker, date, open, close, high, low, volume, change_1d, change_7d, change_30d)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10)",
                    rusqlite::params![
                        entity_id, ticker, today,
                        quote.o, quote.c, quote.h, quote.l,
                        change_1d, change_7d, change_30d,
                    ],
                )?;
                stored += 1;

                // Log API usage
                crate::db::log_api_usage(&conn, "finnhub", "quote", "fetch_price", 0, 0);
            }
            Ok(None) => {} // No data (market closed, invalid ticker)
            Err(e) => {
                tracing::warn!("Finnhub {} failed: {}", ticker, e);
                errors += 1;
                if errors > 5 {
                    tracing::warn!("Finnhub: too many errors, stopping early");
                    break;
                }
            }
        }

        // Rate limit: 100ms between requests (stay under 60/min = 1/sec)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    tracing::info!("Finnhub: stored {} prices ({} errors)", stored, errors);

    // Backfill 30-day candle history for tickers missing 7d/30d changes.
    // Open-position tickers first — on a day with many fresh tickers the
    // LIMIT 30 otherwise picks an arbitrary subset and a held ticker can miss
    // the cut (ARM did on 2026-07-19).
    let needs_backfill: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT ep.ticker FROM entity_prices ep
             WHERE ep.date = ?1 AND (ep.change_7d IS NULL OR ep.change_30d IS NULL)
             GROUP BY ep.ticker
             ORDER BY (ep.ticker IN (SELECT ticker FROM paper_trades WHERE status = 'open')) DESC
             LIMIT 30"
        )?;
        stmt.query_map([&today], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if !needs_backfill.is_empty() {
        tracing::info!("Finnhub: backfilling 30-day candles for {} tickers", needs_backfill.len());
        let mut backfilled = 0;
        for ticker in &needs_backfill {
            match backfill_candles(&client, &api_key, &conn, ticker, 35).await {
                Ok(n) if n > 0 => backfilled += 1,
                Ok(_) => {}
                Err(e) => tracing::debug!("Candle backfill failed for {}: {}", ticker, e),
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }

        // Now recompute 7d/30d changes for today's prices using the backfilled history
        if backfilled > 0 {
            for ticker in &needs_backfill {
                let current: Option<f64> = conn.query_row(
                    "SELECT close FROM entity_prices WHERE ticker = ?1 AND date = ?2",
                    rusqlite::params![ticker, today], |row| row.get(0),
                ).ok();
                if let Some(price) = current {
                    let c7 = compute_change(&conn, ticker, price, 7);
                    let c30 = compute_change(&conn, ticker, price, 30);
                    conn.execute(
                        "UPDATE entity_prices SET change_7d = ?1, change_30d = ?2
                         WHERE ticker = ?3 AND date = ?4",
                        rusqlite::params![c7, c30, ticker, today],
                    ).ok();
                }
            }
            tracing::info!("Finnhub: backfilled candles for {} tickers, recomputed 7d/30d changes", backfilled);
        }
    }

    Ok(stored)
}

/// Fetch a single quote from Finnhub.
async fn fetch_quote(
    client: &reqwest::Client,
    api_key: &str,
    ticker: &str,
) -> anyhow::Result<Option<FinnhubQuote>> {
    let url = format!(
        "https://finnhub.io/api/v1/quote?symbol={}&token={}",
        ticker, api_key
    );

    let resp = client.get(&url).send().await?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        tracing::warn!("Finnhub rate limited, pausing 2s");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        return Ok(None);
    }

    if !resp.status().is_success() {
        anyhow::bail!("Finnhub API returned {}", resp.status());
    }

    let quote: FinnhubQuote = resp.json().await?;

    // Finnhub returns c=0 for invalid tickers or when market is closed
    if quote.c == 0.0 && quote.o == 0.0 {
        return Ok(None);
    }

    Ok(Some(quote))
}

/// Compute price change % from N days ago.
fn compute_change(conn: &Connection, ticker: &str, current_price: f64, days: i64) -> Option<f64> {
    let past_price: Option<f64> = conn
        .query_row(
            "SELECT close FROM entity_prices
             WHERE ticker = ?1 AND date <= date('now', ?2)
             ORDER BY date DESC LIMIT 1",
            rusqlite::params![ticker, format!("-{} days", days)],
            |row| row.get(0),
        )
        .ok();

    past_price.and_then(|past| {
        if past > 0.0 {
            Some(((current_price - past) / past) * 100.0)
        } else {
            None
        }
    })
}

/// Backfill candle history for a ticker (used during daily pipeline).
///
/// Primary source: Alpaca's free IEX daily bars (same keys the trading side
/// already uses) — verified live against ARM 2026-07-19, recent closes match
/// the consolidated tape within cents. The legacy Finnhub candle endpoint is
/// premium-gated on the current key (403 "You don't have access to this
/// resource", so it had silently never backfilled anything) and is only
/// attempted when Alpaca keys are absent.
async fn backfill_candles(
    client: &reqwest::Client,
    api_key: &str,
    conn: &Connection,
    ticker: &str,
    days_back: i64,
) -> anyhow::Result<usize> {
    let alpaca_key = std::env::var("ALPACA_API_KEY").unwrap_or_default();
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY").unwrap_or_default();
    if !alpaca_key.is_empty() && !alpaca_secret.is_empty() {
        return backfill_candles_alpaca(client, &alpaca_key, &alpaca_secret, conn, ticker, days_back).await;
    }
    backfill_candles_finnhub(client, api_key, conn, ticker, days_back).await
}

/// Alpaca daily bars → entity_prices rows. INSERT OR IGNORE keeps existing
/// quote-sourced rows (e.g. today's) authoritative.
async fn backfill_candles_alpaca(
    client: &reqwest::Client,
    alpaca_key: &str,
    alpaca_secret: &str,
    conn: &Connection,
    ticker: &str,
    days_back: i64,
) -> anyhow::Result<usize> {
    let start = (chrono::Utc::now() - chrono::Duration::days(days_back))
        .format("%Y-%m-%d")
        .to_string();
    let url = format!(
        "https://data.alpaca.markets/v2/stocks/{}/bars?timeframe=1Day&start={}&limit=60&adjustment=raw&feed=iex",
        ticker, start
    );

    let resp = client
        .get(&url)
        .header("APCA-API-KEY-ID", alpaca_key)
        .header("APCA-API-SECRET-KEY", alpaca_secret)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Alpaca bars API returned {}", resp.status());
    }

    let data: serde_json::Value = resp.json().await?;
    let bars = match data.get("bars").and_then(|v| v.as_array()) {
        Some(b) if !b.is_empty() => b.clone(),
        _ => return Ok(0),
    };

    let entity_id: Option<i64> = conn
        .query_row(
            "SELECT MIN(entity_id) FROM entity_tickers WHERE ticker = ?1",
            [ticker],
            |row| row.get(0),
        )
        .ok();

    let mut stored = 0;
    for bar in &bars {
        // "t" is RFC3339 ("2026-06-15T04:00:00Z") — the date prefix is the day.
        let date: String = bar
            .get("t")
            .and_then(|v| v.as_str())
            .map(|t| t.chars().take(10).collect())
            .unwrap_or_default();
        let close = bar.get("c").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if date.len() != 10 || close <= 0.0 {
            continue;
        }
        let open = bar.get("o").and_then(|v| v.as_f64());
        let high = bar.get("h").and_then(|v| v.as_f64());
        let low = bar.get("l").and_then(|v| v.as_f64());

        conn.execute(
            "INSERT OR IGNORE INTO entity_prices (entity_id, ticker, date, open, close, high, low)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![entity_id, ticker, date, open, close, high, low],
        )?;
        stored += 1;
    }

    crate::db::log_api_usage(conn, "alpaca", "bars", "backfill_candles", 0, 0);
    Ok(stored)
}

/// Legacy Finnhub candle backfill — premium-only on free keys.
async fn backfill_candles_finnhub(
    client: &reqwest::Client,
    api_key: &str,
    conn: &Connection,
    ticker: &str,
    days_back: i64,
) -> anyhow::Result<usize> {
    let now = chrono::Utc::now().timestamp();
    let from = now - (days_back * 86400);

    let url = format!(
        "https://finnhub.io/api/v1/stock/candle?symbol={}&resolution=D&from={}&to={}&token={}",
        ticker, from, now, api_key
    );

    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Finnhub candle API returned {}", resp.status());
    }

    let data: serde_json::Value = resp.json().await?;
    let status = data.get("s").and_then(|s| s.as_str()).unwrap_or("no_data");
    if status != "ok" {
        return Ok(0);
    }

    let closes = data.get("c").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let opens = data.get("o").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let highs = data.get("h").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let lows = data.get("l").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let timestamps = data.get("t").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let entity_id: Option<i64> = conn
        .query_row("SELECT entity_id FROM entity_tickers WHERE ticker = ?1", [ticker], |row| row.get(0))
        .ok();

    let mut stored = 0;
    for i in 0..closes.len() {
        let ts = timestamps.get(i).and_then(|v| v.as_i64()).unwrap_or(0);
        let date = chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        if date.is_empty() { continue; }

        let close = closes.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let open = opens.get(i).and_then(|v| v.as_f64());
        let high = highs.get(i).and_then(|v| v.as_f64());
        let low = lows.get(i).and_then(|v| v.as_f64());

        conn.execute(
            "INSERT OR IGNORE INTO entity_prices (entity_id, ticker, date, open, close, high, low)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![entity_id, ticker, date, open, close, high, low],
        )?;
        stored += 1;
    }

    Ok(stored)
}

/// Fetch historical daily candles for backtesting.
/// Uses Finnhub stock candle endpoint.
pub async fn fetch_historical(
    db_path: &std::path::Path,
    ticker: &str,
    days_back: i64,
) -> anyhow::Result<usize> {
    let api_key = match std::env::var("FINNHUB_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => anyhow::bail!("FINNHUB_API_KEY not set"),
    };

    let conn = Connection::open(db_path)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let now = chrono::Utc::now().timestamp();
    let from = now - (days_back * 86400);

    let url = format!(
        "https://finnhub.io/api/v1/stock/candle?symbol={}&resolution=D&from={}&to={}&token={}",
        ticker, from, now, api_key
    );

    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Finnhub candle API returned {}", resp.status());
    }

    let data: serde_json::Value = resp.json().await?;

    let status = data.get("s").and_then(|s| s.as_str()).unwrap_or("no_data");
    if status != "ok" {
        return Ok(0);
    }

    let closes = data.get("c").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let opens = data.get("o").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let highs = data.get("h").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let lows = data.get("l").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let volumes = data.get("v").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let timestamps = data.get("t").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // Look up entity_id for this ticker
    let entity_id: Option<i64> = conn
        .query_row(
            "SELECT entity_id FROM entity_tickers WHERE ticker = ?1",
            [ticker],
            |row| row.get(0),
        )
        .ok();

    let mut stored = 0;
    for i in 0..closes.len() {
        let ts = timestamps.get(i).and_then(|v| v.as_i64()).unwrap_or(0);
        let date = chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        if date.is_empty() {
            continue;
        }

        let close = closes.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let open = opens.get(i).and_then(|v| v.as_f64());
        let high = highs.get(i).and_then(|v| v.as_f64());
        let low = lows.get(i).and_then(|v| v.as_f64());
        let volume = volumes.get(i).and_then(|v| v.as_i64());

        conn.execute(
            "INSERT OR IGNORE INTO entity_prices
             (entity_id, ticker, date, open, close, high, low, volume)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![entity_id, ticker, date, open, close, high, low, volume],
        )?;
        stored += 1;
    }

    crate::db::log_api_usage(&conn, "finnhub", "candle", "fetch_historical", 0, 0);
    Ok(stored)
}
