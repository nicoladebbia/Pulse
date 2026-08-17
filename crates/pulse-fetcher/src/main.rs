mod article_text;
mod pipeline;
mod sources;
mod claude;
mod contextual;
pub(crate) mod db;
mod dedup;
mod embeddings;
pub(crate) mod market_prices;
pub(crate) mod calibration;
pub(crate) mod position_management;
pub(crate) mod position_sizing;
pub(crate) mod edge_report;

use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Holds an exclusive `flock` on a lockfile for the lifetime of the process. The kernel
/// releases it on ANY exit — clean, panic, or SIGKILL — so unlike a pidfile there is no
/// stale-lock state to repair.
struct SingleInstanceLock(#[allow(dead_code)] std::fs::File);

/// Take the whole-machine "a fetch is running" lock, or return None if another fetcher
/// already holds it.
///
/// Two fetchers on the same DB write the same SQLite file AND the same progress file,
/// with `started_at` ping-ponging between runs. Nothing prevented that before: the app's
/// FETCHING flag only guards its own spawns, and the news-based skip check exits early
/// only when the day ALREADY has news — precisely not the case on a failing day, which
/// is when scheduled slots and a manual "Refresh Briefing" are most likely to overlap.
fn acquire_single_instance_lock(db_path: &std::path::Path) -> Option<SingleInstanceLock> {
    use std::os::unix::io::AsRawFd;
    let path = db_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("fetcher.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    // LOCK_NB: never queue behind the running fetch — a slot that can't run should exit
    // immediately and let the next one try.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if locked {
        Some(SingleInstanceLock(file))
    } else {
        None
    }
}

#[derive(Parser, Debug)]
#[command(name = "pulse-fetcher", about = "Daily news fetch pipeline for Pulse")]
struct Args {
    /// Fetch mode: 'daily' for scheduled fetch, 'manual' for on-demand
    #[arg(long, default_value = "daily")]
    mode: String,

    /// Path to the SQLite database
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Force re-fetch even if today's briefing already exists
    #[arg(long, default_value_t = false)]
    force: bool,

    /// backfill-signals mode: start date (YYYY-MM-DD), inclusive
    #[arg(long)]
    start: Option<String>,

    /// backfill-signals mode: end date (YYYY-MM-DD), inclusive
    #[arg(long)]
    end: Option<String>,

    /// backfill-signals mode: lookback window in days for the gov/political/momentum
    /// dimensions (see pipeline::recompute_signals_pipeline's window_days param)
    #[arg(long, default_value_t = 30)]
    window_days: i64,

    /// backfill-signals mode: explicit acknowledgment that --db-path is NOT the live
    /// production DB. Required to run against any path that resolves to the real
    /// Application Support location; the mode refuses otherwise.
    #[arg(long, default_value_t = false)]
    i_know_this_is_a_copy: bool,

    /// backfill-prices mode: comma-separated ticker list to backfill candle history for.
    #[arg(long)]
    tickers: Option<String>,

    /// backfill-prices mode: how many days back to fetch candles for (Alpaca caps at
    /// ~60 bars per call).
    #[arg(long, default_value_t = 60)]
    days_back: i64,

    /// backfill-embeddings mode: cap on stories selected per run. The wall-clock budget
    /// (--max-minutes) is the real limiter; this is a secondary guard on query size.
    #[arg(long, default_value_t = 20000)]
    limit: usize,

    /// backfill-embeddings mode: wall-clock budget in minutes. The Voyage free tier does
    /// ~30 stories/min, so the default 120min clears ~3.6k stories per run — enough to
    /// drain a 15k backlog over a few nights without ever overlapping the daily fetch.
    #[arg(long, default_value_t = 120)]
    max_minutes: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("pulse_fetcher=info".parse()?))
        .init();

    let args = Args::parse();

    // Auto-load .env from project directory
    let env_path = dirs::home_dir()
        .unwrap_or_default()
        .join("Projects/Pulse/.env");
    if env_path.exists() {
        dotenvy::from_path(&env_path).ok();
        tracing::info!("Loaded API keys from {}", env_path.display());
    }

    let db_path = args.db_path.unwrap_or_else(|| {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.pulse.app")
            .join("pulse.db")
    });

    tracing::info!("Pulse fetcher starting in {} mode", args.mode);
    tracing::info!("Database: {}", db_path.display());

    // Must run before any pipeline work so a forced-block test exercises the same code
    // path a real 403 would, from the first call onward.
    crate::claude::client::apply_forced_block_from_env();

    match args.mode.as_str() {
        "notify-test" => {
            // Self-test for the failure-alert path. Fires the FAILED and DEGRADED
            // banners exactly as a real failure would, so delivery can be verified
            // from the launchd context without waiting for an actual outage.
            // (Proved the fire-and-exit timing bug + the .status() fix on 2026-06-22.)
            tracing::info!("Firing test failure + degraded notifications (blocking delivery)...");
            notify_failure("TEST: this is what a failed daily run looks like.");
            crate::pipeline::notify_degraded_test("TEST: this is what a degraded run looks like.");
            tracing::info!("Test notifications fired");
        }
        "backfill-embeddings" => {
            // Hour gate FIRST — before the lock, before any DB read. launchd wakes this
            // mode EVERY hour because an hourly interval is the only calendar shape that
            // fires at all (see BACKFILL_HOURS); 22 of those 24 wakeups must cost nothing
            // and must never touch the lock the daily fetch may be holding.
            let hour = {
                use chrono::Timelike;
                chrono::Local::now().hour()
            };
            if !args.force && !is_backfill_hour(hour) {
                tracing::debug!("Hour {} is not a backfill slot {:?} — exiting.", hour, BACKFILL_HOURS);
                return Ok(());
            }
            // Shares the daily fetch's lock: this mode is now SCHEDULED, so it must never
            // run alongside a daily fetch that is writing the same tables and racing the
            // same Voyage rate limit. A collision costs nothing — whichever loses exits.
            let _instance_lock = match acquire_single_instance_lock(&db_path) {
                Some(lock) => lock,
                None => {
                    tracing::info!(
                        "A fetch is already running — skipping backfill, the next slot will retry."
                    );
                    return Ok(());
                }
            };
            // Yield to the daily briefing. The backfill holds the lock for up to
            // --max-minutes, and the daily slots are hourly, so a backfill started before
            // the day's news is in could block the exact slot that catches a clean Groq
            // window. Once today's news EXISTS every daily slot skips in <1s anyway, so
            // that is the only window where holding the lock is free.
            if !args.force && news_today(&db_path) == 0 {
                tracing::info!(
                    "Today's briefing has no news yet — deferring backfill so it cannot \
                     block a daily slot. Next backfill slot will retry (--force to override)."
                );
                return Ok(());
            }
            tracing::info!("Backfilling embeddings for stories without them...");
            backfill_embeddings(&db_path, args.limit, args.max_minutes).await?;
        }
        "extract-entities" => {
            tracing::info!("Extracting entities from all stories...");
            extract_entities(&db_path).await?;
        }
        "daily" => {
            // Single-instance guard FIRST — before the skip check, the preflight probe and
            // any DB read. A second fetcher must cost nothing and must not touch the
            // progress file the running one owns.
            let _instance_lock = match acquire_single_instance_lock(&db_path) {
                Some(lock) => lock,
                None => {
                    tracing::info!(
                        "Another pulse-fetcher is already running — exiting (its progress file is authoritative)."
                    );
                    return Ok(());
                }
            };

            // Idempotent skip: a day is "done" only if today's daily briefing already
            // has NEWS stories. This is deliberately NOT `status='complete'` — a run
            // blocked at the Groq summarize step still writes financial stories (they
            // skip summarization) and marks the briefing complete with ZERO news, which
            // is exactly the degraded state we want a LATER slot to retry, not skip.
            // Success == news present (source_type='news'). See groq-vpn-block-plan.md.
            if !args.force {
                let n = news_today(&db_path);
                if n > 0 {
                    tracing::info!(
                        "Already fetched {} news stories for {} — skipping (use --force to override)",
                        n,
                        chrono::Local::now().format("%Y-%m-%d")
                    );
                    return Ok(());
                }
            }

            // Preflight reachability probe. If the source IP is on Groq's blocklist (VPN
            // exit node / school network) every Groq call this run will 403.
            //
            // This used to exit 0 silently and wait for a later slot. That was the right
            // call when Groq was the only provider — but it is why 10 of 45 days had NO
            // briefing at all (2026-07-03..12, 2026-08-08..13): the block persisted across
            // every slot, so "a later slot will retry" quietly became "no briefing for a
            // week". Anthropic answers 200 on exactly the network where Groq answers 403.
            //
            // So: latch the block and RUN ANYWAY on Anthropic. A briefing that costs more
            // beats no briefing. `PULSE_NO_ANTHROPIC_FALLBACK=1` restores the old
            // skip-and-wait behaviour if a blocked day should stay cheap instead.
            if !crate::claude::client::groq_reachable().await {
                let fallback_disabled = std::env::var("PULSE_NO_ANTHROPIC_FALLBACK")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                if fallback_disabled {
                    tracing::info!("Groq unreachable and Anthropic fallback disabled — skipping this slot silently.");
                    // Staleness alarm: skips can repeat for DAYS with zero user-visible
                    // signal (paid 2026-08-07→09). If the newest briefing is >36h old,
                    // fire ONE notification per calendar day (stamp file dedupes slots).
                    notify_if_stale(&db_path);
                    return Ok(());
                }
                crate::claude::client::mark_groq_blocked("preflight probe returned blocked");
                tracing::info!("Groq unreachable — running this briefing on Anthropic instead of skipping the slot.");
            }

            // Notify-on-failure safety net: a scheduled daily run that errors AFTER a
            // passing preflight (so the network was reachable) must not fail silently.
            if let Err(e) = pipeline::run(&db_path).await {
                // Durable failure record for the always-visible progress bar: a clean
                // bail (cost cap, mid-run block) can happen before any ProgressWriter
                // stage write, so main.rs stamps the terminal "failed" state here. This
                // is what makes "run crashed 2h ago" queryable by the app.
                pipeline::write_failed_state(
                    &pipeline::progress_file_path(&db_path),
                    &format!("{}", e),
                );
                notify_failure(&format!("Daily run aborted: {}", e));
                return Err(e);
            }

            // Also run freedoms pipeline after daily
            tracing::info!("Running Four Freedoms pipeline...");
            if let Err(e) = pipeline::run_freedoms(&db_path).await {
                tracing::warn!("Freedoms pipeline failed (non-fatal): {}", e);
            }

            // Last thing on a SUCCESSFUL run: a run can finish clean and still
            // have computed every score without one of its inputs. Checked here
            // rather than in the failure path precisely because the failure path
            // is not where this hides.
            notify_stale_signal_sources(&db_path);
        }
        "freedoms" => {
            tracing::info!("Running Four Freedoms pipeline...");
            pipeline::run_freedoms(&db_path).await?;
        }
        "fetch-form4" => {
            tracing::info!("Fetching targeted Form 4 filings for tracked companies...");
            match pipeline::run_targeted_form4(&db_path).await {
                Ok(n) => tracing::info!("Targeted Form 4 complete: {} new filings inserted", n),
                Err(e) => tracing::error!("Targeted Form 4 fetch failed: {}", e),
            }
        }
        "backfill-tickers" => {
            // Apply the CIK-mapping fix to ALL unmapped entities in one pass
            // (the daily pipeline caps at 200/run). Expands the tradeable/watchlist universe.
            tracing::info!("Backfilling ticker mappings for all unmapped entities...");
            match pipeline::backfill_tickers(&db_path) {
                Ok(n) => tracing::info!("Backfill complete: {} new ticker mappings", n),
                Err(e) => tracing::error!("Ticker backfill failed: {}", e),
            }
        }
        "recanonicalize" => {
            // Rebuild the entity-canonical graph from scratch under the fixed match logic,
            // then re-run ticker/signal/cross-signal computation. Un-merges groups the old
            // substring match over-merged (e.g. 129 unrelated companies -> canonical "Arm").
            // Pure DB work, no Groq. Run against a copy first.
            tracing::info!("Rebuilding entity-canonical graph (recanonicalize)...");
            match pipeline::run_recanonicalize(&db_path) {
                Ok(n) => tracing::info!("Recanonicalize complete: {} entities regrouped", n),
                Err(e) => tracing::error!("Recanonicalize failed: {}", e),
            }
        }
        "manage-positions" => {
            // Run ONLY the exit-evaluation phase. Honors EXIT_DRY_RUN (default true).
            tracing::info!("Running position management (exit evaluation) only...");
            match pipeline::run_position_management(&db_path).await {
                Ok(n) => tracing::info!("Position management complete: {} action(s)", n),
                Err(e) => tracing::error!("Position management failed: {}", e),
            }
        }
        "auto-trade" => {
            // Run ONLY the auto-buy phase (Phase 13.5) against the live PAPER account.
            // Same gate (AUTO_TRADE_ENABLED), same candidate query, same paper endpoint
            // as the daily pipeline — a no-op when disarmed. Manual buy trigger for when
            // the daily run skipped Phase 13.5 at the already-fetched guard.
            tracing::info!("Running auto-trade (buy path) only...");
            match pipeline::run_auto_trade(&db_path).await {
                Ok(n) => tracing::info!("Auto-trade complete: {} order(s) placed", n),
                Err(e) => tracing::error!("Auto-trade failed: {}", e),
            }
        }
        "refresh-prices" => {
            // Run ONLY the market-price fetch (Phase 12.5) in isolation: quotes
            // for tickers missing today's row (open positions first) plus the
            // 35-day candle backfill for fresh tickers. Read-only toward
            // trading — never places orders.
            tracing::info!("Running market-price refresh only...");
            match market_prices::fetch_prices(&db_path).await {
                Ok(n) => tracing::info!("Price refresh complete: {} quotes stored", n),
                Err(e) => tracing::error!("Price refresh failed: {}", e),
            }
        }
        "calibrate" => {
            // Run ONLY the calibration phase (Phase 14) in isolation. Calibration
            // is MEASURE-ONLY: it refreshes displayed P&L + computes Brier/hit-rate
            // and NEVER places orders or closes trades. This mode exists to prove
            // that: run it with an open position and confirm zero Alpaca sells.
            tracing::info!("Running calibration (measure-only) in isolation...");
            match calibration::run_calibration(&db_path).await {
                Ok(r) => tracing::info!(
                    "Calibration complete: {} positions measured, {} Brier scores, {} resolved",
                    r.positions_evaluated, r.brier_scores_updated, r.signal_analysis.total_resolved
                ),
                Err(e) => tracing::error!("Calibration failed: {}", e),
            }
        }
        "backfill-signals" => {
            // Historical signal recompute for the corrected-backtest exercise
            // (calibration-backtest-universe plan, Phase 2). Refuses to run against
            // the live production DB path unless --i-know-this-is-a-copy is passed —
            // this wipes and rewrites cross_signals rows in the given date range.
            let start = args.start.as_deref().unwrap_or_else(|| {
                tracing::error!("backfill-signals requires --start YYYY-MM-DD");
                std::process::exit(1);
            });
            let end = args.end.as_deref().unwrap_or_else(|| {
                tracing::error!("backfill-signals requires --end YYYY-MM-DD");
                std::process::exit(1);
            });
            match pipeline::run_backfill_signals(&db_path, start, end, args.window_days, args.i_know_this_is_a_copy) {
                Ok(n) => tracing::info!("Backfill-signals complete: {} days processed", n),
                Err(e) => {
                    tracing::error!("Backfill-signals failed: {}", e);
                    return Err(e);
                }
            }
        }
        "backfill-prices" => {
            // Ad-hoc candle backfill for specific tickers, bypassing the daily
            // pipeline's 200/30-per-day caps (see market_prices::backfill_candles_for_tickers).
            let tickers: Vec<String> = args.tickers.as_deref().unwrap_or_else(|| {
                tracing::error!("backfill-prices requires --tickers TICK1,TICK2,...");
                std::process::exit(1);
            }).split(',').map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()).collect();
            match market_prices::backfill_candles_for_tickers(&db_path, &tickers, args.days_back).await {
                Ok(n) => tracing::info!("Backfill-prices complete: {} candles stored across {} tickers", n, tickers.len()),
                Err(e) => {
                    tracing::error!("Backfill-prices failed: {}", e);
                    return Err(e);
                }
            }
        }
        "edge-report" => {
            // Step 6: re-measure the paper-trading edge on REAL fills placed after
            // the 2026-07-15 ticker fix + arming. Reads ONLY paper_trades, applies an
            // N-gate (no verdict below N=20), reports both % and $ expectancy, and
            // refuses cross-condition comparison. Read-only — never trades.
            match edge_report::run_edge_report(&db_path) {
                Ok(_) => {}
                Err(e) => tracing::error!("Edge report failed: {}", e),
            }
        }
        other => {
            tracing::info!("Running in {} mode", other);
            pipeline::run(&db_path).await?;
        }
    }

    tracing::info!("Pulse fetch complete");
    Ok(())
}

