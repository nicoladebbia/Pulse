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
];

fn estimate_cost(provider: &str, model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
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
