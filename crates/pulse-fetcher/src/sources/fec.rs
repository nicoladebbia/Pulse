use super::RawArticle;
use serde::Deserialize;

/// FEC (Federal Election Commission) — Campaign contributions >$10K.
/// API: https://api.open.fec.gov/v1/
/// Free key, 60 req/min (1000/hr). Env var: FEC_API_KEY

#[derive(Deserialize)]
struct FecResponse {
    results: Vec<FecContribution>,
}

#[derive(Deserialize)]
struct FecContribution {
    contributor_name: Option<String>,
    committee: Option<FecCommittee>,
    contribution_receipt_amount: Option<f64>,
    contribution_receipt_date: Option<String>,
    contributor_employer: Option<String>,
    contributor_occupation: Option<String>,
    memo_text: Option<String>,
    sub_id: Option<String>,
}

#[derive(Deserialize)]
struct FecCommittee {
    name: Option<String>,
    party: Option<String>,
    committee_type: Option<String>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let api_key = match std::env::var("FEC_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("FEC: skipping (FEC_API_KEY not set)");
            return Ok(Vec::new());
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Fetch large individual contributions for current election cycle
    let current_year = chrono::Local::now().format("%Y").to_string().parse::<i32>().unwrap_or(2026);
    // FEC uses 2-year periods: 2025-2026 = 2026
    let two_year_period = if current_year % 2 == 0 { current_year } else { current_year + 1 };
    let url = format!(
        "https://api.open.fec.gov/v1/schedules/schedule_a/?api_key={}&min_amount=10000&sort=-contribution_receipt_date&per_page=100&two_year_transaction_period={}",
        api_key, two_year_period
    );

    super::API_CALLS.fec.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client
        .get(&url)
        .header("User-Agent", "Pulse/1.0")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("FEC API returned {}", resp.status());
    }

    let data: FecResponse = resp.json().await?;
    tracing::info!("FEC: {} contributions returned", data.results.len());

    let articles: Vec<RawArticle> = data
        .results
        .into_iter()
        .filter_map(|c| {
            let contributor = c.contributor_name.as_deref().unwrap_or("Unknown");
            let amount = c.contribution_receipt_amount.unwrap_or(0.0);
            let committee = c.committee.as_ref();
            let recipient = committee
                .and_then(|cm| cm.name.as_deref())
                .unwrap_or("Unknown Committee");
            let party = committee
                .and_then(|cm| cm.party.as_deref())
                .unwrap_or("N/A");
            let date = c.contribution_receipt_date.clone();
            let employer = c.contributor_employer.as_deref().unwrap_or("N/A");
            let sub_id = c.sub_id.as_deref().unwrap_or("unknown");

            if amount < 10_000.0 {
                return None;
            }

            let amount_str = format_amount(amount);
            let title = format!("{} contributes {} to {} ({})", contributor, amount_str, recipient, party);

            let content_snippet = format!(
                "Campaign contribution: {} contributed {} to {} on {}. Party: {}. Employer: {}.",
                contributor, amount_str, recipient,
                date.as_deref().unwrap_or("unknown date"),
                party, employer,
            );

            let metadata = serde_json::json!({
                "contributor": contributor,
                "recipient": recipient,
                "amount": amount,
                "party": party,
                "employer": employer,
                "date": date,
                "sub_id": sub_id,
            });

            Some(RawArticle {
                title,
                url: format!("https://www.fec.gov/data/receipts/?sub_id={}", sub_id),
                source_name: "FEC".to_string(),
                source_url: "https://api.open.fec.gov".to_string(),
                published_at: date,
                content_snippet,
                sector: "finance".to_string(),
                feed_id: format!("fec_{}", sub_id),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
            })
        })
        .collect();

    tracing::info!("FEC: {} articles produced", articles.len());
    Ok(articles)
}

fn format_amount(amount: f64) -> String {
    if amount >= 1_000_000.0 {
        format!("${:.1}M", amount / 1_000_000.0)
    } else if amount >= 1_000.0 {
        format!("${:.0}K", amount / 1_000.0)
    } else {
        format!("${:.0}", amount)
    }
}
