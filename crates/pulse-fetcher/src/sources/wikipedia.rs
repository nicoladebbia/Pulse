use super::RawArticle;

/// Wikipedia Pageviews API — free proxy for search interest.
/// API: https://wikimedia.org/api/rest_v1/metrics/pageviews/per-article/{project}/{access}/{agent}/{article}/{granularity}/{start}/{end}
/// No API key needed. Rate limit: ~100 req/s (generous).
///
/// Strategy: For entities with tickers (public companies), fetch their Wikipedia page views.
/// Compare last 7 days vs prior 30-day average. A spike in page views correlates with
/// search interest and often precedes price moves.

pub async fn fetch(db_path: &std::path::Path) -> anyhow::Result<Vec<RawArticle>> {
    let conn = rusqlite::Connection::open(db_path)?;

    // Get entities with tickers that have recent signals (worth tracking)
    let entities: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT ec.canonical_name, et.ticker
             FROM entity_canonical ec
             JOIN entities e ON e.canonical_id = ec.id
             JOIN entity_tickers et ON et.entity_id = e.id
             WHERE et.ticker IS NOT NULL
             LIMIT 50"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if entities.is_empty() {
        // Fallback: try entities table directly
        let mut stmt = conn.prepare(
            "SELECT DISTINCT e.name, et.ticker
             FROM entities e
             JOIN entity_tickers et ON et.entity_id = e.id
             WHERE et.ticker IS NOT NULL
             LIMIT 50"
        )?;
        let fallback: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        if fallback.is_empty() {
            tracing::info!("Wikipedia Pageviews: no entities with tickers found, skipping");
            return Ok(Vec::new());
        }
        return fetch_pageviews(&fallback).await;
    }

    fetch_pageviews(&entities).await
}

async fn fetch_pageviews(entities: &[(String, String)]) -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let today = chrono::Local::now();
    let end = today.format("%Y%m%d").to_string();
    // Pull 37 days; the recent-7d vs prior-30d split is done in-code below by
    // slicing the returned daily series (no separate 7d query window needed).
    let start_37d = (today - chrono::Duration::days(37)).format("%Y%m%d").to_string();
    let today_str = today.format("%Y-%m-%d").to_string();

    let mut articles = Vec::new();

    for (entity_name, ticker) in entities {
        // Convert entity name to Wikipedia article title
        // Try exact name first, then with common suffixes for disambiguation
        let base_title = entity_name
            .replace(' ', "_")
            .replace("&", "%26");

        // Try multiple title variants for disambiguation
        let title_variants = vec![
            base_title.clone(),
            format!("{}_(company)", base_title),
            format!("{}_(corporation)", base_title),
        ];

        let mut found_resp = None;
        for wiki_title in &title_variants {
            let url = format!(
                "https://wikimedia.org/api/rest_v1/metrics/pageviews/per-article/en.wikipedia/all-access/all-agents/{}/daily/{}/{}",
                wiki_title, start_37d, end
            );

            super::API_CALLS.wikipedia.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = match client
                .get(&url)
                .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            };

            found_resp = Some((resp, wiki_title.clone()));
            break;
        }

        let Some((resp, wiki_title)) = found_resp else {
            continue;
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        let items = body.get("items").and_then(|v| v.as_array());
        let Some(items) = items else { continue };

        if items.len() < 10 {
            continue; // Not enough data for meaningful comparison
        }

        // Split into recent 7d and prior 30d
        let total_views: i64 = items.iter()
            .filter_map(|item| item.get("views").and_then(|v| v.as_i64()))
            .sum();

        let recent_7d: i64 = items.iter()
            .rev()
            .take(7)
            .filter_map(|item| item.get("views").and_then(|v| v.as_i64()))
            .sum();

        let prior_30d = total_views - recent_7d;
        let prior_daily_avg = if items.len() > 7 {
            prior_30d as f64 / (items.len() - 7) as f64
        } else {
            prior_30d as f64 / 30.0
        };

        let recent_daily_avg = recent_7d as f64 / 7.0;

        // Calculate delta percentage
        let views_delta_pct = if prior_daily_avg > 10.0 {
            ((recent_daily_avg - prior_daily_avg) / prior_daily_avg) * 100.0
        } else {
            0.0 // Too low baseline, skip
        };

        // Only report significant changes (>20% spike or drop)
        if views_delta_pct.abs() < 20.0 {
            continue;
        }

        let direction = if views_delta_pct > 0.0 { "spike" } else { "drop" };
        let title = format!(
            "{} Wikipedia views {} {:.0}% ({:.0}/day → {:.0}/day)",
            entity_name, direction, views_delta_pct.abs(), prior_daily_avg, recent_daily_avg
        );

        let metadata = serde_json::json!({
            "entity_name": entity_name,
            "ticker": ticker,
            "views_7d": recent_7d,
            "views_daily_avg_7d": recent_daily_avg,
            "views_daily_avg_30d": prior_daily_avg,
            "views_delta_pct": views_delta_pct,
            "direction": direction,
        });

        articles.push(RawArticle {
            title,
            url: format!("https://en.wikipedia.org/wiki/{}", wiki_title),
            source_name: "Wikipedia Pageviews".to_string(),
            source_url: "https://wikimedia.org".to_string(),
            published_at: Some(today_str.clone()),
            content_snippet: format!(
                "Wikipedia page views for {} changed {:.0}% over the past week compared to 30-day average.",
                entity_name, views_delta_pct
            ),
            sector: "finance".to_string(),
            feed_id: format!("wikipedia_{}", ticker.to_lowercase()),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
        });

        // Rate limit: 120ms between requests (well within 100 req/s)
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }

    tracing::info!("Wikipedia Pageviews: {} significant changes detected", articles.len());
    Ok(articles)
}
