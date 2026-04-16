use tauri::State;
use serde::{Deserialize, Serialize};
use crate::db::DbState;
use crate::services::cross_signals::{self, CrossSignal};

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
    pub change_1d: Option<f64>,
    pub change_7d: Option<f64>,
    pub change_30d: Option<f64>,
    pub entity_name: Option<String>,
}

/// Get latest prices for entities with ticker mappings.
#[tauri::command]
pub fn get_entity_prices(db: State<'_, DbState>, limit: Option<usize>) -> Result<Vec<EntityPrice>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let limit = limit.unwrap_or(50);

    let mut stmt = conn
        .prepare(
            "SELECT ep.ticker, ep.date, ep.close, ep.change_1d, ep.change_7d, ep.change_30d,
                    (SELECT e.name FROM entity_tickers et
                     JOIN entities e ON e.id = et.entity_id
                     WHERE et.ticker = ep.ticker
                     ORDER BY et.confidence DESC LIMIT 1) AS entity_name
             FROM entity_prices ep
             WHERE ep.date = (SELECT MAX(date) FROM entity_prices)
             GROUP BY ep.ticker
             ORDER BY ep.close DESC
             LIMIT ?1"
        )
        .map_err(|e| e.to_string())?;

    let prices = stmt
        .query_map([limit as i64], |row| {
            Ok(EntityPrice {
                ticker: row.get(0)?,
                date: row.get(1)?,
                close: row.get(2)?,
                change_1d: row.get(3)?,
                change_7d: row.get(4)?,
                change_30d: row.get(5)?,
                entity_name: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    Ok(prices)
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
        let mut stmt = conn.prepare(
            "SELECT et.entity_id, et.ticker FROM entity_tickers et
             WHERE et.is_public = 1 AND et.confidence >= 0.8
             ORDER BY et.confidence DESC LIMIT 25"
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

        #[derive(serde::Deserialize)]
        struct Q { c: f64, pc: f64 }
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

            conn.execute(
                "INSERT OR REPLACE INTO entity_prices (entity_id, ticker, date, close, change_1d, change_7d, change_30d)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![entity_id, ticker, today, quote.c, change_1d, change_7d, change_30d],
            ).ok();
            updated += 1;
        }
        // Drop lock, rate limit
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
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

    // Get top cross-signal entities — ONLY tradeable (with ticker)
    let mut stmt = conn
        .prepare(
            "SELECT cs.entity_id, e.name, cs.ticker, cs.compound_score,
                    cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                    cs.government_signal, cs.search_trend, cs.patent_signal,
                    cs.supply_chain, cs.political_signal, cs.source_diversity,
                    cs.convergence_detected
             FROM cross_signals cs
             JOIN entities e ON e.id = cs.entity_id
             JOIN entity_tickers et ON et.entity_id = cs.entity_id
             WHERE cs.compound_score > 0.05
               AND e.entity_type = 'company'
             ORDER BY
               cs.source_diversity DESC,
               cs.compound_score DESC
             LIMIT ?1"
        )
        .map_err(|e| e.to_string())?;

    let rows: Vec<(i64, String, Option<String>, f64, f64, f64, f64, f64, f64, f64, f64, f64, i64, bool)> = stmt
        .query_map([limit as i64], |row| {
            Ok((
                row.get(0)?, row.get::<_, String>(1).unwrap_or_default(),
                row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                row.get(12)?, row.get::<_, i32>(13).unwrap_or(0) != 0,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut evidence_list = Vec::new();

    for (entity_id, name, ticker, score, insider, inst, news, gov, search, patent, supply, political, diversity, convergence) in rows {
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

        // Get price
        let (price, price_change) = ticker.as_ref().map(|t| {
            let p: Option<(f64, Option<f64>)> = conn.query_row(
                "SELECT close, change_1d FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
                [t], |row| Ok((row.get(0)?, row.get(1)?)),
            ).ok();
            p.unwrap_or((0.0, None))
        }).unwrap_or((0.0, None));

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

    let sources = vec![
        ("SEC EDGAR", "sec_edgar", "edgar", "Form 4, 8-K, Form D insider/material filings"),
        ("USASpending", "usaspending", "usaspending", "Government contracts > $1M"),
        ("Federal Register", "federal_register", "federal_register", "Proposed and final rules"),
        ("FRED", "fred", "fred", "20 key economic indicators"),
        ("FEC", "fec", "fec", "Campaign contributions > $10K"),
        ("EIA", "eia", "eia", "Oil, gas, energy market data"),
        ("LDA Lobbying", "lda", "lda", "Senate lobbying disclosures"),
        ("USPTO", "uspto", "patent", "Patent filings (API migrating)"),
        ("Finnhub", "finnhub", "finnhub", "Real-time stock quotes"),
        ("Alpaca", "alpaca", "alpaca", "Paper trading execution"),
    ];

    let mut health = Vec::new();
    for (name, api_provider, feed_prefix, desc) in sources {
        // Count stories with matching feed_id prefix
        let story_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stories WHERE source_type = 'financial' AND feed_id LIKE ?1 AND created_at >= datetime('now', '-7 days')",
            [format!("{}%", feed_prefix)],
            |row| row.get(0),
        ).unwrap_or(0);

        // Also check api_usage
        let api_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM api_usage WHERE provider = ?1 AND created_at >= datetime('now', '-7 days')",
            [api_provider],
            |row| row.get(0),
        ).unwrap_or(0);

        let total = story_count.max(api_count);

        let last_fetch: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM api_usage WHERE provider = ?1",
            [feed_prefix],
            |row| row.get(0),
        ).unwrap_or(None);

        let status = if total > 0 { "active" } else if name.contains("USPTO") { "migrating" } else { "inactive" };

        health.push(SourceHealth {
            name: name.to_string(),
            status: status.to_string(),
            last_count: total,
            last_fetch,
            description: desc.to_string(),
        });
    }

    Ok(health)
}
