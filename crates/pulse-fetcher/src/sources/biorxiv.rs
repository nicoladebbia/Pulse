//! bioRxiv RSS — fresh preprint papers for health/longevity research.
//! Routes to freedom_health.

use super::RawArticle;

const BIORXIV_FEEDS: &[(&str, &str, &str)] = &[
    ("freedom_health", "bioRxiv: Physiology", "https://connect.biorxiv.org/biorxiv_xml.php?subject=physiology"),
    ("freedom_health", "bioRxiv: Neuroscience", "https://connect.biorxiv.org/biorxiv_xml.php?subject=neuroscience"),
    ("freedom_health", "bioRxiv: Immunology", "https://connect.biorxiv.org/biorxiv_xml.php?subject=immunology"),
];

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .user_agent("Pulse/0.1 (academic RSS reader)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let futures: Vec<_> = BIORXIV_FEEDS.iter().map(|(sector, name, url)| {
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
            Err(e) => tracing::warn!("bioRxiv feed failed (non-fatal): {}", e),
        }
    }
    tracing::info!("bioRxiv: {} preprints total", articles.len());
    Ok(articles)
}

async fn fetch_single(
    client: &reqwest::Client,
    sector: &str,
    name: &str,
    url: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    super::API_CALLS.biorxiv.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            feed_id: format!("biorxiv:{}", url),
            language: "en".to_string(),
            source_type: "news".to_string(),
            financial_metadata: None,
        });
    }
    Ok(articles)
}
