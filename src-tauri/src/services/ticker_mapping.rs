use rusqlite::Connection;
use serde::Deserialize;
use std::collections::HashMap;

/// Entity-to-ticker mapping using SEC's free company_tickers.json.
/// URL: https://www.sec.gov/files/company_tickers.json
/// Maps CIK -> (ticker, company name). Updated daily.

#[derive(Debug, Deserialize)]
struct SecCompanyEntry {
    cik_str: String,
    ticker: String,
    title: String,
}

/// Download SEC company_tickers.json and build a lookup map.
/// Uses async reqwest since the Tauri app doesn't have blocking feature.
pub async fn download_ticker_map() -> anyhow::Result<HashMap<String, (String, String)>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client
        .get("https://www.sec.gov/files/company_tickers.json")
        .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("SEC company_tickers.json returned {}", resp.status());
    }

    // The JSON is { "0": {...}, "1": {...}, ... }
    let raw: HashMap<String, SecCompanyEntry> = resp.json().await?;

    // Build name->ticker lookup (lowercase name for matching)
    let mut map = HashMap::with_capacity(raw.len());
    for entry in raw.values() {
        let name_lower = entry.title.to_lowercase();
        map.insert(
            name_lower,
            (entry.ticker.clone(), entry.cik_str.clone()),
        );
    }

    tracing::info!("Loaded {} company-to-ticker mappings from SEC", map.len());
    Ok(map)
}

/// Resolve an entity name to a ticker.
/// Tries exact match first, then fuzzy (dropping common suffixes).
pub fn resolve_ticker(
    name: &str,
    sec_map: &HashMap<String, (String, String)>,
) -> Option<(String, String, f64)> {
    let name_lower = name.to_lowercase().trim().to_string();

    // 1. Exact match
    if let Some((ticker, cik)) = sec_map.get(&name_lower) {
        return Some((ticker.clone(), cik.clone(), 1.0));
    }

    // 2. Try without common suffixes
    let suffixes = [
        " inc", " inc.", " corp", " corp.", " ltd", " ltd.", " llc",
        " co", " co.", " plc", " sa", " ag", " se", " nv",
        " holdings", " group", " technologies", " technology",
        " international", " solutions", " systems", " enterprises",
    ];
    for suffix in &suffixes {
        let stripped = name_lower.trim_end_matches(suffix);
        if stripped != name_lower {
            // Try the stripped version
            for (key, (ticker, cik)) in sec_map.iter() {
                let key_stripped = suffixes
                    .iter()
                    .fold(key.as_str(), |s, sfx| s.trim_end_matches(sfx));
                if key_stripped == stripped {
                    return Some((ticker.clone(), cik.clone(), 0.8));
                }
            }
        }
    }

    // 3. Try contains match (entity name contained in SEC name or vice versa)
    if name_lower.len() >= 5 {
        for (key, (ticker, cik)) in sec_map.iter() {
            if key.contains(&name_lower) || name_lower.contains(key.as_str()) {
                return Some((ticker.clone(), cik.clone(), 0.6));
            }
        }
    }

    None
}

/// Auto-populate entity_tickers table for all company entities.
pub async fn populate_entity_tickers(conn: &Connection) -> anyhow::Result<usize> {
    let sec_map = match download_ticker_map().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("Failed to download SEC ticker data: {}", e);
            return Ok(0);
        }
    };

    // Get all company entities without a ticker mapping
    let mut stmt = conn.prepare(
        "SELECT e.id, e.name FROM entities e
         WHERE e.entity_type = 'company'
         AND e.id NOT IN (SELECT entity_id FROM entity_tickers)"
    )?;

    let entities: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if entities.is_empty() {
        return Ok(0);
    }

    let mut mapped = 0;
    for (entity_id, name) in &entities {
        if let Some((ticker, cik, confidence)) = resolve_ticker(name, &sec_map) {
            conn.execute(
                "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![entity_id, ticker, cik, confidence],
            )?;
            mapped += 1;
        }
    }

    tracing::info!(
        "Ticker mapping: {}/{} company entities mapped",
        mapped,
        entities.len()
    );
    Ok(mapped)
}

/// Check if an entity is tradeable (has a ticker mapping and is public).
pub fn is_tradeable(conn: &Connection, entity_id: i64) -> bool {
    conn.query_row(
        "SELECT is_public FROM entity_tickers WHERE entity_id = ?1",
        [entity_id],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Get ticker for an entity.
pub fn get_ticker(conn: &Connection, entity_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT ticker FROM entity_tickers WHERE entity_id = ?1",
        [entity_id],
        |row| row.get(0),
    )
    .ok()
}
