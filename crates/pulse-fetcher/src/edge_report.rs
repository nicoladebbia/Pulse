//! Step 6 — re-measure the paper-trading edge on REAL fills placed AFTER the
//! 2026-07-15 ticker fix + arming. This is the deliberately conservative readout
//! that must show a positive expectancy before anyone discusses live trading.
//!
//! # Why this is not the backtester
//! `services/backtester.rs` simulates over historical `cross_signals`, whose
//! compound scores are FROZEN from the old (poisoned-ticker) pipeline — repairing
//! `cross_signals.ticker` cannot un-freeze those scores, so the backtest can never
//! validate the armed engine. Only real fills the armed engine actually placed
//! measure the real edge. This module reads `paper_trades` and nothing else.
//!
//! # The verify-floor discipline baked in (see feedback_verify_floor_first)
//! - **Arm-date cutoff.** Only fills with `entry_date >= ARM_DATE` count. Pre-arm
//!   trades are a DIFFERENT experiment: poisoned tickers, and pre-2026-04-28 rows
//!   predate the $50 sizing floor (the phantom $0.01 INTC trades). Mixing them in
//!   is the cross-condition-comparison error. They are excluded, never "cleaned".
//! - **N-gate.** Below `MIN_SAMPLE` resolved trades the report REFUSES a verdict and
//!   prints "insufficient sample, N=x". One or nine trades is a hypothesis, not an
//!   edge. No N=1 verdicts.
//! - **No cross-condition comparison.** This report does not compare against the
//!   April n=9 history or the 2026-06-05 no-edge backtest — different corpus,
//!   different tickers, different clock. It reports THIS engine's fills, full stop.
//! - **Both % and $.** Win-rate and avg-win/avg-loss are in %, but expectancy is
//!   also reported in $/trade and net $, because under position-size variance the
//!   two diverge (the April data was +20.8% avg yet −$116 net — legitimately).

use rusqlite::Connection;
use std::path::Path;

/// Fills before this date are a different experiment (poisoned tickers / pre-clamp
/// sizing) and are excluded. This is the date auto-buy was armed + the cross_signals
/// tickers were repaired.
const ARM_DATE: &str = "2026-07-15";

/// Minimum resolved (closed OR stopped_out) trades before a verdict is allowed.
/// Below this the sample is noise. 20 is the floor the report will not cross; a
/// confident "positive edge" call wants more, but 20 is where it stops saying
/// "insufficient sample" and starts reporting the (still-provisional) numbers.
const MIN_SAMPLE: usize = 20;

// ── Once-fired latch ────────────────────────────────────────────────────────
// The daily run fires at 7/12/18/22 as four separate processes, so an in-memory
// "already notified" flag never survives. Persist the latch in a fetcher-private
// table. The SvelteKit app has no interest in these flags, so this is created
// inline (CREATE TABLE IF NOT EXISTS) rather than via a schema migration —
// keeps it out of the two-binary migration registration entirely.

fn ensure_flags_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS fetcher_flags (
            key    TEXT PRIMARY KEY,
            set_at TEXT NOT NULL
        );",
    )
}

/// True if this flag has already been set (the event already fired once).
fn flag_is_set(conn: &Connection, key: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM fetcher_flags WHERE key = ?1)",
        [key],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Set the flag (idempotent). `now` is passed in — the fetcher stamps time at the
/// call site, this module stays clock-free for testability.
fn set_flag(conn: &Connection, key: &str, now: &str) {
    let _ = conn.execute(
        "INSERT OR IGNORE INTO fetcher_flags (key, set_at) VALUES (?1, ?2)",
        rusqlite::params![key, now],
    );
}

const FLAG_FIRST_CLOSE: &str = "first_post_arm_close_confirmed";
const FLAG_EDGE_SAMPLE_READY: &str = "edge_sample_reached_min";

/// Notification callback signature — decouples this module from osascript.
/// main.rs passes `notify_failure`-style closures so tests can pass a no-op.
pub type Notifier<'a> = &'a dyn Fn(&str, &str);

#[derive(Debug, Default)]
pub struct EdgeReport {
    pub resolved: usize,
    pub open: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
    pub avg_win_pct: f64,
    pub avg_loss_pct: f64,
    pub expectancy_pct: f64,
    pub expectancy_dollars: f64,
    pub net_dollars: f64,
    pub sufficient_sample: bool,
}

