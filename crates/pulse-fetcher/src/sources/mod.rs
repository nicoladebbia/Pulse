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
    articles.extend(google.unwrap_or_default());
    articles.extend(rss.unwrap_or_default());
    articles.extend(hn.unwrap_or_default());

    Ok(articles)
}
