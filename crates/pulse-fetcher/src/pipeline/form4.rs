use super::*;

/// Entity-targeted Form 4 fetch. The default EDGAR fetch grabs the global recent-50
/// Form 4s (a firehose), so insider data only lands for whichever companies happen
/// to file in that window — leaving ~83% of converging companies with no insider
/// signal even though insider is the largest-weighted dimension (0.24).
///
/// This fetches Form 4s PER TRACKED CIK via SEC's submissions API, mirroring the
/// proven per-CIK pattern in bootstrap_historical.py. Bounded to companies with a
/// ticker mapping (the watchlist universe) and recent signal interest, throttled to
/// stay under SEC's 10 req/s. Inserts new Form 4 stories that the existing
/// enrichment phase + Stage 3 aggregation then pick up.
///
/// NOTE: SEC submissions API wants the 10-digit ZERO-PADDED CIK (opposite of the
/// company_tickers.json unpadded form used in ticker mapping).
/// Public entry point for `--mode fetch-form4` (standalone test/manual run).
pub(crate) async fn run_targeted_form4(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    crate::db::run_migrations(&conn)?;
    drop(conn);
    fetch_targeted_form4(db_path).await
}

pub(crate) async fn fetch_targeted_form4(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;

    // Target CIKs: ticker-mapped entities that have appeared in cross_signals
    // recently (the companies that actually feed the watchlist). Bounded to 150 to
    // keep SEC calls reasonable (~1 req/company + throttle).
    let ciks: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT et.cik, et.ticker
             FROM entity_tickers et
             JOIN cross_signals cs ON cs.entity_id = et.entity_id
             WHERE et.cik IS NOT NULL AND et.cik != ''
               AND cs.computed_at >= date('now', '-30 days')
             LIMIT 150"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if ciks.is_empty() {
        return Ok(0);
    }

    // Financial stories attach to the most recent briefing (stories.briefing_id is
    // NOT NULL). If no briefing exists yet, skip — the daily pipeline creates one.
    let briefing_id: i64 = match conn.query_row(
        "SELECT id FROM briefings ORDER BY created_at DESC LIMIT 1", [], |row| row.get(0),
    ) {
        Ok(id) => id,
        Err(_) => {
            tracing::warn!("Targeted Form 4: no briefing exists yet, skipping");
            return Ok(0);
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut inserted = 0;

    for (cik, ticker) in &ciks {
        let padded = format!("{:0>10}", cik.trim_start_matches('0'));
        let url = format!("https://data.sec.gov/submissions/CIK{}.json", padded);

        let resp = match client.get(&url)
            .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
            .send().await
        {
            Ok(r) if r.status().is_success() => r,
            Ok(r) if r.status().as_u16() == 429 => {
                tracing::warn!("Targeted Form 4: SEC 429, stopping after {} inserted", inserted);
                break;
            }
            Ok(_) | Err(_) => { tokio::time::sleep(std::time::Duration::from_millis(150)).await; continue; }
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => continue,
        };

        // Recent filings live under filings.recent, parallel arrays.
        let recent = &json["filings"]["recent"];
        let forms = recent["form"].as_array();
        let accessions = recent["accessionNumber"].as_array();
        let dates = recent["filingDate"].as_array();
        let (Some(forms), Some(accessions), Some(dates)) = (forms, accessions, dates) else {
            tracing::warn!("Targeted Form 4: {} — recent arrays missing (forms/acc/dates)", ticker);
            continue;
        };

        let company = json["name"].as_str().unwrap_or(ticker).to_string();

        // Walk recent filings; pick Form 4s from the last 30 days not already stored.
        let cutoff = chrono::Local::now().date_naive()
            .checked_sub_days(chrono::Days::new(30))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        for i in 0..forms.len().min(accessions.len()).min(dates.len()) {
            if forms[i].as_str() != Some("4") { continue; }
            let filing_date = dates[i].as_str().unwrap_or("");
            if filing_date < cutoff.as_str() { break; } // recent is date-desc, stop early
            let accession = accessions[i].as_str().unwrap_or("");
            if accession.is_empty() { continue; }

            // Skip if already stored.
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM stories WHERE financial_metadata LIKE ?1)",
                [format!("%{}%", accession)],
                |row| row.get(0),
            ).unwrap_or(false);
            if exists { continue; }

            // Insert a bare Form 4 story; transaction details get filled by the
            // existing enrich_form4_stories phase on a later run.
            let metadata = serde_json::json!({
                "cik": cik,
                "accession_number": accession,
                "ticker": ticker,
                "filing_date": filing_date,
            });
            let url_str = format!("https://www.sec.gov/cgi-bin/browse-edgar?action=getcompany&CIK={}", padded);
            let headline = format!("{} insider filing (Form 4)", company);
            let summary = format!("Form 4 insider transaction filed by {} on {}", company, filing_date);
            // Unique URL per filing (accession-based) so url_hash doesn't collide.
            let filing_url = format!("{}#{}", url_str, accession);
            let res = conn.execute(
                "INSERT OR IGNORE INTO stories
                   (briefing_id, headline, original_title, original_url, content_snippet,
                    source_name, source_type, financial_metadata, published_at, created_at,
                    sector, summary, key_facts, why_it_matters, what_to_watch,
                    url_hash, title_hash)
                 VALUES (?1, ?2, ?2, ?3, ?6, 'SEC EDGAR 4', 'financial', ?4, ?5,
                         datetime('now'), 'finance', ?6, '[]', '', '', ?7, ?8)",
                rusqlite::params![
                    briefing_id,
                    headline,
                    filing_url,
                    metadata.to_string(),
                    filing_date,
                    summary,
                    crate::dedup::url_hash(&filing_url),
                    crate::dedup::title_hash(&headline),
                ],
            );
            match res {
                Ok(n) => inserted += n,
                Err(e) => tracing::warn!("Targeted Form 4: insert failed for {} {}: {}", ticker, accession, e),
            }
        }

        // SEC rate limit: stay well under 10 req/s.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    Ok(inserted)
}

