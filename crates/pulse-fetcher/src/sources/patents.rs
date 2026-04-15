use super::RawArticle;

/// USPTO Patents — Patent filing clusters.
///
/// STATUS: PatentsView API migrating to USPTO Open Data Portal (data.uspto.gov).
/// Both the old (api.patentsview.org) and intermediate (search.patentsview.org) are down.
/// This module tries the new endpoint and gracefully degrades if unavailable.
///
/// Once data.uspto.gov API goes live, this will auto-activate.

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Try USPTO Open Data Portal (new endpoint)
    match fetch_from_odp(&client).await {
        Ok(articles) if !articles.is_empty() => {
            tracing::info!("USPTO ODP: {} patent articles", articles.len());
            return Ok(articles);
        }
        Ok(_) => tracing::info!("USPTO ODP: no data returned (API may not be live yet)"),
        Err(e) => tracing::info!("USPTO ODP unavailable (expected during migration): {}", e),
    }

    // Try PatentsView search API (intermediate)
    match fetch_from_patentsview(&client).await {
        Ok(articles) if !articles.is_empty() => {
            tracing::info!("PatentsView: {} patent articles", articles.len());
            return Ok(articles);
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("PatentsView unavailable: {}", e),
    }

    tracing::info!("USPTO: all patent APIs unavailable (migration in progress), skipping");
    Ok(Vec::new())
}

/// Try the new USPTO Open Data Portal API.
async fn fetch_from_odp(client: &reqwest::Client) -> anyhow::Result<Vec<RawArticle>> {
    // The ODP API structure is TBD — try common REST patterns
    let url = "https://data.uspto.gov/api/v1/patent/grant?limit=50&sort=-grantDate";

    let resp = client
        .get(url)
        .header("User-Agent", "Pulse/1.0 (news aggregator)")
        .header("Accept", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("USPTO ODP returned {}", resp.status());
    }

    let body = resp.text().await?;

    // Try to parse as JSON — if it's HTML, the API isn't live yet
    if body.starts_with("<!") || body.starts_with("<html") {
        anyhow::bail!("USPTO ODP returned HTML (API not live yet)");
    }

    let data: serde_json::Value = serde_json::from_str(&body)?;

    let patents = data
        .get("results")
        .or_else(|| data.get("patents"))
        .or_else(|| data.get("data"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let articles: Vec<RawArticle> = patents
        .into_iter()
        .filter_map(|p| parse_patent_to_article(&p, "USPTO ODP"))
        .collect();

    Ok(articles)
}

/// Try the PatentsView search API (may still work).
async fn fetch_from_patentsview(client: &reqwest::Client) -> anyhow::Result<Vec<RawArticle>> {
    let url = "https://search.patentsview.org/api/v1/patent/?per_page=50&sort=-patent_date";

    let resp = client
        .get(url)
        .header("User-Agent", "Pulse/1.0")
        .header("Accept", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("PatentsView returned {}", resp.status());
    }

    let body = resp.text().await?;
    if body.starts_with("<!") || body.starts_with("<html") {
        anyhow::bail!("PatentsView returned HTML");
    }

    let data: serde_json::Value = serde_json::from_str(&body)?;
    let patents = data
        .get("patents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let articles: Vec<RawArticle> = patents
        .into_iter()
        .filter_map(|p| parse_patent_to_article(&p, "PatentsView"))
        .collect();

    Ok(articles)
}

/// Parse a patent JSON object into a RawArticle.
fn parse_patent_to_article(patent: &serde_json::Value, source: &str) -> Option<RawArticle> {
    let patent_number = patent
        .get("patent_number")
        .or_else(|| patent.get("patentNumber"))
        .or_else(|| patent.get("number"))
        .and_then(|v| v.as_str())?;

    let title = patent
        .get("patent_title")
        .or_else(|| patent.get("inventionTitle"))
        .or_else(|| patent.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Patent");

    let date = patent
        .get("patent_date")
        .or_else(|| patent.get("grantDate"))
        .or_else(|| patent.get("date"))
        .and_then(|v| v.as_str());

    let assignee = patent
        .get("assignee_organization")
        .or_else(|| patent.get("assigneeEntityName"))
        .or_else(|| patent.get("assignee"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Assignee");

    let abstract_text = patent
        .get("patent_abstract")
        .or_else(|| patent.get("abstract"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let headline = format!("{} granted patent: {}", assignee, truncate(title, 100));
    let content_snippet = format!(
        "Patent granted: {} ({}). Assignee: {}. Abstract: {}",
        title, patent_number, assignee,
        truncate(abstract_text, 300),
    );

    let metadata = serde_json::json!({
        "patent_number": patent_number,
        "assignee": assignee,
        "title": title,
        "date": date,
        "source_api": source,
    });

    Some(RawArticle {
        title: headline,
        url: format!("https://patents.google.com/patent/US{}", patent_number),
        source_name: "USPTO".to_string(),
        source_url: "https://data.uspto.gov".to_string(),
        published_at: date.map(|d| d.to_string()),
        content_snippet,
        sector: "tech".to_string(),
        feed_id: format!("patent_{}", patent_number),
        language: "en".to_string(),
        source_type: "financial".to_string(),
        financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
    })
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
