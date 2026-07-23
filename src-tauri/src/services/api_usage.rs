use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Pricing per 1M tokens (input/output) as of 2025
const PRICING: &[(&str, &str, f64, f64)] = &[
    // (provider, model_prefix, input_per_1m, output_per_1m)
    ("anthropic", "claude-haiku", 0.25, 1.25),
    ("anthropic", "claude-sonnet", 3.0, 15.0),
    ("anthropic", "claude-opus", 15.0, 75.0),
    ("groq", "llama-3.1-8b", 0.05, 0.08),
    ("groq", "llama-3.3-70b", 0.59, 0.79),
    ("voyage", "voyage-3-lite", 0.02, 0.0), // embeddings: input only
    ("tavily", "search", 0.0, 0.0),          // free tier
    // Financial data APIs (all free)
    ("fred", "economic", 0.0, 0.0),
    ("finnhub", "quote", 0.0, 0.0),
    ("fec", "campaign", 0.0, 0.0),
    ("eia", "energy", 0.0, 0.0),
    ("sec_edgar", "filing", 0.0, 0.0),
    ("usaspending", "contract", 0.0, 0.0),
    ("federal_register", "rule", 0.0, 0.0),
    ("alpaca", "trading", 0.0, 0.0),
];

/// Rate limits for all tracked external APIs.
/// (provider, calls_per_minute, calls_per_hour, calls_per_day, description)
/// Use -1 for "no limit" on any dimension.
pub const FINANCIAL_RATE_LIMITS: &[(&str, i64, i64, i64, &str)] = &[
    // Financial / government data APIs
    ("fred",              120,   -1,   -1,     "FRED (120/min)"),
    ("finnhub",            60,   -1,   -1,     "Finnhub (60/min)"),
    ("fec",                60, 1000,   -1,     "FEC (1000/hr)"),
    ("eia",                -1,   -1,   -1,     "EIA (no limit)"),
    ("sec_edgar",         600,   -1,   -1,     "SEC EDGAR (10/sec)"),
    ("usaspending",        -1,   -1,   -1,     "USASpending (no limit)"),
    ("federal_register",   -1,   -1,   -1,     "Federal Register (no limit)"),
    ("alpaca",            200,   -1,   -1,     "Alpaca (200/min)"),
    ("lda",                -1,   -1,   -1,     "Senate LDA (no limit)"),
    ("uspto",              -1,   -1,   -1,     "Google Patents (no limit)"),
    // News / content APIs
    ("google_news",        -1,   -1,   -1,     "Google News RSS"),
    ("hacker_news",        -1,   -1,   -1,     "Hacker News (Algolia)"),
    ("reddit",             -1,  600,   -1,     "Reddit (600/hr)"),
    ("arxiv",              -1,   -1,   -1,     "ArXiv"),
    ("biorxiv",            -1,   -1,   -1,     "bioRxiv"),
    ("rss_feeds",          -1,   -1,   -1,     "RSS Feeds"),
    ("wikipedia",          -1,   -1,   -1,     "Wikipedia"),
];

pub fn estimate_cost(provider: &str, model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
    for &(p, m, in_price, out_price) in PRICING {
        if provider == p && model.contains(m) {
            return (input_tokens as f64 * in_price + output_tokens as f64 * out_price) / 1_000_000.0;
        }
    }
    0.0
}

