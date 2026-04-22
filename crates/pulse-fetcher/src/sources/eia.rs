use super::RawArticle;
use serde::Deserialize;

/// EIA (Energy Information Administration) — Energy market data.
/// API: https://api.eia.gov/v2/
/// Free key. Env var: EIA_API_KEY

/// Key energy series to track
const SERIES: &[(&str, &str, &str)] = &[
    // (route, series description, data key)
    ("petroleum/pri/spt/data", "WTI Crude Oil Spot Price", "value"),
    ("natural-gas/pri/sum/data", "Natural Gas Spot Price", "value"),
    ("electricity/retail-sales/data", "Electricity Retail Sales", "revenue"),
];

#[derive(Deserialize)]
struct EiaResponse {
    response: Option<EiaData>,
}

#[derive(Deserialize)]
struct EiaData {
    data: Option<Vec<serde_json::Value>>,
    #[allow(dead_code)]
    total: Option<serde_json::Value>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let api_key = match std::env::var("EIA_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("EIA: skipping (EIA_API_KEY not set)");
            return Ok(Vec::new());
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut articles = Vec::new();

    // Fetch petroleum spot prices (most actionable)
    match fetch_petroleum(&client, &api_key).await {
        Ok(a) => articles.extend(a),
        Err(e) => tracing::warn!("EIA petroleum fetch failed: {}", e),
    }

    // Fetch natural gas
    match fetch_natgas(&client, &api_key).await {
        Ok(a) => articles.extend(a),
        Err(e) => tracing::warn!("EIA natural gas fetch failed: {}", e),
    }

    tracing::info!("EIA: {} articles produced", articles.len());
    Ok(articles)
}

async fn fetch_petroleum(client: &reqwest::Client, api_key: &str) -> anyhow::Result<Vec<RawArticle>> {
    let url = format!(
        "https://api.eia.gov/v2/petroleum/pri/spt/data/?api_key={}&frequency=daily&data[0]=value&facets[product][]=EPCBRENT&facets[product][]=EPCWTI&sort[0][column]=period&sort[0][direction]=desc&length=4",
        api_key
    );

    super::API_CALLS.eia.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("EIA petroleum API returned {}", resp.status());
    }

    let body = resp.text().await?;
    let data: EiaResponse = match serde_json::from_str(&body) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("EIA petroleum parse error: {} (body: {})", e, &body[..body.len().min(200)]);
            return Ok(Vec::new());
        }
    };

    let entries = data.response
        .and_then(|r| r.data)
        .unwrap_or_default();

    if entries.is_empty() {
        tracing::warn!("EIA petroleum: API returned 0 data entries");
    }

    let mut articles = Vec::new();
    let mut seen_products = std::collections::HashSet::new();

    for entry in &entries {
        let product = entry.get("product-name").and_then(|v| v.as_str()).unwrap_or("Unknown");
        // EIA returns value as string, not number
        let value = entry.get("value")
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0.0);
        let period = entry.get("period").and_then(|v| v.as_str()).unwrap_or("unknown");
        let product_id = entry.get("product").and_then(|v| v.as_str()).unwrap_or("UNK");

        // Only take latest per product
        if !seen_products.insert(product_id.to_string()) {
            continue;
        }
        if value == 0.0 {
            continue;
        }

        let title = format!("{} at ${:.2}/barrel ({})", product, value, period);
        let content_snippet = format!(
            "Energy market: {} reported at ${:.2} per barrel on {}.",
            product, value, period,
        );

        let metadata = serde_json::json!({
            "series": "petroleum_spot",
            "product": product,
            "product_id": product_id,
            "value": value,
            "unit": "$/barrel",
            "period": period,
        });

        articles.push(RawArticle {
            title,
            url: format!("https://www.eia.gov/petroleum/"),
            source_name: "EIA".to_string(),
            source_url: "https://api.eia.gov".to_string(),
            published_at: Some(period.to_string()),
            content_snippet,
            sector: "finance".to_string(),
            feed_id: format!("eia_petroleum_{}", product_id.to_lowercase()),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
        });
    }

    Ok(articles)
}

async fn fetch_natgas(client: &reqwest::Client, api_key: &str) -> anyhow::Result<Vec<RawArticle>> {
    let url = format!(
        "https://api.eia.gov/v2/natural-gas/pri/fut/data/?api_key={}&frequency=daily&data[0]=value&facets[process][]=FRP&sort[0][column]=period&sort[0][direction]=desc&length=2",
        api_key
    );

    super::API_CALLS.eia.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("EIA natural gas API returned {}", resp.status());
    }

    let body = resp.text().await?;
    let data: EiaResponse = match serde_json::from_str(&body) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("EIA natgas parse error: {} (body: {})", e, &body[..body.len().min(200)]);
            return Ok(Vec::new());
        }
    };

    let entries = data.response
        .and_then(|r| r.data)
        .unwrap_or_default();

    let mut articles = Vec::new();

    if let Some(entry) = entries.first() {
        // EIA returns value as string, not number
        let value = entry.get("value")
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0.0);
        let period = entry.get("period").and_then(|v| v.as_str()).unwrap_or("unknown");

        if value > 0.0 {
            let title = format!("Natural Gas Futures at ${:.2}/MMBtu ({})", value, period);
            let content_snippet = format!(
                "Energy market: Natural gas futures reported at ${:.2} per MMBtu on {}.",
                value, period,
            );
            let metadata = serde_json::json!({
                "series": "natural_gas_futures",
                "value": value,
                "unit": "$/MMBtu",
                "period": period,
            });

            articles.push(RawArticle {
                title,
                url: "https://www.eia.gov/naturalgas/".to_string(),
                source_name: "EIA".to_string(),
                source_url: "https://api.eia.gov".to_string(),
                published_at: Some(period.to_string()),
                content_snippet,
                sector: "finance".to_string(),
                feed_id: "eia_natgas".to_string(),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
            });
        }
    }

    Ok(articles)
}
