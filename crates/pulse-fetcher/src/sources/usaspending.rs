use super::RawArticle;
use serde::Deserialize;

/// USASpending.gov — Government contract awards > $1M from the last 7 days.
/// Free API, no key required. POST to search endpoint.

#[derive(Deserialize)]
struct UsaSpendingResponse {
    results: Vec<UsaSpendingAward>,
}

#[derive(Deserialize)]
struct UsaSpendingAward {
    #[serde(rename = "Award ID")]
    award_id: Option<String>,
    #[serde(rename = "Recipient Name")]
    recipient_name: Option<String>,
    #[serde(rename = "Award Amount")]
    award_amount: Option<f64>,
    #[serde(rename = "Awarding Agency")]
    awarding_agency: Option<String>,
    #[serde(rename = "Description")]
    description: Option<String>,
    #[serde(rename = "Start Date")]
    start_date: Option<String>,
    #[serde(rename = "NAICS Code")]
    naics_code: Option<String>,
    #[serde(rename = "Award Type")]
    award_type: Option<String>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let week_ago = (chrono::Local::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();

    let body = serde_json::json!({
        "filters": {
            "time_period": [{"start_date": week_ago, "end_date": today}],
            "award_amounts": [{"lower_bound": 1000000}],
            "award_type_codes": ["A", "B", "C", "D"]  // Contracts only (A=BPA, B=Purchase Order, C=Delivery Order, D=Definitive)
        },
        "fields": [
            "Award ID", "Recipient Name", "Award Amount", "Awarding Agency",
            "Description", "Start Date", "NAICS Code", "Award Type"
        ],
        "limit": 100,
        "page": 1,
        "sort": "Award Amount",
        "order": "desc"
    });

    super::API_CALLS.usaspending.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client
        .post("https://api.usaspending.gov/api/v2/search/spending_by_award/")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("USASpending API returned {}", resp.status());
    }

    let data: UsaSpendingResponse = resp.json().await?;
    tracing::info!("USASpending: {} awards returned", data.results.len());

    let articles: Vec<RawArticle> = data
        .results
        .into_iter()
        .filter_map(|award| {
            let recipient = award.recipient_name.as_deref().unwrap_or("Unknown");
            let amount = award.award_amount.unwrap_or(0.0);
            let agency = award.awarding_agency.as_deref().unwrap_or("Unknown Agency");
            let description = award.description.as_deref().unwrap_or("No description");
            let award_id = award.award_id.as_deref().unwrap_or("unknown");
            let award_type = award.award_type.as_deref().unwrap_or("contract");
            let naics = award.naics_code.as_deref().unwrap_or("N/A");

            if amount < 1_000_000.0 {
                return None;
            }

            let amount_str = format_amount(amount);
            let desc_truncated = if description.len() > 80 {
                format!("{}...", &description[..77])
            } else {
                description.to_string()
            };

            let title = format!(
                "{} wins {} {} contract: {}",
                recipient, amount_str, agency, desc_truncated
            );

            let content_snippet = format!(
                "Government contract award: {} received a {} {} from {}. \
                 Description: {}. NAICS: {}. Award type: {}. Award ID: {}.",
                recipient, amount_str, award_type, agency, description, naics, award_type, award_id
            );

            let metadata = serde_json::json!({
                "award_id": award_id,
                "recipient": recipient,
                "amount": amount,
                "agency": agency,
                "naics": naics,
                "award_type": award_type,
                "description": description,
            });

            let sector = naics_to_sector(naics);

            Some(RawArticle {
                title,
                url: format!("https://www.usaspending.gov/award/{}", award_id),
                source_name: "USASpending".to_string(),
                source_url: "https://api.usaspending.gov".to_string(),
                published_at: award.start_date,
                content_snippet,
                sector,
                feed_id: "usaspending".to_string(),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
            })
        })
        .collect();

    tracing::info!("USASpending: {} articles after filtering", articles.len());
    Ok(articles)
}

/// Map NAICS code prefix to Pulse sector.
fn naics_to_sector(naics: &str) -> String {
    match naics.chars().take(2).collect::<String>().as_str() {
        "54" => "tech".to_string(),  // Professional/Scientific/Technical
        "51" => "tech".to_string(),  // Information
        "33" => "tech".to_string(),  // Manufacturing (electronics, computers)
        "31" | "32" => "finance".to_string(), // Manufacturing (other)
        "52" => "finance".to_string(), // Finance and Insurance
        "92" => "finance".to_string(), // Public Administration
        _ => "finance".to_string(),
    }
}

/// Format dollar amounts for readable display.
fn format_amount(amount: f64) -> String {
    if amount >= 1_000_000_000.0 {
        format!("${:.1}B", amount / 1_000_000_000.0)
    } else if amount >= 1_000_000.0 {
        format!("${:.1}M", amount / 1_000_000.0)
    } else if amount >= 1_000.0 {
        format!("${:.0}K", amount / 1_000.0)
    } else {
        format!("${:.0}", amount)
    }
}