/// Enrich stored Form 4 stories that are missing transaction data.
/// Downloads the actual XML from EDGAR and parses buy/sell/shares/price.
pub(crate) async fn enrich_form4_stories(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Find Form 4 stories missing transaction_code (not yet enriched)
    let mut stmt = conn.prepare(
        "SELECT id, financial_metadata FROM stories
         WHERE source_type = 'financial'
           AND source_name LIKE '%EDGAR 4%'
           AND json_valid(financial_metadata)
           AND json_extract(financial_metadata, '$.transaction_code') IS NULL
           AND created_at >= datetime('now', '-7 days')
         LIMIT 30"
    )?;

    let candidates: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() { return Ok(0); }

    let mut enriched = 0;

    for (story_id, metadata_str) in &candidates {
        let meta: serde_json::Value = serde_json::from_str(metadata_str)?;
        let cik = meta.get("cik").and_then(|v| v.as_str()).unwrap_or("");
        let accession = meta.get("accession_number").and_then(|v| v.as_str()).unwrap_or("");

        if cik.is_empty() || accession.is_empty() { continue; }

        let cik_clean = cik.trim_start_matches('0');
        let accession_nd = accession.replace('-', "");

        // Try to find the Form 4 XML via index page
        let index_url = format!("https://www.sec.gov/Archives/edgar/data/{}/{}/", cik_clean, accession_nd);

        let index_resp = client
            .get(&index_url)
            .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
            .send()
            .await;

        let xml = if let Ok(resp) = index_resp {
            let status = resp.status();
            if status.is_success() {
                let html = resp.text().await.unwrap_or_default();
                let mut found_xml = None;
                for part in html.split("href=\"") {
                    let href = part.split('"').next().unwrap_or("");
                    if href.ends_with(".xml") && !href.contains("R1") && !href.contains("R2") && !href.contains("index") {
                        let full_url = if href.starts_with("/") {
                            format!("https://www.sec.gov{}", href)
                        } else {
                            format!("{}{}", index_url, href)
                        };
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        if let Ok(xml_resp) = client.get(&full_url)
                            .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
                            .send().await
                            && xml_resp.status().is_success() {
                                let text = xml_resp.text().await.unwrap_or_default();
                                if text.contains("<ownershipDocument") {
                                    found_xml = Some(text);
                                    break;
                                }
                            }
                    }
                }
                found_xml
            } else {
                if status.as_u16() == 429 {
                    tracing::warn!("Form 4 enrichment: SEC rate limit (429), stopping early after {} enriched", enriched);
                    break;
                }
                None
            }
        } else { None };

        let Some(xml) = xml else {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            continue;
        };

        // Parse using the same logic as edgar.rs
        let txn_code = extract_xml_value_simple(&xml, "transactionCode").unwrap_or_else(|| "?".to_string());
        let shares = extract_nested_val(&xml, "transactionShares").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        let price = extract_nested_val(&xml, "transactionPricePerShare").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        let total_value = shares * price;
        let owner_name = extract_xml_value_simple(&xml, "rptOwnerName").unwrap_or_default();
        let is_officer = xml.contains("<isOfficer>1</isOfficer>") || xml.contains("<isOfficer>true</isOfficer>");
        let is_director = xml.contains("<isDirector>1</isDirector>") || xml.contains("<isDirector>true</isDirector>");
        let officer_title = extract_xml_value_simple(&xml, "officerTitle").unwrap_or_default();
        let post_shares = extract_nested_val(&xml, "sharesOwnedFollowingTransaction").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);

        // Classify
        let (classification, signal_weight) = classify_form4_trade(&txn_code, is_officer, is_director, total_value, shares, post_shares);

        // Update metadata
        let mut updated: serde_json::Value = serde_json::from_str(metadata_str)?;
        updated["transaction_code"] = serde_json::json!(txn_code);
        updated["shares"] = serde_json::json!(shares);
        updated["price_per_share"] = serde_json::json!(price);
        updated["total_value"] = serde_json::json!(total_value);
        updated["owner_name"] = serde_json::json!(owner_name);
        updated["is_officer"] = serde_json::json!(is_officer);
        updated["is_director"] = serde_json::json!(is_director);
        updated["officer_title"] = serde_json::json!(officer_title);
        updated["post_transaction_shares"] = serde_json::json!(post_shares);
        updated["trade_classification"] = serde_json::json!(classification);
        updated["signal_weight"] = serde_json::json!(signal_weight);

        conn.execute(
            "UPDATE stories SET financial_metadata = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&updated)?, story_id],
        ).ok();

        enriched += 1;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }

    Ok(enriched)
}

