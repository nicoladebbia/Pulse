//! Pre-curation with Jev (TypeSafe's scoring model) instead of one big LLM call.
//!
//! The LLM pre-curator reads ~1,000+ headlines in one prompt and returns the best
//! ~140 indices. Jev has no "rank N items" call: each article is scored on its own
//! (one request each, run concurrently) and the top scores are picked per sector.
//! It is fast and cheap (~$0.03 a run) but, unlike the LLM, does not see the other
//! headlines — duplicates are left to the dedup phase that runs before this.
//!
//! Opt-in: only used when `TYPESAFE_API_KEY` is set. Any failure falls back to the
//! LLM pre-curator, so a bad key or an outage costs time, never the briefing.

use std::collections::HashSet;

use futures::StreamExt;
use serde_json::{Value, json};

use crate::sources::RawArticle;

const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
const MODEL: &str = "jev-latest";
const QUESTION: &str = "newsworthy";
/// Well under Jev's 80 requests/second limit.
const CONCURRENCY: usize = 30;
/// More failed scores than this and the result is not trusted: fall back.
const MAX_FAILED_FRACTION: f64 = 0.2;

pub fn api_key() -> Option<String> {
    std::env::var("TYPESAFE_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

fn endpoint() -> String {
    let base = std::env::var("TYPESAFE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
    format!("{}/v1/systemone", base.trim_end_matches('/'))
}

fn request_body(article: &RawArticle) -> Value {
    json!({
        "model": MODEL,
        "state": {
            "sector": article.sector,
            "source": article.source_name,
            "title": article.title,
            "snippet": article.content_snippet.chars().take(600).collect::<String>(),
        },
        "questions": {
            QUESTION: {
                "type": "score",
                "instructions": "How important is this article for a daily intelligence briefing \
                    on AI & LLMs, Miami Beach, Italy, and Tech & Innovation? Substantive, \
                    concrete news ranks high; clickbait, listicles, opinion, ads and \
                    off-topic items rank low.",
                "criteria": [
                    "Junk: ad, listicle, clickbait, or off-topic",
                    "Minor: routine or low-substance",
                    "Relevant: solid news for its sector",
                    "Important: significant development readers should know",
                    "Major: top story of the day for its sector"
                ]
            }
        }
    })
}

/// The 0..=4 score from a Jev response, or None if the answer is missing.
pub(crate) fn parse_score(body: &Value) -> Option<f64> {
    body.get("answers")?.get(QUESTION)?.get("score")?.as_f64().filter(|s| s.is_finite())
}

/// Top `max_keep` article indices: an equal share per sector first (so a sector
/// with lower absolute scores is not starved), then the best of the rest.
/// Returned most important first, like the LLM pre-curator.
pub(crate) fn select_balanced(scored: &[(usize, &str, f64)], max_keep: usize) -> Vec<usize> {
    let mut by_score: Vec<_> = scored.to_vec();
    by_score.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let sectors: HashSet<&str> = scored.iter().map(|s| s.1).collect();
    let share = if sectors.is_empty() { 0 } else { max_keep / sectors.len() };
    let mut taken: HashSet<usize> = HashSet::new();
    for sector in &sectors {
        by_score.iter().filter(|s| s.1 == *sector).take(share).for_each(|s| {
            taken.insert(s.0);
        });
    }
    for s in &by_score {
        if taken.len() >= max_keep {
            break;
        }
        taken.insert(s.0);
    }
    by_score.into_iter().map(|s| s.0).filter(|i| taken.contains(i)).collect()
}

async fn score_one(http: &reqwest::Client, key: &str, url: &str, article: &RawArticle) -> anyhow::Result<f64> {
    let body = request_body(article);
    let mut last_err = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(attempt * 2)).await;
        }
        let resp = match http.post(url).bearer_auth(key).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let code = resp.status().as_u16();
        if code == 401 {
            anyhow::bail!("TypeSafe rejected the API key (HTTP 401)");
        }
        // 429 rate limit and 529 overloaded are the documented retryable statuses.
        if code == 429 || code == 529 || resp.status().is_server_error() {
            last_err = format!("HTTP {code}");
            continue;
        }
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {code}: {}", text.chars().take(160).collect::<String>());
        }
        let value: Value = resp.json().await?;
        return parse_score(&value).ok_or_else(|| anyhow::anyhow!("no score in Jev response"));
    }
    anyhow::bail!("{last_err}")
}

