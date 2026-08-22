use super::RawArticle;
use serde::Deserialize;

/// Senate LDA (Lobbying Disclosure Act) — Lobbying filings.
/// API: https://lda.gov/api/v1/
/// FREE — no API key required!
/// Replaces old lda.senate.gov (dies June 30, 2026).

#[derive(Deserialize)]
// Fields mirror the Senate LDA API response shape; some are parsed but not yet consumed.
#[allow(dead_code)]
struct LdaResponse {
    results: Vec<LdaFiling>,
}

#[derive(Deserialize)]
// Fields mirror the Senate LDA API response shape; some are parsed but not yet consumed.
#[allow(dead_code)]
struct LdaFiling {
    filing_uuid: Option<String>,
    filing_type_display: Option<String>,
    filing_year: Option<i32>,
    income: Option<String>,
    expenses: Option<String>,
    dt_posted: Option<String>,
    registrant: Option<LdaRegistrant>,
    client: Option<LdaClient>,
    lobbying_activities: Option<Vec<LdaActivity>>,
}

#[derive(Deserialize)]
// Fields mirror the Senate LDA API response shape; some are parsed but not yet consumed.
#[allow(dead_code)]
struct LdaRegistrant {
    name: Option<String>,
}

#[derive(Deserialize)]
// Fields mirror the Senate LDA API response shape; some are parsed but not yet consumed.
#[allow(dead_code)]
struct LdaClient {
    name: Option<String>,
    general_description: Option<String>,
}

#[derive(Deserialize)]
// Fields mirror the Senate LDA API response shape; some are parsed but not yet consumed.
#[allow(dead_code)]
struct LdaActivity {
    general_issue_code_display: Option<String>,
    description: Option<String>,
    government_entities: Option<Vec<LdaGovEntity>>,
}

#[derive(Deserialize)]
// Fields mirror the Senate LDA API response shape; some are parsed but not yet consumed.
#[allow(dead_code)]
struct LdaGovEntity {
    name: Option<String>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let thirty_days_ago = (chrono::Local::now() - chrono::Duration::days(30))
        .format("%Y-%m-%d")
        .to_string();

    // Fetch recent filings with actual lobbying activity (skip "no activity" filings)
    // Filter for filings with income > 0 to get substantive disclosures
    let url = format!(
        "https://lda.gov/api/v1/filings/?filing_dt_posted_after={}&page_size=100&ordering=-dt_posted",
        thirty_days_ago
    );

    super::API_CALLS.lda.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client
        .get(&url)
        .header("User-Agent", "Pulse/1.0 (news aggregator)")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("LDA API returned {}", resp.status());
    }

    let data: LdaResponse = resp.json().await?;
    tracing::info!("LDA: {} filings returned", data.results.len());

    let articles: Vec<RawArticle> = data
        .results
        .into_iter()
        .filter_map(|filing| {
            let uuid = filing.filing_uuid.as_deref()?;
            let client_name = filing.client.as_ref()?.name.as_deref()?;
            let registrant_name = filing.registrant.as_ref()
                .and_then(|r| r.name.as_deref())
                .unwrap_or("Unknown firm");

            // Parse income/expenses
            let income: f64 = filing.income.as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let expenses: f64 = filing.expenses.as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let amount = if income > 0.0 { income } else { expenses };

            // Skip "no activity" filings (no amount, no lobbying activities)
            let activities = filing.lobbying_activities.as_deref().unwrap_or(&[]);
            if amount == 0.0 && activities.is_empty() {
                return None;
            }

            // Extract lobbying issues and government entities
            let issues: Vec<String> = activities
                .iter()
                .filter_map(|a| a.general_issue_code_display.clone())
                .collect();
            let issue_str = if issues.is_empty() {
                "unspecified issues".to_string()
            } else {
                issues.join(", ")
            };

            let gov_entities: Vec<String> = activities
                .iter()
                .flat_map(|a| {
                    a.government_entities.as_deref().unwrap_or(&[])
                        .iter()
                        .filter_map(|g| g.name.clone())
                })
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            let descriptions: Vec<String> = activities
                .iter()
                .filter_map(|a| a.description.clone())
                .collect();
            let desc_summary = if descriptions.is_empty() {
                String::new()
            } else {
                truncate(&descriptions.join(" | "), 300)
            };

            let amount_str = format_amount(amount);
            let title = if amount > 0.0 {
                format!("{} spent {} lobbying on {}", client_name, amount_str, issue_str)
            } else {
                format!("{} lobbied on {}", client_name, issue_str)
            };

            let content_snippet = format!(
                "Lobbying disclosure: {} hired {} to lobby {}. Amount: {}. Issues: {}. {}",
                client_name, registrant_name,
                if gov_entities.is_empty() { "Congress".to_string() } else { gov_entities.join(", ") },
                amount_str, issue_str, desc_summary,
            );

            let metadata = serde_json::json!({
                "filing_uuid": uuid,
                "client": client_name,
                "registrant": registrant_name,
                "amount": amount,
                "issues": issues,
                "government_entities": gov_entities,
                "filing_type": filing.filing_type_display,
                "filing_year": filing.filing_year,
            });

            Some(RawArticle {
                title,
                url: format!("https://lda.gov/filings/public/filing/{}/print/", uuid),
                source_name: "Senate LDA".to_string(),
                source_url: "https://lda.gov".to_string(),
                published_at: filing.dt_posted.as_ref().map(|d| d[..10].to_string()),
                content_snippet,
                sector: "finance".to_string(),
                feed_id: format!("lda_{}", uuid),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
            })
        })
        .collect();

    tracing::info!("LDA: {} articles produced (with activity)", articles.len());
    Ok(articles)
}

fn format_amount(amount: f64) -> String {
    if amount >= 1_000_000.0 {
        format!("${:.1}M", amount / 1_000_000.0)
    } else if amount >= 1_000.0 {
        format!("${:.0}K", amount / 1_000.0)
    } else if amount > 0.0 {
        format!("${:.0}", amount)
    } else {
        "undisclosed amount".to_string()
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
