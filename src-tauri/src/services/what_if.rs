//! What-if backtester: baseline strategy vs a paper-proposed variant, under
//! identical conditions, on a train window and a held-out window.
//!
//! The stored `cross_signals.compound_score` was written under whatever weights
//! were live that day (they changed 2026-08-17), so neither arm replays it. Both
//! RE-SCORE every stored row from its stored normalised dimensions with
//! `StrategyParams::score` — verified to reproduce the stored compound on
//! 15,436/15,439 post-reweight rows, and re-checked on every run (`fidelity`).
//!
//! Honest-by-construction rules:
//!   - baseline and variant see the same rows, prices, windows and exits model;
//!   - the verdict is read on the HOLDOUT, and a thin sample reads INCONCLUSIVE;
//!   - the caller reports how many hypotheses have been tried on this history.
//!
//! Known blind spot: the fetcher only stores rows with compound >= 0.01 or
//! converged under the weights live that day, so a variant that would have
//! promoted a then-unstored row cannot see it.

use crate::services::backtester::{self, BacktestConfig, BacktestResult, SignalCandidate};
use pulse_weights::{StrategyDelta, StrategyParams};
use rusqlite::{params, Connection};
use serde::Serialize;

/// Fewer closed trades than this in either holdout arm → INCONCLUSIVE.
pub const MIN_HOLDOUT_TRADES: usize = 20;
/// Share of signal days used for the train window; the rest is holdout.
pub const TRAIN_FRACTION: f64 = 0.7;
/// Fidelity is checked on the most recent rows; below this match rate the
/// result carries a warning (weights changed recently, or scoring drifted).
const FIDELITY_WINDOW_DAYS: i64 = 14;
const FIDELITY_OK: f64 = 0.99;

struct Row {
    ticker: String,
    entity_name: String,
    signal_date: String,
    stored_compound: f64,
    norms: [f64; 8],
    diversity: i64,
}

