use super::RawArticle;
use serde::Deserialize;

/// SEC EDGAR — Direct HTTP to SEC's EFTS full-text search API.
/// Fetches recent Form 4 (insider trades), 8-K (material events),
/// Form D (private placements), and 13F (institutional holdings).
///
/// SEC requires User-Agent with name+email. Rate limit: 10 req/sec.
/// No API key needed.

const SEC_USER_AGENT: &str = "Pulse/1.0 (pulse-app@example.com)";

// --- Form 4: Insider Trades ---

#[derive(Deserialize)]
struct EdgarSearchResponse {
    hits: EdgarHits,
}

#[derive(Deserialize)]
struct EdgarHits {
    hits: Vec<EdgarHit>,
}

#[derive(Deserialize)]
struct EdgarHit {
    _source: EdgarSource,
    _id: String,
}

#[derive(Deserialize)]
struct EdgarSource {
    display_names: Option<Vec<String>>,
    file_date: Option<String>,
    period_of_report: Option<String>,
    #[serde(default)]
    form: Option<String>,
    adsh: Option<String>,
    ciks: Option<Vec<String>>,
    file_description: Option<String>,
}

/// Fetch all financial filings from SEC EDGAR.
pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut articles = Vec::new();

    // Fetch each filing type with delays to respect rate limits
    match fetch_filing_type(&client, "4", "edgar_form4", 50).await {
        Ok(a) => {
            tracing::info!("SEC EDGAR Form 4: {} filings", a.len());
            articles.extend(a);
        }
        Err(e) => tracing::warn!("SEC EDGAR Form 4 failed: {}", e),
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    match fetch_filing_type(&client, "8-K", "edgar_8k", 30).await {
        Ok(a) => {
            tracing::info!("SEC EDGAR 8-K: {} filings", a.len());
            articles.extend(a);
        }
        Err(e) => tracing::warn!("SEC EDGAR 8-K failed: {}", e),
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    match fetch_filing_type(&client, "D", "edgar_formd", 30).await {
        Ok(a) => {
            tracing::info!("SEC EDGAR Form D: {} filings", a.len());
            articles.extend(a);
        }
        Err(e) => tracing::warn!("SEC EDGAR Form D failed: {}", e),
    }

    tracing::info!("SEC EDGAR total: {} articles", articles.len());
    Ok(articles)
}

/// Fetch a specific filing type from EDGAR's full-text search.
async fn fetch_filing_type(
    client: &reqwest::Client,
    form_type: &str,
    feed_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<RawArticle>> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let week_ago = (chrono::Local::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();

    // EDGAR full-text search system (EFTS)
    let url = format!(
        "https://efts.sec.gov/LATEST/search-index?q=%22%22&dateRange=custom&startdt={}&enddt={}&forms={}&from=0&size={}",
        week_ago, today, form_type, limit
    );

    let resp = client
        .get(&url)
        .header("User-Agent", SEC_USER_AGENT)
        .header("Accept", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        // Fallback: try the RSS feed approach
        return fetch_via_rss(client, form_type, feed_id).await;
    }

    let data: EdgarSearchResponse = match resp.json().await {
        Ok(d) => d,
        Err(_) => return fetch_via_rss(client, form_type, feed_id).await,
    };

    let mut articles = Vec::new();
    let mut form4_xml_count = 0;
    let mut eight_k_parsed = 0;

    for hit in data.hits.hits {
        let src = hit._source;
        let display_names = src.display_names.as_deref().unwrap_or(&[]);
        let entity = display_names
            .iter()
            .find(|n| !n.contains("CIK"))
            .or(display_names.first())
            .map(|n| n.split("  (CIK").next().unwrap_or(n).trim().to_string())
            .unwrap_or_else(|| "Unknown Entity".to_string());

        let form = src.form.as_deref().unwrap_or(form_type).to_string();
        let file_date = src.file_date.clone();
        let accession = src.adsh.clone().unwrap_or_else(|| hit._id.clone());
        let cik = src.ciks.as_ref().and_then(|c| c.last().cloned());

        let base_url = format!(
            "https://www.sec.gov/Archives/edgar/data/{}/{}",
            cik.as_deref().unwrap_or("0"),
            accession.replace('-', "")
        );

        // For Form 4: download and parse the actual XML for transaction details
        let form4_data = if form == "4" && form4_xml_count < 30 {
            match download_form4_xml(client, &base_url, &accession).await {
                Ok(data) => { form4_xml_count += 1; Some(data) }
                Err(_) => None,
            }
        } else { None };

        // For 8-K: download HTML and parse Item numbers
        let eight_k_data = if form == "8-K" && eight_k_parsed < 20 {
            match download_8k_items(client, &base_url).await {
                Ok(data) => { eight_k_parsed += 1; Some(data) }
                Err(_) => None,
            }
        } else { None };

        let (title, content_snippet) = if let Some(ref f4) = form4_data {
            build_form4_text(&entity, f4)
        } else if let Some(ref ek) = eight_k_data {
            build_8k_text(&entity, ek)
        } else {
            build_filing_text(&form, &entity, &src)
        };

        let mut metadata = serde_json::json!({
            "filing_type": form,
            "accession_number": accession,
            "entity_name": entity,
            "file_date": file_date,
            "cik": cik,
            "display_names": display_names,
        });

        // Merge Form 4 transaction data
        if let Some(f4) = form4_data {
            metadata["transaction_code"] = serde_json::json!(f4.transaction_code);
            metadata["shares"] = serde_json::json!(f4.shares);
            metadata["price_per_share"] = serde_json::json!(f4.price_per_share);
            metadata["total_value"] = serde_json::json!(f4.total_value);
            metadata["is_officer"] = serde_json::json!(f4.is_officer);
            metadata["is_director"] = serde_json::json!(f4.is_director);
            metadata["officer_title"] = serde_json::json!(f4.officer_title);
            metadata["owner_name"] = serde_json::json!(f4.owner_name);
            metadata["post_transaction_shares"] = serde_json::json!(f4.post_transaction_shares);
            metadata["ownership_type"] = serde_json::json!(f4.ownership_type);
            let (classification, signal_weight) = classify_insider_trade(&f4);
            metadata["trade_classification"] = serde_json::json!(classification);
            metadata["signal_weight"] = serde_json::json!(signal_weight);
        }

        // Merge 8-K event data
        if let Some(ek) = eight_k_data {
            metadata["item_numbers"] = serde_json::json!(ek.item_numbers);
            metadata["event_classification"] = serde_json::json!(ek.event_classification);
            metadata["event_severity"] = serde_json::json!(ek.event_severity);
            metadata["content_preview"] = serde_json::json!(ek.content_preview);
        }

        articles.push(RawArticle {
            title,
            url: base_url,
            source_name: format!("SEC EDGAR {}", form),
            source_url: "https://www.sec.gov".to_string(),
            published_at: file_date,
            content_snippet,
            sector: "finance".to_string(),
            feed_id: feed_id.to_string(),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
        });
    }

    if form4_xml_count > 0 {
        tracing::info!("SEC EDGAR: parsed {} Form 4 XMLs with transaction details", form4_xml_count);
    }
    if eight_k_parsed > 0 {
        tracing::info!("SEC EDGAR: parsed {} 8-K filings with event classification", eight_k_parsed);
    }

    Ok(articles)
}

/// Fallback: fetch recent filings via SEC's RSS feeds.
async fn fetch_via_rss(
    client: &reqwest::Client,
    form_type: &str,
    feed_id: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    // SEC provides RSS feeds for recent filings
    let url = format!(
        "https://www.sec.gov/cgi-bin/browse-edgar?action=getcompany&type={}&dateb=&owner=include&count=40&search_text=&action=getcompany&output=atom",
        form_type
    );

    let resp = client
        .get(&url)
        .header("User-Agent", SEC_USER_AGENT)
        .send()
        .await?;

    if !resp.status().is_success() {
        // Final fallback: use EDGAR's recent filings RSS
        return fetch_recent_filings_rss(client, form_type, feed_id).await;
    }

    let body = resp.text().await?;

    // Parse the Atom feed for filing entries
    let mut articles = Vec::new();
    for entry in body.split("<entry>").skip(1).take(30) {
        let title = extract_xml_field(entry, "title").unwrap_or_default();
        let link = extract_xml_attr(entry, "link", "href").unwrap_or_default();
        let updated = extract_xml_field(entry, "updated");
        let summary = extract_xml_field(entry, "summary").unwrap_or_default();

        if title.is_empty() || link.is_empty() {
            continue;
        }

        let metadata = serde_json::json!({
            "filing_type": form_type,
            "source": "rss_fallback",
        });

        articles.push(RawArticle {
            title: format!("[SEC {}] {}", form_type, clean_html(&title)),
            url: link,
            source_name: format!("SEC EDGAR {}", form_type),
            source_url: "https://www.sec.gov".to_string(),
            published_at: updated,
            content_snippet: clean_html(&summary),
            sector: "finance".to_string(),
            feed_id: feed_id.to_string(),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
        });
    }

    tracing::info!(
        "SEC EDGAR {} (RSS fallback): {} filings",
        form_type,
        articles.len()
    );
    Ok(articles)
}

/// Use SEC's latest filings RSS feed as last resort.
async fn fetch_recent_filings_rss(
    client: &reqwest::Client,
    form_type: &str,
    feed_id: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    let url = format!(
        "https://www.sec.gov/cgi-bin/browse-edgar?action=getcompany&type={}&dateb=&owner=include&count=20&search_text=&action=getcompany&output=atom",
        form_type
    );

    let resp = client
        .get(&url)
        .header("User-Agent", SEC_USER_AGENT)
        .send()
        .await?;

    let body = resp.text().await?;
    let mut articles = Vec::new();

    // Simple XML parsing for Atom entries
    for entry in body.split("<entry>").skip(1).take(20) {
        let title = extract_xml_field(entry, "title").unwrap_or_default();
        let link = extract_xml_attr(entry, "link", "href").unwrap_or_default();
        let updated = extract_xml_field(entry, "updated");

        if title.is_empty() {
            continue;
        }

        let metadata = serde_json::json!({
            "filing_type": form_type,
            "source": "recent_filings_rss",
        });

        articles.push(RawArticle {
            title: format!("[SEC {}] {}", form_type, clean_html(&title)),
            url: if link.is_empty() {
                format!("https://www.sec.gov/cgi-bin/browse-edgar?type={}", form_type)
            } else {
                link
            },
            source_name: format!("SEC EDGAR {}", form_type),
            source_url: "https://www.sec.gov".to_string(),
            published_at: updated,
            content_snippet: format!("SEC {} filing by {}", form_type, clean_html(&title)),
            sector: "finance".to_string(),
            feed_id: feed_id.to_string(),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
        });
    }

    tracing::info!(
        "SEC EDGAR {} (recent filings RSS): {} filings",
        form_type,
        articles.len()
    );
    Ok(articles)
}

/// Parsed Form 4 transaction data from XML.
struct Form4Data {
    transaction_code: String,   // P=Purchase, S=Sale, A=Award, M=Exercise, G=Gift, F=Tax
    shares: f64,
    price_per_share: f64,
    total_value: f64,
    is_officer: bool,
    is_director: bool,
    officer_title: String,
    owner_name: String,
    post_transaction_shares: f64,
    ownership_type: String,     // D=Direct, I=Indirect
}

/// Download and parse Form 4 XML from EDGAR filing.
async fn download_form4_xml(
    client: &reqwest::Client,
    base_url: &str,
    accession: &str,
) -> anyhow::Result<Form4Data> {
    // The filing index lists all documents. The primary XML is usually {accession}.xml
    let accession_nodash = accession.replace('-', "");
    // Try the direct XML URL pattern
    let xml_url = format!("{}/{}.xml", base_url, accession_nodash);

    let resp = client
        .get(&xml_url)
        .header("User-Agent", SEC_USER_AGENT)
        .send()
        .await?;

    let xml = if resp.status().is_success() {
        resp.text().await?
    } else {
        // Try index page to find XML document
        let index_url = format!("{}/", base_url);
        let index_resp = client
            .get(&index_url)
            .header("User-Agent", SEC_USER_AGENT)
            .send()
            .await?;

        if !index_resp.status().is_success() {
            anyhow::bail!("Form 4 index not available");
        }

        let index_html = index_resp.text().await?;
        // Find .xml link in the index
        let xml_file = index_html
            .split("href=\"")
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .find(|href| href.ends_with(".xml") && !href.contains("R1") && !href.contains("R2"))
            .ok_or_else(|| anyhow::anyhow!("No XML found in Form 4 index"))?;

        let full_xml_url = if xml_file.starts_with("http") {
            xml_file.to_string()
        } else {
            format!("{}/{}", base_url, xml_file)
        };

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        let xml_resp = client
            .get(&full_xml_url)
            .header("User-Agent", SEC_USER_AGENT)
            .send()
            .await?;

        if !xml_resp.status().is_success() {
            anyhow::bail!("Form 4 XML download failed");
        }
        xml_resp.text().await?
    };

    // Rate limit
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    parse_form4_xml(&xml)
}

/// Parse Form 4 XML to extract transaction details.
fn parse_form4_xml(xml: &str) -> anyhow::Result<Form4Data> {
    // Extract reporting owner info
    let owner_name = extract_xml_field(xml, "rptOwnerName")
        .unwrap_or_default();
    let is_officer = xml.contains("<isOfficer>1</isOfficer>")
        || xml.contains("<isOfficer>true</isOfficer>");
    let is_director = xml.contains("<isDirector>1</isDirector>")
        || xml.contains("<isDirector>true</isDirector>");
    let officer_title = extract_xml_field(xml, "officerTitle")
        .unwrap_or_default();

    // Extract the first non-derivative transaction
    let (transaction_code, shares, price, ownership) = if let Some(txn_block) =
        xml.split("<nonDerivativeTransaction>").nth(1)
    {
        let code = extract_xml_nested_value(txn_block, "transactionCode")
            .unwrap_or_else(|| "?".to_string());
        let shares = extract_xml_nested_value(txn_block, "transactionShares")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let price = extract_xml_nested_value(txn_block, "transactionPricePerShare")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let ownership = extract_xml_nested_value(txn_block, "directOrIndirectOwnership")
            .unwrap_or_else(|| "D".to_string());
        (code, shares, price, ownership)
    } else {
        // Try nonDerivativeHolding (no transaction, just position report)
        ("H".to_string(), 0.0, 0.0, "D".to_string())
    };

    // Post-transaction shares
    let post_shares = xml
        .split("<sharesOwnedFollowingTransaction>")
        .nth(1)
        .and_then(|s| extract_xml_field(s, "value"))
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let total_value = shares * price;

    Ok(Form4Data {
        transaction_code,
        shares,
        price_per_share: price,
        total_value,
        is_officer,
        is_director,
        officer_title,
        owner_name,
        post_transaction_shares: post_shares,
        ownership_type: ownership,
    })
}

/// Extract a <value>X</value> nested inside a parent element.
fn extract_xml_nested_value(block: &str, parent_tag: &str) -> Option<String> {
    let open = format!("<{}", parent_tag);
    let start = block.find(&open)?;
    let section = &block[start..];
    // Find <value>X</value> within this section
    let val_start = section.find("<value>")? + 7;
    let val_end = section[val_start..].find("</value>")? + val_start;
    Some(section[val_start..val_end].trim().to_string())
}

/// Classify an insider trade into signal categories.
/// Returns (classification, signal_weight).
fn classify_insider_trade(f4: &Form4Data) -> (&'static str, f64) {
    match f4.transaction_code.as_str() {
        "P" => {
            // Open market purchase — strongest signal
            if f4.is_officer && f4.total_value >= 100_000.0 {
                ("strong_buy", 1.0)
            } else if f4.is_officer && f4.total_value >= 25_000.0 {
                ("moderate_buy", 0.7)
            } else if f4.is_director && f4.total_value >= 50_000.0 {
                ("moderate_buy", 0.6)
            } else if f4.total_value >= 10_000.0 {
                ("small_buy", 0.3)
            } else {
                ("minimal_buy", 0.1)
            }
        }
        "S" => {
            // Sale — usually routine but can be informative
            if f4.is_officer && f4.post_transaction_shares > 0.0 && f4.shares > 0.0 {
                let pct_sold = f4.shares / (f4.post_transaction_shares + f4.shares);
                if pct_sold > 0.20 {
                    ("informative_sale", -0.3)
                } else {
                    ("routine_sale", 0.0)
                }
            } else {
                ("routine_sale", 0.0)
            }
        }
        "A" => ("award", 0.0),          // Stock award — no signal
        "M" => ("option_exercise", 0.0), // Option exercise — usually routine
        "G" => ("gift", 0.0),           // Gift — no signal
        "F" => ("tax_withholding", 0.0), // Tax withholding sale — no signal
        "H" => ("holding_report", 0.0), // Just a position report
        _ => ("unknown", 0.0),
    }
}

/// Parsed 8-K event data.
struct EightKData {
    item_numbers: Vec<String>,
    event_classification: String,
    event_severity: f64,       // 0.0 to 1.0 (negative events use negative in cross-signal)
    content_preview: String,   // First ~500 chars of filing content
}

/// SEC 8-K Item number → event classification and severity.
/// Based on SEC standardized Item numbers.
fn classify_8k_items(items: &[String]) -> (String, f64) {
    for item in items {
        let (class, sev) = match item.as_str() {
            "2.01" => ("acquisition_completion", 0.9),
            "2.02" => ("earnings_results", 0.85),
            "1.01" => ("material_agreement", 0.8),
            "7.01" => ("regulation_fd", 0.7),
            "2.05" => ("restructuring", -0.6),
            "2.06" => ("material_impairment", -0.8),
            "1.02" => ("agreement_termination", -0.5),
            "3.01" => ("delisting_notice", -0.9),
            "4.01" => ("auditor_change", -0.4),
            "5.02" => ("executive_change", 0.5),
            "5.01" => ("corporate_governance", 0.3),
            "5.03" => ("bylaws_amendment", 0.2),
            "5.07" => ("shareholder_vote", 0.4),
            "8.01" => ("other_event", 0.3),
            "9.01" => continue,
            _ => continue,
        };
        return (class.to_string(), sev);
    }
    ("routine_filing".to_string(), 0.1)
}

/// Download 8-K filing and extract Item numbers.
async fn download_8k_items(
    client: &reqwest::Client,
    base_url: &str,
) -> anyhow::Result<EightKData> {
    // Download the filing index to find the main 8-K document
    let index_url = format!("{}/", base_url);
    let resp = client
        .get(&index_url)
        .header("User-Agent", SEC_USER_AGENT)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("8-K index not available");
    }

    let index_html = resp.text().await?;

    // Find the primary 8-K document (usually .htm or .html, not exhibits)
    let doc_href = index_html
        .split("href=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .find(|href| {
            let h = href.to_lowercase();
            (h.ends_with(".htm") || h.ends_with(".html"))
                && !h.contains("ex-") && !h.contains("exhibit")
                && !h.contains("r1.htm") && !h.contains("r2.htm")
        });

    let doc_url = match doc_href {
        Some(href) if href.starts_with("http") => href.to_string(),
        Some(href) => format!("{}/{}", base_url, href),
        None => anyhow::bail!("No 8-K document found in index"),
    };

    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    let doc_resp = client
        .get(&doc_url)
        .header("User-Agent", SEC_USER_AGENT)
        .send()
        .await?;

    if !doc_resp.status().is_success() {
        anyhow::bail!("8-K document download failed");
    }

    let body = doc_resp.text().await?;
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    // Extract Item numbers from the HTML
    let item_numbers = extract_8k_item_numbers(&body);

    // Get clean text preview (strip HTML)
    let clean_text = clean_html(&body);
    let content_preview: String = clean_text.chars().take(500).collect();

    let (event_classification, event_severity) = classify_8k_items(&item_numbers);

    Ok(EightKData {
        item_numbers,
        event_classification,
        event_severity,
        content_preview,
    })
}

