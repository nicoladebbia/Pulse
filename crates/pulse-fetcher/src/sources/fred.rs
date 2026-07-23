use super::RawArticle;
use serde::Deserialize;

/// FRED (Federal Reserve Economic Data) — Key economic indicators.
/// Free API key from https://fred.stlouisfed.org/docs/api/api_key.html
/// 120 requests/minute. Env var: FRED_API_KEY

const SERIES: &[(&str, &str)] = &[
    ("GDP", "Gross Domestic Product"),
    ("CPIAUCSL", "Consumer Price Index"),
    ("UNRATE", "Unemployment Rate"),
    ("FEDFUNDS", "Federal Funds Rate"),
    ("DCOILWTICO", "WTI Crude Oil Price"),
    ("PPIACO", "Producer Price Index (All Commodities)"),
    ("SP500", "S&P 500"),
    ("VIXCLS", "VIX Volatility Index"),
    ("DGS10", "10-Year Treasury Yield"),
    ("INDPRO", "Industrial Production Index"),
    ("HOUST", "Housing Starts"),
    ("UMCSENT", "Consumer Sentiment"),
    ("TOTALSA", "Total Vehicle Sales"),
    ("JTSJOL", "Job Openings (JOLTS)"),
    ("RSXFS", "Retail Sales"),
    ("BAMLH0A0HYM2", "High Yield Spread"),
    ("T10Y2Y", "10Y-2Y Treasury Spread"),
    ("WALCL", "Fed Balance Sheet"),
    ("M2SL", "M2 Money Supply"),
    ("DTWEXBGS", "Dollar Index"),
];

#[derive(Deserialize)]
struct FredResponse {
    observations: Option<Vec<FredObservation>>,
}

#[derive(Deserialize)]
struct FredObservation {
    date: Option<String>,
    value: Option<String>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let api_key = match std::env::var("FRED_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("FRED: skipping (FRED_API_KEY not set)");
            return Ok(Vec::new());
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut articles = Vec::new();

    for (series_id, series_name) in SERIES {
        match fetch_series(&client, &api_key, series_id, series_name).await {
            Ok(Some(article)) => articles.push(article),
            Ok(None) => {} // No new data
            Err(e) => tracing::warn!("FRED {} failed: {}", series_id, e),
        }

        // Rate limit: stay well under 120/min
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    tracing::info!("FRED: {} indicators fetched", articles.len());
    Ok(articles)
}

async fn fetch_series(
    client: &reqwest::Client,
    api_key: &str,
    series_id: &str,
    series_name: &str,
) -> anyhow::Result<Option<RawArticle>> {
    let url = format!(
        "https://api.stlouisfed.org/fred/series/observations?series_id={}&api_key={}&file_type=json&sort_order=desc&limit=2",
        series_id, api_key
    );

    super::API_CALLS.fred.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("FRED API returned {} for {}", resp.status(), series_id);
    }

    let data: FredResponse = resp.json().await?;
    let obs = data.observations.unwrap_or_default();

    if obs.is_empty() {
        return Ok(None);
    }

    let latest = &obs[0];
    let latest_value: f64 = latest
        .value
        .as_deref()
        .unwrap_or(".")
        .parse()
        .unwrap_or(f64::NAN);
    let latest_date = latest.date.as_deref().unwrap_or("unknown");

    if latest_value.is_nan() {
        return Ok(None); // "." means no data available
    }

    // Compute change from previous observation
    let (change_pct, prev_value) = if obs.len() >= 2 {
        let prev: f64 = obs[1]
            .value
            .as_deref()
            .unwrap_or(".")
            .parse()
            .unwrap_or(f64::NAN);
        if !prev.is_nan() && prev != 0.0 {
            let pct = ((latest_value - prev) / prev.abs()) * 100.0;
            (Some(pct), Some(prev))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    let change_str = change_pct
        .map(|p| {
            if p >= 0.0 {
                format!("+{:.1}%", p)
            } else {
                format!("{:.1}%", p)
            }
        })
        .unwrap_or_default();

    let direction = change_pct
        .map(|p| if p >= 0.0 { "up" } else { "down" })
        .unwrap_or("unchanged");

    let title = if change_str.is_empty() {
        format!("{} at {:.2}", series_name, latest_value)
    } else {
        format!("{} at {:.2} ({})", series_name, latest_value, change_str)
    };

    let content_snippet = format!(
        "Economic indicator: {} ({}) reported at {:.4} on {}. {}{}",
        series_name,
        series_id,
        latest_value,
        latest_date,
        prev_value
            .map(|p| format!("Previous value: {:.4}. ", p))
            .unwrap_or_default(),
        change_pct
            .map(|p| format!("Change: {:.1}% ({}).", p, direction))
            .unwrap_or_default(),
    );

    let metadata = serde_json::json!({
        "series_id": series_id,
        "series_name": series_name,
        "value": latest_value,
        "previous_value": prev_value,
        "change_pct": change_pct,
        "date": latest_date,
    });

    Ok(Some(RawArticle {
        title,
        url: format!("https://fred.stlouisfed.org/series/{}", series_id),
        source_name: "FRED".to_string(),
        source_url: "https://api.stlouisfed.org".to_string(),
        published_at: latest.date.clone(),
        content_snippet,
        sector: "finance".to_string(),
        // 2026-07-23: dedup keys on (feed_id, url), and neither varied by date —
        // so after each series' first-ever successful fetch, every later day's
        // genuinely new reading was silently discarded as a duplicate forever.
        // Confirmed exactly: 20 series in SERIES, 20 total FRED stories in the
        // DB, frozen since the first run. Appending the observation date makes
        // each day's real update a distinct row while still deduping same-day
        // re-fetches (multiple fetch cycles per day hit the same date).
        feed_id: format!("fred_{}_{}", series_id.to_lowercase(), latest_date),
        language: "en".to_string(),
        source_type: "financial".to_string(),
        financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
    }))
}
