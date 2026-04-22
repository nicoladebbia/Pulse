//! ArXiv RSS — fresh economics and quantitative finance papers.
//! Routes to freedom_wealth (research-grade wealth intelligence).

use super::RawArticle;

const ARXIV_FEEDS: &[(&str, &str, &str)] = &[
    ("freedom_wealth", "ArXiv: Economics", "https://export.arxiv.org/rss/econ.GN"),
    ("freedom_wealth", "ArXiv: Quantitative Finance", "https://export.arxiv.org/rss/q-fin.GN"),
    ("freedom_wealth", "ArXiv: Portfolio Management", "https://export.arxiv.org/rss/q-fin.PM"),
];

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .user_agent("Pulse/0.1 (academic RSS reader)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let futures: Vec<_> = ARXIV_FEEDS.iter().map(|(sector, name, url)| {
        let client = client.clone();
        let sector = sector.to_string();
        let name = name.to_string();
        let url = url.to_string();
        async move { fetch_single(&client, &sector, &name, &url).await }
    }).collect();

    let results = futures::future::join_all(futures).await;
    let mut articles = Vec::new();
    for result in results {
        match result {
            Ok(mut a) => articles.append(&mut a),
            Err(e) => tracing::warn!("ArXiv feed failed (non-fatal): {}", e),
        }
    }
    tracing::info!("ArXiv: {} papers total", articles.len());
    Ok(articles)
}

async fn fetch_single(
    client: &reqwest::Client,
    sector: &str,
    name: &str,
    url: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    super::API_CALLS.arxiv.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bytes = client.get(url).send().await?.error_for_status()?.bytes().await?;
    let feed = feed_rs::parser::parse(&bytes[..])?;

    let mut articles = Vec::new();
    for entry in feed.entries {
        let title = entry.title.map(|t| t.content).unwrap_or_default();
        let link = entry.links.first().map(|l| l.href.clone()).unwrap_or_default();
        if link.is_empty() || title.is_empty() { continue; }

        let summary = entry.summary.map(|s| s.content).unwrap_or_default();
        let pub_date = entry.published.or(entry.updated);
        let published_at = pub_date.map(|dt| dt.to_rfc3339());

        articles.push(RawArticle {
            title,
            url: link.clone(),
            source_name: name.to_string(),
            source_url: url.to_string(),
            published_at,
            content_snippet: summary,
            sector: sector.to_string(),
            feed_id: format!("arxiv:{}", url),
            language: "en".to_string(),
            source_type: "news".to_string(),
            financial_metadata: None,
        });
    }
    Ok(articles)
}
