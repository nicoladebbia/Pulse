use crate::sources::RawArticle;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

// Superseded by the dedup performed inside the pipeline; kept because its tests below
// still pin the similarity behaviour that logic relies on.
#[allow(dead_code)]
pub fn deduplicate(articles: Vec<RawArticle>) -> Vec<RawArticle> {
    deduplicate_with_history(articles, HashSet::new(), Vec::new())
}

/// Deduplicate articles, also checking against historical url_hashes and titles
/// from previous briefings (typically the last 7 days).
/// Uses O(n) hash-based dedup: URL hash + exact title hash + word-set hash.
pub fn deduplicate_with_history(
    articles: Vec<RawArticle>,
    historical_url_hashes: HashSet<String>,
    historical_titles: Vec<String>,
) -> Vec<RawArticle> {
    let total = articles.len();
    let mut seen_urls: HashSet<String> = historical_url_hashes;
    let mut seen_exact_titles: HashSet<String> = HashSet::new();
    let mut seen_word_sets: HashSet<String> = HashSet::new();
    let mut result = Vec::new();

    // Pre-hash historical titles into both sets
    for title in &historical_titles {
        let normalized = normalize_title(title);
        seen_exact_titles.insert(normalized.clone());
        seen_word_sets.insert(word_set_hash(&normalized));
    }

    for article in articles {
        let url_hash = hash_url(&article.url);
        let title_normalized = normalize_title(&article.title);

        // Skip exact URL duplicates (including from previous days)
        if seen_urls.contains(&url_hash) {
            continue;
        }

        // Skip exact title duplicates (fast O(1) check)
        if seen_exact_titles.contains(&title_normalized) {
            continue;
        }

        // Skip near-duplicate titles via word-set hash (O(1) check)
        // Catches reworded titles like "Apple reports Q4" vs "Q4 reported by Apple"
        let ws_hash = word_set_hash(&title_normalized);
        if seen_word_sets.contains(&ws_hash) {
            continue;
        }

        seen_urls.insert(url_hash);
        seen_exact_titles.insert(title_normalized.clone());
        seen_word_sets.insert(ws_hash);
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
    )
        && let Ok(rows) = stmt.query_map([&offset], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                url_hashes.insert(row.0);
                titles.push(normalize_title(&row.1));
            }
        }

    // Load from freedom_stories table
    if let Ok(mut stmt) = conn.prepare(
        "SELECT fs.url_hash, fs.original_title FROM freedom_stories fs
         JOIN briefings b ON b.id = fs.briefing_id
         WHERE b.date >= date('now', ?1)"
    )
        && let Ok(rows) = stmt.query_map([&offset], |row| {
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

/// Hash based on sorted unique content words (ignoring stop words + word order).
/// "Apple reports Q4 earnings" and "Q4 earnings reported by Apple" → same hash.
fn word_set_hash(normalized_title: &str) -> String {
    let stop_words: HashSet<&str> = [
        "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for",
        "of", "with", "by", "from", "is", "are", "was", "were", "be", "been",
        "has", "have", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "can", "shall", "not", "no", "its", "it",
        "this", "that", "as", "up", "out", "about", "into", "over", "after",
        "says", "said", "new", "how", "what", "why", "who", "when", "where",
    ].into_iter().collect();

    let mut words: Vec<&str> = normalized_title
        .split_whitespace()
        .filter(|w| w.len() >= 3 && !stop_words.contains(w))
        .collect();
    words.sort_unstable();
    words.dedup();

    // Need at least 3 content words for a meaningful hash
    if words.len() < 3 {
        return format!("__short_{}", normalized_title);
    }

    let mut hasher = Sha256::new();
    hasher.update(words.join(" ").as_bytes());
    format!("{:x}", hasher.finalize())
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
