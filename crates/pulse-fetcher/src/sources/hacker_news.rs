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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut articles = Vec::new();

    for (sector, url) in ENDPOINTS {
        match fetch_endpoint(&client, sector, url).await {
            Ok(a) => articles.extend(a),
            Err(e) => tracing::warn!("HN fetch error for {}: {}", sector, e),
        }
    }

    // Exclusive route: productivity-adjacent HN stories move from tech/ai
    // to freedom_time (not duplicated). Keeps the freedom feed fresh
    // with real tech productivity signal without polluting tech sector.
    let mut retagged = 0;
    for article in &mut articles {
        if is_productivity_story(&article.title) {
            article.sector = "freedom_time".to_string();
            retagged += 1;
        }
    }
    if retagged > 0 {
        tracing::info!("HN: retagged {} productivity stories to freedom_time", retagged);
    }

    Ok(articles)
}

fn is_productivity_story(title: &str) -> bool {
    let t = title.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "productiv", "automat", "async work", "async communication",
        "deep work", "solopren", "indie hacker", "notion", "obsidian",
        "todo app", "calendar", "pomodoro", "time management",
        "focus app", "distraction", "workflow automat", "zapier",
        "n8n", "second brain", "pkm", "task management",
    ];
    KEYWORDS.iter().any(|k| t.contains(k))
}

async fn fetch_endpoint(
    client: &reqwest::Client,
    sector: &str,
    url: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    super::API_CALLS.hacker_news.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                source_type: "news".to_string(),
                financial_metadata: None,
            })
        })
        .collect();

    tracing::info!("Fetched {} articles from HN ({})", articles.len(), sector);
    Ok(articles)
}
