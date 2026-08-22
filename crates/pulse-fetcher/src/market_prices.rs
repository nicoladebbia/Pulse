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
    #[allow(dead_code)] // part of Finnhub's quote payload; not consumed yet
    t: i64,  // timestamp
}

/// Finnhub quotes we fetch per day. ~4,100 public tickers compete for these.
pub const DAILY_PRICE_SLOTS: usize = 200;

/// Write one candle, filling gaps in a row that already exists.
///
/// This replaces the `INSERT OR IGNORE` that every candle path used to issue.
/// `OR IGNORE` was aimed at the right thing — a quote row for today is more
/// authoritative than a settled candle, so `close` must not be overwritten —
/// but it threw away the whole row rather than just the close, and rows with a
/// close and no range do exist. ATR needs the range, so every backfill aimed at
/// repairing one was silently discarded.
///
/// Measured 2026-08-17: 330 rows across 31 tickers in the trailing 45 days hold
/// a close with NULL high/low — the window `ATR_MAX_STALENESS_DAYS` reads. They
/// were written by the app's own `refresh_prices`
/// (`src-tauri/src/commands/cross_signals.rs`), which deserialised only `c`/`pc`
/// from the Finnhub quote and stored it with `INSERT OR REPLACE`, nulling the
/// range on every date it touched. That writer made the holes and this one could
/// not fill them; both halves are fixed, and this half also guards against any
/// future writer that leaves a column empty.
///
/// The `WHERE` clause on the conflict branch is what makes the return value
/// mean something: an already-complete row updates nothing and reports 0, a
/// fresh insert or a filled hole reports 1. Callers sum this, so their counter
/// now counts rows actually written rather than rows attempted.
///
/// It has to test the incoming value too, not just the stored one. `volume` is
/// NULL in all 19,517 live rows and no caller supplies it, so a bare
/// `volume IS NULL` made the guard true for every conflict, the update ran on
/// every row, and `changes()` reported 1 — reintroducing the attempt-counting
/// this function exists to remove. A column is only worth updating when the row
/// lacks it *and* the caller has it.
#[allow(clippy::too_many_arguments)] // one parameter per column; a struct here would only rename them
fn upsert_candle(
    conn: &Connection,
    entity_id: Option<i64>,
    ticker: &str,
    date: &str,
    open: Option<f64>,
    close: f64,
    high: Option<f64>,
    low: Option<f64>,
    volume: Option<i64>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO entity_prices (entity_id, ticker, date, open, close, high, low, volume)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(ticker, date) DO UPDATE SET
             open   = COALESCE(open, excluded.open),
             high   = COALESCE(high, excluded.high),
             low    = COALESCE(low, excluded.low),
             volume = COALESCE(volume, excluded.volume)
         WHERE (open   IS NULL AND excluded.open   IS NOT NULL)
            OR (high   IS NULL AND excluded.high   IS NOT NULL)
            OR (low    IS NULL AND excluded.low    IS NOT NULL)
            OR (volume IS NULL AND excluded.volume IS NOT NULL)",
        rusqlite::params![entity_id, ticker, date, open, close, high, low, volume],
    )
}

