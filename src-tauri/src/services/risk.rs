//! Portfolio risk: how much is at stake right now, where it is concentrated, how
//! the book moves with the market, and how deep the account has fallen.
//!
//! Everything here is direction-aware. Alpaca reports a short's `market_value`
//! and `qty` as negative, so a signed sum is the net position and the absolute
//! values give gross exposure. The report is a pure function of its inputs; the
//! command in `commands::risk` gathers them.

use pulse_alpaca::DailyBar;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

/// Daily returns needed on both sides before a beta means anything.
const MIN_BETA_RETURNS: usize = 30;
/// SPY moves the stress test applies, in %.
const STRESS_MOVES: [f64; 4] = [-10.0, -5.0, 5.0, 10.0];
/// A single name above this share of equity gets a warning.
const BIG_POSITION_PCT: f64 = 10.0;
/// One industry above this share of equity (gross) gets a warning. Matches the
/// 30% cap the auto-trader applies to new buys.
const BIG_INDUSTRY_PCT: f64 = 30.0;
const UNKNOWN_INDUSTRY: &str = "Unknown";

/// One open position as Alpaca reports it.
#[derive(Debug, Clone)]
pub struct HeldPosition {
    pub symbol: String,
    pub qty: f64,
    pub price: f64,
    /// Negative for a short.
    pub market_value: f64,
    pub unrealized_pl: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionRisk {
    pub symbol: String,
    pub side: String,
    pub industry: String,
    pub market_value: f64,
    pub pct_of_equity: f64,
    pub unrealized_pl: f64,
    pub stop_price: Option<f64>,
    /// Dollars lost if the stop fills at its price. None without a stop.
    pub stop_loss_dollars: Option<f64>,
    pub beta: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndustryRisk {
    pub industry: String,
    pub count: usize,
    pub long_value: f64,
    pub short_value: f64,
    pub net_value: f64,
    /// Gross (long + short) as a share of equity.
    pub gross_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StressScenario {
    pub label: String,
    pub spy_move_pct: f64,
    pub pnl_dollars: f64,
    pub pnl_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DrawdownPoint {
    pub date: String,
    pub equity: f64,
    /// 0 at a new high, negative below it, in %.
    pub drawdown_pct: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DrawdownInfo {
    pub current_pct: f64,
    pub max_pct: f64,
    pub peak_equity: f64,
    pub peak_date: Option<String>,
    pub points: Vec<DrawdownPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskReport {
    pub equity: f64,
    pub cash: f64,
    pub long_value: f64,
    /// Positive number: the market value of all shorts.
    pub short_value: f64,
    pub gross_pct: f64,
    pub net_pct: f64,
    pub cash_pct: f64,
    pub position_count: usize,
    pub largest_position_pct: f64,
    pub positions: Vec<PositionRisk>,
    pub industries: Vec<IndustryRisk>,
    /// Sum of the loss if every held stop fills at its price.
    pub stop_loss_dollars: f64,
    pub stop_loss_pct: f64,
    pub positions_without_stop: usize,
    /// Net exposure weighted by each name's beta, as a share of equity.
    pub beta_exposure_pct: Option<f64>,
    /// Share of gross exposure whose beta could be measured.
    pub beta_coverage_pct: f64,
    pub stress: Vec<StressScenario>,
    pub drawdown: DrawdownInfo,
    pub warnings: Vec<String>,
}

pub struct RiskInputs<'a> {
    pub equity: f64,
    pub cash: f64,
    pub positions: &'a [HeldPosition],
    /// Ticker -> live broker stop price.
    pub stops: &'a HashMap<String, f64>,
    /// Ticker -> industry.
    pub industries: &'a HashMap<String, String>,
    /// Daily bars by symbol, including SPY. Oldest first.
    pub bars: &'a HashMap<String, Vec<DailyBar>>,
    /// (date, account value) snapshots, oldest first.
    pub equity_history: &'a [(String, f64)],
}

/// Beta of `asset` against `market` over the dates both have, from daily
/// close-to-close returns. None when there is too little overlap or the market
/// did not move.
pub fn beta(asset: &[DailyBar], market: &[DailyBar]) -> Option<f64> {
    let market_by_date: HashMap<&str, f64> =
        market.iter().map(|b| (b.date.as_str(), b.close)).collect();
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    let mut prev: Option<(f64, f64)> = None;
    for b in asset {
        let Some(&m) = market_by_date.get(b.date.as_str()) else {
            continue;
        };
        if !b.close.is_finite() || !m.is_finite() || b.close <= 0.0 || m <= 0.0 {
            continue;
        }
        if let Some((pa, pm)) = prev {
            pairs.push((b.close / pa - 1.0, m / pm - 1.0));
        }
        prev = Some((b.close, m));
    }
    if pairs.len() < MIN_BETA_RETURNS {
        return None;
    }
    let n = pairs.len() as f64;
    let mean_a = pairs.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_m = pairs.iter().map(|p| p.1).sum::<f64>() / n;
    let cov = pairs
        .iter()
        .map(|p| (p.0 - mean_a) * (p.1 - mean_m))
        .sum::<f64>()
        / n;
    let var = pairs.iter().map(|p| (p.1 - mean_m).powi(2)).sum::<f64>() / n;
    if var <= 0.0 || !var.is_finite() || !cov.is_finite() {
        return None;
    }
    Some(cov / var)
}

fn pct(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        part / whole * 100.0
    } else {
        0.0
    }
}

/// The snapshots plus today's live equity, so a new high that has not been
/// snapshotted yet does not read as a drawdown.
pub fn with_live_equity(history: &[(String, f64)], today: &str, equity: f64) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = history.iter().filter(|(d, _)| d.as_str() != today).cloned().collect();
    if equity.is_finite() && equity > 0.0 {
        out.push((today.to_string(), equity));
    }
    out
}

pub fn drawdown(history: &[(String, f64)]) -> DrawdownInfo {
    let mut info = DrawdownInfo::default();
    let mut peak = 0.0_f64;
    let mut peak_date: Option<String> = None;
    for (date, value) in history {
        if !value.is_finite() || *value <= 0.0 {
            continue;
        }
        if *value > peak {
            peak = *value;
            peak_date = Some(date.clone());
        }
        let dd = (value / peak - 1.0) * 100.0;
        if dd < info.max_pct {
            info.max_pct = dd;
        }
        info.current_pct = dd;
        info.points.push(DrawdownPoint {
            date: date.clone(),
            equity: *value,
            drawdown_pct: dd,
        });
    }
    info.peak_equity = peak;
    info.peak_date = peak_date;
    info
}

pub fn build_report(inp: &RiskInputs) -> RiskReport {
    let equity = if inp.equity.is_finite() && inp.equity > 0.0 {
        inp.equity
    } else {
        0.0
    };
    let market = inp.bars.get("SPY").map(Vec::as_slice).unwrap_or_default();

    let mut positions: Vec<PositionRisk> = inp
        .positions
        .iter()
        .map(|p| {
            let short = p.qty < 0.0 || p.market_value < 0.0;
            let stop = inp.stops.get(&p.symbol).copied().filter(|s| *s > 0.0);
            let stop_loss = stop.map(|s| {
                let per_share = if short { s - p.price } else { p.price - s };
                (per_share * p.qty.abs()).max(0.0)
            });
            PositionRisk {
                symbol: p.symbol.clone(),
                side: if short { "short" } else { "long" }.to_string(),
                industry: inp
                    .industries
                    .get(&p.symbol)
                    .cloned()
                    .unwrap_or_else(|| UNKNOWN_INDUSTRY.to_string()),
                market_value: p.market_value,
                pct_of_equity: pct(p.market_value.abs(), equity),
                unrealized_pl: p.unrealized_pl,
                stop_price: stop,
                stop_loss_dollars: stop_loss,
                beta: inp.bars.get(&p.symbol).and_then(|b| beta(b, market)),
            }
        })
        .collect();
    positions.sort_by(|a, b| {
        b.pct_of_equity
            .partial_cmp(&a.pct_of_equity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let long_value: f64 = positions
        .iter()
        .filter(|p| p.side == "long")
        .map(|p| p.market_value.abs())
        .sum();
    let short_value: f64 = positions
        .iter()
        .filter(|p| p.side == "short")
        .map(|p| p.market_value.abs())
        .sum();
    let gross = long_value + short_value;

    let mut by_industry: HashMap<String, IndustryRisk> = HashMap::new();
    for p in &positions {
        let e = by_industry
            .entry(p.industry.clone())
            .or_insert_with(|| IndustryRisk {
                industry: p.industry.clone(),
                count: 0,
                long_value: 0.0,
                short_value: 0.0,
                net_value: 0.0,
                gross_pct: 0.0,
            });
        e.count += 1;
        if p.side == "short" {
            e.short_value += p.market_value.abs();
        } else {
            e.long_value += p.market_value.abs();
        }
    }
    let mut industries: Vec<IndustryRisk> = by_industry
        .into_values()
        .map(|mut i| {
            i.net_value = i.long_value - i.short_value;
            i.gross_pct = pct(i.long_value + i.short_value, equity);
            i
        })
        .collect();
    industries.sort_by(|a, b| {
        b.gross_pct
            .partial_cmp(&a.gross_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let stop_loss_dollars: f64 = positions.iter().filter_map(|p| p.stop_loss_dollars).sum();
    let positions_without_stop = positions.iter().filter(|p| p.stop_price.is_none()).count();

    // Beta-weighted exposure; names without a beta count as 1.0 in the stress
    // test (a market-sized move) but are excluded from the headline figure so
    // it is never a guess.
    let measured_gross: f64 = positions
        .iter()
        .filter(|p| p.beta.is_some())
        .map(|p| p.market_value.abs())
        .sum();
    let beta_coverage_pct = pct(measured_gross, gross);
    let beta_exposure_pct = if positions.is_empty() || measured_gross <= 0.0 {
        None
    } else {
        let net: f64 = positions
            .iter()
            .filter_map(|p| p.beta.map(|b| b * p.market_value))
            .sum();
        Some(pct(net, equity))
    };
    let stress: Vec<StressScenario> = STRESS_MOVES
        .iter()
        .map(|mv| {
            let pnl: f64 = positions
                .iter()
                .map(|p| p.beta.unwrap_or(1.0) * p.market_value * mv / 100.0)
                .sum();
            StressScenario {
                label: format!("S&P 500 {}{:.0}%", if *mv >= 0.0 { "+" } else { "" }, mv),
                spy_move_pct: *mv,
                pnl_dollars: pnl,
                pnl_pct: pct(pnl, equity),
            }
        })
        .collect();

    let largest_position_pct = positions.first().map(|p| p.pct_of_equity).unwrap_or(0.0);
    let mut warnings = Vec::new();
    if let Some(p) = positions
        .first()
        .filter(|p| p.pct_of_equity > BIG_POSITION_PCT)
    {
        warnings.push(format!(
            "{} is {:.0}% of the account",
            p.symbol, p.pct_of_equity
        ));
    }
    for i in industries
        .iter()
        .filter(|i| i.industry != UNKNOWN_INDUSTRY && i.gross_pct > BIG_INDUSTRY_PCT)
    {
        warnings.push(format!(
            "{} is {:.0}% of the account",
            i.industry, i.gross_pct
        ));
    }
    if positions_without_stop > 0 {
        warnings.push(format!(
            "{} position(s) have no stop at the broker",
            positions_without_stop
        ));
    }
    if !positions.is_empty() && beta_coverage_pct < 100.0 {
        warnings.push(format!("Market sensitivity measured for {:.0}% of the book; the rest is assumed to move 1:1 with the S&P 500", beta_coverage_pct));
    }

    RiskReport {
        equity,
        cash: inp.cash,
        long_value,
        short_value,
        gross_pct: pct(gross, equity),
        net_pct: pct(long_value - short_value, equity),
        cash_pct: pct(inp.cash, equity),
        position_count: positions.len(),
        largest_position_pct,
        positions,
        industries,
        stop_loss_dollars,
        stop_loss_pct: pct(stop_loss_dollars, equity),
        positions_without_stop,
        beta_exposure_pct,
        beta_coverage_pct,
        stress,
        drawdown: drawdown(inp.equity_history),
        warnings,
    }
}

/// Live broker stop price per ticker for the open trades. A stop only counts
/// while its order id is set: the fetcher clears the id (not the price) when a
/// stop is cancelled or gone. With several open trades in one ticker the
/// tightest stop wins: highest for longs, lowest for shorts.
pub fn load_stops(conn: &Connection) -> rusqlite::Result<HashMap<String, f64>> {
    let mut stmt = conn.prepare(
        "SELECT ticker, direction, broker_stop_price FROM paper_trades
         WHERE status = 'open' AND stop_order_id IS NOT NULL AND broker_stop_price IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)))?;
    let mut out: HashMap<String, f64> = HashMap::new();
    for row in rows {
        let (ticker, direction, stop) = row?;
        if !stop.is_finite() || stop <= 0.0 {
            continue;
        }
        let short = direction == "short";
        out.entry(ticker)
            .and_modify(|cur| {
                if (short && stop < *cur) || (!short && stop > *cur) {
                    *cur = stop;
                }
            })
            .or_insert(stop);
    }
    Ok(out)
}

/// Cached industry per ticker (Finnhub profile, refreshed by the fetcher).
pub fn load_industries(conn: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT ticker, industry FROM ticker_industry WHERE industry IS NOT NULL AND industry != ''")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(date: String, close: f64) -> DailyBar {
        DailyBar {
            date,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        }
    }

    fn series(closes: &[f64]) -> Vec<DailyBar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| bar(format!("2026-{:02}-{:02}", 1 + i / 28, 1 + i % 28), *c))
            .collect()
    }

    /// 40 closes alternating up and down so returns are non-zero.
    fn market() -> Vec<DailyBar> {
        let mut c = 100.0;
        let closes: Vec<f64> = (0..40)
            .map(|i| {
                c *= if i % 2 == 0 { 1.01 } else { 0.995 };
                c
            })
            .collect();
        series(&closes)
    }

    fn pos(symbol: &str, qty: f64, price: f64) -> HeldPosition {
        HeldPosition {
            symbol: symbol.into(),
            qty,
            price,
            market_value: qty * price,
            unrealized_pl: 0.0,
        }
    }

    #[test]
    fn beta_of_the_market_against_itself_is_one() {
        let m = market();
        assert!((beta(&m, &m).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn beta_of_a_double_mover_is_two() {
        let m = market();
        let mut c = 100.0;
        let mut prev = m[0].close;
        let doubled: Vec<f64> = m
            .iter()
            .map(|b| {
                let r = b.close / prev - 1.0;
                prev = b.close;
                c *= 1.0 + 2.0 * r;
                c
            })
            .collect();
        let a = series(&doubled);
        assert!((beta(&a, &m).unwrap() - 2.0).abs() < 0.05);
    }

    #[test]
    fn beta_needs_enough_history_and_a_moving_market() {
        let m = market();
        assert!(beta(&m[..10], &m[..10]).is_none());
        let flat = series(&[100.0; 40]);
        assert!(beta(&m, &flat).is_none());
        assert!(beta(&[], &m).is_none());
    }

    #[test]
    fn beta_ignores_nan_closes() {
        let m = market();
        let mut a = m.clone();
        a[5].close = f64::NAN;
        let b = beta(&a, &m);
        assert!(b.map(|x| x.is_finite()).unwrap_or(true));
    }

    fn report(
        positions: &[HeldPosition],
        stops: &HashMap<String, f64>,
        industries: &HashMap<String, String>,
    ) -> RiskReport {
        let mut bars = HashMap::new();
        bars.insert("SPY".to_string(), market());
        bars.insert("AAA".to_string(), market());
        let history = vec![
            ("2026-01-01".to_string(), 100_000.0),
            ("2026-01-02".to_string(), 110_000.0),
            ("2026-01-03".to_string(), 99_000.0),
        ];
        build_report(&RiskInputs {
            equity: 100_000.0,
            cash: 50_000.0,
            positions,
            stops,
            industries,
            bars: &bars,
            equity_history: &history,
        })
    }

    #[test]
    fn long_and_short_exposure_are_separate() {
        let positions = [pos("AAA", 100.0, 200.0), pos("BBB", -50.0, 100.0)];
        let r = report(&positions, &HashMap::new(), &HashMap::new());
        assert_eq!(r.long_value, 20_000.0);
        assert_eq!(r.short_value, 5_000.0);
        assert!((r.gross_pct - 25.0).abs() < 1e-9);
        assert!((r.net_pct - 15.0).abs() < 1e-9);
        assert_eq!(
            r.positions.iter().find(|p| p.symbol == "BBB").unwrap().side,
            "short"
        );
    }

    #[test]
    fn stop_loss_mirrors_for_shorts() {
        let positions = [pos("AAA", 100.0, 200.0), pos("BBB", -50.0, 100.0)];
        let stops = HashMap::from([("AAA".to_string(), 180.0), ("BBB".to_string(), 110.0)]);
        let r = report(&positions, &stops, &HashMap::new());
        assert_eq!(r.stop_loss_dollars, 2_000.0 + 500.0);
        assert_eq!(r.positions_without_stop, 0);
    }

    #[test]
    fn a_stop_already_in_profit_costs_nothing() {
        // Long at 200 with the stop trailed up to 210: no loss left to take.
        let positions = [pos("AAA", 10.0, 200.0)];
        let stops = HashMap::from([("AAA".to_string(), 210.0)]);
        assert_eq!(
            report(&positions, &stops, &HashMap::new()).stop_loss_dollars,
            0.0
        );
    }

    #[test]
    fn missing_stops_are_counted_and_warned() {
        let r = report(&[pos("AAA", 10.0, 10.0)], &HashMap::new(), &HashMap::new());
        assert_eq!(r.positions_without_stop, 1);
        assert!(r.warnings.iter().any(|w| w.contains("no stop")));
    }

    #[test]
    fn industries_group_and_unknown_does_not_warn() {
        let positions = [
            pos("AAA", 100.0, 200.0),
            pos("CCC", 100.0, 150.0),
            pos("ZZZ", 100.0, 400.0),
        ];
        let ind = HashMap::from([
            ("AAA".to_string(), "Tech".to_string()),
            ("CCC".to_string(), "Tech".to_string()),
        ]);
        let r = report(&positions, &HashMap::new(), &ind);
        let tech = r.industries.iter().find(|i| i.industry == "Tech").unwrap();
        assert_eq!(tech.count, 2);
        assert!((tech.gross_pct - 35.0).abs() < 1e-9);
        assert!(r.warnings.iter().any(|w| w.contains("Tech")));
        assert!(!r.warnings.iter().any(|w| w.contains("Unknown")));
    }

    #[test]
    fn stress_loses_on_longs_and_gains_on_shorts_when_the_market_falls() {
        let long = report(
            &[pos("AAA", 100.0, 200.0)],
            &HashMap::new(),
            &HashMap::new(),
        );
        let down = long
            .stress
            .iter()
            .find(|s| s.spy_move_pct == -10.0)
            .unwrap();
        assert!(down.pnl_dollars < 0.0);
        let short = report(
            &[pos("BBB", -100.0, 200.0)],
            &HashMap::new(),
            &HashMap::new(),
        );
        let down = short
            .stress
            .iter()
            .find(|s| s.spy_move_pct == -10.0)
            .unwrap();
        assert!(down.pnl_dollars > 0.0);
    }

    #[test]
    fn drawdown_tracks_peak_and_current() {
        let d = drawdown(&[
            ("a".into(), 100.0),
            ("b".into(), 120.0),
            ("c".into(), 90.0),
            ("d".into(), 108.0),
        ]);
        assert_eq!(d.peak_equity, 120.0);
        assert_eq!(d.peak_date.as_deref(), Some("b"));
        assert!((d.max_pct - -25.0).abs() < 1e-9);
        assert!((d.current_pct - -10.0).abs() < 1e-9);
    }

    #[test]
    fn only_live_stops_count_and_the_tightest_wins() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE paper_trades (ticker TEXT, direction TEXT, status TEXT, stop_order_id TEXT, broker_stop_price REAL);
             INSERT INTO paper_trades VALUES ('AAA','long','open','o1',90.0), ('AAA','long','open','o2',95.0),
               ('BBB','short','open','o3',120.0), ('BBB','short','open','o4',110.0),
               ('CCC','long','open',NULL,50.0),
               ('DDD','long','closed','o5',70.0);",
        )
        .unwrap();
        let stops = load_stops(&c).unwrap();
        assert_eq!(stops.get("AAA"), Some(&95.0));
        assert_eq!(stops.get("BBB"), Some(&110.0));
        assert!(!stops.contains_key("CCC"), "a cancelled stop leaves its price behind");
        assert!(!stops.contains_key("DDD"));
    }

    #[test]
    fn live_equity_replaces_todays_snapshot() {
        let h = vec![("2026-10-09".to_string(), 100.0), ("2026-10-10".to_string(), 90.0)];
        let merged = with_live_equity(&h, "2026-10-10", 105.0);
        assert_eq!(merged, vec![("2026-10-09".to_string(), 100.0), ("2026-10-10".to_string(), 105.0)]);
        assert_eq!(drawdown(&merged).current_pct, 0.0);
        assert_eq!(with_live_equity(&h, "2026-10-11", f64::NAN).len(), 2);
    }

    #[test]
    fn empty_book_is_a_valid_report() {
        let r = report(&[], &HashMap::new(), &HashMap::new());
        assert_eq!(r.position_count, 0);
        assert!(r.beta_exposure_pct.is_none());
        assert_eq!(r.stop_loss_dollars, 0.0);
    }

    #[test]
    fn zero_equity_does_not_divide_by_zero() {
        let positions = [pos("AAA", 1.0, 1.0)];
        let bars = HashMap::new();
        let r = build_report(&RiskInputs {
            equity: 0.0,
            cash: 0.0,
            positions: &positions,
            stops: &HashMap::new(),
            industries: &HashMap::new(),
            bars: &bars,
            equity_history: &[],
        });
        assert!(r.gross_pct.is_finite());
    }
}