/// Log an API call's token usage.
pub fn log_usage(
    conn: &Connection,
    provider: &str,
    model: &str,
    endpoint: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<()> {
    let cost = estimate_cost(provider, model, input_tokens, output_tokens);
    conn.execute(
        "INSERT INTO api_usage (provider, model, endpoint, input_tokens, output_tokens, estimated_cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![provider, model, endpoint, input_tokens, output_tokens, cost],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub model: String,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost_usd: f64,
    pub call_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyCost {
    pub date: String,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    pub period: String,
    pub total_cost_usd: f64,
    pub today_cost_usd: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_calls: i64,
    pub by_provider: Vec<ProviderUsage>,
    pub daily: Vec<DailyCost>,
}

/// Get usage stats for a given number of days back.
pub fn get_usage(conn: &Connection, days: u32) -> Result<UsageStats> {
    let period = format!("last {} days", days);

    let mut stmt = conn.prepare(
        "SELECT provider, COALESCE(model, 'unknown'),
                SUM(input_tokens), SUM(output_tokens),
                SUM(estimated_cost_usd), COUNT(*)
         FROM api_usage
         WHERE created_at >= datetime('now', ?1)
         GROUP BY provider, model
         ORDER BY SUM(estimated_cost_usd) DESC",
    )?;

    let modifier = format!("-{} days", days);
    let rows: Vec<ProviderUsage> = stmt
        .query_map(params![modifier], |row| {
            Ok(ProviderUsage {
                provider: row.get(0)?,
                model: row.get(1)?,
                total_input_tokens: row.get(2)?,
                total_output_tokens: row.get(3)?,
                total_cost_usd: row.get(4)?,
                call_count: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let total_cost: f64 = rows.iter().map(|r| r.total_cost_usd).sum();
    let total_input: i64 = rows.iter().map(|r| r.total_input_tokens).sum();
    let total_output: i64 = rows.iter().map(|r| r.total_output_tokens).sum();
    let total_calls: i64 = rows.iter().map(|r| r.call_count).sum();

    // Today's cost
    let today_cost: f64 = conn.query_row(
        "SELECT COALESCE(SUM(estimated_cost_usd), 0.0) FROM api_usage WHERE DATE(created_at) = DATE('now')",
        [],
        |row| row.get(0),
    ).unwrap_or(0.0);

    // Daily breakdown for sparkline
    let mut daily_stmt = conn.prepare(
        "SELECT DATE(created_at) as day, SUM(estimated_cost_usd) as cost
         FROM api_usage
         WHERE created_at >= datetime('now', ?1)
         GROUP BY day
         ORDER BY day ASC",
    )?;

    let daily: Vec<DailyCost> = daily_stmt
        .query_map(params![modifier], |row| {
            Ok(DailyCost {
                date: row.get(0)?,
                cost: row.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(UsageStats {
        period,
        total_cost_usd: total_cost,
        today_cost_usd: today_cost,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_calls,
        by_provider: rows,
        daily,
    })
}

// ---------------------------------------------------------------------------
// Financial API quota tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinancialApiQuota {
    pub provider: String,
    pub description: String,
    pub calls_today: i64,
    pub calls_this_hour: i64,
    pub limit_per_minute: i64,   // -1 = no limit
    pub limit_per_hour: i64,     // -1 = no limit
    pub limit_per_day: i64,      // -1 = no limit
    pub last_call_at: Option<String>,
}

/// Get usage quotas for all financial APIs.
pub fn get_financial_quotas(conn: &Connection) -> Result<Vec<FinancialApiQuota>> {
    let mut quotas = Vec::new();

    for &(provider, rpm, rph, daily_limit, desc) in FINANCIAL_RATE_LIMITS {
        let calls_today: i64 = conn.query_row(
            "SELECT COUNT(*) FROM api_usage WHERE provider = ?1 AND DATE(created_at) = DATE('now')",
            params![provider],
            |row| row.get(0),
        ).unwrap_or(0);

        let calls_this_hour: i64 = conn.query_row(
            "SELECT COUNT(*) FROM api_usage WHERE provider = ?1 AND created_at >= datetime('now', '-1 hour')",
            params![provider],
            |row| row.get(0),
        ).unwrap_or(0);

        let last_call: Option<String> = conn.query_row(
            "SELECT MAX(created_at) FROM api_usage WHERE provider = ?1",
            params![provider],
            |row| row.get(0),
        ).unwrap_or(None);

        quotas.push(FinancialApiQuota {
            provider: provider.to_string(),
            description: desc.to_string(),
            calls_today,
            calls_this_hour,
            limit_per_minute: rpm,
            limit_per_hour: rph,
            limit_per_day: daily_limit,
            last_call_at: last_call,
        });
    }

    Ok(quotas)
}