/// Choose which tickers get one of the day's limited price slots.
///
/// Two groups are pinned to the front, because for them a missing candle is not
/// a gap in a chart — it is a decision the system cannot make:
///
/// 1. **Open positions.** The exit engine's trailing stop and profit target are
///    computed from `entity_prices` candles, so a held ticker with no rows has
///    no ATR and falls back to a crude fixed stop. Found 2026-07-19: ARM had 0
///    rows while holding a $4k position.
/// 2. **Recently signalled tickers.** Same failure one step earlier. A
///    convergence signal is what the trader acts on, yet a signalled ticker used
///    to compete on `entity_tickers.confidence` — an entity-extraction score
///    with nothing to do with trading — and lost. Measured 2026-08-17: 13 of the
///    14 tickers that signalled in the previous 30 days had no bar that day, and
///    all 14 were `is_public = 1`, so they were losing the tie-break, not the
///    eligibility check. Names as liquid as MU, QCOM, CMCSA and NU had never
///    received a single row; NU alone had signalled 44 times.
///
/// The 30-day window keeps the pinned set bounded — it was 14 tickers of the 200
/// slots when this was written, against 65 for all time. Pinning costs no extra
/// API calls; it only reorders who gets the calls already being made.
///
/// `GROUP BY` dedups tickers mapped by several entity rows so they don't burn
/// two slots. Front position also keeps both groups ahead of any Finnhub 429s
/// that hit later in the list.
pub fn select_tickers_to_price(
    conn: &Connection,
    today: &str,
    limit: usize,
) -> anyhow::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT MIN(et.entity_id), et.ticker FROM entity_tickers et
         WHERE et.is_public = 1
         AND et.ticker NOT IN (
             SELECT ticker FROM entity_prices WHERE date = ?1
         )
         GROUP BY et.ticker
         ORDER BY (et.ticker IN (SELECT ticker FROM paper_trades WHERE status = 'open')) DESC,
                  (et.ticker IN (
                      SELECT ticker FROM cross_signals
                      WHERE convergence_detected = 1
                        AND ticker IS NOT NULL AND ticker <> ''
                        AND computed_at >= date(?1, '-30 days')
                  )) DESC,
                  MAX(et.confidence) DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![today, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
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

    let tickers = select_tickers_to_price(&conn, &today, DAILY_PRICE_SLOTS)?;

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
    // Held and recently-signalled tickers first, for the same reason they are
    // pinned in `select_tickers_to_price`: on a day with many fresh tickers the
    // LIMIT 30 otherwise picks an arbitrary subset and a held ticker can miss
    // the cut (ARM did on 2026-07-19).
    //
    // This list matters more to the exit engine than the quote list does. A
    // daily quote contributes one bar; `compute_atr` needs over half of a
    // 14-day period inside its freshness window before it will return an ATR at
    // all, and this backfill is what supplies those bars in one call. A
    // newly-signalled ticker that misses it is priced but still un-stoppable.
    let needs_backfill: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT ep.ticker FROM entity_prices ep
             WHERE ep.date = ?1 AND (ep.change_7d IS NULL OR ep.change_30d IS NULL)
             GROUP BY ep.ticker
             ORDER BY (ep.ticker IN (SELECT ticker FROM paper_trades WHERE status = 'open')) DESC,
                      (ep.ticker IN (
                          SELECT ticker FROM cross_signals
                          WHERE convergence_detected = 1
                            AND ticker IS NOT NULL AND ticker <> ''
                            AND computed_at >= date(?1, '-30 days')
                      )) DESC
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
                // `n` counts rows actually written, not bars received — see
                // `upsert_candle`. That distinction is the whole point: this
                // counter used to increment once per bar the API returned, so a
                // run whose every write was discarded still reported success.
                //
                // Measured 2026-08-17 — twelve freshly-quoted signalled tickers
                // went through this loop (their change_7d/change_30d were
                // recomputed, which only happens for names in this list) and
                // ended the run with one usable bar each, while
                // `--mode backfill-prices` stored 28 apiece for NVDA/META/AAPL
                // seconds later off the same binary and keys. The mechanism
                // found for that is the `INSERT OR IGNORE` those writes used:
                // a ticker with a quote-only row on every date in the window
                // has every candle silently dropped and keeps zero range. That
                // is fixed at the write, and this arm now reports the residue
                // instead of a guess about it.
                Ok(_) => tracing::warn!(
                    "Candle backfill wrote no rows for {ticker} — priced today but ATR history unchanged"
                ),
                Err(e) => tracing::warn!("Candle backfill FAILED for {ticker}: {e}"),
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

/// Alpaca daily bars → entity_prices rows via `upsert_candle`, which keeps an
/// existing row's `close` authoritative while still filling its missing range.
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

        stored += upsert_candle(conn, entity_id, ticker, &date, open, close, high, low, None)?;
    }

    crate::db::log_api_usage(conn, "alpaca", "bars", "backfill_candles", 0, 0);
    Ok(stored)
}