/// Extract Item numbers from 8-K filing HTML.
/// Items appear as "Item 1.01", "Item 2.02", etc. in the document text.
fn extract_8k_item_numbers(html: &str) -> Vec<String> {
    let text = clean_html(html).to_lowercase();
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Match patterns like "item 1.01", "item 2.02", etc.
    let mut pos = 0;
    while let Some(idx) = text[pos..].find("item ") {
        let start = pos + idx + 5;
        if start + 4 <= text.len() {
            let candidate = &text[start..start + 4];
            // Must be X.XX format (digit.digit digit)
            let chars: Vec<char> = candidate.chars().collect();
            if chars.len() >= 4
                && chars[0].is_ascii_digit()
                && chars[1] == '.'
                && chars[2].is_ascii_digit()
                && chars[3].is_ascii_digit()
            {
                let item = format!("{}.{}{}", chars[0], chars[2], chars[3]);
                if seen.insert(item.clone()) {
                    items.push(item);
                }
            }
        }
        pos = start;
    }

    items
}

/// Build human-readable title/snippet for a parsed 8-K.
fn build_8k_text(entity: &str, ek: &EightKData) -> (String, String) {
    let event_desc = match ek.event_classification.as_str() {
        "acquisition_completion" => "completed acquisition/disposition",
        "earnings_results" => "released financial results",
        "material_agreement" => "entered material agreement",
        "regulation_fd" => "made Regulation FD disclosure",
        "restructuring" => "announced restructuring/exit activities",
        "material_impairment" => "reported material impairment",
        "agreement_termination" => "terminated material agreement",
        "delisting_notice" => "received delisting notice",
        "auditor_change" => "changed auditor",
        "executive_change" => "announced executive departure/appointment",
        "corporate_governance" => "updated corporate governance",
        "shareholder_vote" => "reported shareholder vote results",
        "other_event" => "reported material event",
        _ => "filed 8-K disclosure",
    };

    let items_str = if ek.item_numbers.is_empty() {
        String::new()
    } else {
        format!(" (Items: {})", ek.item_numbers.join(", "))
    };

    let severity_label = if ek.event_severity >= 0.8 { "Major" }
        else if ek.event_severity >= 0.5 { "Significant" }
        else if ek.event_severity <= -0.5 { "Negative" }
        else if ek.event_severity <= -0.3 { "Concerning" }
        else { "Material" };

    let title = format!("{}: {} {}{}", severity_label, entity, event_desc, items_str);
    let snippet = format!(
        "SEC 8-K: {} {}. Event classification: {} (severity: {:.0}%). {}",
        entity, event_desc, ek.event_classification,
        ek.event_severity.abs() * 100.0,
        if ek.content_preview.len() > 50 {
            format!("Preview: {}...", &ek.content_preview[..50])
        } else {
            String::new()
        }
    );

    (title, snippet)
}

