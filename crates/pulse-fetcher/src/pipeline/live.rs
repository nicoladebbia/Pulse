use super::*;

use std::collections::BTreeSet;

/// Companies tracked for new Form 4s, one SEC request each. Each live run
/// checks a quarter of them (rotating by the 5-minute slot), so every company
/// is checked every 20 minutes. Checking all 250 every 5 minutes made ~12,000
/// SEC requests a day and drew 429s; EDGAR's latest-filings feed, read every
/// run, still catches any new Form 4 within 5 minutes.
const LIVE_FORM4_LIMIT: usize = 250;
const LIVE_FORM4_SHARES: usize = 4;

/// What one live run found.
#[derive(Debug, Default)]
pub(crate) struct LiveReport {
    pub form4_new: usize,
    pub form4_enriched: usize,
    pub filings_new: usize,
    pub full_sources: bool,
    pub signals: usize,
    /// Tickers that became buy-grade (the auto-trade entry query) in this run.
    pub new_candidates: Vec<String>,
}

/// Intraday signal refresh, no AI calls (`--mode live-signals`).
///
/// The daily pipeline computes signals twice a day, so a Form 4 or 8-K filed
/// at 11:00 waited until 21:00 to count. This reruns the filing-driven part of
/// that pipeline: targeted Form 4s, the EDGAR Form 4 / 8-K feeds (every run),
/// the other government and financial sources (once an hour), entity
/// extraction from their metadata, then signals and cross-signal scores.
/// News momentum still needs the LLM and comes from the daily run; this run
/// recomputes scores with the news already stored.
///
/// `full_sources` asks for the hourly tier (USASpending, Federal Register,
/// FRED, FEC, EIA, lobbying, patents, full EDGAR incl. 13F).
pub(crate) async fn run_live_signals(db_path: &Path, full_sources: bool) -> anyhow::Result<LiveReport> {
    {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        crate::db::run_migrations(&conn)?;
    }
    let mut report = LiveReport { full_sources, ..Default::default() };
    sources::API_CALLS.reset();

    let before = {
        let conn = rusqlite::Connection::open(db_path)?;
        buy_grade_tickers(&conn)
    };

    let slot = chrono::Local::now().timestamp() as usize / 300 % LIVE_FORM4_SHARES;
    match fetch_targeted_form4(db_path, LIVE_FORM4_LIMIT, Some((slot, LIVE_FORM4_SHARES))).await {
        Ok(n) => report.form4_new = n,
        Err(e) => tracing::warn!("Live: targeted Form 4 failed: {}", e),
    }
    let sec_calls = || sources::API_CALLS.sec_edgar.load(std::sync::atomic::Ordering::Relaxed);
    let after_form4 = sec_calls();
    let sec_ok = || !sources::SEC_THROTTLED.load(std::sync::atomic::Ordering::Relaxed);
    if sec_ok() {
        match enrich_form4_stories(db_path).await {
            Ok(n) => report.form4_enriched = n,
            Err(e) => tracing::warn!("Live: Form 4 enrichment failed: {}", e),
        }
    }

    let after_enrich = sec_calls();
    let raw = if !sec_ok() {
        tracing::warn!("Live: SEC is throttling, skipping the filing feeds this run");
        Vec::new()
    } else if full_sources {
        let (articles, failed) = sources::collect_financial_sources().await;
        if !failed.is_empty() {
            tracing::warn!("Live: {} source(s) failed: {}", failed.len(), failed.join(", "));
        }
        articles
    } else {
        sources::edgar::fetch_live().await.unwrap_or_else(|e| {
            tracing::warn!("Live: EDGAR fetch failed: {}", e);
            Vec::new()
        })
    };
    tracing::info!(
        "Live: SEC requests: {} company checks, {} enrichment, {} filing feeds",
        after_form4, after_enrich - after_form4, sec_calls() - after_enrich
    );
    let fresh = dedup_financial_articles(db_path, raw);
    if !fresh.is_empty() {
        let stories = financial_stories_from(fresh);
        // Financial stories belong to today's briefing; before the morning
        // briefing exists they wait for the next run.
        match write_financial_stories(db_path, &stories) {
            Ok(n) => {
                report.filings_new = n;
                record_financial_dedup(db_path, &stories);
            }
            Err(e) => tracing::info!("Live: filings not stored yet: {}", e),
        }
    }

    if let Ok(conn) = rusqlite::Connection::open(db_path) {
        for (provider, calls) in sources::API_CALLS.snapshot() {
            crate::db::log_fetch_calls(&conn, provider, "live_signals", calls);
        }
    }

    if let Err(e) = extract_entities_from_financial_metadata(db_path) {
        tracing::warn!("Live: entity extraction failed: {}", e);
    }
    if let Err(e) = populate_tickers(db_path) {
        tracing::warn!("Live: ticker mapping failed: {}", e);
    }

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        recompute_signals_pipeline(&conn, &today, 90)?;
    }
    report.signals = compute_cross_signals(db_path, &today)?;

    let after = {
        let conn = rusqlite::Connection::open(db_path)?;
        buy_grade_tickers(&conn)
    };
    report.new_candidates = after.difference(&before).cloned().collect();
    Ok(report)
}