/// Ad-hoc candle backfill for a specific ticker list, independent of the daily
/// pipeline's 200/30-per-day caps and open-position pinning (see `fetch_prices`).
/// Used by `--mode backfill-prices` to fill price coverage for entities that
/// never won a daily-fetch slot — e.g. calibration-backtest-universe Task 3.0
/// found signal candidates (ORCL/TME/NVRI/GSCE/ACN) with no price rows near
/// their open/resolve dates because the live fetcher had never reached them.
pub async fn backfill_candles_for_tickers(
    db_path: &std::path::Path,
    tickers: &[String],
    days_back: i64,
) -> anyhow::Result<usize> {
    let api_key = std::env::var("FINNHUB_API_KEY").unwrap_or_default();
    let conn = Connection::open(db_path)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut total = 0;
    for ticker in tickers {
        match backfill_candles(&client, &api_key, &conn, ticker, days_back).await {
            Ok(n) => {
                tracing::info!("backfill-prices {}: stored {} candles", ticker, n);
                total += n;
            }
            Err(e) => tracing::warn!("backfill-prices {}: failed: {}", ticker, e),
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    Ok(total)
}

#[derive(Debug, Deserialize)]
struct FinnhubProfile {
    #[serde(rename = "marketCapitalization", default)]
    market_cap: f64, // in millions USD; Finnhub returns 0/missing for no profile (e.g. non-equities)
}

/// Universe quality gate: reject sub-$300M market cap / sub-$1 price tickers, and
/// require Alpaca to confirm the symbol is actually tradable, before a signal can
/// enter the auto-trade buy path. Cached in `ticker_eligibility_cache` (7-day TTL)
/// so the live auto-trade loop doesn't re-hit Finnhub/Alpaca every cycle for the
/// same name. Fails CLOSED — any missing data point (no Finnhub profile, no
/// price, Alpaca error) rejects the ticker rather than passing it through.
///
/// Built after the calibration-backtest-universe Task 3.0 audit found the
/// client/registrant entity-type swap bug (see `pipeline.rs`'s
/// `extract_entities_from_financial_metadata`): fixing that bug makes lobbying
/// "client" names newly ticker-eligible, which includes trade associations and
/// nonprofits (e.g. "SPORTS BETTING ALLIANCE") that could fuzzy-match a real
/// ticker. This gate is the safety net — Task 5.1 (market cap/price) + Task 5.2
/// (Alpaca tradability), 2026-07-24.
pub async fn check_ticker_universe_eligibility(
    client: &reqwest::Client,
    conn: &Connection,
    finnhub_key: &str,
    alpaca_key: &str,
    alpaca_secret: &str,
    ticker: &str,
) -> bool {
    const MIN_MARKET_CAP_MILLIONS: f64 = 300.0;
    const MIN_PRICE: f64 = 1.0;

    if let Ok(eligible) = conn.query_row(
        "SELECT eligible FROM ticker_eligibility_cache
         WHERE ticker = ?1 AND checked_at >= datetime('now', '-7 days')",
        [ticker],
        |row| row.get::<_, i64>(0),
    ) {
        return eligible == 1;
    }

    let mut market_cap: Option<f64> = None;
    let mut last_price: Option<f64> = None;
    let mut alpaca_tradable = false;
    let mut alpaca_status = String::new();
    // Tracks whether each upstream call actually succeeded, so a rejection can be
    // logged as "data says ineligible" vs "couldn't reach Finnhub/Alpaca" — these
    // look identical downstream (both leave the Option unset / bool false) but are
    // operationally very different: the latter fails EVERY candidate closed and,
    // unlogged distinctly, is indistinguishable from a quiet zero-trades day.
    let mut finnhub_profile_ok = false;
    let mut finnhub_quote_ok = false;
    let mut alpaca_ok = false;

    if !finnhub_key.is_empty() {
        match client
            .get(format!("https://finnhub.io/api/v1/stock/profile2?symbol={}&token={}", ticker, finnhub_key))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                finnhub_profile_ok = true;
                if let Ok(profile) = resp.json::<FinnhubProfile>().await
                    && profile.market_cap > 0.0 {
                        market_cap = Some(profile.market_cap);
                    }
            }
            Ok(resp) => {
                tracing::warn!("Universe gate: Finnhub profile2 for {} returned status {}", ticker, resp.status());
            }
            Err(e) => {
                tracing::warn!("Universe gate: Finnhub profile2 request failed for {}: {}", ticker, e);
            }
        }
        match client
            .get(format!("https://finnhub.io/api/v1/quote?symbol={}&token={}", ticker, finnhub_key))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                finnhub_quote_ok = true;
                if let Ok(quote) = resp.json::<FinnhubQuote>().await
                    && quote.c > 0.0 {
                        last_price = Some(quote.c);
                    }
            }
            Ok(resp) => {
                tracing::warn!("Universe gate: Finnhub quote for {} returned status {}", ticker, resp.status());
            }
            Err(e) => {
                tracing::warn!("Universe gate: Finnhub quote request failed for {}: {}", ticker, e);
            }
        }
    } else {
        tracing::warn!("Universe gate: FINNHUB_API_KEY unset — {} will fail closed", ticker);
    }

    if !alpaca_key.is_empty() && !alpaca_secret.is_empty() {
        match client
            .get(format!("https://paper-api.alpaca.markets/v2/assets/{}", ticker))
            .header("APCA-API-KEY-ID", alpaca_key)
            .header("APCA-API-SECRET-KEY", alpaca_secret)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                alpaca_ok = true;
                if let Ok(asset) = resp.json::<serde_json::Value>().await {
                    alpaca_tradable = asset.get("tradable").and_then(|v| v.as_bool()).unwrap_or(false);
                    alpaca_status = asset.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                }
            }
            Ok(resp) => {
                tracing::warn!("Universe gate: Alpaca assets for {} returned status {}", ticker, resp.status());
            }
            Err(e) => {
                tracing::warn!("Universe gate: Alpaca assets request failed for {}: {}", ticker, e);
            }
        }
    } else {
        tracing::warn!("Universe gate: Alpaca keys unset — {} will fail closed", ticker);
    }

    if !finnhub_profile_ok || !finnhub_quote_ok || !alpaca_ok {
        tracing::warn!(
            "Universe gate: {} — one or more upstream checks unreachable (finnhub_profile_ok={}, finnhub_quote_ok={}, alpaca_ok={}); rejection below may be an outage, not a real ineligibility",
            ticker, finnhub_profile_ok, finnhub_quote_ok, alpaca_ok
        );
    }

    let eligible = market_cap.map(|m| m >= MIN_MARKET_CAP_MILLIONS).unwrap_or(false)
        && last_price.map(|p| p >= MIN_PRICE).unwrap_or(false)
        && alpaca_tradable
        && alpaca_status == "active";

    let _ = conn.execute(
        "INSERT INTO ticker_eligibility_cache
             (ticker, market_cap_millions, last_price, alpaca_tradable, alpaca_status, eligible, checked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(ticker) DO UPDATE SET
             market_cap_millions = excluded.market_cap_millions,
             last_price = excluded.last_price,
             alpaca_tradable = excluded.alpaca_tradable,
             alpaca_status = excluded.alpaca_status,
             eligible = excluded.eligible,
             checked_at = excluded.checked_at",
        rusqlite::params![ticker, market_cap, last_price, alpaca_tradable as i64, alpaca_status, eligible as i64],
    );

    if !eligible {
        tracing::info!(
            "Universe gate: {} REJECTED (market_cap={:?}M, price={:?}, alpaca_tradable={}, alpaca_status={:?})",
            ticker, market_cap, last_price, alpaca_tradable, alpaca_status
        );
    }

    eligible
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

        stored += upsert_candle(conn, entity_id, ticker, &date, open, close, high, low, None)?;
    }

    Ok(stored)
}

