//! The go-live gate: a fixed checklist for "has the paper bot earned real money?".
//!
//! The thresholds are written down here, before the results are known, so they can't be
//! shifted to fit a good-looking stretch. Pulse never switches itself to real money: this only
//! reports whether the evidence is there, and the decision stays with the owner.
//!
//! Every number comes from the closed paper trades (merged ledger fragments and test trades
//! left out) and the daily account snapshots.

use rusqlite::Connection;
use serde::Serialize;

/// Closed trades needed before any average means anything. At ~9 points of spread per trade
/// (what the live scorecard shows), 100 trades give a standard error near 0.9 points.
pub const MIN_TRADES: usize = 100;
/// Calendar days between the first entry and the last exit: more than one market mood.
pub const MIN_TRACK_DAYS: i64 = 90;
/// Trades that carry a comparison with the S&P 500 needed to test the edge at all.
pub const MIN_COMPARED_TRADES: usize = 30;
/// t-statistic of the average per-trade return over the S&P 500. 2 is the usual bar for
/// "probably not luck"; it is a floor, not proof (trades that overlap in time are not fully
/// independent, which flatters it).
pub const MIN_T_STAT: f64 = 2.0;
/// Gross profit over gross loss.
pub const MIN_PROFIT_FACTOR: f64 = 1.3;
/// Deepest drop of the account from its high, in percent.
pub const MAX_DRAWDOWN_PCT: f64 = 15.0;

#[derive(Debug, Clone)]
pub struct TradeFact {
    pub pnl: f64,
    /// Return over the S&P 500 for the same days, when the learning run computed it.
    pub excess_pct: Option<f64>,
    pub entry_date: String,
    pub exit_date: String,
}

