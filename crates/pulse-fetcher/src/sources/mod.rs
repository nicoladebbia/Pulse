pub mod google_news;
pub mod rss_feeds;
pub mod hacker_news;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawArticle {
    pub title: String,
    pub url: String,
    pub source_name: String,
    pub source_url: String,
    pub published_at: Option<String>,
    pub content_snippet: String,
    pub sector: String,
    pub feed_id: String,
    pub language: String,
}

pub async fn collect_all() -> anyhow::Result<Vec<RawArticle>> {
    let (google, rss, hn) = tokio::join!(
        google_news::fetch_all(),
        rss_feeds::fetch_all(),
        hacker_news::fetch(),
    );

    let mut articles = Vec::new();
    let mut failed_sources = Vec::new();

    match google {
        Ok(a) => {
            tracing::info!("Google News: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::error!("Google News FAILED: {}", e);
            failed_sources.push("Google News");
        }
    }
    match rss {
        Ok(a) => {
            tracing::info!("RSS feeds: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::error!("RSS feeds FAILED: {}", e);
            failed_sources.push("RSS feeds");
        }
    }
    match hn {
        Ok(a) => {
            tracing::info!("Hacker News: {} articles", a.len());
            articles.extend(a);
        }
        Err(e) => {
            tracing::error!("Hacker News FAILED: {}", e);
            failed_sources.push("Hacker News");
        }
    }

    if !failed_sources.is_empty() {
        tracing::warn!("SOURCE HEALTH: {} source(s) failed: {}", failed_sources.len(), failed_sources.join(", "));
    }
    if articles.len() < 50 {
        tracing::warn!("SOURCE HEALTH: only {} total articles (expected 100+)", articles.len());
    }

    Ok(articles)
}
