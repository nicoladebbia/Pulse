use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Cross-signal read layer. Scoring/convergence detection is computed
/// exclusively by the fetcher (`compute_cross_signals` in
/// crates/pulse-fetcher/src/pipeline.rs) and written to the `cross_signals`
/// table; this module only serves what's already there. (2026-07-23: removed
/// a second, unused compute engine that had drifted to a different
/// convergence threshold than the live one — see git history if it's ever
/// needed again.)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossSignal {
    pub entity_id: i64,
    pub entity_name: String,
    pub ticker: Option<String>,
    pub compound_score: f64,
    pub insider_signal: f64,
    pub institutional_flow: f64,
    pub news_momentum: f64,
    pub government_signal: f64,
    pub search_trend: f64,
    pub patent_signal: f64,
    pub supply_chain: f64,
    pub political_signal: f64,
    pub source_diversity: i64,
    pub convergence_detected: bool,
    pub computed_at: Option<String>,
}

/// Get entities with convergence detected, ordered by compound score.
///
/// Applies two read-path correctness controls the raw table doesn't enforce:
///  - **Freshness (F3):** only rows computed in the last 7 days, matching `get_top_signals`
///    and `get_signal_evidence`. Without this, months-old convergence rows surface as current.
///  - **Noise (F2):** federal-filing / LLC / ALL-CAPS gov-payload entities are dropped via the
///    shared `entity_noise` filter (same one the Trends radar uses). The SQL over-fetches so the
///    post-filter result still reaches `limit` real companies.
pub fn get_convergence_signals(conn: &Connection, limit: usize) -> anyhow::Result<Vec<CrossSignal>> {
    // Over-fetch: noise entities are dropped after the query, so ask for more rows than needed
    // to avoid a short list once the LLC/gov-filing rows are removed.
    let fetch = (limit * 4).max(limit + 40) as i64;
    let mut stmt = conn.prepare(
        "WITH latest AS (
           SELECT cs.*, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY computed_at DESC) AS rn
           FROM cross_signals cs
           WHERE computed_at >= date('now', '-7 days')
         )
         SELECT cs.entity_id, e.name,
                COALESCE(cs.ticker, et.ticker) AS ticker, cs.compound_score,
                cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                cs.government_signal, cs.search_trend, cs.patent_signal,
                cs.supply_chain, cs.political_signal, cs.source_diversity,
                cs.convergence_detected, cs.computed_at
         FROM latest cs
         LEFT JOIN entities e ON e.id = cs.entity_id
         LEFT JOIN entity_tickers et ON et.entity_id = cs.entity_id
         WHERE cs.rn = 1 AND cs.convergence_detected = 1
         ORDER BY cs.compound_score DESC
         LIMIT ?1"
    )?;

    let signals = stmt
        .query_map([fetch], |row| {
            Ok(CrossSignal {
                entity_id: row.get(0)?,
                entity_name: row.get::<_, String>(1).unwrap_or_else(|_| "Unknown".to_string()),
                ticker: row.get(2)?,
                compound_score: row.get(3)?,
                insider_signal: row.get(4)?,
                institutional_flow: row.get(5)?,
                news_momentum: row.get(6)?,
                government_signal: row.get(7)?,
                search_trend: row.get(8)?,
                patent_signal: row.get(9)?,
                supply_chain: row.get(10)?,
                political_signal: row.get(11)?,
                source_diversity: row.get(12)?,
                convergence_detected: row.get::<_, i32>(13)? != 0,
                computed_at: row.get(14).ok(),
            })
        })?
        .filter_map(|r| r.ok())
        // F2: drop federal-filing / LLC / ALL-CAPS gov-payload noise entities.
        .filter(|s: &CrossSignal| !crate::services::entity_noise::is_noise_entity(&s.entity_name))
        .take(limit)
        .collect();

    Ok(signals)
}

/// Get top cross-signal scores (regardless of convergence).
pub fn get_top_signals(conn: &Connection, limit: usize) -> anyhow::Result<Vec<CrossSignal>> {
    let mut stmt = conn.prepare(
        "WITH latest AS (
           SELECT cs.*, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY computed_at DESC) AS rn
           FROM cross_signals cs
           WHERE computed_at >= date('now', '-7 days')
         )
         SELECT cs.entity_id, e.name, cs.ticker, cs.compound_score,
                cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                cs.government_signal, cs.search_trend, cs.patent_signal,
                cs.supply_chain, cs.political_signal, cs.source_diversity,
                cs.convergence_detected, cs.computed_at
         FROM latest cs
         LEFT JOIN entities e ON e.id = cs.entity_id
         WHERE cs.rn = 1
         ORDER BY cs.compound_score DESC
         LIMIT ?1"
    )?;

    let signals = stmt
        .query_map([limit as i64], |row| {
            Ok(CrossSignal {
                entity_id: row.get(0)?,
                entity_name: row.get::<_, String>(1).unwrap_or_else(|_| "Unknown".to_string()),
                ticker: row.get(2)?,
                compound_score: row.get(3)?,
                insider_signal: row.get(4)?,
                institutional_flow: row.get(5)?,
                news_momentum: row.get(6)?,
                government_signal: row.get(7)?,
                search_trend: row.get(8)?,
                patent_signal: row.get(9)?,
                supply_chain: row.get(10)?,
                political_signal: row.get(11)?,
                source_diversity: row.get(12)?,
                convergence_detected: row.get::<_, i32>(13)? != 0,
                computed_at: row.get(14).ok(),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(signals)
}