/// Score every article with Jev and pick the best `max_keep`, sector-balanced.
pub async fn pre_curate(key: &str, articles: &[RawArticle], max_keep: usize) -> anyhow::Result<Vec<usize>> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = endpoint();
    let started = std::time::Instant::now();

    let results: Vec<(usize, anyhow::Result<f64>)> = futures::stream::iter(articles.iter().enumerate())
        .map(|(i, a)| {
            let (http, url) = (&http, &url);
            async move { (i, score_one(http, key, url, a).await) }
        })
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;

    let mut scored = Vec::with_capacity(results.len());
    let mut failed = 0usize;
    let mut sample_err = None;
    for (i, r) in results {
        match r {
            Ok(score) => scored.push((i, articles[i].sector.as_str(), score)),
            Err(e) => {
                failed += 1;
                sample_err.get_or_insert_with(|| format!("{e:#}"));
            }
        }
    }
    if articles.is_empty() || failed as f64 > articles.len() as f64 * MAX_FAILED_FRACTION {
        anyhow::bail!(
            "Jev scored {}/{} articles (e.g. {})",
            scored.len(),
            articles.len(),
            sample_err.unwrap_or_else(|| "no articles".into())
        );
    }
    let picked = select_balanced(&scored, max_keep);
    tracing::info!(
        "Jev scored {}/{} articles in {:.1}s, kept {}",
        scored.len(),
        articles.len(),
        started.elapsed().as_secs_f64(),
        picked.len()
    );
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_score_from_a_documented_response() {
        let body = json!({
            "model": "jev-1.13.0",
            "answers": {"newsworthy": {"type": "score", "score": 2.43, "confidence": 0.4}},
            "usage": {"input_tokens": 300, "output_tokens": 10}
        });
        assert_eq!(parse_score(&body), Some(2.43));
        assert_eq!(parse_score(&json!({"answers": {"other": {"score": 1.0}}})), None);
        assert_eq!(parse_score(&json!({"error": "bad"})), None);
    }

    #[test]
    fn every_sector_gets_its_share_even_with_lower_scores() {
        // "italy" scores lower across the board but must still get 2 of 6.
        let scored = vec![
            (0, "ai", 4.0), (1, "ai", 3.9), (2, "ai", 3.8), (3, "ai", 3.7),
            (4, "tech", 3.6), (5, "tech", 3.5), (6, "tech", 3.4),
            (7, "italy", 1.0), (8, "italy", 0.5), (9, "italy", 0.2),
        ];
        let picked = select_balanced(&scored, 6);
        assert_eq!(picked.len(), 6);
        assert!(picked.contains(&7) && picked.contains(&8));
        assert!(!picked.contains(&9));
        // Most important first.
        assert_eq!(picked[0], 0);
    }

    #[test]
    fn leftover_slots_go_to_the_best_remaining_articles() {
        // "miami" has only one article; its unused share goes to the top scorers.
        let scored = vec![(0, "ai", 3.0), (1, "ai", 2.9), (2, "ai", 2.8), (3, "miami", 0.1)];
        let picked = select_balanced(&scored, 3);
        assert_eq!(picked, vec![0, 1, 3]);
    }

    #[test]
    fn never_returns_more_than_asked_or_more_than_exist() {
        let scored = vec![(0, "ai", 1.0), (1, "tech", 2.0)];
        assert_eq!(select_balanced(&scored, 10).len(), 2);
        assert!(select_balanced(&[], 10).is_empty());
    }

    #[test]
    fn nan_scores_do_not_panic() {
        let scored = vec![(0, "ai", f64::NAN), (1, "ai", 1.0)];
        assert_eq!(select_balanced(&scored, 1).len(), 1);
    }
}