pub(crate) fn extract_xml_value_simple(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

pub(crate) fn extract_nested_val(xml: &str, parent_tag: &str) -> Option<String> {
    let open = format!("<{}>", parent_tag);
    let close = format!("</{}>", parent_tag);
    let start = xml.find(&open)?;
    let end = xml.find(&close).unwrap_or(start + 200).min(start + 200);
    let section = &xml[start..end];
    let val_start = section.find("<value>")? + 7;
    let val_end = section[val_start..].find("</value>")? + val_start;
    Some(section[val_start..val_end].trim().to_string())
}

pub(crate) fn classify_form4_trade(code: &str, is_officer: bool, is_director: bool, total_value: f64, shares: f64, post_shares: f64) -> (&'static str, f64) {
    match code {
        "P" => {
            if is_officer && total_value >= 100_000.0 { ("strong_buy", 1.0) }
            else if is_officer && total_value >= 25_000.0 { ("moderate_buy", 0.7) }
            else if is_director && total_value >= 50_000.0 { ("moderate_buy", 0.6) }
            else if total_value >= 10_000.0 { ("small_buy", 0.3) }
            else { ("minimal_buy", 0.1) }
        }
        "S" => {
            if is_officer && post_shares > 0.0 && shares > 0.0 {
                let pct = shares / (post_shares + shares);
                if pct > 0.20 { return ("informative_sale", -0.3); }
            }
            ("routine_sale", 0.0)
        }
        "A" => ("award", 0.0),
        "M" => ("option_exercise", 0.0),
        "G" => ("gift", 0.0),
        "F" => ("tax_withholding", 0.0),
        _ => ("unknown", 0.0),
    }
}

/// Classify ambiguous 8-K filings (Item 8.01 "other_event") using Claude Haiku.
/// Only processes recent unclassified 8-Ks. ~$0.0005 per call.
pub(crate) async fn classify_ambiguous_8ks(db_path: &Path) -> anyhow::Result<usize> {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return Ok(0),
    };

    let conn = rusqlite::Connection::open(db_path)?;

    // Find 8-K stories with "other_event" classification and content_preview
    let mut stmt = conn.prepare(
        "SELECT id, financial_metadata FROM stories
         WHERE source_type = 'financial'
           AND source_name LIKE '%EDGAR 8-K%'
           AND json_valid(financial_metadata)
           AND json_extract(financial_metadata, '$.event_classification') = 'other_event'
           AND json_extract(financial_metadata, '$.content_preview') IS NOT NULL
           AND json_extract(financial_metadata, '$.llm_classification') IS NULL
           AND created_at >= datetime('now', '-3 days')
         LIMIT 15"
    )?;

    let candidates: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() { return Ok(0); }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut classified = 0;

    for (story_id, metadata_str) in &candidates {
        let meta: serde_json::Value = serde_json::from_str(metadata_str)?;
        let preview = meta.get("content_preview").and_then(|v| v.as_str()).unwrap_or("");
        let entity = meta.get("entity_name").and_then(|v| v.as_str()).unwrap_or("Unknown");

        if preview.len() < 20 { continue; }

        let body = serde_json::json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 150,
            "system": "Classify this SEC 8-K filing into exactly one category. Return only valid JSON.\n\nCategories:\n- earnings_surprise: unexpected financial results\n- acquisition: M&A activity, merger, purchase\n- partnership: strategic alliance, joint venture\n- restructuring: layoffs, cost cuts, exit activities\n- product_launch: new product, service, or technology\n- regulatory: FDA approval, compliance action, government\n- executive_change: C-suite departure or hire\n- bankruptcy_risk: going concern, debt default\n- shareholder_action: buyback, dividend, activist\n- capital_raise: debt offering, equity raise, IPO\n- litigation: lawsuit, settlement, legal action\n- other: none of the above\n\nReturn: {\"category\": \"...\", \"severity\": 0.0-1.0, \"summary\": \"one sentence\"}",
            "messages": [{"role": "user", "content": format!("Company: {}\n\n8-K content:\n{}", entity, &preview[..preview.len().min(1500)])}]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() { continue; }

        let response: serde_json::Value = resp.json().await?;
        let text = response["content"][0]["text"].as_str().unwrap_or("{}");

        // Parse the JSON response
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') { &text[start..=end] } else { text }
        } else { text };

        if let Ok(result) = serde_json::from_str::<serde_json::Value>(json_str) {
            let category = result.get("category").and_then(|v| v.as_str()).unwrap_or("other");
            let severity = result.get("severity").and_then(|v| v.as_f64()).unwrap_or(0.3);
            let summary = result.get("summary").and_then(|v| v.as_str()).unwrap_or("");

            // Map to actual severity sign (some categories are negative)
            let signed_severity = match category {
                "restructuring" | "bankruptcy_risk" | "litigation" => -severity,
                _ => severity,
            };

            // Update the financial_metadata JSON with LLM classification
            let mut updated_meta: serde_json::Value = serde_json::from_str(metadata_str)?;
            updated_meta["llm_classification"] = serde_json::json!(category);
            updated_meta["event_classification"] = serde_json::json!(category);
            updated_meta["event_severity"] = serde_json::json!(signed_severity);
            updated_meta["llm_summary"] = serde_json::json!(summary);

            conn.execute(
                "UPDATE stories SET financial_metadata = ?1 WHERE id = ?2",
                rusqlite::params![serde_json::to_string(&updated_meta)?, story_id],
            ).ok();

            classified += 1;
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    if classified > 0 {
        log_usage(db_path, "anthropic", "claude-haiku", "8k_classification",
            (classified * 500) as i64, (classified * 50) as i64);
    }

    Ok(classified)
}
