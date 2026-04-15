use super::RawArticle;
use serde::Deserialize;

/// SBIR.gov — Small Business Innovation Research awards.
/// Free API, no key required. Paginated JSON responses.

#[derive(Deserialize)]
struct SbirResponse {
    results: Option<Vec<SbirAward>>,
}

#[derive(Deserialize)]
struct SbirAward {
    #[serde(rename = "awardId")]
    award_id: Option<String>,
    firm: Option<String>,
    #[serde(rename = "awardTitle")]
    award_title: Option<String>,
    agency: Option<String>,
    branch: Option<String>,
    phase: Option<String>,
    #[serde(rename = "awardAmount")]
    award_amount: Option<f64>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(rename = "awardYear")]
    award_year: Option<String>,
    #[serde(rename = "dateAwarded")]
    date_awarded: Option<String>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let current_year = chrono::Local::now().format("%Y").to_string();

    // Fetch recent awards (first 100) for current year
    let url = format!(
        "https://www.sbir.gov/api/awards.json?year={}&rows=100&start=0",
        current_year
    );

    let resp = client
        .get(&url)
        .header("User-Agent", "Pulse/1.0 (news aggregator)")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("SBIR API returned {}", resp.status());
    }

    let data: SbirResponse = resp.json().await?;
    let awards = data.results.unwrap_or_default();
    tracing::info!("SBIR: {} awards returned for {}", awards.len(), current_year);

    let articles: Vec<RawArticle> = awards
        .into_iter()
        .filter_map(|award| {
            let firm = award.firm.as_deref()?;
            let award_title = award.award_title.as_deref().unwrap_or("Untitled");
            let agency = award.agency.as_deref().unwrap_or("Unknown");
            let phase = award.phase.as_deref().unwrap_or("Unknown");
            let amount = award.award_amount.unwrap_or(0.0);
            let abstract_text = award.abstract_text.as_deref().unwrap_or("No abstract");
            let award_id = award.award_id.as_deref().unwrap_or("unknown");

            let amount_str = format_amount(amount);
            let title = format!(
                "{} wins {} SBIR Phase {} from {}: {}",
                firm,
                amount_str,
                phase,
                agency,
                truncate(award_title, 80)
            );

            let content_snippet = format!(
                "SBIR award: {}. Firm: {}. Agency: {}. Phase: {}. Amount: {}. Abstract: {}",
                award_title,
                firm,
                agency,
                phase,
                amount_str,
                truncate(abstract_text, 300)
            );

            let metadata = serde_json::json!({
                "award_id": award_id,
                "firm": firm,
                "agency": agency,
                "phase": phase,
                "amount": amount,
                "award_title": award_title,
                "abstract": truncate(abstract_text, 500),
                "award_year": award.award_year,
            });

            // Composite dedup key
            let dedup_id = format!("{}_{}", agency, award_id);

            let sector = agency_to_sector(agency);

            // Use award date or fall back to year
            let published = award
                .date_awarded
                .or_else(|| award.award_year.map(|y| format!("{}-01-01", y)));

            Some(RawArticle {
                title,
                url: format!("https://www.sbir.gov/node/{}", award_id),
                source_name: "SBIR".to_string(),
                source_url: "https://www.sbir.gov".to_string(),
                published_at: published,
                content_snippet,
                sector,
                feed_id: format!("sbir_{}", dedup_id),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
            })
        })
        .collect();

    tracing::info!("SBIR: {} articles produced", articles.len());
    Ok(articles)
}

fn agency_to_sector(agency: &str) -> String {
    let lower = agency.to_lowercase();
    if lower.contains("defense") || lower.contains("dod") || lower.contains("army")
        || lower.contains("navy") || lower.contains("air force")
    {
        "tech".to_string()
    } else if lower.contains("energy") || lower.contains("doe") {
        "tech".to_string()
    } else if lower.contains("health") || lower.contains("nih") || lower.contains("hhs") {
        "tech".to_string()
    } else if lower.contains("nasa") || lower.contains("nsf") {
        "tech".to_string()
    } else {
        "finance".to_string()
    }
}

fn format_amount(amount: f64) -> String {
    if amount >= 1_000_000.0 {
        format!("${:.1}M", amount / 1_000_000.0)
    } else if amount >= 1_000.0 {
        format!("${:.0}K", amount / 1_000.0)
    } else if amount > 0.0 {
        format!("${:.0}", amount)
    } else {
        "undisclosed".to_string()
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
