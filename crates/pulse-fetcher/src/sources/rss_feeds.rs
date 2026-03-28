use super::RawArticle;

const FEEDS: &[(&str, &str, &str, &str)] = &[
    ("ai", "OpenAI Blog", "https://openai.com/blog/rss/", "en"),
    ("ai", "Google AI Blog", "https://ai.googleblog.com/feeds/posts/default", "en"),
    ("ai", "DeepMind Blog", "https://deepmind.com/blog/feed/basic/", "en"),
    ("ai", "HuggingFace Blog", "https://huggingface.co/blog/feed.xml", "en"),
    ("ai", "ArXiv AI", "https://rss.arxiv.org/rss/cs.AI", "en"),
    ("ai", "VentureBeat AI", "https://venturebeat.com/category/ai/feed/", "en"),
    ("tech", "TechCrunch", "https://techcrunch.com/feed/", "en"),
    ("tech", "The Verge", "https://www.theverge.com/rss/index.xml", "en"),
    ("italy", "ANSA Politica", "https://www.ansa.it/sito/notizie/politica/politica_rss.xml", "it"),
    ("italy", "La Repubblica", "https://www.repubblica.it/rss/politica/rss2.0.xml", "it"),
    ("italy", "Corriere della Sera", "http://xml2.corriereobjects.it/rss/homepage.xml", "it"),
    ("miami", "WSVN Miami", "https://wsvn.com/feed", "en"),
];

pub async fn fetch_all() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let futures: Vec<_> = FEEDS
        .iter()
        .map(|(sector, name, url, lang)| {
            let client = client.clone();
            let sector = sector.to_string();
            let name = name.to_string();
            let url = url.to_string();
            let lang = lang.to_string();
            async move { fetch_single(&client, &sector, &name, &url, &lang).await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    let mut all_articles = Vec::new();

    for result in results {
        match result {
            Ok(articles) => all_articles.extend(articles),
            Err(e) => tracing::warn!("RSS fetch error: {}", e),
        }
    }

    Ok(all_articles)
}

async fn fetch_single(
    client: &reqwest::Client,
    sector: &str,
    name: &str,
    url: &str,
    lang: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    let response = client
        .get(url)
        .header("User-Agent", "Pulse/0.1")
        .send()
        .await?
        .bytes()
        .await?;

    let feed = feed_rs::parser::parse(&response[..])?;
    let mut articles = Vec::new();

    // Only take articles from the last 24 hours
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);

    for entry in feed.entries.iter().take(20) {
        let pub_date = entry.published.or(entry.updated);

        // Skip old articles if we have a date
        if let Some(dt) = pub_date {
            if dt < cutoff {
                continue;
            }
        }

        let title = entry.title.as_ref().map(|t| t.content.clone()).unwrap_or_default();
        let link = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();
        let snippet = entry
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .or_else(|| {
                entry.content.as_ref().and_then(|c| c.body.clone())
            })
            .unwrap_or_default();

        if !title.is_empty() && !link.is_empty() {
            articles.push(RawArticle {
                title,
                url: link,
                source_name: name.to_string(),
                source_url: url.to_string(),
                published_at: pub_date.map(|dt| dt.to_rfc3339()),
                content_snippet: snippet.chars().take(500).collect(),
                sector: sector.to_string(),
                feed_id: format!("rss_{}", name.to_lowercase().replace(' ', "_")),
                language: lang.to_string(),
            });
        }
    }

    tracing::info!("Fetched {} articles from {}", articles.len(), name);
    Ok(articles)
}
