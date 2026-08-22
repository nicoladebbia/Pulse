use super::RawArticle;
use serde::Deserialize;

/// Federal Register — Proposed rules and final rules from the US government.
/// Free API, no key required. Publishes every business day.

#[derive(Deserialize)]
struct FedRegResponse {
    results: Vec<FedRegDocument>,
}

#[derive(Deserialize)]
struct FedRegDocument {
    title: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    agencies: Option<Vec<FedRegAgency>>,
    document_number: Option<String>,
    publication_date: Option<String>,
    html_url: Option<String>,
    #[serde(rename = "type")]
    doc_type: Option<String>,
    cfr_references: Option<Vec<FedRegCfr>>,
}

#[derive(Deserialize)]
struct FedRegAgency {
    name: Option<String>,
}

#[derive(Deserialize)]
struct FedRegCfr {
    title: Option<serde_json::Value>,
    part: Option<serde_json::Value>,
}

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let yesterday = (chrono::Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    // Fetch proposed rules + final rules (skip notices — too noisy)
    super::API_CALLS.federal_register.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client
        .get("https://www.federalregister.gov/api/v1/documents.json")
        .query(&[
            ("conditions[publication_date][gte]", yesterday.as_str()),
            ("conditions[type][]", "RULE"),
            ("conditions[type][]", "PRORULE"),
            ("fields[]", "title"),
            ("fields[]", "abstract"),
            ("fields[]", "agencies"),
            ("fields[]", "document_number"),
            ("fields[]", "publication_date"),
            ("fields[]", "html_url"),
            ("fields[]", "type"),
            ("fields[]", "cfr_references"),
            ("per_page", "100"),
        ])
        .header("User-Agent", "Pulse/1.0 (news aggregator)")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Federal Register API returned {}", resp.status());
    }

    let body = resp.text().await?;
    let data: FedRegResponse = match serde_json::from_str(&body) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("Federal Register JSON parse error: {} (body starts with: {})",
                e, &body[..body.len().min(200)]);
            anyhow::bail!("Federal Register JSON parse error: {}", e);
        }
    };
    tracing::info!("Federal Register: {} documents returned", data.results.len());

    let articles: Vec<RawArticle> = data
        .results
        .into_iter()
        .filter_map(|doc| {
            let title_text = doc.title.as_deref()?;
            let doc_number = doc.document_number.as_deref()?;
            let html_url = doc.html_url.as_deref()?;

            let abstract_text = doc.abstract_text.as_deref().unwrap_or("No abstract available");
            let doc_type = doc.doc_type.as_deref().unwrap_or("RULE");
            let type_label = match doc_type {
                "PRORULE" => "Proposed Rule",
                "RULE" => "Final Rule",
                _ => doc_type,
            };

            let agencies: Vec<String> = doc
                .agencies
                .as_ref()
                .map(|a| {
                    a.iter()
                        .filter_map(|ag| ag.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            let agency_str = if agencies.is_empty() {
                "Unknown Agency".to_string()
            } else {
                agencies.join(", ")
            };

            let cfr_list: Vec<String> = doc
                .cfr_references
                .as_ref()
                .map(|refs| {
                    refs.iter()
                        .filter_map(|c| {
                            Some(format!("{} CFR Part {}", c.title.as_ref()?, c.part.as_ref()?))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let cfr_str = if cfr_list.is_empty() {
                "N/A".to_string()
            } else {
                cfr_list.join(", ")
            };

            let headline = format!("{}: {}", agency_str, truncate(title_text, 120));

            let content_snippet = format!(
                "Regulatory action ({}): {}. Agencies: {}. CFR references: {}.",
                type_label, abstract_text, agency_str, cfr_str
            );

            let metadata = serde_json::json!({
                "document_number": doc_number,
                "agencies": agencies,
                "cfr_references": cfr_str,
                "rule_type": type_label,
                "abstract": abstract_text,
            });

            let sector = agency_to_sector(&agency_str);

            Some(RawArticle {
                title: headline,
                url: html_url.to_string(),
                source_name: "Federal Register".to_string(),
                source_url: "https://www.federalregister.gov".to_string(),
                published_at: doc.publication_date,
                content_snippet,
                sector,
                feed_id: "federal_register".to_string(),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
            })
        })
        .collect();

    tracing::info!("Federal Register: {} articles produced", articles.len());
    Ok(articles)
}

fn agency_to_sector(agency: &str) -> String {
    let lower = agency.to_lowercase();
    if lower.contains("securities") || lower.contains("sec") || lower.contains("treasury") {
        "finance".to_string()
    // Health/FDA/NIH agencies bucket to "tech" alongside FCC/commerce/patent
    // because the news sectors are ai/miami/italy/tech + finance — there is no
    // health NEWS sector (health exists only as a freedoms category).
    } else if lower.contains("fda")
        || lower.contains("food")
        || lower.contains("health")
        || lower.contains("nih")
        || lower.contains("fcc")
        || lower.contains("commerce")
        || lower.contains("patent")
        || lower.contains("technology")
    {
        "tech".to_string()
    } else {
        "finance".to_string()
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