/// Build human-readable title/snippet for a parsed Form 4.
fn build_form4_text(entity: &str, f4: &Form4Data) -> (String, String) {
    let action = match f4.transaction_code.as_str() {
        "P" => "purchased",
        "S" => "sold",
        "A" => "was awarded",
        "M" => "exercised options for",
        "G" => "gifted",
        "F" => "withheld for taxes",
        _ => "reported transaction of",
    };

    let role = if f4.is_officer && !f4.officer_title.is_empty() {
        format!("{} ({})", f4.owner_name, f4.officer_title)
    } else if f4.is_director {
        format!("{} (Director)", f4.owner_name)
    } else if !f4.owner_name.is_empty() {
        f4.owner_name.clone()
    } else {
        "Insider".to_string()
    };

    let title = if f4.total_value >= 1_000_000.0 {
        format!("{} insider {} ${:.1}M of stock ({} shares)", entity, action, f4.total_value / 1_000_000.0, f4.shares as i64)
    } else if f4.total_value >= 1_000.0 {
        format!("{} insider {} ${:.0}K of stock ({} shares)", entity, action, f4.total_value / 1_000.0, f4.shares as i64)
    } else {
        format!("{} insider {} {} shares", entity, action, f4.shares as i64)
    };

    let snippet = format!(
        "SEC Form 4: {} {} {} shares of {} at ${:.2}/share (total ${:.0}). Post-transaction holdings: {} shares.",
        role, action, f4.shares as i64, entity, f4.price_per_share, f4.total_value, f4.post_transaction_shares as i64,
    );

    (title, snippet)
}

