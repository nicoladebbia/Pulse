//! Research lane: quant-finance papers → triage → full-text deep read → proposals
//! the what-if backtester can test.
//!
//! Runs as `--mode research`, and non-fatally on every hourly `daily` wake (both
//! when the day is already fetched and after a fresh fetch), so batches submitted
//! in one wake are collected in the next. arXiv itself is queried at most every
//! INGEST_EVERY_HOURS.

pub mod anthropic;
pub mod fulltext;
pub mod ingest;
pub mod ledger;
mod parity;
pub mod read;
pub mod system_card;
pub mod triage;

use std::path::Path;

/// Days looked back on a normal ingest. arXiv announces once per weekday, so 4
/// days covers a weekend plus one missed run.
const DEFAULT_LOOKBACK_DAYS: i64 = 4;
const INGEST_EVERY_HOURS: i64 = 6;

pub struct RunOptions {
    /// YYYY-MM-DD; papers submitted before this are ignored. None = DEFAULT_LOOKBACK_DAYS.
    /// Setting it also forces an ingest regardless of INGEST_EVERY_HOURS.
    pub since: Option<String>,
    /// Cap on papers ingested per run.
    pub limit: usize,
}

fn ingest_due(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT value <= datetime('now', ?1) FROM research_state WHERE key = 'last_ingest_at'",
        [format!("-{INGEST_EVERY_HOURS} hours")],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(true)
}

pub async fn run(db_path: &Path, opts: RunOptions) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    crate::db::run_migrations(&conn)?;
    let cap = ledger::cap();

    // 1. Ingest.
    if opts.since.is_some() || ingest_due(&conn) {
        let since = opts.since.clone().unwrap_or_else(|| {
            (chrono::Utc::now() - chrono::Duration::days(DEFAULT_LOOKBACK_DAYS))
                .format("%Y-%m-%d")
                .to_string()
        });
        let client = reqwest::Client::builder()
            .user_agent("Pulse/1.0 (personal research reader; mailto:nicolagiovannidebbia@gmail.com)")
            .build()?;
        let papers = ingest::fetch_since(&client, &since, opts.limit).await?;
        let inserted = ingest::upsert(&conn, &papers)?;
        conn.execute(
            "INSERT INTO research_state (key, value) VALUES ('last_ingest_at', datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
            [],
        )?;
        tracing::info!("research: {} papers since {} ({} new)", papers.len(), since, inserted);
    }

    let key = match anthropic::api_key() {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("research: {e}; triage and reads skipped");
            return Ok(());
        }
    };
    let card = system_card::render(&pulse_weights::StrategyParams::live(), &system_card::history_stats(&conn));

    // 2. Triage new papers. 3. Collect finished reads BEFORE submitting, so the
    // cap sees their real cost instead of the estimate. 4. Submit new reads.
    let (to_read, skipped) = triage::run(&conn, &key, &card, cap).await?;
    let collected = read::collect(&conn, &key).await?;
    let submitted = read::submit(&conn, &key, &card, cap).await?;
    tracing::info!(
        "research: triaged {} ({} to read, {} skipped), collected {}, submitted {}; spent today ${:.3} of ${:.2}",
        to_read + skipped,
        to_read,
        skipped,
        collected,
        submitted,
        ledger::spent_today(&conn),
        cap
    );
    Ok(())
}