/// Fire a macOS notification when a scheduled fetch fails or produces a degraded
/// briefing. This is the safety net for silent failures: the June 2026 incident
/// (NordVPN routing Groq through a blocked VPN exit IP -> every summary 403'd ->
/// empty briefing aborted) ran for ~2.5 days unnoticed because nothing alerted.
///
/// Mirrors the existing success notification in pipeline::send_notification, which
/// already delivers reliably from the launchd context. This is DETECTION, not
/// prevention: it tells Nicola the morning after instead of days later. True
/// prevention is NordVPN split-tunnel (free, GUI) or a provider fallback (costs $).
fn notify_failure(summary: &str) {
    // Must use .status() (blocks until osascript finishes), NOT .spawn(): this is
    // called right before the process exits with Err, and a fire-and-forget spawn
    // gets killed before notificationd delivers the banner. Measured 2026-06-22:
    // fire-and-exit dropped the banner silently; blocking delivers it.
    // best-effort; never let a notification failure mask the real error.
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display notification "{}" with title "Pulse fetch FAILED" sound name "Basso""#,
            summary.replace('"', "'").replace('\\', "")
        ))
        .status();
}

/// Preflight silent-skip staleness alarm. Skipping one blocked slot is expected;
/// skipping every slot for days is an outage the user must hear about. If the
/// newest briefing (any type) is older than 36h when a skip happens, notify —
/// at most once per calendar day via a stamp file next to the DB. Best-effort
/// throughout: a broken DB read must never turn a clean skip into a crash.
fn notify_if_stale(db_path: &std::path::Path) {
    const STALE_HOURS: i64 = 36;
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return;
    };
    let last: Option<String> = conn
        .query_row("SELECT MAX(created_at) FROM briefings", [], |row| row.get(0))
        .ok()
        .flatten();
    let Some(last) = last else { return };
    let Ok(last_ts) = chrono::NaiveDateTime::parse_from_str(&last, "%Y-%m-%d %H:%M:%S") else {
        return;
    };
    let age_hours = (chrono::Utc::now().naive_utc() - last_ts).num_hours();
    if age_hours < STALE_HOURS {
        return;
    }
    // Once per day across the 4 slots: stamp file holds the last alert date.
    let stamp = db_path.with_file_name("stale-alert-date.txt");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if std::fs::read_to_string(&stamp).map(|s| s.trim() == today).unwrap_or(false) {
        return;
    }
    let _ = std::fs::write(&stamp, &today);
    tracing::warn!("No briefing for {}h — firing staleness alert", age_hours);
    notify_failure(&format!(
        "No briefing for {}h — fetch slots keep getting skipped (Groq 403-blocked from this network). Check VPN/network.",
        age_hours
    ));
}