/// Tickers the auto-trade entry query would consider right now: the newest
/// row per ticker converging above 0.30 from the last day, not already held.
/// Kept in step with the `latest` CTE in `auto_trade_on_convergence`.
fn buy_grade_tickers(conn: &rusqlite::Connection) -> BTreeSet<String> {
    conn.prepare(
        "WITH latest AS (
             SELECT ticker, ROW_NUMBER() OVER (PARTITION BY ticker ORDER BY computed_at DESC, compound_score DESC) AS rn
             FROM cross_signals
             WHERE convergence_detected = 1 AND ticker IS NOT NULL
               AND compound_score > 0.3
               AND computed_at >= date('now', '-1 day')
         )
         SELECT ticker FROM latest
         WHERE rn = 1 AND ticker NOT IN (SELECT ticker FROM paper_trades WHERE status = 'open')",
    )
    .and_then(|mut stmt| {
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buy_grade_skips_weak_old_held_and_non_converging() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO entities (id, name, name_normalized, entity_type, first_seen, last_seen) VALUES
                 (1,'a','a','company','2026-01-01','2026-01-01'), (2,'b','b','company','2026-01-01','2026-01-01'),
                 (3,'c','c','company','2026-01-01','2026-01-01'), (4,'d','d','company','2026-01-01','2026-01-01'),
                 (5,'e','e','company','2026-01-01','2026-01-01');
             INSERT INTO cross_signals (entity_id, ticker, compound_score, convergence_detected, computed_at) VALUES
                 (1, 'AAA', 0.40, 1, date('now')),
                 (2, 'BBB', 0.25, 1, date('now')),
                 (3, 'CCC', 0.40, 0, date('now')),
                 (4, 'DDD', 0.40, 1, date('now', '-5 days')),
                 (5, 'EEE', 0.40, 1, date('now'));
             INSERT INTO paper_trades (ticker, direction, entry_price, entry_date, position_size, confidence, signal_profile, status)
                 VALUES ('EEE', 'long', 10, date('now'), 100, 0.4, '{}', 'open');",
        )
        .unwrap();
        assert_eq!(buy_grade_tickers(&conn).into_iter().collect::<Vec<_>>(), vec!["AAA".to_string()]);
    }

    #[test]
    fn fetch_calls_log_one_row_with_the_count() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        crate::db::log_fetch_calls(&conn, "sec_edgar", "live_signals", 312);
        crate::db::log_fetch_calls(&conn, "fred", "live_signals", 0);
        let (rows, calls): (i64, i64) = conn
            .query_row("SELECT COUNT(*), SUM(calls) FROM api_usage WHERE provider IN ('sec_edgar', 'fred')", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((rows, calls), (1, 312));
    }
}
