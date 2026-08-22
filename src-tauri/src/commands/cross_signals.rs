use tauri::State;
use serde::{Deserialize, Serialize};
use crate::db::DbState;
use crate::services::cross_signals::{self, CrossSignal};

/// One `cross_signals` row as the convergence query reads it, in SELECT order:
/// `(id, entity_name, ticker, compound_score, insider, institutional, news,
///  government, search, patent, supply, political, source_diversity,
///  convergence_detected, signal_profile)`.
type ConvergenceRow = (
    i64,
    String,
    Option<String>,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    f64,
    i64,
    bool,
    Option<String>,
);

/// Get entities with the strongest cross-signal scores.
/// Cross-signals are computed during the daily pipeline run — never recompute from the UI.
#[tauri::command]
pub fn get_cross_signals(db: State<'_, DbState>, limit: Option<usize>) -> Result<Vec<CrossSignal>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(20);
    cross_signals::get_top_signals(&conn, limit).map_err(|e| e.to_string())
}

/// Get entities with convergence detected (multi-signal alignment).
#[tauri::command]
pub fn get_convergence_alerts(db: State<'_, DbState>, limit: Option<usize>) -> Result<Vec<CrossSignal>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(10);

    cross_signals::get_convergence_signals(&conn, limit).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityPrice {
    pub ticker: String,
    pub date: String,
    pub close: f64,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub change_1d: Option<f64>,
    pub change_7d: Option<f64>,
    pub change_30d: Option<f64>,
    pub entity_name: Option<String>,
}

/// Get the MOST-RECENT price row per ticker (not the global latest date).
///
/// Previously this filtered `WHERE date = (SELECT MAX(date) FROM entity_prices)`,
/// which dropped every ticker whose newest row was any earlier date — on a day
/// when only ~142 of ~525 tickers refreshed, 383 tickers with a perfectly good
/// 1-day-old price rendered as "no price." We now pick each ticker's own latest
/// row via ROW_NUMBER, so a slightly-stale-but-valid price still shows (the
/// frontend surfaces the date + a staleness badge). No extra Finnhub calls —
/// this reads rows already stored.
#[tauri::command]
pub fn get_entity_prices(db: State<'_, DbState>, limit: Option<usize>) -> Result<Vec<EntityPrice>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(100);

    let mut stmt = conn
        .prepare(
            "WITH latest AS (
                 SELECT ticker, date, close, open, high, low, change_1d, change_7d, change_30d,
                        ROW_NUMBER() OVER (PARTITION BY ticker ORDER BY date DESC) AS rn
                 FROM entity_prices
             )
             SELECT l.ticker, l.date, l.close, l.open, l.high, l.low,
                    l.change_1d, l.change_7d, l.change_30d,
                    (SELECT e.name FROM entity_tickers et
                     JOIN entities e ON e.id = et.entity_id
                     WHERE et.ticker = l.ticker
                     ORDER BY et.confidence DESC LIMIT 1) AS entity_name
             FROM latest l
             WHERE l.rn = 1
             ORDER BY l.date DESC, l.close DESC
             LIMIT ?1"
        )
        .map_err(|e| e.to_string())?;

    let prices = stmt
        .query_map([limit as i64], |row| {
            Ok(EntityPrice {
                ticker: row.get(0)?,
                date: row.get(1)?,
                close: row.get(2)?,
                open: row.get(3)?,
                high: row.get(4)?,
                low: row.get(5)?,
                change_1d: row.get(6)?,
                change_7d: row.get(7)?,
                change_30d: row.get(8)?,
                entity_name: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(prices)
}

/// Store one refreshed quote without destroying what the row already knows.
///
/// The refresh owns the close and the change columns — that is what it went to
/// fetch. It does **not** own the day's range: a settled daily candle written by
/// `pulse-fetcher`'s backfill is a better high/low than an intraday quote's, so
/// an existing value is kept and the quote only fills a gap.
///
/// `INSERT OR REPLACE` could not express that. It deletes the conflicting row
/// and inserts a new one, so every unlisted column reverts to NULL — the range
/// included. Measured 2026-08-17: 330 rows over 31 tickers in the trailing 45
/// days held a close with no range, and `pulse-fetcher`'s candle backfill used
/// `INSERT OR IGNORE`, which could never repair them. This is the writer that
/// made the holes; that was the writer that could not fill them.
#[allow(clippy::too_many_arguments)]
fn write_refreshed_quote(
    conn: &rusqlite::Connection,
    entity_id: i64,
    ticker: &str,
    today: &str,
    close: f64,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    change_1d: Option<f64>,
    change_7d: Option<f64>,
    change_30d: Option<f64>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO entity_prices
           (entity_id, ticker, date, open, close, high, low, change_1d, change_7d, change_30d)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(ticker, date) DO UPDATE SET
             close      = excluded.close,
             change_1d  = excluded.change_1d,
             change_7d  = excluded.change_7d,
             change_30d = excluded.change_30d,
             open       = COALESCE(open, excluded.open),
             high       = COALESCE(high, excluded.high),
             low        = COALESCE(low, excluded.low)",
        rusqlite::params![
            entity_id, ticker, today, open, close, high, low, change_1d, change_7d, change_30d
        ],
    )
}

/// Refresh prices for top tickers from Finnhub. Called on signals page load.
#[tauri::command]
pub async fn refresh_prices(db: State<'_, DbState>) -> Result<usize, String> {
    let api_key = std::env::var("FINNHUB_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Ok(0);
    }

    // Get tickers to refresh (from DB, drop lock before async)
    let tickers: Vec<(i64, String)> = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        // Open-position tickers always refresh first — they feed the trade
        // detail page and exit-level display; the confidence-ranked rest fill
        // the remaining slots (same fix as pulse-fetcher market_prices.rs).
        let mut stmt = conn.prepare(
            "SELECT MIN(et.entity_id), et.ticker FROM entity_tickers et
             WHERE et.is_public = 1 AND (et.confidence >= 0.8
                 OR et.ticker IN (SELECT ticker FROM paper_trades WHERE status = 'open'))
             GROUP BY et.ticker
             ORDER BY (et.ticker IN (SELECT ticker FROM paper_trades WHERE status = 'open')) DESC,
                      MAX(et.confidence) DESC
             LIMIT 25"
        ).map_err(|e| e.to_string())?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };
    // Lock dropped

    if tickers.is_empty() { return Ok(0); }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build().map_err(|e| e.to_string())?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut updated = 0;

    for (entity_id, ticker) in &tickers {
        let url = format!("https://finnhub.io/api/v1/quote?symbol={}&token={}", ticker, api_key);
        let resp = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };

        // `h`, `l` and `o` are in every Finnhub quote payload and were simply
        // not being read. Dropping them was not neutral: the write below used
        // INSERT OR REPLACE, so each refresh replaced a full candle with a
        // close-only row and erased the day's range. ATR is computed from that
        // range, and this loop refreshes open positions FIRST — so the tickers
        // whose exit levels depend on ATR were the ones being blinded.
        #[derive(serde::Deserialize)]
        struct Q {
            c: f64,
            pc: f64,
            #[serde(default)]
            h: Option<f64>,
            #[serde(default)]
            l: Option<f64>,
            #[serde(default)]
            o: Option<f64>,
        }
        let quote: Q = match resp.json().await {
            Ok(q) => q,
            Err(_) => continue,
        };
        if quote.c == 0.0 { continue; }

        let change_1d = if quote.pc > 0.0 {
            Some(((quote.c - quote.pc) / quote.pc) * 100.0)
        } else { None };

        // Re-acquire lock briefly to write
        {
            let conn = db.0.lock().map_err(|e| e.to_string())?;

            // Compute 7d/30d from historical data
            let change_7d: Option<f64> = conn.query_row(
                "SELECT close FROM entity_prices WHERE ticker = ?1 AND date <= date('now', '-7 days') ORDER BY date DESC LIMIT 1",
                [&ticker], |row| row.get(0),
            ).ok().and_then(|past: f64| if past > 0.0 { Some(((quote.c - past) / past) * 100.0) } else { None });

            let change_30d: Option<f64> = conn.query_row(
                "SELECT close FROM entity_prices WHERE ticker = ?1 AND date <= date('now', '-30 days') ORDER BY date DESC LIMIT 1",
                [&ticker], |row| row.get(0),
            ).ok().and_then(|past: f64| if past > 0.0 { Some(((quote.c - past) / past) * 100.0) } else { None });

            write_refreshed_quote(
                &conn, *entity_id, ticker, &today, quote.c,
                quote.o, quote.h, quote.l, change_1d, change_7d, change_30d,
            ).ok();
            updated += 1;
        }
        // Drop lock, rate limit
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }

    // Log Finnhub API usage: 1 row per actual quote call (not batched)
    if updated > 0 {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(
            "INSERT INTO api_usage (provider, model, endpoint, input_tokens, output_tokens, estimated_cost_usd) VALUES ('finnhub', 'quote', 'refresh_prices', 0, 0, 0.0)"
        ).map_err(|e| e.to_string())?;
        for _ in 0..updated {
            stmt.execute([]).ok();
        }
    }

    Ok(updated)
}

/// A recent financial event from any source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinancialEvent {
    pub id: i64,
    pub source_name: String,
    pub feed_id: String,
    pub headline: String,
    pub summary: String,
    pub published_at: Option<String>,
    pub sector: String,
    pub financial_metadata: Option<String>,
}