/// The sources that feed a weighted signal dimension, the weight they carry in
/// the compound score, and how long each may go quiet before that is an outage.
///
/// `notify_if_stale` above only asks whether a briefing was produced. A single
/// source can therefore die while briefings keep appearing every morning, and
/// the dimension it feeds silently reads 0.0 for every entity. Both halves of
/// that were live when this was written (measured 2026-08-17):
///
///   Senate LDA    — 403 Forbidden on EVERY run since 2026-08-03, 14 days,
///                   logged only as `SOURCE HEALTH: ... LDA` at warn level.
///                   political_signal is tied for the LARGEST weight at 0.2391.
///   Google Patents— no stored story since 2026-06-01, 77 days, while the
///                   endpoint itself answers and the fetch logs successes.
///
/// Thresholds come from each source's measured AVERAGE gap between story-days
/// over its history, times ~3.5, floored at 7 — deliberately not its WORST
/// observed gap, because the worst gap is usually a previous undetected outage
/// and using it would bake the bug into the baseline. LDA's worst gap was 13
/// days against an average of 1, which is exactly that trap: a 20-day threshold
/// would still be silent about today's two-week hole.
///
/// FRED is absent on purpose — it feeds supply_chain, which is zero-weighted.
const SIGNAL_SOURCES: &[(&str, &str, f64, i64)] = &[
    // (source_name in `stories`, dimension it feeds, weight, max quiet days)
    ("SEC EDGAR 4", "insider_signal", 0.2391, 7),
    ("Senate LDA", "political_signal", 0.2391, 7),
    ("USASpending", "government_signal", 0.1848, 7),
    ("Federal Register", "government_signal", 0.1848, 7),
    ("Wikipedia Pageviews", "search_trend", 0.0543, 14),
    ("Google Patents", "patent_signal", 0.0435, 21),
];

