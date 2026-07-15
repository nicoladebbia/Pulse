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

    // Resolved post-arm trades: (pnl_pct, pnl_dollars). date(entry_date) normalizes
    // both 'YYYY-MM-DD' and full-timestamp entry_date formats.
    let rows: Vec<(f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(pnl_pct, 0.0), COALESCE(pnl, 0.0)
             FROM paper_trades
             WHERE status IN ('closed', 'stopped_out')
               AND date(entry_date) >= ?1",
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