/// Fetch historical daily candles for backtesting.
/// Uses Finnhub stock candle endpoint.
// Reachable entry point for `--mode backfill-prices`; no in-process caller, so rustc
// cannot see it is used.
#[allow(dead_code)]
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

        stored += upsert_candle(&conn, entity_id, ticker, &date, open, close, high, low, volume)?;
    }

    crate::db::log_api_usage(&conn, "finnhub", "candle", "fetch_historical", 0, 0);
    Ok(stored)
}


#[cfg(test)]
mod slot_priority_tests {
    use super::*;

    /// A universe of `noise` high-confidence tickers that would fill every slot,
    /// plus the named specials at the *lowest* possible confidence.
    fn db(noise: usize, specials: &[&str]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entity_tickers (entity_id INTEGER PRIMARY KEY, ticker TEXT NOT NULL,
                is_public INTEGER DEFAULT 1, confidence REAL DEFAULT 1.0);
             CREATE TABLE entity_prices (ticker TEXT NOT NULL, date TEXT NOT NULL, close REAL);
             CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, ticker TEXT, status TEXT);
             CREATE TABLE cross_signals (id INTEGER PRIMARY KEY AUTOINCREMENT, ticker TEXT,
                convergence_detected INTEGER DEFAULT 0, computed_at TEXT);",
        )
        .unwrap();
        for i in 0..noise {
            conn.execute(
                "INSERT INTO entity_tickers (entity_id, ticker, confidence) VALUES (?1, ?2, 1.0)",
                rusqlite::params![i as i64 + 1000, format!("NOISE{i}")],
            )
            .unwrap();
        }
        for (i, t) in specials.iter().enumerate() {
            conn.execute(
                "INSERT INTO entity_tickers (entity_id, ticker, confidence) VALUES (?1, ?2, 0.01)",
                rusqlite::params![i as i64 + 1, t],
            )
            .unwrap();
        }
        conn
    }

    fn picked(conn: &Connection, limit: usize) -> Vec<String> {
        select_tickers_to_price(conn, "2026-08-17", limit)
            .unwrap()
            .into_iter()
            .map(|(_, t)| t)
            .collect()
    }

    #[test]
    fn a_signalled_ticker_outranks_the_confidence_field() {
        let conn = db(300, &["SIGNALLED"]);
        conn.execute(
            "INSERT INTO cross_signals (ticker, convergence_detected, computed_at)
             VALUES ('SIGNALLED', 1, '2026-08-10')",
            [],
        )
        .unwrap();
        let got = picked(&conn, DAILY_PRICE_SLOTS);
        assert_eq!(got.len(), DAILY_PRICE_SLOTS);
        assert!(
            got.contains(&"SIGNALLED".to_string()),
            "a ticker the trader acts on must not lose its slot to entity-extraction confidence"
        );
    }

    #[test]
    fn an_open_position_still_outranks_a_signal() {
        let conn = db(300, &["SIGNALLED", "HELD"]);
        conn.execute(
            "INSERT INTO cross_signals (ticker, convergence_detected, computed_at)
             VALUES ('SIGNALLED', 1, '2026-08-10')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO paper_trades (ticker, status) VALUES ('HELD', 'open')",
            [],
        )
        .unwrap();
        // Only one slot: money already committed beats a candidate entry.
        assert_eq!(picked(&conn, 1), vec!["HELD".to_string()]);
    }

    #[test]
    fn a_stale_signal_does_not_hold_a_slot_forever() {
        let conn = db(300, &["OLD"]);
        conn.execute(
            "INSERT INTO cross_signals (ticker, convergence_detected, computed_at)
             VALUES ('OLD', 1, '2026-01-05')",
            [],
        )
        .unwrap();
        assert!(!picked(&conn, DAILY_PRICE_SLOTS).contains(&"OLD".to_string()));
    }

    #[test]
    fn a_non_convergent_signal_earns_no_pin() {
        let conn = db(300, &["WEAK"]);
        conn.execute(
            "INSERT INTO cross_signals (ticker, convergence_detected, computed_at)
             VALUES ('WEAK', 0, '2026-08-10')",
            [],
        )
        .unwrap();
        assert!(!picked(&conn, DAILY_PRICE_SLOTS).contains(&"WEAK".to_string()));
    }

    #[test]
    fn an_empty_ticker_never_takes_a_slot() {
        // Not observed live — 2026-08-17 the table held 1,091 convergence rows
        // with a NULL ticker (entities with no listed equity) and zero empty
        // strings. NULL is excluded by the IS NOT NULL clause; this guards the
        // `<> ''` clause beside it, which nothing else exercises.
        let conn = db(10, &[""]);
        conn.execute(
            "INSERT INTO cross_signals (ticker, convergence_detected, computed_at)
             VALUES ('', 1, '2026-08-10')",
            [],
        )
        .unwrap();
        assert!(!picked(&conn, 5).contains(&String::new()));
    }

    #[test]
    fn the_entity_id_travels_with_the_ticker() {
        // The caller writes this id straight into entity_prices.entity_id, so a
        // GROUP BY that returned the wrong MIN — or a swapped tuple — would file
        // every quote under the wrong entity without failing any ticker check.
        let conn = db(5, &["SIGNALLED"]);
        conn.execute(
            "INSERT INTO cross_signals (ticker, convergence_detected, computed_at)
             VALUES ('SIGNALLED', 1, '2026-08-10')",
            [],
        )
        .unwrap();
        // Same ticker mapped by a second, higher entity row: MIN must win.
        conn.execute(
            "INSERT INTO entity_tickers (entity_id, ticker, confidence)
             VALUES (9999, 'SIGNALLED', 0.9)",
            [],
        )
        .unwrap();
        let rows = select_tickers_to_price(&conn, "2026-08-17", DAILY_PRICE_SLOTS).unwrap();
        let hit: Vec<_> = rows.iter().filter(|(_, t)| t == "SIGNALLED").collect();
        assert_eq!(hit.len(), 1, "a ticker mapped twice must not burn two slots");
        assert_eq!(hit[0].0, 1, "expected the MIN entity_id, got {}", hit[0].0);
    }

    #[test]
    fn a_ticker_already_priced_today_is_not_refetched() {
        let conn = db(3, &["DONE"]);
        conn.execute(
            "INSERT INTO entity_prices (ticker, date, close) VALUES ('DONE', '2026-08-17', 10.0)",
            [],
        )
        .unwrap();
        assert!(!picked(&conn, 10).contains(&"DONE".to_string()));
    }
}