/// A source that has stopped producing stories takes its signal dimension to
/// zero without failing the run. Report it, once per calendar day.
///
/// Deliberately a STATE check over `stories` rather than an event check on the
/// fetch result: it catches a source that errors (LDA) and a source whose fetch
/// reports success while nothing lands (Patents) with one rule, and a single
/// transient 403 on one of the four daily slots cannot trip it.
///
/// Best-effort throughout — a broken DB read must never turn a good run into a
/// crash on the way out.
fn notify_stale_signal_sources(db_path: &std::path::Path) {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return;
    };

    let dead = stale_signal_sources(&conn);
    if dead.is_empty() {
        return;
    }

    let stamp = db_path.with_file_name("stale-source-alert-date.txt");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if std::fs::read_to_string(&stamp).map(|s| s.trim() == today).unwrap_or(false) {
        return;
    }
    let _ = std::fs::write(&stamp, &today);

    let summary = dead.join("; ");
    tracing::warn!("SIGNAL SOURCE STALE: {}", summary);
    notify_failure(&format!(
        "Signal sources have gone quiet — scores are being computed without them: {summary}"
    ));
}

/// The decision half, split out so it can be tested without shelling out to
/// `osascript`. Returns one human-readable line per stale source, empty when all
/// of them are current.
fn stale_signal_sources(conn: &rusqlite::Connection) -> Vec<String> {
    let mut dead: Vec<String> = Vec::new();
    for (source, dimension, weight, max_quiet_days) in SIGNAL_SOURCES {
        let days: Option<i64> = conn
            .query_row(
                "SELECT CAST(julianday('now') - julianday(MAX(date(created_at))) AS INTEGER)
                 FROM stories WHERE source_name = ?1",
                [source],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        // No rows at all is not reported here: a source that has never produced
        // anything is a setup problem, not a regression, and would alarm every
        // day forever without telling the user anything new.
        let Some(days) = days else { continue };
        if days > *max_quiet_days {
            dead.push(format!(
                "{source} ({dimension}, {:.0}% of the score) — {days}d quiet",
                weight * 100.0
            ));
        }
    }

    dead
}

/// Repair the embedding gap left by dropped batches during the daily run.
///
/// Bounded on purpose so this can be SCHEDULED rather than run by hand. At the Voyage
/// free tier (3 RPM = 30 stories/min) the Aug-2026 backlog of ~15.8k stories is ~9 hours
/// of wall clock, which must not be one unbounded job that collides with the daily fetch.
/// `max_minutes` is the real budget; `limit` is a secondary cap on rows selected.
///
/// Newest stories first: a story from this week matters more to search than one from
/// April, and if the budget runs out mid-backlog the useful half is already done.
async fn backfill_embeddings(
    db_path: &std::path::Path,
    limit: usize,
    max_minutes: u64,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_minutes * 60);
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    // Any mode that writes to the DB must be on the current schema. This mode is now
    // scheduled independently of the daily fetch, so it can be the first process to touch
    // the DB after an update.
    db::run_migrations(&conn)?;

    let (before_embedded, total_stories) = coverage(&conn)?;
    tracing::info!(
        "Embedding coverage before backfill: {}/{} ({:.1}%)",
        before_embedded,
        total_stories,
        pct(before_embedded, total_stories)
    );

    // Find stories without embeddings, newest first.
    let stories: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.headline, s.summary, s.key_facts
             FROM stories s
             LEFT JOIN story_embeddings se ON se.story_id = s.id
             WHERE se.story_id IS NULL
             ORDER BY s.id DESC
             LIMIT ?1"
        )?;
        stmt.query_map([limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?.collect::<Result<Vec<_>, _>>()?
    };

    if stories.is_empty() {
        tracing::info!("All stories already have embeddings");
        return Ok(());
    }

    let pause = embeddings::rate_limit_pause_secs();
    tracing::info!(
        "Backfilling {} stories (budget {}min, {}s between batches)",
        stories.len(),
        max_minutes,
        pause
    );

    let mut filled = 0usize;
    let mut failed = 0usize;
    let mut out_of_time = false;

    // Batch by tokens, not a fixed 10. The free tier caps 3 RPM *and* 10K TPM; at the
    // measured 76-token average a 10-story request spent ~2.3K TPM, so the run paid the
    // full rate-limit pause for a quarter-full request and the 90-minute budget cleared
    // about a quarter of what it could. See embeddings::VOYAGE_REQUEST_TOKEN_BUDGET.
    let all_texts: Vec<String> = stories.iter().map(|(_, headline, summary, key_facts)| {
        format!("{}. {}. {}", headline, summary, key_facts)
    }).collect();
    let batches = embeddings::batch_by_tokens(&all_texts, embeddings::VOYAGE_REQUEST_TOKEN_BUDGET);

    for (batch_no, &(bstart, bend)) in batches.iter().enumerate() {
        let chunk = &stories[bstart..bend];
        if std::time::Instant::now() >= deadline {
            out_of_time = true;
            tracing::info!(
                "Backfill budget of {}min reached after {} batches — remaining stories \
                 are picked up by the next scheduled run.",
                max_minutes, batch_no
            );
            break;
        }
        if batch_no > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(pause)).await;
        }

        let texts: Vec<String> = all_texts[bstart..bend].to_vec();
        let ids: Vec<i64> = chunk.iter().map(|(id, _, _, _)| *id).collect();

        match embeddings::generate_from_texts(&texts).await {
            Ok(embs) => {
                // Voyage returns embeddings positionally; a short response would silently
                // misalign story ids with vectors, so refuse rather than corrupt.
                if embs.len() != ids.len() {
                    failed += ids.len();
                    tracing::error!(
                        "Backfill batch {}: got {} embeddings for {} stories — skipping \
                         batch rather than risk misaligning vectors with story ids",
                        batch_no + 1, embs.len(), ids.len()
                    );
                    continue;
                }
                for (i, emb) in embs.iter().enumerate() {
                    if emb.len() != 512 {
                        failed += 1;
                        tracing::warn!(
                            "Backfill: story {} got {} dims, expected 512 — skipped",
                            ids[i], emb.len()
                        );
                        continue;
                    }
                    let blob: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
                    conn.execute(
                        "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                        rusqlite::params![ids[i], blob],
                    )?;
                    filled += 1;
                }
                tracing::info!(
                    "Backfill batch {}: embedded {} stories (IDs {}..{})",
                    batch_no + 1, chunk.len(), ids[ids.len() - 1], ids[0]
                );
            }
            Err(e) => {
                failed += ids.len();
                tracing::warn!("Backfill batch {} failed after retries: {}", batch_no + 1, e);
            }
        }
    }

    // Cost accounting — the backfill was previously invisible to the daily cost cap.
    if filled > 0 {
        pipeline::log_usage(
            db_path,
            "voyage",
            "voyage-3-lite",
            "backfill_embeddings",
            (filled as i64) * 200,
            0,
        );
    }

    let (after_embedded, total_after) = coverage(&conn)?;
    tracing::info!(
        "Backfill done: +{} embedded, {} failed. Coverage {:.1}% -> {:.1}% ({}/{}){}",
        filled,
        failed,
        pct(before_embedded, total_stories),
        pct(after_embedded, total_after),
        after_embedded,
        total_after,
        if out_of_time { " [budget reached]" } else { "" }
    );
    Ok(())
}

