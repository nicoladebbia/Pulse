use crate::sources::RawArticle;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub fn deduplicate(articles: Vec<RawArticle>) -> Vec<RawArticle> {
    let total = articles.len();
    let mut seen_urls: HashSet<String> = HashSet::new();
    let mut seen_titles: HashSet<String> = HashSet::new();
    let mut result = Vec::new();

    for article in articles {
        let url_hash = hash_url(&article.url);
        let title_normalized = normalize_title(&article.title);

        // Skip exact URL duplicates
        if seen_urls.contains(&url_hash) {
            continue;
        }

        // Skip near-title matches
        if seen_titles.iter().any(|existing| {
            trigram_similarity(existing, &title_normalized) > 0.75
        }) {
            continue;
        }

        seen_urls.insert(url_hash);
        seen_titles.insert(title_normalized);
        result.push(article);
    }

    tracing::info!("Dedup: {} -> {} articles", total, result.len());

    result
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