/// Compute the post-arm edge and log it. Reads ONLY `paper_trades`.
pub fn run_edge_report(db_path: &Path) -> anyhow::Result<EdgeReport> {
    let conn = Connection::open(db_path)?;

    // Resolved post-arm trades with a REAL realized outcome: (pnl_pct, pnl_dollars).
    // A row with NULL pnl/exit_price is an unknown outcome (e.g. a 404/reconcile
    // no-fill close), NOT a $0 flat trade — including it via COALESCE(...,0) would
    // pad N and drag expectancy toward zero. Exclude it here; verify_pnl_writes
    // separately alarms the human that such a row exists. The two must agree on the
    // same dataset: flagged-for-review ⇒ excluded-from-measurement. date(entry_date)
    // normalizes both 'YYYY-MM-DD' and full-timestamp entry_date formats.
    let rows: Vec<(f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT pnl_pct, pnl
             FROM paper_trades
             WHERE status IN ('closed', 'stopped_out')
               AND date(entry_date) >= ?1
               AND pnl IS NOT NULL
               AND pnl_pct IS NOT NULL
               AND exit_price IS NOT NULL",
        )?;
        stmt.query_map([ARM_DATE], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    let open: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM paper_trades
             WHERE status = 'open' AND date(entry_date) >= ?1",
            [ARM_DATE],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut rep = EdgeReport {
        resolved: rows.len(),
        open,
        ..Default::default()
    };

    if rows.is_empty() {
        rep.sufficient_sample = false;
        log_report(&rep);
        return Ok(rep);
    }

    let win_pcts: Vec<f64> = rows.iter().map(|(p, _)| *p).filter(|p| *p > 0.0).collect();
    let loss_pcts: Vec<f64> = rows.iter().map(|(p, _)| *p).filter(|p| *p <= 0.0).collect();

    rep.wins = win_pcts.len();
    rep.losses = loss_pcts.len();
    rep.win_rate = rep.wins as f64 / rep.resolved as f64;
    rep.avg_win_pct = mean(&win_pcts);
    rep.avg_loss_pct = mean(&loss_pcts);
    rep.expectancy_pct = rows.iter().map(|(p, _)| *p).sum::<f64>() / rep.resolved as f64;
    rep.net_dollars = rows.iter().map(|(_, d)| *d).sum::<f64>();
    rep.expectancy_dollars = rep.net_dollars / rep.resolved as f64;
    rep.sufficient_sample = rep.resolved >= MIN_SAMPLE;

    log_report(&rep);
    Ok(rep)
}

/// A resolved post-arm trade whose realized-P&L write is broken.
#[derive(Debug)]
pub struct BadPnlRow {
    pub id: i64,
    pub ticker: String,
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
    pub exit_price: Option<f64>,
    pub reason: String,
}

/// GAP A — verify the realized-P&L WRITE on EVERY close path, and confirm-once.
///
/// Runs after Phase 13.6 (position management). This is a PATH-AGNOSTIC table
/// query, deliberately NOT an inline check in the sell branch — because trades
/// also close via the 404/reconcile path (pipeline.rs:5012), and its no-fill
/// sub-branch (5039-5050) writes `status='closed'` with pnl/exit_price left NULL.
/// edge-report reads any resolved row via COALESCE(pnl,0), so a NULL-pnl close
/// silently counts as a $0 flat trade and drags expectancy toward zero. An inline
/// sell-branch check would never see reconcile-closed rows; this query catches a
/// bad write from ANY close path.
///
/// Two independent behaviours:
///  (a) a PERMANENT breakage alarm — fires whenever a resolved post-arm row
///      violates the P&L invariant (fires every run until the row is fixed; a
///      corrupt substrate feeding the go/no-go decision must not be one-shot);
///  (b) a ONE-TIME "first close confirmed" notification — rule #8: the close→pnl
///      chain was verified statically, this is the first RUNTIME observation.
///
/// Invariant (both derive from exit−entry, so any mismatch = broken write):
///   exit_price NOT NULL  AND  pnl NOT NULL  AND  pnl_pct NOT NULL
///   AND  sign(pnl) == sign(pnl_pct)
/// One resolved `paper_trades` row: `(id, ticker, pnl, pnl_pct, exit_price)`.
/// The three money columns are nullable — a row can be closed with the P&L
/// write missing, which is the whole thing this module screens for.
type ResolvedTrade = (i64, String, Option<f64>, Option<f64>, Option<f64>);

pub fn verify_pnl_writes(db_path: &Path, now: &str, notify: Notifier) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    ensure_flags_table(&conn)?;

    // All resolved post-arm rows, so we can both count (for the first-close latch)
    // and screen each for the invariant.
    let rows: Vec<ResolvedTrade> = {
        let mut stmt = conn.prepare(
            "SELECT id, ticker, pnl, pnl_pct, exit_price
             FROM paper_trades
             WHERE status IN ('closed', 'stopped_out')
               AND date(entry_date) >= ?1
             ORDER BY exit_date",
        )?;
        stmt.query_map([ARM_DATE], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    };

    if rows.is_empty() {
        return Ok(()); // nothing resolved yet — first close hasn't happened
    }

    // (a) Breakage screen — path-agnostic, fires every run while broken.
    let mut bad: Vec<BadPnlRow> = Vec::new();
    for (id, ticker, pnl, pnl_pct, exit_price) in &rows {
        let reason = if exit_price.is_none() {
            Some("exit_price NULL (reconcile no-fill close)")
        } else if pnl.is_none() || pnl_pct.is_none() {
            Some("pnl/pnl_pct NULL on a resolved row")
        } else if pnl.unwrap().signum() != pnl_pct.unwrap().signum()
            && pnl.unwrap() != 0.0
            && pnl_pct.unwrap() != 0.0
        {
            Some("sign(pnl) != sign(pnl_pct) — broken write")
        } else {
            None
        };
        if let Some(r) = reason {
            bad.push(BadPnlRow {
                id: *id,
                ticker: ticker.clone(),
                pnl: *pnl,
                pnl_pct: *pnl_pct,
                exit_price: *exit_price,
                reason: r.to_string(),
            });
        }
    }

    if !bad.is_empty() {
        for b in &bad {
            tracing::error!(
                "PNL-WRITE BROKEN: trade {} {} — {} (pnl={:?} pnl_pct={:?} exit_price={:?})",
                b.id, b.ticker, b.reason, b.pnl, b.pnl_pct, b.exit_price
            );
        }
        let first = &bad[0];
        notify(
            "Pulse: paper-trade P&L write BROKEN",
            &format!(
                "{} resolved trade(s) have bad P&L (edge-report substrate corrupt). First: {} — {}.",
                bad.len(), first.ticker, first.reason
            ),
        );
    }

    // (b) First-close confirmation — one-time, rule-#8 runtime verification.
    if !flag_is_set(&conn, FLAG_FIRST_CLOSE) {
        // Use the earliest resolved row as "the first close".
        let (id, ticker, pnl, pnl_pct, exit_price) = &rows[0];
        let healthy = exit_price.is_some() && pnl.is_some() && pnl_pct.is_some();
        tracing::info!(
            "FIRST POST-ARM CLOSE observed: trade {} {} @ exit {:?} ({:?}%, ${:?}) — invariant {}",
            id, ticker, exit_price, pnl_pct, pnl,
            if healthy { "OK" } else { "VIOLATED (see alarm above)" }
        );
        notify(
            "Pulse: first paper exit closed",
            &format!(
                "{} closed at {:+.1}% (${:+.2}). Realized-P&L write {}.",
                ticker,
                pnl_pct.unwrap_or(0.0),
                pnl.unwrap_or(0.0),
                if healthy { "verified OK" } else { "LOOKS BROKEN — review" }
            ),
        );
        set_flag(&conn, FLAG_FIRST_CLOSE, now);
    }

    Ok(())
}

/// GAP B — announce ONCE when the post-arm sample first reaches N=MIN_SAMPLE.
///
/// Runs after Phase 14 (calibration). Does NOT render a verdict and does NOT
/// touch any live/arming flag — a human runs `--mode edge-report` and reads it.
/// This only removes the "remember to check in 3 weeks" dependency: the run
/// tells you when there's finally enough data to look at. Fires exactly once
/// (latched), stays silent both before the threshold and on every run after.
pub fn maybe_announce_edge_sample(db_path: &Path, now: &str, notify: Notifier) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    ensure_flags_table(&conn)?;

    if flag_is_set(&conn, FLAG_EDGE_SAMPLE_READY) {
        return Ok(()); // already announced
    }

    // Count the SAME set run_edge_report measures — resolved rows with a real
    // realized outcome. A NULL-pnl no-fill row is excluded there, so it must be
    // excluded here too; otherwise this could announce "N=20 ready" on a sample
    // padded with phantom rows edge-report won't count, mis-tripping the gate.
    let resolved: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM paper_trades
             WHERE status IN ('closed', 'stopped_out') AND date(entry_date) >= ?1
               AND pnl IS NOT NULL AND pnl_pct IS NOT NULL AND exit_price IS NOT NULL",
            [ARM_DATE],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if resolved < MIN_SAMPLE {
        return Ok(()); // not enough data yet — stay silent
    }

    tracing::info!(
        "EDGE SAMPLE READY: {} post-arm resolved trades (>= {}). Run `--mode edge-report` to review. \
         NO auto-verdict — a human reads it before any live discussion.",
        resolved, MIN_SAMPLE
    );
    notify(
        "Pulse: paper-edge sample ready to review",
        &format!(
            "{} resolved paper trades since arming. Run edge-report and read the expectancy — \
             no auto go-live.",
            resolved
        ),
    );
    set_flag(&conn, FLAG_EDGE_SAMPLE_READY, now);
    Ok(())
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

fn log_report(rep: &EdgeReport) {
    tracing::info!("========== PAPER EDGE REPORT (fills since {}) ==========", ARM_DATE);
    tracing::info!("Resolved trades: {}  |  Still open: {}", rep.resolved, rep.open);

    if rep.resolved == 0 {
        tracing::info!(
            "N=0 — no resolved fills since arming yet. First scheduled run: check the \
             12:00/18:00/22:00 launchd cycles. NO verdict possible."
        );
        tracing::info!("=========================================================");
        return;
    }

    tracing::info!(
        "Win rate: {:.0}% ({}W / {}L)  |  avg win {:+.1}%  avg loss {:+.1}%",
        rep.win_rate * 100.0, rep.wins, rep.losses, rep.avg_win_pct, rep.avg_loss_pct
    );
    tracing::info!(
        "Expectancy: {:+.2}%/trade  |  {:+.2} $/trade  |  net {:+.2} $",
        rep.expectancy_pct, rep.expectancy_dollars, rep.net_dollars
    );

    if !rep.sufficient_sample {
        tracing::info!(
            "INSUFFICIENT SAMPLE (N={} < {}). These numbers are a hypothesis, NOT an edge. \
             No go/no-go verdict until N >= {}. Do NOT compare to the April n=9 history or the \
             2026-06-05 backtest — different tickers, corpus, and clock.",
            rep.resolved, MIN_SAMPLE, MIN_SAMPLE
        );
    } else {
        let verdict = if rep.expectancy_dollars > 0.0 && rep.expectancy_pct > 0.0 {
            "POSITIVE expectancy on both $ and % — the armed engine shows edge on paper. \
             Necessary-but-not-sufficient for live; confirm the win/loss asymmetry is not one \
             lucky outlier before any live discussion."
        } else {
            "NON-POSITIVE expectancy. The armed engine does NOT show edge on paper. Do not go live."
        };
        tracing::info!("N>={} reached. {}", MIN_SAMPLE, verdict);
    }
    tracing::info!("=========================================================");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // Minimal paper_trades schema mirroring the live columns the hooks read.
    fn seed_conn() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        let f = tempfile::NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE paper_trades (
                id INTEGER PRIMARY KEY, ticker TEXT NOT NULL, direction TEXT NOT NULL DEFAULT 'long',
                entry_price REAL NOT NULL, entry_date TEXT NOT NULL, exit_price REAL, exit_date TEXT,
                position_size REAL NOT NULL DEFAULT 100, confidence REAL NOT NULL DEFAULT 0.5,
                signal_profile TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'open',
                pnl REAL, pnl_pct REAL);",
        )
        .unwrap();
        (f, path)
    }

    fn insert(
        path: &std::path::Path, ticker: &str, entry_date: &str, status: &str,
        exit_price: Option<f64>, pnl: Option<f64>, pnl_pct: Option<f64>,
    ) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO paper_trades (ticker, entry_price, entry_date, status, exit_price, pnl, pnl_pct)
             VALUES (?1, 10.0, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![ticker, entry_date, status, exit_price, pnl, pnl_pct],
        )
        .unwrap();
    }

    // A capturing notifier — records (title, body) so tests assert on notifications.
    struct Capture(RefCell<Vec<(String, String)>>);
    impl Capture {
        fn new() -> Self { Capture(RefCell::new(Vec::new())) }
        fn notifier(&self) -> impl Fn(&str, &str) + '_ {
            move |t: &str, b: &str| self.0.borrow_mut().push((t.into(), b.into()))
        }
        fn count(&self) -> usize { self.0.borrow().len() }
        fn last_body(&self) -> String { self.0.borrow().last().map(|(_, b)| b.clone()).unwrap_or_default() }
    }

    #[test]
    fn first_close_confirms_once_then_silent() {
        let (_f, path) = seed_conn();
        // one healthy post-arm close
        insert(&path, "ARM", "2026-07-16", "closed", Some(11.0), Some(10.0), Some(10.0));

        let cap = Capture::new();
        let n = cap.notifier();
        verify_pnl_writes(&path, "2026-07-16T12:00:00", &n).unwrap();
        assert_eq!(cap.count(), 1, "first close should notify once");
        assert!(cap.last_body().contains("verified OK"), "healthy write => OK: {}", cap.last_body());

        // second run, same (still-only) close — latch must suppress it
        verify_pnl_writes(&path, "2026-07-16T18:00:00", &n).unwrap();
        assert_eq!(cap.count(), 1, "second run must be silent (latched)");
    }

    #[test]
    fn breakage_alarm_catches_null_pnl_reconcile_row() {
        // The exact substrate hole: 404/reconcile no-fill close → status='closed'
        // with pnl / exit_price left NULL (pipeline.rs:5042-5045).
        let (_f, path) = seed_conn();
        insert(&path, "GHOST", "2026-07-16", "closed", None, None, None);

        let cap = Capture::new();
        let n = cap.notifier();
        verify_pnl_writes(&path, "2026-07-16T12:00:00", &n).unwrap();
        // Fires BOTH: breakage alarm + first-close(violated) — assert the alarm is present.
        assert!(
            cap.0.borrow().iter().any(|(t, _)| t.contains("BROKEN")),
            "NULL-pnl reconcile close must trip the breakage alarm"
        );

        // Breakage alarm is NOT latched — must fire again next run while still broken.
        let before = cap.0.borrow().iter().filter(|(t, _)| t.contains("BROKEN")).count();
        verify_pnl_writes(&path, "2026-07-16T18:00:00", &n).unwrap();
        let after = cap.0.borrow().iter().filter(|(t, _)| t.contains("BROKEN")).count();
        assert!(after > before, "breakage alarm must re-fire while the row stays broken");
    }

    #[test]
    fn sign_mismatch_is_flagged() {
        let (_f, path) = seed_conn();
        // pnl positive but pnl_pct negative = broken write
        insert(&path, "SKEW", "2026-07-16", "stopped_out", Some(9.0), Some(5.0), Some(-4.0));
        let cap = Capture::new();
        let n = cap.notifier();
        verify_pnl_writes(&path, "2026-07-16T12:00:00", &n).unwrap();
        assert!(cap.0.borrow().iter().any(|(t, _)| t.contains("BROKEN")), "sign mismatch must alarm");
    }

    #[test]
    fn pre_arm_rows_are_ignored_by_verifier() {
        let (_f, path) = seed_conn();
        // a pre-arm broken row (NULL pnl) must NOT trip the alarm — different experiment
        insert(&path, "OLDINTC", "2026-04-16", "stopped_out", None, None, None);
        let cap = Capture::new();
        let n = cap.notifier();
        verify_pnl_writes(&path, "2026-07-16T12:00:00", &n).unwrap();
        assert_eq!(cap.count(), 0, "pre-arm rows are out of scope — no alarm, no first-close");
    }

    #[test]
    fn edge_sample_announces_once_at_threshold() {
        let (_f, path) = seed_conn();
        // 19 resolved post-arm trades — below threshold, silent
        for i in 0..(MIN_SAMPLE - 1) {
            insert(&path, &format!("T{i}"), "2026-07-16", "closed", Some(11.0), Some(1.0), Some(10.0));
        }
        let cap = Capture::new();
        let n = cap.notifier();
        maybe_announce_edge_sample(&path, "2026-07-16T12:00:00", &n).unwrap();
        assert_eq!(cap.count(), 0, "below N={} must be silent", MIN_SAMPLE);

        // add the 20th → crosses threshold, announces once
        insert(&path, "T_final", "2026-07-16", "closed", Some(11.0), Some(1.0), Some(10.0));
        maybe_announce_edge_sample(&path, "2026-07-16T12:00:00", &n).unwrap();
        assert_eq!(cap.count(), 1, "crossing N={} announces once", MIN_SAMPLE);
        assert!(cap.last_body().contains("no auto go-live"), "must disclaim auto-verdict");

        // next run — latched, silent even though still >= threshold
        maybe_announce_edge_sample(&path, "2026-07-16T18:00:00", &n).unwrap();
        assert_eq!(cap.count(), 1, "announce is latched — fires exactly once");
    }

    // The composition test the two units missed individually: on a dataset holding
    // a NULL-pnl no-fill row, the verifier and the measurement must AGREE — the row
    // is flagged-for-review (verifier alarms) AND excluded-from-measurement
    // (edge-report + sample-counter don't count it). Without the NULL-exclusion fix,
    // edge-report would COALESCE it to a $0 flat trade: N padded, expectancy dragged.
    #[test]
    fn verifier_and_measurement_agree_on_null_pnl_row() {
        let (_f, path) = seed_conn();
        // two real post-arm closes (one win, one loss) + one phantom no-fill close
        insert(&path, "REALW", "2026-07-16", "closed", Some(12.0), Some(20.0), Some(20.0));
        insert(&path, "REALL", "2026-07-16", "stopped_out", Some(8.0), Some(-20.0), Some(-20.0));
        insert(&path, "GHOST", "2026-07-16", "closed", None, None, None); // 404/reconcile no-fill

        // Measurement side: the phantom is EXCLUDED — N counts only the two real outcomes.
        let rep = run_edge_report(&path).unwrap();
        assert_eq!(rep.resolved, 2, "edge-report must exclude the NULL-pnl row from N");
        assert_eq!(rep.wins, 1);
        assert_eq!(rep.losses, 1);
        // Expectancy reflects only real trades: +20% and -20% => 0%, NOT dragged by a phantom $0.
        assert!((rep.expectancy_pct).abs() < 1e-9, "expectancy = mean(+20,-20) = 0, not diluted");

        // Verifier side: the SAME phantom row IS flagged for review.
        let cap = Capture::new();
        let n = cap.notifier();
        verify_pnl_writes(&path, "2026-07-16T12:00:00", &n).unwrap();
        assert!(
            cap.0.borrow().iter().any(|(t, _)| t.contains("BROKEN")),
            "the row excluded from measurement must still be flagged by the verifier"
        );

        // Sample-counter side: counts the same set as edge-report — 2, not 3.
        // (Seed 18 more real closes to cross MIN_SAMPLE only via real rows; the phantom
        // must never be the row that trips the gate.)
        for i in 0..(MIN_SAMPLE - 2) {
            insert(&path, &format!("R{i}"), "2026-07-16", "closed", Some(11.0), Some(1.0), Some(10.0));
        }
        // Now: MIN_SAMPLE real + 1 phantom. Counter must announce (real count == MIN_SAMPLE).
        let cap2 = Capture::new();
        let n2 = cap2.notifier();
        maybe_announce_edge_sample(&path, "2026-07-16T12:00:00", &n2).unwrap();
        assert_eq!(cap2.count(), 1, "counter reaches N via real rows only, phantom excluded");

        // Sanity: with the phantom REMOVED the report is unchanged for the two originals —
        // i.e. its presence never contributed to the measured numbers.
        Connection::open(&path).unwrap()
            .execute("DELETE FROM paper_trades WHERE ticker = 'GHOST'", []).unwrap();
        let rep_no_ghost = run_edge_report(&path).unwrap();
        assert_eq!(rep_no_ghost.resolved, rep.resolved + (MIN_SAMPLE - 2),
            "removing the phantom changes nothing measured — it was never counted");
    }
}