/// Local hours at which the scheduled embedding backfill is allowed to do work.
///
/// This lives in the BINARY, not the plist, because launchd will not honour it. Measured on
/// this machine (macOS 25.5, `gui/501` domain) on 2026-08-15: a LaunchAgent whose
/// `StartCalendarInterval` names an `Hour` never fires. Four controlled probes, identical
/// except for the calendar spec, one boundary each:
///
///   array of dicts, Hour+Minute ....... runs = 0
///   single dict,    Hour+Minute ....... runs = 0
///   single dict,    Hour+Minute in UTC. runs = 0   (rules out a timezone reading)
///   single dict,    Minute only ....... runs = 1   FIRED
///
/// All four were confirmed loaded with armed triggers via `launchctl print`, and the three
/// that failed were still at `runs = 0` eight minutes past their boundary, which rules out
/// deferral or coalescing. `com.pulse.daily-fetch` uses a Minute-only interval and has
/// fired every hour for twenty consecutive runs. The root cause in launchd is not known and
/// does not need to be: the plist now uses the shape that demonstrably fires — hourly — and
/// this gate decides which of those wakeups actually does anything.
///
/// The plist wakes at :30, so these are the 02:30 and 14:30 wakeups. Tradeoff worth knowing:
/// a slot the machine sleeps through is SKIPPED, not deferred, because the gate only sees
/// the hour it actually woke in. That is acceptable here — the daily fetch has fired every
/// hour round the clock for 20+ runs, so this machine is awake at both slots — and the
/// in-pipeline drain still runs on every briefing regardless.
const BACKFILL_HOURS: [u32; 2] = [2, 14];

