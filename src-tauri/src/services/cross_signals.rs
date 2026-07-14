use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Cross-signal detection engine.
/// Computes compound scores across 8 signal dimensions for each entity,
/// detects convergence when 3+ signals align, and generates trading signals.

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

/// Signal weights — active dimensions only. Dimensions without data sources
/// (institutional_flow, search_trend, import_volume) are zeroed out;
/// their weight is redistributed to dimensions with real data.
/// Active weights sum to 1.0: 0.25 + 0.25 + 0.20 + 0.10 + 0.20 = 1.0
const WEIGHT_INSIDER: f64 = 0.25;       // SEC Form 4 data
const WEIGHT_INSTITUTIONAL: f64 = 0.0;  // No data source yet
const WEIGHT_NEWS: f64 = 0.25;          // News entity mentions + acceleration
const WEIGHT_GOVERNMENT: f64 = 0.20;    // USASpending + Federal Register
const WEIGHT_SEARCH: f64 = 0.0;         // No data source yet
const WEIGHT_PATENT: f64 = 0.10;        // USPTO filings
const WEIGHT_SUPPLY_CHAIN: f64 = 0.0;   // No data source yet
const WEIGHT_POLITICAL: f64 = 0.20;     // LDA lobbying + FEC

/// Compute compound score for an entity from its signal dimensions.
pub fn compute_compound_score(
    insider: f64,
    institutional: f64,
    news: f64,
    government: f64,
    search: f64,
    patent: f64,
    supply_chain: f64,
    political: f64,
) -> f64 {
    let raw = insider * WEIGHT_INSIDER
        + institutional * WEIGHT_INSTITUTIONAL
        + news * WEIGHT_NEWS
        + government * WEIGHT_GOVERNMENT
        + search * WEIGHT_SEARCH
        + patent * WEIGHT_PATENT
        + supply_chain * WEIGHT_SUPPLY_CHAIN
        + political * WEIGHT_POLITICAL;

    // Clamp to [0, 1]
    raw.max(0.0).min(1.0)
}

/// Detect convergence: 3+ signal types independently positive (>0.3)
/// AND source_diversity >= 3.
pub fn detect_convergence(
    insider: f64,
    institutional: f64,
    news: f64,
    government: f64,
    search: f64,
    patent: f64,
    supply_chain: f64,
    political: f64,
    source_diversity: i64,
) -> bool {
    let threshold = 0.3;
    let positive_count = [
        insider, institutional, news, government,
        search, patent, supply_chain, political,
    ]
    .iter()
    .filter(|&&v| v > threshold)
    .count();

    positive_count >= 3 && source_diversity >= 3
}

/// Normalize a raw signal value to [0, 1] using a sigmoid-like function.
/// `scale` controls how quickly values saturate (higher = more sensitive).
fn normalize_signal(value: f64, scale: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    // Sigmoid normalization: 1 - e^(-value/scale)
    1.0 - (-value / scale).exp()
}

/// Load calibrated weights from DB, fall back to defaults.
fn load_calibrated_weights(conn: &Connection) -> [f64; 8] {
    let defaults = [WEIGHT_INSIDER, WEIGHT_INSTITUTIONAL, WEIGHT_NEWS, WEIGHT_GOVERNMENT,
                    WEIGHT_SEARCH, WEIGHT_PATENT, WEIGHT_SUPPLY_CHAIN, WEIGHT_POLITICAL];

    let json: Option<String> = conn.query_row(
        "SELECT value FROM user_profile WHERE key = 'calibrated_weights'",
        [], |row| row.get(0),
    ).ok();

    if let Some(json_str) = json {
        if let Ok(pairs) = serde_json::from_str::<Vec<(String, f64)>>(&json_str) {
            let mut w = defaults;
            for (key, val) in &pairs {
                match key.as_str() {
                    "insider_signal" => w[0] = *val,
                    "institutional_flow" => w[1] = *val,
                    "news_momentum" => w[2] = *val,
                    "government_signal" => w[3] = *val,
                    "search_trend" => w[4] = *val,
                    "patent_signal" => w[5] = *val,
                    "supply_chain" => w[6] = *val,
                    "political_signal" => w[7] = *val,
                    _ => {}
                }
            }
            return w;
        }
    }

    defaults
}

