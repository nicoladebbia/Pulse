use super::RawArticle;
use serde::Deserialize;

#[derive(Deserialize)]
struct HNResponse {
    hits: Vec<HNHit>,
}

#[derive(Deserialize)]
struct HNHit {
    title: Option<String>,
    url: Option<String>,
    #[serde(rename = "objectID")]
    object_id: String,
    points: Option<u32>,
    created_at: Option<String>,
}

const ENDPOINTS: &[(&str, &str)] = &[
    ("tech", "https://hn.algolia.com/api/v1/search?tags=front_page&hitsPerPage=30"),
    ("ai", "https://hn.algolia.com/api/v1/search?query=AI+LLM+GPT+Claude+Anthropic&tags=story&hitsPerPage=20"),
];

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::new();
    let mut articles = Vec::new();

    for (sector, url) in ENDPOINTS {
        match fetch_endpoint(&client, sector, url).await {
            Ok(a) => articles.extend(a),
            Err(e) => tracing::warn!("HN fetch error for {}: {}", sector, e),
        }
    }

    Ok(articles)
}

async fn fetch_endpoint(
    client: &reqwest::Client,
    sector: &str,
    url: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    let resp: HNResponse = client
        .get(url)
        .send()
        .await?
        .json()
        .await?;

    let articles: Vec<RawArticle> = resp
        .hits
        .into_iter()
        .filter_map(|hit| {
            let title = hit.title?;
            let url = hit.url.unwrap_or_else(|| {
                format!("https://news.ycombinator.com/item?id={}", hit.object_id)
            });

            Some(RawArticle {
                title,
                url,
                source_name: "Hacker News".to_string(),
                source_url: "https://news.ycombinator.com".to_string(),
                published_at: hit.created_at,
                content_snippet: format!("{} points", hit.points.unwrap_or(0)),
                sector: sector.to_string(),
                feed_id: format!("hn_{}", sector),
                language: "en".to_string(),
            })
        })
        .collect();

    tracing::info!("Fetched {} articles from HN ({})", articles.len(), sector);
    Ok(articles)
}