/// Whether `hour` (0-23, local) is one of the backfill slots.
fn is_backfill_hour(hour: u32) -> bool {
    BACKFILL_HOURS.contains(&hour)
}

/// How many NEWS stories today's daily briefing already has.
///
/// This is the project's definition of "the day is done", and it is deliberately NOT
/// `briefings.status='complete'`: a run blocked at the Groq summarize step still writes
/// financial stories (they skip summarization) and marks the briefing complete with ZERO
/// news — exactly the degraded state a later slot should RETRY, not skip.
/// See groq-vpn-block-plan.md. Returns 0 if the DB is missing or unreadable.
fn news_today(db_path: &std::path::Path) -> i64 {
    if !db_path.exists() {
        return 0;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return 0;
    };
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    conn.query_row(
        "SELECT COUNT(*) FROM stories s \
         JOIN briefings b ON s.briefing_id = b.id \
         WHERE b.date = ?1 AND b.briefing_type = 'daily' AND s.source_type = 'news'",
        [&today],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// (stories with an embedding, total stories)
fn coverage(conn: &rusqlite::Connection) -> anyhow::Result<(i64, i64)> {
    let embedded: i64 = conn.query_row(
        "SELECT count(*) FROM stories s
         JOIN story_embeddings e ON e.story_id = s.id",
        [],
        |r| r.get(0),
    )?;
    let total: i64 = conn.query_row("SELECT count(*) FROM stories", [], |r| r.get(0))?;
    Ok((embedded, total))
}

fn pct(part: i64, whole: i64) -> f64 {
    if whole == 0 { 0.0 } else { 100.0 * part as f64 / whole as f64 }
}

async fn extract_entities(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    // Run migration 003 if needed
    let applied: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        stmt.query_map([], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?
    };
    if !applied.contains(&3) {
        conn.execute_batch(include_str!("../../../migrations/003_intelligence.sql"))?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (3)", [])?;
    }

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    // Get all stories that don't have entity_mentions yet
    let stories: Vec<(i64, String, String, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.headline, s.summary, s.sector, b.date, s.why_it_matters
             FROM stories s
             JOIN briefings b ON b.id = s.briefing_id
             LEFT JOIN entity_mentions em ON em.story_id = s.id
             WHERE em.id IS NULL
             ORDER BY s.id"
        )?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
        })?.collect::<Result<Vec<_>, _>>()?
    };

    if stories.is_empty() {
        tracing::info!("All stories already have entities extracted");
        return Ok(());
    }

    tracing::info!("Found {} stories to extract entities from", stories.len());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Process in batches of 15 stories per Haiku call
    for (batch_idx, chunk) in stories.chunks(30).enumerate() {
        let mut stories_text = String::new();
        for (id, headline, summary, sector, date, why) in chunk {
            stories_text.push_str(&format!(
                "\n[Story {}] [{}] [{}] {}\n{}\n{}\n",
                id, sector, date, headline, summary, why
            ));
        }

        let body = serde_json::json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 2000,
            "system": r#"Extract named entities from these news stories. Return valid JSON only.

For each entity provide:
- name: The canonical name (e.g., "OpenAI" not "openai")
- entity_type: one of "company", "person", "topic", "product", "regulation"
- sentiment: -1.0 to 1.0 (how the story portrays this entity)
- context: One brief sentence about the mention
- story_id: The story ID number from the [Story N] tag

Return: {"entities": [{"name": "...", "entity_type": "...", "sentiment": 0.5, "context": "...", "story_id": 123}]}

Focus on the MOST important entities (max 5 per story). Prioritize companies, key people, and products over generic topics."#,
            "messages": [{"role": "user", "content": stories_text}]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            tracing::warn!("Haiku entity extraction failed for batch {}: {}", batch_idx, err);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        let response: serde_json::Value = resp.json().await?;
        let text = response["content"][0]["text"].as_str().unwrap_or("{}");

        // Extract JSON from response
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else { text }
        } else { text };

        #[derive(serde::Deserialize)]
        struct ExtractedEntity {
            name: String,
            entity_type: String,
            sentiment: f64,
            context: Option<String>,
            story_id: i64,
        }
        #[derive(serde::Deserialize)]
        struct ExtractionResult {
            entities: Vec<ExtractedEntity>,
        }

        match serde_json::from_str::<ExtractionResult>(json_str) {
            Ok(result) => {
                let mut stored = 0;
                let valid_types = ["company", "person", "topic", "product", "regulation"];
                for ent in &result.entities {
                    // Validate entity_type against CHECK constraint
                    let entity_type = ent.entity_type.to_lowercase();
                    let entity_type = entity_type.trim();
                    if !valid_types.contains(&entity_type) {
                        continue; // Skip invalid types
                    }
                    let name_normalized = ent.name.to_lowercase().trim().to_string();
                    if name_normalized.is_empty() { continue; }

                    // Find the sector and date for this story
                    let story_meta = chunk.iter().find(|(id, _, _, _, _, _)| *id == ent.story_id);
                    let (sector, date) = match story_meta {
                        Some((_, _, _, s, d, _)) => (s.as_str(), d.as_str()),
                        None => continue,
                    };

                    // Upsert entity
                    conn.execute(
                        "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6)
                         ON CONFLICT(name_normalized, entity_type) DO UPDATE SET
                           last_seen = MAX(last_seen, ?5),
                           sentiment_avg = (sentiment_avg * mention_count + ?6) / (mention_count + 1),
                           mention_count = mention_count + 1",
                        rusqlite::params![ent.name, name_normalized, entity_type, sector, date, ent.sentiment],
                    )?;

                    let entity_id: i64 = conn.query_row(
                        "SELECT id FROM entities WHERE name_normalized = ?1 AND entity_type = ?2",
                        rusqlite::params![name_normalized, entity_type],
                        |row| row.get(0),
                    )?;

                    conn.execute(
                        "INSERT INTO entity_mentions (entity_id, story_id, sentiment, context, mentioned_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![entity_id, ent.story_id, ent.sentiment, ent.context, date],
                    )?;

                    stored += 1;
                }
                tracing::info!("Batch {}: extracted {} entities from {} stories", batch_idx + 1, stored, chunk.len());
            }
            Err(e) => {
                tracing::warn!("Failed to parse entity extraction result for batch {}: {}", batch_idx, e);
            }
        }

        // Rate limit
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // Recompute signals
    tracing::info!("Recomputing signals...");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let signal_count = recompute_signals(&conn, &today)?;
    tracing::info!("Computed {} signals", signal_count);

    let entity_count: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;
    let mention_count: i64 = conn.query_row("SELECT COUNT(*) FROM entity_mentions", [], |r| r.get(0))?;
    tracing::info!("Entity extraction complete. {} entities, {} mentions", entity_count, mention_count);
    Ok(())
}