fn load_rows(conn: &Connection, start: &str, end: &str) -> Result<Vec<Row>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(et.ticker, cs.ticker), COALESCE(e.name, cs.ticker, ''), date(cs.computed_at),
                    cs.compound_score,
                    cs.insider_signal, cs.institutional_flow, cs.news_momentum, cs.government_signal,
                    cs.search_trend, cs.patent_signal, cs.supply_chain, cs.political_signal,
                    COALESCE(cs.source_diversity, 0)
             FROM cross_signals cs
             LEFT JOIN entity_tickers et ON et.entity_id = cs.entity_id
             LEFT JOIN entities e ON e.id = cs.entity_id
             WHERE date(cs.computed_at) >= ?1 AND date(cs.computed_at) <= ?2
               AND COALESCE(et.ticker, cs.ticker) IS NOT NULL
             ORDER BY date(cs.computed_at) ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![start, end], |r| {
            let mut norms = [0.0; 8];
            for (i, n) in norms.iter_mut().enumerate() {
                *n = r.get::<_, Option<f64>>(4 + i)?.unwrap_or(0.0);
            }
            Ok(Row {
                ticker: r.get(0)?,
                entity_name: r.get(1)?,
                signal_date: r.get(2)?,
                stored_compound: r.get(3)?,
                norms,
                diversity: r.get(12)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Rescore rows under `p`: converged AND compound >= min_score, sized by tier.
fn candidates(rows: &[Row], p: &StrategyParams, from: &str, to: &str) -> Vec<SignalCandidate> {
    let mut out: Vec<SignalCandidate> = rows
        .iter()
        .filter(|r| r.signal_date.as_str() >= from && r.signal_date.as_str() <= to)
        .filter_map(|r| {
            let (compound, converged) = p.score(&r.norms, r.diversity);
            (converged && compound >= p.min_score).then(|| SignalCandidate {
                ticker: r.ticker.clone(),
                entity_name: r.entity_name.clone(),
                compound_score: compound,
                signal_date: r.signal_date.clone(),
                signal_profile: String::new(),
                size_pct: Some(p.size_pct(compound)),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.signal_date
            .cmp(&b.signal_date)
            .then(b.compound_score.partial_cmp(&a.compound_score).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

fn config(p: &StrategyParams, start: &str, end: &str) -> BacktestConfig {
    BacktestConfig {
        start_date: start.to_string(),
        end_date: end.to_string(),
        min_score: p.min_score,
        stop_loss_pct: p.stop_loss_pct,
        take_profit_pct: p.take_profit_pct,
        max_hold_days: p.max_hold_days,
        max_positions: p.max_positions,
        position_size_pct: 5.0, // unused: every candidate carries its tier size
        exit_model: crate::services::backtester::ExitModel::FixedPct,
        use_live_tiers: false,
        risk_sizing: None,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub signals: usize,
    pub trades: usize,
    pub open_at_end: usize,
    pub hit_rate: f64,
    pub avg_return_pct: f64,
    pub total_return_pct: f64,
    pub max_drawdown_pct: f64,
    pub sharpe: f64,
}

impl From<&BacktestResult> for Summary {
    fn from(r: &BacktestResult) -> Self {
        Summary {
            signals: r.total_signals,
            trades: r.trades_taken,
            open_at_end: r.open_at_end,
            hit_rate: r.hit_rate,
            avg_return_pct: r.avg_return_pct,
            total_return_pct: r.total_return_pct,
            max_drawdown_pct: r.max_drawdown_pct,
            sharpe: r.sharpe_ratio,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Arm {
    pub baseline: Summary,
    pub variant: Summary,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fidelity {
    pub rows_checked: usize,
    pub rows_matched: usize,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhatIfResult {
    pub train_window: (String, String),
    pub holdout_window: (String, String),
    pub train: Arm,
    pub holdout: Arm,
    /// "better" | "worse" | "mixed" | "inconclusive" — read on the holdout.
    pub verdict: String,
    pub verdict_reason: String,
    pub fidelity: Fidelity,
    /// Adjustments made to the delta (renormalisation, revived dimensions).
    pub notes: Vec<String>,
    pub variant_params: StrategyParams,
}

fn verdict(train: &Arm, holdout: &Arm) -> (String, String) {
    let (b, v) = (&holdout.baseline, &holdout.variant);
    if b.trades < MIN_HOLDOUT_TRADES || v.trades < MIN_HOLDOUT_TRADES {
        return (
            "inconclusive".into(),
            format!(
                "holdout has {} baseline and {} variant closed trades; {} each are needed before a difference means anything",
                b.trades, v.trades, MIN_HOLDOUT_TRADES
            ),
        );
    }
    let better = |b: &Summary, v: &Summary| v.sharpe > b.sharpe && v.total_return_pct > b.total_return_pct;
    let worse = |b: &Summary, v: &Summary| v.sharpe < b.sharpe && v.total_return_pct < b.total_return_pct;
    if better(b, v) && better(&train.baseline, &train.variant) {
        ("better".into(), "higher Sharpe and return on BOTH train and holdout".into())
    } else if worse(b, v) {
        ("worse".into(), "lower Sharpe and return on the holdout".into())
    } else {
        ("mixed".into(), "the windows or the metrics disagree — no reliable improvement".into())
    }
}

fn fidelity(conn: &Connection) -> Result<Fidelity, String> {
    let end: String = conn
        .query_row("SELECT COALESCE(MAX(date(computed_at)), '') FROM cross_signals", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if end.is_empty() {
        return Ok(Fidelity { rows_checked: 0, rows_matched: 0, ok: false });
    }
    let start: String = conn
        .query_row("SELECT date(?1, ?2)", params![end, format!("-{FIDELITY_WINDOW_DAYS} days")], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let live = StrategyParams::live();
    let rows = load_rows(conn, &start, &end)?;
    let matched = rows
        .iter()
        .filter(|r| (live.score(&r.norms, r.diversity).0 - r.stored_compound).abs() < 1e-6)
        .count();
    let ok = !rows.is_empty() && matched as f64 / rows.len() as f64 >= FIDELITY_OK;
    Ok(Fidelity { rows_checked: rows.len(), rows_matched: matched, ok })
}

pub fn run(conn: &Connection, delta: &StrategyDelta, start: &str, end: &str) -> Result<WhatIfResult, String> {
    let base = StrategyParams::live();
    let (variant, notes) = base.apply(delta)?;

    let rows = load_rows(conn, start, end)?;
    let mut days: Vec<&str> = rows.iter().map(|r| r.signal_date.as_str()).collect();
    days.dedup(); // rows are date-ordered
    if days.len() < 10 {
        return Err(format!("only {} signal days in {start}..{end}; need at least 10", days.len()));
    }
    let cut = ((days.len() as f64) * TRAIN_FRACTION).round() as usize;
    let cut = cut.clamp(1, days.len() - 1);
    let (train_from, train_to) = (days[0].to_string(), days[cut - 1].to_string());
    let (hold_from, hold_to) = (days[cut].to_string(), days[days.len() - 1].to_string());

    let sim = |p: &StrategyParams, from: &str, to: &str| -> Result<Summary, String> {
        let r = backtester::simulate(conn, &config(p, from, to), candidates(&rows, p, from, to))?;
        Ok(Summary::from(&r))
    };
    let train = Arm { baseline: sim(&base, &train_from, &train_to)?, variant: sim(&variant, &train_from, &train_to)? };
    let holdout = Arm { baseline: sim(&base, &hold_from, &hold_to)?, variant: sim(&variant, &hold_from, &hold_to)? };
    let (v, reason) = verdict(&train, &holdout);

    Ok(WhatIfResult {
        train_window: (train_from, train_to),
        holdout_window: (hold_from, hold_to),
        train,
        holdout,
        verdict: v,
        verdict_reason: reason,
        fidelity: fidelity(conn)?,
        notes,
        variant_params: variant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulse_weights::DimensionWeight;

    /// 40 trading days, 6 tickers. Each day every ticker has a cross_signals row;
    /// prices drift so trades resolve through SL/TP within the window.
    fn fixture() -> Connection {
        let conn = crate::db::connection::initialize_in_memory().unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let tickers = ["AAA", "BBB", "CCC", "DDD", "EEE", "FFF"];
        for d in 0..60i64 {
            let date = (start + chrono::Duration::days(d)).format("%Y-%m-%d").to_string();
            for (k, t) in tickers.iter().enumerate() {
                // Deterministic walk: up-trending names are odd k, down-trending even.
                let drift = if k % 2 == 1 { 0.012 } else { -0.009 };
                let wobble = ((d * 7 + k as i64 * 13) % 11) as f64 / 400.0 - 0.0125;
                let close = 100.0 * (1.0 + drift * d as f64 + wobble);
                conn.execute(
                    "INSERT INTO entity_prices (ticker, date, close, high, low) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![t, date, close, close * 1.03, close * 0.97],
                )
                .unwrap();
                if d < 40 {
                    // insider strong for odd k, news strong for everyone every 3rd day.
                    let insider = if k % 2 == 1 { 0.9 } else { 0.35 };
                    let news = if (d + k as i64) % 3 == 0 { 0.8 } else { 0.1 };
                    let gov = 0.2 + (k as f64) * 0.05;
                    let live = StrategyParams::live();
                    let norms = [insider, 0.0, news, gov, 0.0, 0.0, 0.0, 0.0];
                    let (compound, conv) = live.score(&norms, 2);
                    conn.execute(
                        "INSERT INTO cross_signals (ticker, compound_score, insider_signal, news_momentum,
                             government_signal, source_diversity, convergence_detected, computed_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, 2, ?6, ?7)",
                        params![t, compound, insider, news, gov, conv as i32, format!("{date} 08:00:00")],
                    )
                    .unwrap();
                }
            }
        }
        conn
    }

    #[test]
    fn noop_delta_equals_baseline_exactly() {
        let conn = fixture();
        let r = run(&conn, &StrategyDelta::default(), "2026-06-01", "2026-07-10").unwrap();
        assert_eq!(r.train.baseline, r.train.variant);
        assert_eq!(r.holdout.baseline, r.holdout.variant);
        assert!(r.train.baseline.trades > 0, "fixture must produce trades");
        assert!(r.notes.is_empty());
    }

    #[test]
    fn rescoring_reproduces_stored_scores() {
        let conn = fixture();
        let f = fidelity(&conn).unwrap();
        assert!(f.rows_checked > 0);
        assert_eq!(f.rows_checked, f.rows_matched);
        assert!(f.ok);
    }

    #[test]
    fn variant_changes_the_outcome_and_windows_do_not_overlap() {
        let conn = fixture();
        // All weight on insider: only the up-trending (odd) names converge by override.
        let d = StrategyDelta {
            weights: Some(vec![
                DimensionWeight { dimension: "insider_signal".into(), weight: 1.0 },
                DimensionWeight { dimension: "news_momentum".into(), weight: 0.0 },
                DimensionWeight { dimension: "government_signal".into(), weight: 0.0 },
                DimensionWeight { dimension: "search_trend".into(), weight: 0.0 },
            ]),
            ..Default::default()
        };
        let r = run(&conn, &d, "2026-06-01", "2026-07-10").unwrap();
        assert_ne!(r.train.baseline, r.train.variant);
        assert!(r.train_window.1 < r.holdout_window.0);
        // 12 holdout days of 6 names cannot reach 20 closed trades per arm.
        assert_eq!(r.verdict, "inconclusive", "{}", r.verdict_reason);
    }

    #[test]
    fn gate_nobody_passes_yields_zero_trades() {
        let conn = fixture();
        let d = StrategyDelta { min_score: Some(0.99), ..Default::default() };
        let r = run(&conn, &d, "2026-06-01", "2026-07-10").unwrap();
        assert_eq!(r.train.variant.trades, 0);
        assert_eq!(r.train.variant.signals, 0);
        assert_eq!(r.verdict, "inconclusive");
    }

    #[test]
    fn verdict_rules() {
        let s = |trades, sharpe, ret| Summary {
            signals: 100, trades, open_at_end: 0, hit_rate: 50.0, avg_return_pct: 0.0,
            total_return_pct: ret, max_drawdown_pct: 5.0, sharpe,
        };
        let arm = |b, v| Arm { baseline: b, variant: v };
        let good = arm(s(30, 0.5, 2.0), s(30, 0.9, 4.0));
        assert_eq!(verdict(&good, &good).0, "better");
        let bad = arm(s(30, 0.9, 4.0), s(30, 0.5, 2.0));
        assert_eq!(verdict(&good, &bad).0, "worse");
        assert_eq!(verdict(&bad, &good).0, "mixed", "holdout win but train loss is not 'better'");
        let thin = arm(s(19, 0.5, 2.0), s(30, 0.9, 4.0));
        assert_eq!(verdict(&good, &thin).0, "inconclusive");
    }

    /// Manual check against a real database COPY:
    /// `PULSE_WHATIF_DB=/path/to/copy.db cargo test -p pulse --lib what_if::tests::real_db -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_db() {
        let path = std::env::var("PULSE_WHATIF_DB").expect("PULSE_WHATIF_DB");
        let conn = Connection::open(path).unwrap();
        let (start, end): (String, String) = conn
            .query_row("SELECT MIN(date(computed_at)), MAX(date(computed_at)) FROM cross_signals", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let noop = run(&conn, &StrategyDelta::default(), &start, &end).unwrap();
        assert_eq!(noop.train.baseline, noop.train.variant);
        assert_eq!(noop.holdout.baseline, noop.holdout.variant);
        println!("fidelity {:?}", noop.fidelity);
        println!("train {:?} holdout {:?}", noop.train_window, noop.holdout_window);
        println!("baseline train   {:?}", noop.train.baseline);
        println!("baseline holdout {:?}", noop.holdout.baseline);
        let d = StrategyDelta { stop_loss_pct: Some(-6.0), ..Default::default() };
        let r = run(&conn, &d, &start, &end).unwrap();
        println!("SL -6%: holdout variant {:?}", r.holdout.variant);
        println!("verdict {} — {}", r.verdict, r.verdict_reason);
    }
}