/// Compute cross-signal scores for all entities with recent activity.
/// Returns the number of scores computed.
pub fn compute_all_cross_signals(conn: &Connection, today: &str) -> anyhow::Result<usize> {
    let weights = load_calibrated_weights(conn);

    // Get all entities with signals that have source_diversity > 0
    let mut stmt = conn.prepare(
        "SELECT s.topic, s.sector, s.window_7d, s.window_30d, s.acceleration,
                COALESCE(s.source_diversity, 0),
                COALESCE(s.insider_buy_volume, 0),
                COALESCE(s.institutional_flow, 0),
                COALESCE(s.contract_value, 0),
                COALESCE(s.patent_filing_rate, 0),
                COALESCE(s.search_trend_delta, 0),
                COALESCE(s.import_volume_delta, 0),
                COALESCE(s.regulatory_sentiment, 0),
                COALESCE(s.lobbying_spend_delta, 0)
         FROM signals s
         WHERE s.trajectory != 'dormant'
         ORDER BY s.window_30d DESC"
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, f64, i64, f64, f64, f64, f64, f64, f64, f64, f64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                row.get(12)?, row.get(13)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;

    for (topic, _sector, w7, _w30, acceleration, src_diversity,
         insider_vol, inst_flow, contract_val, patent_rate,
         search_delta, import_delta, reg_sentiment, lobby_delta) in &rows
    {
        // Normalize each dimension to [0, 1]
        let insider_norm = normalize_signal(*insider_vol, 1_000_000.0); // $1M scale
        let inst_norm = normalize_signal(*inst_flow, 500_000.0);
        // Pure acceleration ratio (rate_7d / rate_30d), gated by minimum sample.
        // Without the floor, a single mention from a zero baseline blows up acceleration.
        let news_norm = if *w7 >= 3 { normalize_signal(*acceleration, 2.0) } else { 0.0 };
        // Government signal: contract value + regulatory/8-K event severity composite
        let contract_norm = normalize_signal(*contract_val, 10_000_000.0);
        let reg_norm = normalize_signal(*reg_sentiment, 3.0); // 3 regulatory actions or 8-K severity*3
        let gov_norm = (contract_norm * 0.6 + reg_norm * 0.4).min(1.0); // Blend contracts + regulatory
        let search_norm = normalize_signal(*search_delta, 50.0); // 50% change
        let patent_norm = normalize_signal(*patent_rate, 5.0); // 5 patents
        let supply_norm = normalize_signal(*import_delta, 30.0); // 30% change
        let political_norm = normalize_signal(*lobby_delta, 100_000.0); // $100K scale

        // Use calibrated weights
        let compound = (insider_norm * weights[0]
            + inst_norm * weights[1]
            + news_norm * weights[2]
            + gov_norm * weights[3]
            + search_norm * weights[4]
            + patent_norm * weights[5]
            + supply_norm * weights[6]
            + political_norm * weights[7])
            .max(0.0).min(1.0);

        let convergence = detect_convergence(
            insider_norm, inst_norm, news_norm, gov_norm,
            search_norm, patent_norm, supply_norm, political_norm,
            *src_diversity,
        );

        // Only store if there's a meaningful signal
        if compound < 0.01 && !convergence {
            continue;
        }

        // Look up entity_id and ticker
        let topic_lower = topic.to_lowercase();
        let entity_id: Option<i64> = conn.query_row(
            "SELECT id FROM entities WHERE name_normalized = ?1 LIMIT 1",
            [&topic_lower],
            |row| row.get(0),
        ).ok();

        let ticker: Option<String> = entity_id.and_then(|eid| {
            conn.query_row(
                "SELECT ticker FROM entity_tickers WHERE entity_id = ?1",
                [eid],
                |row| row.get(0),
            ).ok()
        });

        let eid = entity_id.unwrap_or(0);

        conn.execute(
            "INSERT INTO cross_signals (entity_id, ticker, compound_score,
                insider_signal, institutional_flow, news_momentum, government_signal,
                search_trend, patent_signal, supply_chain, political_signal,
                source_diversity, convergence_detected, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                eid, ticker, compound,
                insider_norm, inst_norm, news_norm, gov_norm,
                search_norm, patent_norm, supply_norm, political_norm,
                src_diversity, convergence as i32, today,
            ],
        )?;

        count += 1;
    }

    if count > 0 {
        tracing::info!("Computed {} cross-signal scores", count);
    }
    Ok(count)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_compound_score_all_zero() {
        let score = compute_compound_score(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((score - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_compute_compound_score_all_one() {
        // Active: 0.25 + 0 + 0.25 + 0.20 + 0 + 0.10 + 0 + 0.20 = 1.0
        let score = compute_compound_score(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert!((score - 1.0).abs() < 0.001, "got {}", score);
    }

    #[test]
    fn test_compute_compound_score_mixed() {
        // insider=0.8*0.25=0.20, news=0.6*0.25=0.15 → total = 0.35
        let score = compute_compound_score(0.8, 0.0, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert!((score - 0.35).abs() < 0.01, "got {}", score);
    }

    #[test]
    fn test_detect_convergence_true() {
        // 4 signals > 0.3, diversity >= 3
        assert!(detect_convergence(0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 4));
    }

    #[test]
    fn test_detect_convergence_false_low_diversity() {
        // 4 signals > 0.3 but diversity < 3
        assert!(!detect_convergence(0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 2));
    }

    #[test]
    fn test_detect_convergence_false_few_signals() {
        // Only 2 signals > 0.3
        assert!(!detect_convergence(0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 5));
    }

    #[test]
    fn test_normalize_signal() {
        assert!((normalize_signal(0.0, 1.0) - 0.0).abs() < 0.001);
        assert!((normalize_signal(-1.0, 1.0) - 0.0).abs() < 0.001);
        // At scale value, should be ~0.632 (1 - 1/e)
        assert!((normalize_signal(1.0, 1.0) - 0.632).abs() < 0.01);
        // At 3x scale, should be ~0.95
        assert!(normalize_signal(3.0, 1.0) > 0.9);
    }

    #[test]
    fn test_weights_sum_to_one() {
        let sum = WEIGHT_INSIDER + WEIGHT_INSTITUTIONAL + WEIGHT_NEWS
            + WEIGHT_GOVERNMENT + WEIGHT_SEARCH + WEIGHT_PATENT
            + WEIGHT_SUPPLY_CHAIN + WEIGHT_POLITICAL;
        assert!((sum - 1.0).abs() < 0.001, "weights sum to {}", sum);
    }
}
