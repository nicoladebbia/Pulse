use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const TAVILY_MONTHLY_LIMIT: i64 = 1000;
const TAVILY_WARN_THRESHOLD: i64 = 950; // warn at 95% usage

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TavilyQuota {
    pub used: i64,
    pub limit: i64,
    pub remaining: i64,
    pub warning: Option<String>,
}

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

/// Count how many Tavily calls have been made this calendar month.
pub fn get_monthly_usage(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM api_usage
         WHERE provider = 'tavily'
         AND created_at >= date('now', 'start of month')",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// Get current Tavily quota status.
pub fn get_quota(conn: &Connection) -> TavilyQuota {
    let used = get_monthly_usage(conn);
    let remaining = (TAVILY_MONTHLY_LIMIT - used).max(0);
    let warning = if remaining == 0 {
        Some("Tavily monthly limit reached (1,000/1,000). Web search disabled until next month.".to_string())
    } else if used >= TAVILY_WARN_THRESHOLD {
        Some(format!("Tavily credits low: {}/{} used this month. {} remaining.", used, TAVILY_MONTHLY_LIMIT, remaining))
    } else {
        None
    };

    TavilyQuota {
        used,
        limit: TAVILY_MONTHLY_LIMIT,
        remaining,
        warning,
    }
}

/// Check if we can make a Tavily call. Returns Ok(used_count) or Err if limit reached.
pub fn check_quota(conn: &Connection) -> Result<i64> {
    let used = get_monthly_usage(conn);
    if used >= TAVILY_MONTHLY_LIMIT {
        anyhow::bail!(
            "Tavily monthly limit reached ({}/{}). Web search disabled until next month.",
            used, TAVILY_MONTHLY_LIMIT
        );
    }
    if used >= TAVILY_WARN_THRESHOLD {
        tracing::warn!("Tavily credits running low: {}/{} used this month", used, TAVILY_MONTHLY_LIMIT);
    }
    Ok(used)
}

/// Log a successful Tavily call.
pub fn log_call(conn: &Connection) {
    super::api_usage::log_usage(conn, "tavily", "search", "web_search", 0, 0).ok();
}

/// Search the web using Tavily Search API (does NOT access DB — call check_quota before and log_call after).
pub async fn search(query: &str, count: usize) -> Result<Vec<WebResult>> {
    let api_key = std::env::var("TAVILY_API_KEY")
        .context("TAVILY_API_KEY not set — add it to .env for web search")?;

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "max_results": count,
        "search_depth": "basic",
    });

    let resp = client
        .post(TAVILY_SEARCH_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Tavily Search request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Tavily Search returned {}: {}", status, text);
    }

    let tavily_resp: TavilyResponse = resp.json().await.context("Failed to parse Tavily response")?;

    let results = tavily_resp
        .results
        .into_iter()
        .take(count)
        .map(|r| WebResult {
            title: r.title,
            url: r.url,
            description: r.content,
        })
        .collect();

    Ok(results)
}

/// Format web results into a context string for LLM prompts.
pub fn format_web_context(results: &[WebResult]) -> String {
    if results.is_empty() {
        return "No web results found.".to_string();
    }

    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            format!("[Web {}] {} ({})\n  {}", i + 1, r.title, r.url, r.description)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