#[cfg(test)]
mod candle_write_tests {
    use super::*;

    /// The real constraint set, because the bug being guarded here IS the
    /// conflict behaviour. A test table without `UNIQUE(ticker, date)` would
    /// never take the conflict branch and would pass against the old
    /// `INSERT OR IGNORE` too.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entity_prices (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_id INTEGER, ticker TEXT NOT NULL, date TEXT NOT NULL,
                open REAL, close REAL NOT NULL, high REAL, low REAL, volume INTEGER,
                change_1d REAL, change_7d REAL, change_30d REAL,
                UNIQUE(ticker, date));",
        )
        .unwrap();
        conn
    }

    /// A quote row: Finnhub gave a price but no range, so the row has a close
    /// and nothing ATR can use. 330 such rows existed live on 2026-08-17.
    fn quote_only(conn: &Connection, ticker: &str, date: &str, close: f64) {
        conn.execute(
            "INSERT INTO entity_prices (ticker, date, close) VALUES (?1, ?2, ?3)",
            rusqlite::params![ticker, date, close],
        )
        .unwrap();
    }

    fn row(conn: &Connection, ticker: &str, date: &str) -> (Option<f64>, f64, Option<f64>, Option<f64>) {
        conn.query_row(
            "SELECT open, close, high, low FROM entity_prices WHERE ticker = ?1 AND date = ?2",
            rusqlite::params![ticker, date],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn a_candle_fills_the_range_a_quote_row_is_missing() {
        let conn = db();
        quote_only(&conn, "NVDA", "2026-08-01", 100.0);

        let wrote =
            upsert_candle(&conn, Some(7), "NVDA", "2026-08-01", Some(99.0), 101.0, Some(105.0), Some(98.0), None)
                .unwrap();

        let (open, _, high, low) = row(&conn, "NVDA", "2026-08-01");
        assert_eq!(high, Some(105.0), "the candle's high must land in the empty column");
        assert_eq!(low, Some(98.0));
        assert_eq!(open, Some(99.0));
        assert_eq!(wrote, 1, "filling a hole is a write and must be counted as one");
    }

    #[test]
    fn the_quote_close_stays_authoritative() {
        // This is what INSERT OR IGNORE was protecting and the fix must keep:
        // today's quote is live, the settled candle for the same date may
        // disagree, and the quote wins.
        let conn = db();
        quote_only(&conn, "NVDA", "2026-08-01", 100.0);

        upsert_candle(&conn, Some(7), "NVDA", "2026-08-01", None, 101.0, Some(105.0), Some(98.0), None).unwrap();

        let (_, close, high, _) = row(&conn, "NVDA", "2026-08-01");
        assert_eq!(close, 100.0, "the candle's close must not overwrite the quote's");
        assert_eq!(high, Some(105.0), "…while its range still lands");
    }

    #[test]
    fn a_row_that_needs_nothing_reports_no_write() {
        // The counter feeds a `warn!` on zero. A complete row must report 0 so
        // that signal means "nothing landed", not "nothing was asked for".
        let conn = db();
        upsert_candle(&conn, Some(7), "NVDA", "2026-08-01", Some(99.0), 100.0, Some(105.0), Some(98.0), Some(10))
            .unwrap();

        let again =
            upsert_candle(&conn, Some(7), "NVDA", "2026-08-01", Some(99.0), 100.0, Some(105.0), Some(98.0), Some(10))
                .unwrap();

        assert_eq!(again, 0, "a complete row must not be counted as written again");
    }

    #[test]
    fn a_caller_with_nothing_to_add_reports_no_write() {
        // The live shape, and the one the first version of this guard missed.
        // No candle path supplies `volume`, and every stored row has it NULL, so
        // a guard that only asked "is the column empty?" was true on every
        // conflict and counted every row as written. The guard has to ask
        // whether the *caller* can fill the gap.
        let conn = db();
        conn.execute(
            "INSERT INTO entity_prices (ticker, date, open, close, high, low)
             VALUES ('NVDA', '2026-08-01', 99.0, 100.0, 105.0, 98.0)",
            [],
        )
        .unwrap();

        let wrote =
            upsert_candle(&conn, Some(7), "NVDA", "2026-08-01", Some(99.0), 100.0, Some(105.0), Some(98.0), None)
                .unwrap();

        assert_eq!(wrote, 0, "volume is NULL here and stays NULL — that is not a write");
    }

    #[test]
    fn a_fresh_date_is_inserted_and_counted() {
        let conn = db();
        let wrote =
            upsert_candle(&conn, Some(7), "NVDA", "2026-08-01", Some(99.0), 100.0, Some(105.0), Some(98.0), None)
                .unwrap();
        assert_eq!(wrote, 1);
        let (_, close, high, _) = row(&conn, "NVDA", "2026-08-01");
        assert_eq!(close, 100.0);
        assert_eq!(high, Some(105.0));
    }

    #[test]
    fn the_whole_window_becomes_atr_usable_not_just_the_untouched_dates() {
        // The live shape: a ticker quoted every day for a month, then backfilled.
        // Under INSERT OR IGNORE this ended with zero usable bars no matter how
        // many candles the provider returned — which is the symptom that was
        // observed on twelve signalled tickers and could not be explained from
        // the counter, because the counter said every bar had been stored.
        let conn = db();
        for d in 1..=20 {
            quote_only(&conn, "LMT", &format!("2026-08-{d:02}"), 400.0 + d as f64);
        }

        let mut wrote = 0;
        for d in 1..=20 {
            wrote += upsert_candle(
                &conn,
                Some(7),
                "LMT",
                &format!("2026-08-{d:02}"),
                Some(400.0),
                410.0,
                Some(420.0 + d as f64),
                Some(390.0),
                None,
            )
            .unwrap();
        }

        let usable: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entity_prices
                 WHERE ticker = 'LMT' AND high IS NOT NULL AND low IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(usable, 20, "every quoted date must end up ATR-usable");
        assert_eq!(wrote, 20, "and each fill must be counted");

        let (_, close, _, _) = row(&conn, "LMT", "2026-08-05");
        assert_eq!(close, 405.0, "each date keeps its own quote close");
    }
}
