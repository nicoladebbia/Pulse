use super::RawArticle;

const FEEDS: &[(&str, &str, &str)] = &[
    // AI: focus on companies, products, and model releases
    ("ai", "Google News AI Companies", "https://news.google.com/rss/search?q=OpenAI+OR+Anthropic+OR+ChatGPT+OR+Claude+OR+%22Google+Gemini%22+OR+%22Meta+AI%22+OR+Mistral+OR+xAI+OR+Grok+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Products", "https://news.google.com/rss/search?q=%22AI+model%22+OR+%22GPT-5%22+OR+%22new+AI%22+OR+%22AI+agent%22+OR+%22AI+tool%22+OR+%22AI+launch%22+OR+%22AI+release%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami", "https://news.google.com/rss/search?q=%22Miami+Beach%22+OR+%22South+Beach%22+OR+%22Miami-Dade%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("italy", "Google News Italy", "https://news.google.com/rss/search?q=Italy+politics+OR+Italian+government+OR+Meloni+when:1d&hl=en&gl=US&ceid=US:en"),
    ("tech", "Google News Tech", "https://news.google.com/rss/search?q=technology+innovation+startup+electronics+when:1d&hl=en-US&gl=US&ceid=US:en"),
];

pub async fn fetch_all() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::new();
    let mut all_articles = Vec::new();

    let futures: Vec<_> = FEEDS
        .iter()
        .map(|(sector, name, url)| {
            let client = client.clone();
            let sector = sector.to_string();
            let name = name.to_string();
            let url = url.to_string();
            async move { fetch_feed(&client, &sector, &name, &url).await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    for result in results {
        match result {
            Ok(articles) => all_articles.extend(articles),
            Err(e) => tracing::warn!("Google News fetch error: {}", e),
        }
    }

    Ok(all_articles)
}

async fn fetch_feed(
    client: &reqwest::Client,
    sector: &str,
    name: &str,
    url: &str,
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

    for entry in feed.entries.iter().take(25) {
        let title = entry.title.as_ref().map(|t| t.content.clone()).unwrap_or_default();
        let link = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();
        let published = entry
            .published
            .or(entry.updated)
            .map(|dt| dt.to_rfc3339());
        let snippet = entry
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .unwrap_or_default();

        if !title.is_empty() && !link.is_empty() {
            articles.push(RawArticle {
                title,
                url: link,
                source_name: name.to_string(),
                source_url: url.to_string(),
                published_at: published,
                content_snippet: snippet,
                sector: sector.to_string(),
                feed_id: format!("google_news_{}", sector),
                language: "en".to_string(),
            });
        }
    }

    tracing::info!("Fetched {} articles from {}", articles.len(), name);
    Ok(articles)
}