fn build_filing_text(form_type: &str, entity: &str, src: &EdgarSource) -> (String, String) {
    let desc = src.file_description.as_deref().unwrap_or("");
    let date = src
        .period_of_report
        .as_deref()
        .or(src.file_date.as_deref())
        .unwrap_or("unknown date");

    match form_type {
        "4" => {
            let title = format!("{} filed insider transaction report (Form 4)", entity);
            let snippet = format!(
                "Insider trade filing: {} reported insider transactions on {}. {}",
                entity, date, desc
            );
            (title, snippet)
        }
        "8-K" => {
            let title = format!("{} reports material event (8-K)", entity);
            let snippet = format!(
                "Material event: {} filed an 8-K on {} reporting a material event. {}",
                entity, date, desc
            );
            (title, snippet)
        }
        "D" => {
            let title = format!("{} files private placement offering (Form D)", entity);
            let snippet = format!(
                "Private placement: {} filed a Form D on {} for a securities offering. {}",
                entity, date, desc
            );
            (title, snippet)
        }
        "13F-HR" | "13F" => {
            let title = format!("{} reports institutional holdings (13F)", entity);
            let snippet = format!(
                "Institutional holdings: {} filed a 13F on {} reporting portfolio positions. {}",
                entity, date, desc
            );
            (title, snippet)
        }
        _ => {
            let title = format!("{} filed {} with SEC", entity, form_type);
            let snippet = format!(
                "SEC filing: {} filed a {} on {}. {}",
                entity, form_type, date, desc
            );
            (title, snippet)
        }
    }
}

/// Extract text content between XML tags.
fn extract_xml_field(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)?;
    let after_open = xml[start..].find('>')? + start + 1;
    let end = xml[after_open..].find(&close)? + after_open;
    Some(xml[after_open..end].trim().to_string())
}

/// Extract an attribute value from an XML tag.
fn extract_xml_attr(xml: &str, tag: &str, attr: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let start = xml.find(&open)?;
    let tag_end = xml[start..].find('>')? + start;
    let tag_content = &xml[start..tag_end];

    let attr_prefix = format!("{}=\"", attr);
    let attr_start = tag_content.find(&attr_prefix)? + attr_prefix.len();
    let attr_end = tag_content[attr_start..].find('"')? + attr_start;
    Some(tag_content[attr_start..attr_end].to_string())
}

/// Strip HTML tags from text.
fn clean_html(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    result.trim().to_string()
}
