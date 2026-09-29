//! Research spend cap, enforced BEFORE every call.
//!
//! The pipeline's daily cap (`pipeline::cost::check_daily_cost_cap`) is checked
//! only when a run starts, so one run can overshoot it. The research lane is the
//! one place a single run can issue dozens of long-context Opus calls, so it
//! reserves each call's estimated cost against its own cap first.
//!
//! Today's research spend = logged `api_usage` rows with a `research_` endpoint,
//! plus the estimate of every batch read submitted today and not yet collected
//! (its real cost is only known — and logged — when the batch ends).

use anyhow::bail;
use rusqlite::Connection;

pub const DEFAULT_RESEARCH_DAILY_CAP_USD: f64 = 1.50;

pub fn cap() -> f64 {
    std::env::var("PULSE_RESEARCH_DAILY_CAP")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(DEFAULT_RESEARCH_DAILY_CAP_USD)
}

pub fn spent_today(conn: &Connection) -> f64 {
    let logged: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimated_cost_usd), 0.0) FROM api_usage
             WHERE endpoint LIKE 'research_%'
               AND created_at >= date('now') AND created_at < date('now', '+1 day')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0.0);
    let in_flight: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(read_cost_estimate), 0.0) FROM research_papers
             WHERE status = 'reading'
               AND read_submitted_at >= date('now') AND read_submitted_at < date('now', '+1 day')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0.0);
    logged + in_flight
}

/// Ok if `estimate` fits under the cap given today's spend; the caller must then
/// make the spend visible (log it, or mark the paper `reading` with its estimate)
/// before reserving again.
pub fn reserve(conn: &Connection, estimate: f64, cap: f64) -> anyhow::Result<()> {
    let spent = spent_today(conn);
    if spent + estimate > cap {
        bail!(
            "research spend cap: ${spent:.3} spent today + ${estimate:.3} for this call > ${cap:.2} \
             (PULSE_RESEARCH_DAILY_CAP)"
        );
    }
    Ok(())
}

/// Cost of a call at list price, with the batch discount when it ran as a batch.
pub fn cost(model: &str, usage: super::anthropic::Usage, batch: bool) -> f64 {
    let list = pulse_pricing::estimate_cost("anthropic", model, usage.input_tokens, usage.output_tokens);
    if batch {
        list * super::anthropic::BATCH_DISCOUNT
    } else {
        list
    }
}

/// Write the call to `api_usage` with its real (possibly discounted) cost, so both
/// this cap and the pipeline's daily cap see it.
pub fn log(conn: &Connection, model: &str, endpoint: &str, usage: super::anthropic::Usage, batch: bool) -> f64 {
    let c = cost(model, usage, batch);
    conn.execute(
        "INSERT INTO api_usage (provider, model, endpoint, input_tokens, output_tokens, estimated_cost_usd)
         VALUES ('anthropic', ?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![model, endpoint, usage.input_tokens, usage.output_tokens, c],
    )
    .ok();
    c
}

#[cfg(test)]
mod tests {
    use super::super::anthropic::Usage;
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&c).unwrap();
        c
    }

    #[test]
    fn refuses_the_call_that_would_cross_the_cap() {
        let c = db();
        let u = Usage { input_tokens: 36_000, output_tokens: 10_000 }; // a median Opus read
        let one = cost("claude-opus-5", u, true);
        assert!((one - 0.215).abs() < 1e-9, "36k in + 10k out on Opus 5, batch = $0.215, got {one}");

        // Two reads fit under $0.50, the third does not.
        for _ in 0..2 {
            reserve(&c, one, 0.50).unwrap();
            log(&c, "claude-opus-5", "research_read", u, true);
        }
        let err = reserve(&c, one, 0.50).unwrap_err().to_string();
        assert!(err.contains("research spend cap"), "{err}");
    }

    #[test]
    fn in_flight_batches_count_against_the_cap() {
        let c = db();
        c.execute(
            "INSERT INTO research_papers (arxiv_id, title, published_at, status, read_cost_estimate, read_submitted_at)
             VALUES ('1', 't', '2026-09-24', 'reading', 0.40, datetime('now'))",
            [],
        )
        .unwrap();
        assert!((spent_today(&c) - 0.40).abs() < 1e-9);
        assert!(reserve(&c, 0.20, 0.50).is_err(), "0.40 in flight + 0.20 > 0.50");
        // Collected (no longer 'reading'): only the logged cost counts.
        c.execute("UPDATE research_papers SET status = 'read'", []).unwrap();
        assert!(reserve(&c, 0.20, 0.50).is_ok());
    }

    #[test]
    fn other_endpoints_do_not_eat_the_research_cap() {
        let c = db();
        c.execute(
            "INSERT INTO api_usage (provider, model, endpoint, input_tokens, output_tokens, estimated_cost_usd)
             VALUES ('anthropic', 'claude-haiku-4-5', 'analyze', 1, 1, 5.0)",
            [],
        )
        .unwrap();
        assert!(spent_today(&c) < 1e-9);
    }
}
