use crate::sources::RawArticle;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub fn deduplicate(articles: Vec<RawArticle>) -> Vec<RawArticle> {
    deduplicate_with_history(articles, HashSet::new(), Vec::new())
}

/// Deduplicate articles, also checking against historical url_hashes and titles
/// from previous briefings (typically the last 7 days).
pub fn deduplicate_with_history(
    articles: Vec<RawArticle>,
    historical_url_hashes: HashSet<String>,
    historical_titles: Vec<String>,
) -> Vec<RawArticle> {
    let total = articles.len();
    let mut seen_urls: HashSet<String> = historical_url_hashes;
    let mut seen_titles: Vec<String> = historical_titles;
    let mut result = Vec::new();

    for article in articles {
        let url_hash = hash_url(&article.url);
        let title_normalized = normalize_title(&article.title);

        // Skip exact URL duplicates (including from previous days)
        if seen_urls.contains(&url_hash) {
            continue;
        }

        // Skip near-title matches (including from previous days)
        if seen_titles.iter().any(|existing| {
            trigram_similarity(existing, &title_normalized) > 0.75
        }) {
            continue;
        }

        seen_urls.insert(url_hash);
        seen_titles.push(title_normalized);
        result.push(article);
    }

    let historical_filtered = total - result.len();
    tracing::info!("Dedup: {} -> {} articles ({} filtered as duplicates)", total, result.len(), historical_filtered);

    result
}

/// Load recent url_hashes and normalized titles from the database for cross-day dedup.
/// Returns (url_hashes, normalized_titles) from the last `days` days.
pub fn load_recent_hashes(conn: &rusqlite::Connection, days: i64) -> (HashSet<String>, Vec<String>) {
    let offset = format!("-{days} days");

    let mut url_hashes = HashSet::new();
    let mut titles = Vec::new();

    // Load from stories table
    if let Ok(mut stmt) = conn.prepare(
        "SELECT s.url_hash, s.original_title FROM stories s
         JOIN briefings b ON b.id = s.briefing_id
         WHERE b.date >= date('now', ?1)"
    ) {
        if let Ok(rows) = stmt.query_map([&offset], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                url_hashes.insert(row.0);
                titles.push(normalize_title(&row.1));
            }
        }
    }

    // Load from freedom_stories table
    if let Ok(mut stmt) = conn.prepare(
        "SELECT fs.url_hash, fs.original_title FROM freedom_stories fs
         JOIN briefings b ON b.id = fs.briefing_id
         WHERE b.date >= date('now', ?1)"
    ) {
        if let Ok(rows) = stmt.query_map([&offset], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        }) {
            for row in rows.flatten() {
                if !row.0.is_empty() {
                    url_hashes.insert(row.0);
                }
                if !row.1.is_empty() {
                    titles.push(normalize_title(&row.1));
                }
            }
        }
    }

    tracing::info!("Loaded {} historical URL hashes and {} titles for cross-day dedup", url_hashes.len(), titles.len());
    (url_hashes, titles)
}

fn hash_url(url: &str) -> String {
    let normalized = url
        .trim()
        .trim_end_matches('/')
        .to_lowercase()
        .split('?')
        .next()
        .unwrap_or(url)
        .to_string();

    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn trigram_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let trigrams_a: HashSet<&str> = a.as_bytes().windows(3).map(|w| {
        std::str::from_utf8(w).unwrap_or("")
    }).collect();

    let trigrams_b: HashSet<&str> = b.as_bytes().windows(3).map(|w| {
        std::str::from_utf8(w).unwrap_or("")
    }).collect();

    let intersection = trigrams_a.intersection(&trigrams_b).count();
    let union = trigrams_a.union(&trigrams_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Returns the SHA-256 hash of a normalized URL (for DB storage)
pub fn url_hash(url: &str) -> String {
    hash_url(url)
}

/// Returns a hash for near-title matching (for DB storage)
pub fn title_hash(title: &str) -> String {
    let normalized = normalize_title(title);
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}