fn recompute_signals(conn: &rusqlite::Connection, today: &str) -> anyhow::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT e.name, e.sector,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-7 days') THEN 1 ELSE 0 END) as w7,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-30 days') THEN 1 ELSE 0 END) as w30,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-90 days') THEN 1 ELSE 0 END) as w90,
            COUNT(DISTINCT em.mentioned_at) as days_active
         FROM entities e
         JOIN entity_mentions em ON em.entity_id = e.id
         GROUP BY e.name, e.sector"
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, i64, i64)> = stmt.query_map(
        [today],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut count = 0;
    for (topic, sector, w7, w30, w90, days_active) in &rows {
        let acceleration = if *w30 == 0 {
            if *w7 > 0 { 10.0 } else { 0.0 }
        } else {
            let r7 = *w7 as f64 / 7.0;
            let r30 = *w30 as f64 / 30.0;
            if r30 < 0.001 { if *w7 > 0 { 10.0 } else { 0.0 } } else { r7 / r30 }
        };

        let total = (*w30).max(*w7);
        let trajectory = if *w7 == 0 && *w30 == 0 { "dormant" }
            else if total >= 14 && *days_active >= 10 { "dominant" }
            else if total >= 7 && *days_active >= 5 { "hot" }
            else if acceleration < 0.8 && total >= 3 { "fading" }
            else if total >= 3 || *days_active >= 2 { "rising" }
            else if *w7 > 0 { "rising" }
            else { "dormant" };

        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(topic, sector) DO UPDATE SET
               window_7d = ?3, window_30d = ?4, window_90d = ?5,
               acceleration = ?6, trajectory = ?7, updated_at = datetime('now')",
            rusqlite::params![topic, sector, w7, w30, w90, acceleration, trajectory],
        )?;
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod hour_gate_tests {
    use super::*;

    /// The gate exists because launchd will not honour an `Hour` in the plist, so the
    /// binary is woken every hour and must reject 22 of those wakeups. Assert the whole
    /// 24-hour space, not just the two slots — a gate that accepts everything would pass a
    /// test that only checked hours 2 and 14.
    #[test]
    fn accepts_exactly_the_two_slots() {
        let accepted: Vec<u32> = (0..24).filter(|h| is_backfill_hour(*h)).collect();
        assert_eq!(accepted, vec![2, 14], "gate must open at 02:00 and 14:00 and nowhere else");
    }

    /// The failure this guards against is an hourly wakeup starting a 90-minute backfill
    /// that holds the flock against the daily fetch. Name the hours that must be refused.
    #[test]
    fn refuses_the_daily_fetch_window() {
        for h in [0, 1, 3, 7, 8, 9, 12, 13, 15, 20, 23] {
            assert!(!is_backfill_hour(h), "hour {h} must not start a backfill");
        }
    }

    /// Hours come from `chrono::Local::now().hour()`, which is 0-23. A slot outside that
    /// range would be unreachable — the gate would never open and the backfill would go
    /// back to never running, which is the exact bug being fixed here.
    #[test]
    fn every_slot_is_a_reachable_local_hour() {
        for h in BACKFILL_HOURS {
            assert!(h < 24, "slot {h} is not a reachable hour — gate could never open");
            assert!(is_backfill_hour(h), "slot {h} must be accepted by its own gate");
        }
    }
}

#[cfg(test)]
mod stale_source_tests {
    use super::*;

    /// `stories` with just the two columns this check reads.
    fn db(rows: &[(&str, i64)]) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE stories (id INTEGER PRIMARY KEY, source_name TEXT, created_at TEXT);",
        )
        .unwrap();
        for (source, days_ago) in rows {
            conn.execute(
                "INSERT INTO stories (source_name, created_at)
                 VALUES (?1, datetime('now', ?2))",
                rusqlite::params![source, format!("-{days_ago} days")],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn every_current_source_is_silent() {
        let rows: Vec<(&str, i64)> = SIGNAL_SOURCES.iter().map(|(s, _, _, _)| (*s, 0)).collect();
        assert!(stale_signal_sources(&db(&rows)).is_empty());
    }

    #[test]
    fn the_live_lda_outage_is_reported() {
        // The state on 2026-08-17: LDA last produced a story on 2026-08-03 while
        // every other source was within three days. Fourteen days of silence in
        // the joint-largest-weighted dimension, and nothing said so.
        let mut rows: Vec<(&str, i64)> =
            SIGNAL_SOURCES.iter().map(|(s, _, _, _)| (*s, 2)).collect();
        for r in rows.iter_mut() {
            if r.0 == "Senate LDA" {
                r.1 = 14;
            }
        }
        let dead = stale_signal_sources(&db(&rows));
        assert_eq!(dead.len(), 1, "expected only LDA, got {dead:?}");
        assert!(dead[0].contains("Senate LDA"), "{}", dead[0]);
        assert!(
            dead[0].contains("political_signal") && dead[0].contains("24%"),
            "the alert must name the dimension and what it is worth: {}",
            dead[0]
        );
    }

    #[test]
    fn a_source_inside_its_own_threshold_is_not_reported() {
        // Patents legitimately goes quiet for weeks — it rotates companies and
        // re-sees patents it already stored. 20 days is inside its 21-day budget
        // while the same gap would be an outage for a daily source.
        let dead = stale_signal_sources(&db(&[("Google Patents", 20), ("SEC EDGAR 4", 20)]));
        assert_eq!(dead.len(), 1, "{dead:?}");
        assert!(dead[0].contains("SEC EDGAR 4"), "{}", dead[0]);
    }

    #[test]
    fn a_source_that_never_produced_anything_is_not_alarmed_about() {
        // An empty table means "not set up", which would otherwise fire every
        // single day forever and train the user to ignore the notification.
        assert!(stale_signal_sources(&db(&[])).is_empty());
    }

    #[test]
    fn several_dead_sources_are_all_named() {
        let dead = stale_signal_sources(&db(&[
            ("Senate LDA", 14),
            ("Google Patents", 77),
            ("SEC EDGAR 4", 1),
        ]));
        assert_eq!(dead.len(), 2, "{dead:?}");
        assert!(dead.iter().any(|d| d.contains("Senate LDA")));
        assert!(dead.iter().any(|d| d.contains("Google Patents")));
    }

    #[test]
    fn every_weighted_dimension_has_a_source_watching_it() {
        // The point of the table is coverage. If a dimension carries weight and
        // no row here watches its source, this check cannot see it die.
        for dim in [
            "insider_signal",
            "news_momentum",
            "government_signal",
            "search_trend",
            "patent_signal",
            "political_signal",
        ] {
            let watched = SIGNAL_SOURCES.iter().any(|(_, d, _, _)| *d == dim);
            if dim == "news_momentum" {
                // news_momentum is computed over Pulse's own story windows rather
                // than one named source, so a single source_name cannot stand for
                // it. Its death shows up as the whole run producing no stories,
                // which notify_if_stale already covers.
                assert!(!watched, "news_momentum should not be watched by source name");
            } else {
                assert!(watched, "{dim} carries weight but no source is watched for it");
            }
        }
    }
}

#[cfg(test)]
mod backfill_tests {
    use super::*;

    fn db_with(stories: &[(i64, bool)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE briefings (id INTEGER PRIMARY KEY, date TEXT, briefing_type TEXT);
             CREATE TABLE stories (id INTEGER PRIMARY KEY, briefing_id INTEGER,
                 source_type TEXT, headline TEXT, summary TEXT, key_facts TEXT);
             CREATE TABLE story_embeddings (story_id INTEGER PRIMARY KEY, embedding BLOB);",
        )
        .unwrap();
        for (id, embedded) in stories {
            conn.execute(
                "INSERT INTO stories (id, briefing_id, source_type, headline, summary, key_facts)
                 VALUES (?1, 1, 'news', 'h', 's', 'k')",
                rusqlite::params![id],
            )
            .unwrap();
            if *embedded {
                conn.execute(
                    "INSERT INTO story_embeddings (story_id, embedding) VALUES (?1, x'00')",
                    rusqlite::params![id],
                )
                .unwrap();
            }
        }
        (dir, path)
    }

    /// The number the whole fix exists to move. Coverage must count stories WITH an
    /// embedding over ALL stories — the metric that decayed 100% -> 49.7% unnoticed.
    #[test]
    fn coverage_counts_embedded_over_total() {
        let (_d, path) = db_with(&[(1, true), (2, false), (3, true), (4, false)]);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let (embedded, total) = coverage(&conn).unwrap();
        assert_eq!((embedded, total), (2, 4));
        assert_eq!(pct(embedded, total), 50.0);
    }

    #[test]
    fn pct_handles_empty_db_without_dividing_by_zero() {
        assert_eq!(pct(0, 0), 0.0);
    }

    /// The backfill must yield to the daily briefing: it is gated on today's news
    /// EXISTING, because until then an hourly daily slot may still need the lock.
    #[test]
    fn news_today_is_zero_until_a_news_story_lands_today() {
        let (_d, path) = db_with(&[(1, false)]);
        assert_eq!(news_today(&path), 0, "no briefing row yet");

        let conn = rusqlite::Connection::open(&path).unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT INTO briefings (id, date, briefing_type) VALUES (1, ?1, 'daily')",
            [&today],
        )
        .unwrap();
        assert_eq!(news_today(&path), 1, "news present -> backfill may run");
    }

    /// A financial-only briefing (Groq blocked at summarize) marks itself complete with
    /// ZERO news. That day must still read as "not done" so a later slot retries — and
    /// so the backfill does not grab the lock out from under it.
    #[test]
    fn financial_only_day_does_not_count_as_done() {
        let (_d, path) = db_with(&[]);
        let conn = rusqlite::Connection::open(&path).unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT INTO briefings (id, date, briefing_type) VALUES (1, ?1, 'daily')",
            [&today],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stories (id, briefing_id, source_type, headline, summary, key_facts)
             VALUES (99, 1, 'financial', 'h', 's', 'k')",
            [],
        )
        .unwrap();
        assert_eq!(news_today(&path), 0, "financial-only day must NOT read as done");
    }

    #[test]
    fn missing_db_is_not_a_panic() {
        assert_eq!(news_today(std::path::Path::new("/nonexistent/pulse.db")), 0);
    }
}