#[derive(Debug, Clone)]
pub struct GateInput {
    pub trades: Vec<TradeFact>,
    /// `None` when there is no account history yet: the check cannot pass without it.
    pub max_drawdown_pct: Option<f64>,
    /// Open, filled positions with no stop order at the broker.
    pub open_without_stop: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Criterion {
    pub key: &'static str,
    pub label: &'static str,
    pub passed: bool,
    pub current: String,
    pub target: String,
    /// One plain sentence on what it means.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GateReport {
    /// "ready" | "close" | "not_yet"
    pub status: &'static str,
    pub headline: String,
    pub passed: usize,
    pub total: usize,
    pub criteria: Vec<Criterion>,
}

fn day(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

/// Mean, and the t-statistic of the mean against zero. `None` below two values or with no spread.
fn mean_and_t(values: &[f64]) -> Option<(f64, f64)> {
    let n = values.len();
    if n < 2 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    let se = (var / n as f64).sqrt();
    (se > 0.0 && se.is_finite()).then(|| (mean, mean / se))
}

pub fn evaluate(input: &GateInput) -> GateReport {
    let n = input.trades.len();
    let mut criteria = Vec::new();

    criteria.push(Criterion {
        key: "trades",
        label: "Enough closed trades",
        passed: n >= MIN_TRADES,
        current: n.to_string(),
        target: format!("{MIN_TRADES}+"),
        detail: "Fewer trades than this can't tell skill from luck.".into(),
    });

    let first = input.trades.iter().filter_map(|t| day(&t.entry_date)).min();
    let last = input.trades.iter().filter_map(|t| day(&t.exit_date)).max();
    let days = match (first, last) {
        (Some(a), Some(b)) => (b - a).num_days().max(0),
        _ => 0,
    };
    criteria.push(Criterion {
        key: "track_record",
        label: "Long enough track record",
        passed: days >= MIN_TRACK_DAYS,
        current: format!("{days} days"),
        target: format!("{MIN_TRACK_DAYS}+ days"),
        detail: "A few good weeks in one kind of market prove little.".into(),
    });

    let net: f64 = input.trades.iter().map(|t| t.pnl).sum();
    criteria.push(Criterion {
        key: "net_profit",
        label: "Profitable after everything",
        passed: n > 0 && net > 0.0,
        current: format!("{}${:.0}", if net < 0.0 { "-" } else { "+" }, net.abs()),
        target: "above $0".into(),
        detail: "Total booked profit on closed trades, all legs included.".into(),
    });

    let excess: Vec<f64> = input
        .trades
        .iter()
        .filter_map(|t| t.excess_pct)
        .filter(|v| v.is_finite())
        .collect();
    let (edge_ok, edge_now, edge_detail) = if excess.len() < MIN_COMPARED_TRADES {
        (
            false,
            format!("{} compared", excess.len()),
            format!("Needs {MIN_COMPARED_TRADES} trades compared with the S&P 500 before the edge can be tested."),
        )
    } else {
        match mean_and_t(&excess) {
            Some((mean, t)) => (
                mean > 0.0 && t >= MIN_T_STAT,
                format!("{mean:+.2} pts, t = {t:.1}"),
                "Average return per trade over the S&P 500, and how many standard errors it sits above zero.".to_string(),
            ),
            None => (false, "no spread".into(), "The comparison has no variation to test.".to_string()),
        }
    };
    criteria.push(Criterion {
        key: "edge",
        label: "Beats the S&P 500, beyond luck",
        passed: edge_ok,
        current: edge_now,
        target: format!("above 0 and t ≥ {MIN_T_STAT:.0}"),
        detail: edge_detail,
    });

    let gross_win: f64 = input
        .trades
        .iter()
        .filter(|t| t.pnl > 0.0)
        .map(|t| t.pnl)
        .sum();
    let gross_loss: f64 = input
        .trades
        .iter()
        .filter(|t| t.pnl < 0.0)
        .map(|t| -t.pnl)
        .sum();
    let pf = if gross_loss > 0.0 {
        Some(gross_win / gross_loss)
    } else {
        None
    };
    criteria.push(Criterion {
        key: "profit_factor",
        label: "Wins clearly outweigh losses",
        passed: match pf {
            Some(v) => v >= MIN_PROFIT_FACTOR,
            None => n >= MIN_TRADES && gross_win > 0.0,
        },
        current: pf
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "no losses yet".into()),
        target: format!("{MIN_PROFIT_FACTOR:.1}+"),
        detail: "Dollars won per dollar lost.".into(),
    });

    criteria.push(Criterion {
        key: "drawdown",
        label: "Drawdown stayed small",
        passed: input.max_drawdown_pct.is_some_and(|d| d <= MAX_DRAWDOWN_PCT),
        current: input
            .max_drawdown_pct
            .map(|d| format!("{d:.1}%"))
            .unwrap_or_else(|| "no history yet".into()),
        target: format!("at most {MAX_DRAWDOWN_PCT:.0}%"),
        detail: "The deepest fall from the account's high so far.".into(),
    });

    criteria.push(Criterion {
        key: "stops",
        label: "Every position protected",
        passed: input.open_without_stop == 0,
        current: format!("{} without a stop", input.open_without_stop),
        target: "0".into(),
        detail: "Each open position must have its stop-loss at the broker.".into(),
    });

    let passed = criteria.iter().filter(|c| c.passed).count();
    let total = criteria.len();
    let (status, headline) = if passed == total {
        (
            "ready",
            "Every check is met. Real money is now a decision for you, not a given: start small."
                .to_string(),
        )
    } else if total - passed <= 2 {
        (
            "close",
            format!("{passed} of {total} checks met. Close, but not there yet."),
        )
    } else {
        (
            "not_yet",
            format!("{passed} of {total} checks met. Keep it on paper money."),
        )
    };
    GateReport {
        status,
        headline,
        passed,
        total,
        criteria,
    }
}

/// Read the facts from the database. Merged ledger fragments, trades marked as tests and closes
/// with no exit price (reconciled without a fill, so with no booked result) are left out. `pnl` is
/// the whole trade's dollars: every proper close sets it to the booked legs plus the last one.
pub fn load(conn: &Connection) -> rusqlite::Result<GateInput> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(pt.pnl, pt.realized_pnl), tr.excess_pct, pt.entry_date, COALESCE(pt.exit_date, '')
         FROM paper_trades pt
         LEFT JOIN trade_reviews tr ON tr.trade_id = pt.id
         WHERE pt.status IN ('closed', 'stopped_out')
           AND COALESCE(pt.exit_price, 0) > 0
           AND COALESCE(pt.pnl, pt.realized_pnl) IS NOT NULL
           AND COALESCE(pt.exit_reason, '') NOT LIKE 'merged_into%'
           AND json_extract(CASE WHEN json_valid(pt.entry_context) THEN pt.entry_context ELSE '{}' END, '$.event.test') IS NULL",
    )?;
    let trades = stmt
        .query_map([], |r| {
            Ok(TradeFact {
                pnl: r.get(0)?,
                excess_pct: r.get(1)?,
                entry_date: r.get(2)?,
                exit_date: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // No snapshots (or an unreadable table) is "unknown", never "0%".
    let max_drawdown_pct: Option<f64> = conn
        .query_row("SELECT MAX(drawdown_pct) FROM portfolio_snapshots", [], |r| r.get::<_, Option<f64>>(0))
        .unwrap_or(None);
    let open_without_stop: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM paper_trades
             WHERE status = 'open' AND order_status = 'filled' AND COALESCE(stop_order_id, '') = ''",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(GateInput {
        trades,
        max_drawdown_pct,
        open_without_stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(pnl: f64, excess: Option<f64>, entry: &str, exit: &str) -> TradeFact {
        TradeFact {
            pnl,
            excess_pct: excess,
            entry_date: entry.into(),
            exit_date: exit.into(),
        }
    }

    /// 120 trades over ~6 months, steady small edge with real spread.
    fn strong() -> GateInput {
        let trades = (0..120)
            .map(|i| {
                let wiggle = if i % 2 == 0 { 3.0 } else { -1.0 }; // mean +1.0, sd ~2
                trade(
                    wiggle * 50.0 + 10.0,
                    Some(wiggle),
                    "2026-01-05",
                    if i < 119 { "2026-06-01" } else { "2026-07-01" },
                )
            })
            .collect();
        GateInput {
            trades,
            max_drawdown_pct: Some(8.0),
            open_without_stop: 0,
        }
    }

    #[test]
    fn a_proven_record_is_ready() {
        let r = evaluate(&strong());
        assert_eq!(
            r.status,
            "ready",
            "{:?}",
            r.criteria.iter().filter(|c| !c.passed).collect::<Vec<_>>()
        );
        assert_eq!((r.passed, r.total), (7, 7));
    }

    #[test]
    fn too_few_trades_is_never_ready() {
        let mut g = strong();
        g.trades.truncate(40);
        let r = evaluate(&g);
        assert_ne!(r.status, "ready");
        assert!(
            !r.criteria
                .iter()
                .find(|c| c.key == "trades")
                .unwrap()
                .passed
        );
    }

    #[test]
    fn a_luck_sized_edge_does_not_pass() {
        // Positive average but huge spread: t well under 2.
        let trades = (0..120)
            .map(|i| {
                let e = if i % 2 == 0 { 12.0 } else { -10.0 };
                trade(e * 10.0 + 5.0, Some(e), "2026-01-05", "2026-07-01")
            })
            .collect();
        let r = evaluate(&GateInput {
            trades,
            max_drawdown_pct: Some(5.0),
            open_without_stop: 0,
        });
        let edge = r.criteria.iter().find(|c| c.key == "edge").unwrap();
        assert!(!edge.passed, "{}", edge.current);
    }

    #[test]
    fn the_edge_needs_enough_compared_trades() {
        let mut g = strong();
        for t in g.trades.iter_mut().skip(10) {
            t.excess_pct = None;
        }
        let edge = evaluate(&g)
            .criteria
            .into_iter()
            .find(|c| c.key == "edge")
            .unwrap();
        assert!(!edge.passed);
        assert!(edge.detail.contains("Needs 30"), "{}", edge.detail);
    }

    #[test]
    fn a_deep_drawdown_or_an_unprotected_position_blocks_it() {
        let mut g = strong();
        g.max_drawdown_pct = Some(22.0);
        assert_ne!(evaluate(&g).status, "ready");
        let mut g = strong();
        g.open_without_stop = 1;
        let r = evaluate(&g);
        assert_ne!(r.status, "ready");
        assert_eq!(r.status, "close");
    }

    #[test]
    fn the_present_record_is_not_ready() {
        // Roughly the live ledger: ~49 trades, net loss, 5 months.
        let trades = (0..49)
            .map(|i| {
                trade(
                    if i % 3 == 0 { 300.0 } else { -200.0 },
                    Some(if i % 3 == 0 { 4.0 } else { -3.0 }),
                    "2026-04-15",
                    "2026-10-09",
                )
            })
            .collect();
        let r = evaluate(&GateInput {
            trades,
            max_drawdown_pct: Some(8.5),
            open_without_stop: 0,
        });
        assert_eq!(r.status, "not_yet");
    }

    #[test]
    fn mean_and_t_handles_degenerate_input() {
        assert!(mean_and_t(&[]).is_none());
        assert!(mean_and_t(&[1.0]).is_none());
        assert!(mean_and_t(&[2.0, 2.0, 2.0]).is_none(), "no spread");
        let (m, t) = mean_and_t(&[1.0, 3.0]).unwrap();
        assert!((m - 2.0).abs() < 1e-12 && t > 0.0);
    }

    #[test]
    fn load_skips_merged_and_test_trades() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, status TEXT, realized_pnl REAL, pnl REAL, exit_price REAL, exit_reason TEXT,
                entry_context TEXT, entry_date TEXT, exit_date TEXT, order_status TEXT DEFAULT 'filled', stop_order_id TEXT);
             CREATE TABLE trade_reviews (trade_id INTEGER PRIMARY KEY, excess_pct REAL);
             CREATE TABLE portfolio_snapshots (drawdown_pct REAL);
             INSERT INTO paper_trades (id,status,pnl,exit_price,exit_reason,entry_context,entry_date,exit_date) VALUES
               (1,'closed',100,10,'trailing_stop','{}','2026-01-01','2026-01-10'),
               (2,'expired',0.12,10,'merged_into_1',NULL,'2026-01-01','2026-01-10'),
               (3,'closed',-5,10,'manual','{\"event\":{\"test\":true}}','2026-01-01','2026-01-10'),
               (4,'closed',50,10,'signal_decay',NULL,'2026-01-02','2026-01-12'),
               (6,'closed',-300,NULL,'reconcile_no_fill',NULL,'2026-01-02','2026-01-12');
             INSERT INTO paper_trades (id,status,realized_pnl,pnl,exit_price,exit_reason,entry_date,exit_date) VALUES
               (7,'closed',20,35,10,'manual_not_held','2026-01-02','2026-01-12');
             INSERT INTO paper_trades (id,status,pnl,entry_date,stop_order_id) VALUES (5,'open',0,'2026-02-01',NULL);
             INSERT INTO trade_reviews VALUES (1, 2.5);
             INSERT INTO portfolio_snapshots VALUES (4.0), (9.5), (2.0);",
        )
        .unwrap();
        let g = load(&conn).unwrap();
        assert_eq!(g.trades.len(), 3, "trades 1, 4 and 7 count; merged, test and no-exit-price rows do not");
        assert!(g.trades.iter().any(|t| t.pnl == 35.0), "the whole trade's pnl, not only the booked half");
        assert_eq!(g.max_drawdown_pct, Some(9.5));
        assert_eq!(g.open_without_stop, 1);
        assert_eq!(
            g.trades.iter().find(|t| t.pnl == 100.0).unwrap().excess_pct,
            Some(2.5)
        );
    }

    #[test]
    fn no_account_history_does_not_pass_the_drawdown_check() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE paper_trades (id INTEGER PRIMARY KEY, status TEXT, realized_pnl REAL, pnl REAL, exit_price REAL, exit_reason TEXT,
                entry_context TEXT, entry_date TEXT, exit_date TEXT, order_status TEXT DEFAULT 'filled', stop_order_id TEXT);
             CREATE TABLE trade_reviews (trade_id INTEGER PRIMARY KEY, excess_pct REAL);
             CREATE TABLE portfolio_snapshots (drawdown_pct REAL);",
        )
        .unwrap();
        let g = load(&conn).unwrap();
        assert_eq!(g.max_drawdown_pct, None);
        let report = evaluate(&g);
        let c = report.criteria.iter().find(|c| c.key == "drawdown").unwrap();
        assert!(!c.passed, "unknown drawdown must not read as 0%");
    }
}
