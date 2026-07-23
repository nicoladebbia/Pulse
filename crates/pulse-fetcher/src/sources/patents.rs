use super::RawArticle;
use chrono::Datelike;

/// Google Patents — Recent patent publications.
///
/// Uses Google Patents' internal XHR endpoint to search recent patent
/// publications by major tech companies. No API key required.
///
/// This replaces the defunct PatentsView API (migrating to data.uspto.gov).
/// The XHR endpoint is undocumented — if it breaks, fetch() returns empty vec.
///
/// 2026-07-23: this source went fully silent for 52 days (last new story
/// 2026-06-01) despite the endpoint responding fine when called in isolation
/// (verified live: a single assignee query returned 160 real, current
/// results). Reproduced the actual failure mode directly: a burst of ~4
/// requests in quick succession got Google's "Sorry... unusual traffic" 503
/// bot-detection page, which the old code's per-run loop of 10 sequential
/// requests (one per assignee, 5s apart) was well within range of tripping —
/// especially since this runs concurrently with google_news (many
/// google.com-domain feeds) inside the same `tokio::join!`, so cumulative
/// Google-domain request volume from this IP is higher than it looks from
/// this module alone. Also confirmed independently: the `assignee:"X"`
/// query clause doesn't actually filter results (a same-day live test showed
/// assignees like "Capital One Services, Llc" and "Wells Fargo Bank, N.A."
/// returned for an Apple-only query, with `total_num_results` in the
/// hundreds of thousands) — so querying all 10 assignees was mostly just
/// re-fetching overlapping broad results 10x, not 10x the coverage.
/// Fix: 1 request per run instead of 10, rotating through the assignee list
/// by day-of-year so all 10 still get covered over time, with no filtering
/// benefit lost since the assignee clause wasn't filtering anyway.
const PATENT_ASSIGNEES: &[&str] = &[
    "Apple", "Google", "Microsoft", "Amazon", "Meta",
    "NVIDIA", "Intel", "Tesla", "Qualcomm", "Samsung",
];

pub async fn fetch() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let idx = (chrono::Local::now().ordinal0() as usize) % PATENT_ASSIGNEES.len();
    let assignee = PATENT_ASSIGNEES[idx];

    let articles = match fetch_patents_batch(&client, &[assignee]).await {
        Ok(batch) => batch,
        Err(e) => {
            tracing::debug!("Google Patents failed for {}: {}", assignee, e);
            Vec::new()
        }
    };

    if articles.is_empty() {
        tracing::info!("Google Patents: no results for {} (endpoint may be unavailable)", assignee);
    } else {
        tracing::info!("Google Patents: {} patent articles ({})", articles.len(), assignee);
    }

    Ok(articles)
}

/// Fetch recent patents for a batch of assignees via Google Patents XHR.
async fn fetch_patents_batch(
    client: &reqwest::Client,
    assignees: &[&str],
) -> anyhow::Result<Vec<RawArticle>> {
    // Build OR query: (assignee:Apple OR assignee:Google OR ...)
    let assignee_query: String = assignees.iter()
        .map(|a| format!("assignee:\"{}\"", a))
        .collect::<Vec<_>>()
        .join(" OR ");

    // Date range: last 30 days
    let end_date = chrono::Local::now().format("%Y%m%d").to_string();
    let start_date = (chrono::Local::now() - chrono::Duration::days(30))
        .format("%Y%m%d")
        .to_string();

    let query = format!(
        "q=({})&num=20&type=PATENT&country=US&dates={}-{}&sort=new",
        assignee_query, start_date, end_date
    );

    // URL-encode the query using percent-encoding (re-exported by reqwest)
    use std::fmt::Write;
    let mut encoded_query = String::new();
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded_query.push(byte as char);
            }
            _ => {
                let _ = write!(encoded_query, "%{:02X}", byte);
            }
        }
    }
    let url = format!(
        "https://patents.google.com/xhr/query?url={}",
        encoded_query
    );

    super::API_CALLS.uspto.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "application/json")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Google Patents returned {}", resp.status());
    }

    let body = resp.text().await?;

    // Check if we got HTML instead of JSON (sometimes returns challenge page)
    if body.starts_with("<!") || body.starts_with("<html") {
        anyhow::bail!("Google Patents returned HTML (possible rate limit)");
    }

    let data: serde_json::Value = serde_json::from_str(&body)?;

    let results = data
        .get("results")
        .and_then(|r| r.get("cluster"))
        .and_then(|c| c.as_array())
        .and_then(|clusters| clusters.first())
        .and_then(|cluster| cluster.get("result"))
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();

    let articles: Vec<RawArticle> = results
        .into_iter()
        .filter_map(|r| parse_google_patent(&r))
        .collect();

    Ok(articles)
}

/// Parse a Google Patents XHR result into a RawArticle.
fn parse_google_patent(result: &serde_json::Value) -> Option<RawArticle> {
    let patent = result.get("patent")?;

    let publication_number = patent.get("publication_number")
        .and_then(|v| v.as_str())?;

    let title_raw = patent.get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Patent");
    // Strip HTML tags (Google returns <b> tags in results)
    let title = clean_html(title_raw);

    let assignee = patent.get("assignee")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Assignee");

    let grant_date = patent.get("grant_date")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let pub_date = patent.get("publication_date")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let date = grant_date.or(pub_date);

    let snippet_raw = patent.get("snippet")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let snippet = clean_html(snippet_raw);

    let inventor = patent.get("inventor")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let is_grant = grant_date.is_some();
    let doc_type = if is_grant { "patent grant" } else { "patent application" };

    let headline = format!("{} {} — {}: {}", assignee, doc_type, publication_number, truncate(&title, 100));
    let content_snippet = format!(
        "Patent {}: {} ({}). Assignee: {}.{} {}",
        doc_type, title, publication_number, assignee,
        if !inventor.is_empty() { format!(" Inventor: {}.", inventor) } else { String::new() },
        truncate(&snippet, 300),
    );

    let metadata = serde_json::json!({
        "publication_number": publication_number,
        "assignee": assignee,
        "title": title,
        "date": date,
        "inventor": inventor,
        "is_grant": is_grant,
        "source_api": "google_patents",
    });

    Some(RawArticle {
        title: headline,
        url: format!("https://patents.google.com/patent/{}/en", publication_number),
        source_name: "Google Patents".to_string(),
        source_url: "https://patents.google.com".to_string(),
        published_at: date.map(|d| {
            // Convert YYYY-MM-DD format if needed (Google returns YYYY-MM-DD)
            d.to_string()
        }),
        content_snippet,
        sector: "tech".to_string(),
        feed_id: format!("patent_{}", publication_number),
        language: "en".to_string(),
        source_type: "financial".to_string(),
        financial_metadata: Some(serde_json::to_string(&metadata).unwrap_or_default()),
    })
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
    // Also decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&hellip;", "...")
        .trim()
        .to_string()
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