/// Get recent financial events (activity feed).
#[tauri::command]
pub fn get_financial_events(db: State<'_, DbState>, limit: Option<usize>) -> Result<Vec<FinancialEvent>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(30);

    // Show financial stories first, then recent high-relevance news
    let mut stmt = conn
        .prepare(
            "SELECT id, source_name, COALESCE(original_url, '') as feed_id, headline, summary,
                    published_at, sector, financial_metadata
             FROM stories
             WHERE source_type = 'financial'
                OR (relevance_score >= 7 AND created_at >= datetime('now', '-3 days'))
             ORDER BY
               CASE WHEN source_type = 'financial' THEN 0 ELSE 1 END,
               created_at DESC
             LIMIT ?1"
        )
        .map_err(|e| e.to_string())?;

    let events = stmt
        .query_map([limit as i64], |row| {
            Ok(FinancialEvent {
                id: row.get(0)?,
                source_name: row.get(1)?,
                feed_id: row.get(2)?,
                headline: row.get(3)?,
                summary: row.get(4)?,
                published_at: row.get(5)?,
                sector: row.get(6)?,
                financial_metadata: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(events)
}

/// Evidence explaining why a signal is firing for an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEvidence {
    pub entity_name: String,
    pub ticker: Option<String>,
    pub compound_score: f64,
    pub reasons: Vec<String>,
    pub source_stories: Vec<EvidenceStory>,
    pub price: Option<f64>,
    pub price_change_1d: Option<f64>,
    pub price_date: Option<String>,      // staleness indicator for price
    pub computed_at: Option<String>,     // staleness indicator for signal
    pub recommendation: String,
    pub position_size_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceStory {
    pub headline: String,
    pub source_name: String,
    pub published_at: Option<String>,
}

/// Get detailed buy/watch evidence for top signal entities.
#[tauri::command]
pub fn get_signal_evidence(db: State<'_, DbState>, limit: Option<usize>) -> Result<Vec<SignalEvidence>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(10);

    // Get top cross-signal entities — ONLY tradeable (with ticker).
    // 7-day freshness window prevents stale signals from firing Buy buttons.
    let mut stmt = conn
        .prepare(
            "WITH latest AS (
               SELECT cs.*, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY computed_at DESC) AS rn
               FROM cross_signals cs
               WHERE cs.computed_at >= datetime('now', '-7 days')
             )
             SELECT cs.entity_id, e.name, cs.ticker, cs.compound_score,
                    cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                    cs.government_signal, cs.search_trend, cs.patent_signal,
                    cs.supply_chain, cs.political_signal, cs.source_diversity,
                    cs.convergence_detected, cs.computed_at
             FROM latest cs
             JOIN entities e ON e.id = cs.entity_id
             WHERE cs.rn = 1
               AND cs.compound_score > 0.05
               AND e.entity_type = 'company'
               AND EXISTS (SELECT 1 FROM entity_tickers et WHERE et.entity_id = cs.entity_id)
             ORDER BY
               cs.source_diversity DESC,
               cs.compound_score DESC
             LIMIT ?1"
        )
        .map_err(|e| e.to_string())?;

    let rows: Vec<ConvergenceRow> = stmt
        .query_map([limit as i64], |row| {
            Ok((
                row.get(0)?, row.get::<_, String>(1).unwrap_or_default(),
                row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                row.get(12)?, row.get::<_, i32>(13).unwrap_or(0) != 0,
                row.get(14).ok(),
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut evidence_list = Vec::new();

    for (entity_id, name, ticker, score, insider, inst, news, gov, search, patent, supply, political, diversity, convergence, computed_at) in rows {
        // Build human-readable reasons
        let mut reasons = Vec::new();
        if insider > 0.3 { reasons.push(format!("Insider buying signal ({:.0}%)", insider * 100.0)); }
        if news > 0.3 { reasons.push(format!("Strong news momentum ({:.0}%)", news * 100.0)); }
        if gov > 0.3 { reasons.push(format!("Government contract/regulatory signal ({:.0}%)", gov * 100.0)); }
        if inst > 0.3 { reasons.push(format!("Institutional flow signal ({:.0}%)", inst * 100.0)); }
        if search > 0.3 { reasons.push(format!("Search trend acceleration ({:.0}%)", search * 100.0)); }
        if patent > 0.3 { reasons.push(format!("Patent filing cluster ({:.0}%)", patent * 100.0)); }
        if supply > 0.3 { reasons.push(format!("Supply chain signal ({:.0}%)", supply * 100.0)); }
        if political > 0.3 { reasons.push(format!("Political/lobbying signal ({:.0}%)", political * 100.0)); }
        if convergence { reasons.push(format!("{} source types converging", diversity)); }

        // Get recent stories mentioning this entity
        let source_stories: Vec<EvidenceStory> = conn
            .prepare(
                "SELECT s.headline, s.source_name, s.published_at
                 FROM entity_mentions em
                 JOIN stories s ON em.story_id = s.id
                 WHERE em.entity_id = ?1
                 ORDER BY s.created_at DESC LIMIT 5"
            )
            .ok()
            .map(|mut stmt| {
                stmt.query_map([entity_id], |row| {
                    Ok(EvidenceStory {
                        headline: row.get(0)?,
                        source_name: row.get(1)?,
                        published_at: row.get(2)?,
                    })
                })
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default()
            })
            .unwrap_or_default();

        // Get price — restrict to last 3 days so stale prices don't fire Buy buttons
        let (price, price_change, price_date) = ticker.as_ref().map(|t| {
            let p: Option<(f64, Option<f64>, String)> = conn.query_row(
                "SELECT close, change_1d, date FROM entity_prices
                 WHERE ticker = ?1 AND date >= date('now', '-3 days')
                 ORDER BY date DESC LIMIT 1",
                [t], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).ok();
            match p {
                Some((c, ch, d)) => (c, ch, Some(d)),
                None => (0.0, None, None),
            }
        }).unwrap_or((0.0, None, None));

        // Count how many signal types are active (>0.2)
        let active_dimensions = [insider, inst, news, gov, search, patent, supply, political]
            .iter()
            .filter(|&&v| v > 0.2)
            .count();

        // Recommendation — requires multiple signals for buy, not just news
        let recommendation = if convergence && score > 0.6 {
            "Strong Buy — Multi-signal convergence detected".to_string()
        } else if active_dimensions >= 3 && score > 0.4 {
            "Buy — Multiple signal types aligning".to_string()
        } else if active_dimensions >= 2 && score > 0.3 {
            "Watch — Two signal types active, accumulating".to_string()
        } else if ticker.is_some() && active_dimensions >= 1 {
            "Monitor — Single signal, waiting for confirmation from other sources".to_string()
        } else {
            "Tracking — News only, no financial signal yet".to_string()
        };

        let position_pct = if convergence && score > 0.6 { 5.0 }
            else if active_dimensions >= 3 { 2.0 }
            else { 1.0 };

        evidence_list.push(SignalEvidence {
            entity_name: name,
            ticker,
            compound_score: score,
            reasons,
            source_stories,
            price: if price > 0.0 { Some(price) } else { None },
            price_change_1d: price_change,
            price_date,
            computed_at,
            recommendation,
            position_size_pct: position_pct,
        });
    }

    Ok(evidence_list)
}

/// Source health status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceHealth {
    pub name: String,
    pub status: String,
    pub last_count: i64,
    pub last_fetch: Option<String>,
    pub description: String,
}

/// Get data source health.
#[tauri::command]
pub fn get_source_health(db: State<'_, DbState>) -> Result<Vec<SourceHealth>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // (display_name, api_provider, source_name_match, description)
    // source_name_match uses % for LIKE queries when the pipeline writes varied names
    let sources = vec![
        ("SEC EDGAR", "sec_edgar", "SEC EDGAR%", "Form 4, 8-K, Form D insider/material filings"),
        ("USASpending", "usaspending", "USASpending", "Government contracts > $1M"),
        ("Federal Register", "federal_register", "Federal Register", "Proposed and final rules"),
        ("FRED", "fred", "FRED", "20 key economic indicators"),
        ("FEC", "fec", "FEC", "Campaign contributions > $10K"),
        ("EIA", "eia", "EIA", "Oil, gas, energy market data"),
        ("LDA Lobbying", "lda", "Senate LDA", "Senate lobbying disclosures"),
        ("Google Patents", "uspto", "Google Patents", "Patent filings from major assignees"),
        ("Finnhub", "finnhub", "", "Real-time stock quotes"),
        ("Alpaca", "alpaca", "", "Paper trading execution"),
    ];

    let mut health = Vec::new();
    for (name, api_provider, source_match, desc) in sources {
        // Count stories by source_name (Finnhub/Alpaca don't produce stories)
        let story_count: i64 = if source_match.is_empty() {
            0
        } else if source_match.contains('%') {
            conn.query_row(
                "SELECT COUNT(*) FROM stories WHERE source_type = 'financial' AND source_name LIKE ?1 AND created_at >= datetime('now', '-7 days')",
                [source_match],
                |row| row.get(0),
            ).unwrap_or(0)
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM stories WHERE source_type = 'financial' AND source_name = ?1 AND created_at >= datetime('now', '-7 days')",
                [source_match],
                |row| row.get(0),
            ).unwrap_or(0)
        };

        // API-call count in the last 7 days. NOTE: api_usage logs one row per HTTP request
        // regardless of whether any data came back, so this is "did we try", NOT "did we get
        // data". It must never by itself mean a source is healthy (that was the old bug: a
        // dead scraper that 200s an empty page every run looked "active").
        let api_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM api_usage WHERE provider = ?1 AND created_at >= datetime('now', '-7 days')",
            [api_provider],
            |row| row.get(0),
        ).unwrap_or(0);

        let last_fetch: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM api_usage WHERE provider = ?1",
            [api_provider],
            |row| row.get(0),
        ).unwrap_or(None);

        // Honest health (HIGH-1/HIGH-2). A source is only "active" if it PRODUCED DATA in the
        // last 7 days. Sources that don't emit stories (Finnhub quotes, Alpaca execution — empty
        // source_match) are judged by whether they're being called at all. A source that has
        // produced stories historically but none in 7 days is "stale" (dead-but-firing: FRED,
        // EIA, Google Patents were all still calling their APIs while producing nothing for
        // 43-91 days). Never produced anything → "inactive".
        let ever_produced: i64 = if source_match.is_empty() {
            0
        } else {
            let q = if source_match.contains('%') {
                "SELECT COUNT(*) FROM stories WHERE source_type = 'financial' AND source_name LIKE ?1"
            } else {
                "SELECT COUNT(*) FROM stories WHERE source_type = 'financial' AND source_name = ?1"
            };
            conn.query_row(q, [source_match], |row| row.get(0)).unwrap_or(0)
        };

        let (status, last_count) = if source_match.is_empty() {
            // Non-story data source (Finnhub/Alpaca): active iff being called.
            (if api_count > 0 { "active" } else { "inactive" }, api_count)
        } else if story_count > 0 {
            ("active", story_count)
        } else if ever_produced > 0 {
            // Produced before, nothing in 7 days — firing but dead.
            ("stale", 0)
        } else {
            ("inactive", 0)
        };

        health.push(SourceHealth {
            name: name.to_string(),
            status: status.to_string(),
            last_count,
            last_fetch,
            description: desc.to_string(),
        });
    }

    Ok(health)
}

#[cfg(test)]
mod refresh_write_tests {
    use super::write_refreshed_quote;

    fn db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
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

    fn range(conn: &rusqlite::Connection) -> (Option<f64>, Option<f64>, f64) {
        conn.query_row(
            "SELECT high, low, close FROM entity_prices WHERE ticker = 'ARM' AND date = '2026-08-17'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn refreshing_a_price_does_not_erase_the_candle_range() {
        // The bug, exactly: a candle exists for today, the signals page loads,
        // the refresh fires, and ATR loses its input. ARM was the ticker this
        // was first noticed on, holding a $4k position with no usable range.
        let conn = db();
        conn.execute(
            "INSERT INTO entity_prices (ticker, date, open, close, high, low)
             VALUES ('ARM', '2026-08-17', 140.0, 142.0, 148.0, 139.0)",
            [],
        )
        .unwrap();

        write_refreshed_quote(&conn, 1, "ARM", "2026-08-17", 143.5, None, None, None, Some(1.0), None, None)
            .unwrap();

        let (high, low, close) = range(&conn);
        assert_eq!(high, Some(148.0), "the settled high must survive a quote refresh");
        assert_eq!(low, Some(139.0), "…and so must the low");
        assert_eq!(close, 143.5, "while the refreshed close does land — that is the point of the call");
    }

    #[test]
    fn a_quote_that_carries_a_range_fills_an_empty_one() {
        // Forward repair: the 330 rangeless rows already in the DB get a range
        // the next time the signals page refreshes them, without waiting for a
        // backfill run.
        let conn = db();
        conn.execute(
            "INSERT INTO entity_prices (ticker, date, close) VALUES ('ARM', '2026-08-17', 142.0)",
            [],
        )
        .unwrap();

        write_refreshed_quote(
            &conn, 1, "ARM", "2026-08-17", 143.5,
            Some(140.0), Some(149.0), Some(138.0), Some(1.0), None, None,
        )
        .unwrap();

        let (high, low, _) = range(&conn);
        assert_eq!(high, Some(149.0));
        assert_eq!(low, Some(138.0));
    }

    #[test]
    fn the_change_columns_are_always_overwritten() {
        // These belong to the refresh — a stale change_7d is worse than none,
        // so unlike the range they must not be COALESCE-protected.
        let conn = db();
        write_refreshed_quote(&conn, 1, "ARM", "2026-08-17", 142.0, None, None, None, Some(9.9), Some(9.9), Some(9.9))
            .unwrap();
        write_refreshed_quote(&conn, 1, "ARM", "2026-08-17", 143.0, None, None, None, Some(1.1), Some(2.2), Some(3.3))
            .unwrap();

        let (c1, c7, c30): (Option<f64>, Option<f64>, Option<f64>) = conn
            .query_row(
                "SELECT change_1d, change_7d, change_30d FROM entity_prices WHERE ticker = 'ARM'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((c1, c7, c30), (Some(1.1), Some(2.2), Some(3.3)));
    }
}
