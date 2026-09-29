//! Risk-based position sizing, shared by the live trader and the backtester.
//!
//! The score tiers (2/5/10% of equity by compound score) size a trade by how
//! strong the signal looks. Measured on the live ledger through 2026-09-29, the
//! score does not predict returns, so the tiers were sizing on noise: ARM and
//! AIRI each reached ~$15.7k and together lost $4k.
//!
//! This sizes a trade by how much it can lose instead. Each entry risks a fixed
//! fraction of equity, measured to its stop, so a volatile name gets fewer
//! dollars and a quiet one more, and a stopped-out trade costs about the same
//! whichever name it was. Three multipliers move that budget:
//!
//! - **drawdown** shrinks it while the account is below its high-water mark;
//! - **edge** grows it (up to `max_edge_mult`) only once realized results show
//!   a positive expectancy that luck is unlikely to explain, and halves it when
//!   they show a negative one;
//! - **heat** trims it so total open risk across the book stays bounded.
//!
//! Everything here is pure arithmetic on numbers the caller measured. No I/O.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskParams {
    /// Fraction of equity a trade may lose if its stop is hit (0.005 = 0.5%).
    pub risk_per_trade: f64,
    /// Cap on the summed open risk of every position, as a fraction of equity.
    pub max_heat: f64,
    /// Cap on dollars in one ticker, as a fraction of equity. Includes
    /// scale-ins — the tiers' cap did not, which is how ARM reached 16%.
    pub max_ticker_pct: f64,
    /// Stop distance in ATRs. Matches the live trailing stop.
    pub atr_mult: f64,
    /// Narrowest stop distance used for sizing, as a fraction of price. A very
    /// quiet name would otherwise get an enormous position from a tiny ATR.
    pub min_stop_pct: f64,
    /// Widest stop distance, as a fraction of price. Matches the live -15%
    /// hard stop, which fires before a wider ATR stop would.
    pub max_stop_pct: f64,
    /// Stop distance when no ATR is available. Matches the live fixed -10%.
    pub fallback_stop_pct: f64,
    /// Drawdown (fraction) the account can carry before sizes start shrinking.
    pub dd_free: f64,
    /// Drawdown (fraction) at which sizes reach `dd_floor_mult`.
    pub dd_full: f64,
    /// Smallest drawdown multiplier.
    pub dd_floor_mult: f64,
    /// Fraction of full Kelly used once an edge is proven (0.5 = half Kelly).
    pub kelly_fraction: f64,
    /// Largest edge multiplier on `risk_per_trade`.
    pub max_edge_mult: f64,
    /// Edge multiplier when results show a negative expectancy.
    pub negative_edge_mult: f64,
    /// Closed trades needed before the edge gate reads the results at all.
    pub min_trades_for_edge: usize,
    /// Standard errors subtracted from the observed win rate before Kelly, so
    /// a lucky streak is not mistaken for an edge.
    pub win_rate_z: f64,
    /// A scale-in risks this fraction of a full entry's budget.
    pub scale_in_fraction: f64,
    /// Smallest order worth placing, in dollars.
    pub min_notional: f64,
}

impl Default for RiskParams {
    fn default() -> Self {
        RiskParams {
            risk_per_trade: 0.005,
            max_heat: 0.10,
            max_ticker_pct: 0.10,
            atr_mult: 3.0,
            min_stop_pct: 0.03,
            max_stop_pct: 0.15,
            fallback_stop_pct: 0.10,
            dd_free: 0.05,
            dd_full: 0.20,
            dd_floor_mult: 0.25,
            kelly_fraction: 0.5,
            max_edge_mult: 3.0,
            negative_edge_mult: 0.5,
            min_trades_for_edge: 30,
            win_rate_z: 1.0,
            scale_in_fraction: 0.5,
            min_notional: 50.0,
        }
    }
}

/// Realized results the edge gate reads. Returns are per-trade percentages;
/// only their ratio matters.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeStats {
    pub trades: usize,
    pub wins: usize,
    /// Mean return of winning trades, positive.
    pub avg_win: f64,
    /// Mean return of losing trades, as a positive number.
    pub avg_loss: f64,
}

impl EdgeStats {
    /// Build from per-trade returns. A 0.0 return counts as a loss: it paid
    /// costs and earned nothing.
    pub fn from_returns(returns: &[f64]) -> Self {
        let wins: Vec<f64> = returns.iter().copied().filter(|r| *r > 0.0).collect();
        let losses: Vec<f64> = returns.iter().copied().filter(|r| *r <= 0.0).map(f64::abs).collect();
        let mean = |v: &[f64]| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
        EdgeStats {
            trades: returns.len(),
            wins: wins.len(),
            avg_win: mean(&wins),
            avg_loss: mean(&losses),
        }
    }
}

/// Distance from entry to the stop, as a fraction of price.
pub fn stop_distance_pct(price: f64, atr: f64, p: &RiskParams) -> f64 {
    if price > 0.0 && atr > 0.0 && atr.is_finite() {
        (p.atr_mult * atr / price).clamp(p.min_stop_pct, p.max_stop_pct)
    } else {
        p.fallback_stop_pct
    }
}

/// 1.0 until drawdown passes `dd_free`, then linear down to `dd_floor_mult`
/// at `dd_full`, flat after. `drawdown` is a fraction (0.085 = 8.5%).
pub fn drawdown_multiplier(drawdown: f64, p: &RiskParams) -> f64 {
    let dd = if drawdown.is_finite() { drawdown.max(0.0) } else { 0.0 };
    if dd <= p.dd_free {
        return 1.0;
    }
    if dd >= p.dd_full {
        return p.dd_floor_mult;
    }
    let t = (dd - p.dd_free) / (p.dd_full - p.dd_free);
    1.0 - t * (1.0 - p.dd_floor_mult)
}

/// Kelly fraction for a win rate `q` and payoff ratio `b` (avg win / avg loss).
fn kelly(q: f64, b: f64) -> f64 {
    q - (1.0 - q) / b
}

/// How much to scale `risk_per_trade` by, given realized results.
///
/// - Fewer than `min_trades_for_edge` trades: 1.0. There is nothing to read.
/// - Kelly on the *observed* win rate is not positive: the results show a
///   losing strategy, so `negative_edge_mult`.
/// - Otherwise Kelly is recomputed on the win rate lowered by `win_rate_z`
///   standard errors, and `kelly_fraction` of that, expressed in units of
///   `risk_per_trade`, is the multiplier — floored at 1.0 (an edge that is
///   real but not yet provable leaves sizing at baseline) and capped at
///   `max_edge_mult`.
pub fn edge_multiplier(stats: &EdgeStats, p: &RiskParams) -> f64 {
    if stats.trades < p.min_trades_for_edge || stats.trades == 0 {
        return 1.0;
    }
    if stats.avg_loss <= 0.0 {
        // No losing trades at all over a meaningful sample. Payoff is
        // undefined; treat as the strongest edge the cap allows.
        return if stats.wins > 0 { p.max_edge_mult } else { 1.0 };
    }
    let b = stats.avg_win / stats.avg_loss;
    let n = stats.trades as f64;
    let q = stats.wins as f64 / n;
    if b <= 0.0 || kelly(q, b) <= 0.0 {
        return p.negative_edge_mult;
    }
    let q_low = (q - p.win_rate_z * (q * (1.0 - q) / n).sqrt()).max(0.0);
    let f = kelly(q_low, b);
    if f <= 0.0 {
        return 1.0;
    }
    (p.kelly_fraction * f / p.risk_per_trade).clamp(1.0, p.max_edge_mult)
}

/// What the book looks like when a new order is sized.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Book {
    pub equity: f64,
    pub buying_power: f64,
    /// Fraction below the high-water mark (0.085 = 8.5%).
    pub drawdown: f64,
    /// Summed dollars each open position would lose if stopped out now.
    pub open_risk: f64,
}

/// A sizing decision and the numbers behind it, so the caller can log why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sized {
    pub notional: f64,
    /// Dollars this order would lose at its stop.
    pub risk: f64,
    pub stop_pct: f64,
    pub dd_mult: f64,
    pub edge_mult: f64,
}

/// Size one order. `unit` is 1.0 for an entry and `scale_in_fraction` for a
/// scale-in. `existing_exposure` is dollars already held in the ticker.
///
/// `None` when the order would be below `min_notional` after every cap, or
/// the inputs make sizing meaningless (no equity, no price).
pub fn size_order(
    book: &Book,
    price: f64,
    atr: f64,
    existing_exposure: f64,
    edge: &EdgeStats,
    unit: f64,
    p: &RiskParams,
) -> Option<Sized> {
    // NaN fails `> 0.0` too, so a NaN equity or price sizes nothing.
    if book.equity.is_nan() || book.equity <= 0.0 || price.is_nan() || price <= 0.0 {
        return None;
    }
    let stop_pct = stop_distance_pct(price, atr, p);
    let dd_mult = drawdown_multiplier(book.drawdown, p);
    let edge_mult = edge_multiplier(edge, p);

    let wanted_risk = book.equity * p.risk_per_trade * dd_mult * edge_mult * unit;
    let heat_room = (book.equity * p.max_heat - book.open_risk.max(0.0)).max(0.0);
    let risk = wanted_risk.min(heat_room);

    let ticker_room = (book.equity * p.max_ticker_pct - existing_exposure.max(0.0)).max(0.0);
    let notional = (risk / stop_pct).min(ticker_room).min(book.buying_power.max(0.0));

    (notional >= p.min_notional && notional.is_finite()).then_some(Sized {
        notional,
        risk: notional * stop_pct,
        stop_pct,
        dd_mult,
        edge_mult,
    })
}

/// Dollars an open position loses if it is stopped out from `price`.
///
/// With a trailing stop, the distance to it — zero once the stop has moved
/// above the price, since the stop then sells at a profit. Without one, the
/// hard stop's worth of the position's value, the most the live exits allow.
pub fn position_risk(qty: f64, price: f64, trailing_stop: Option<f64>, p: &RiskParams) -> f64 {
    if qty.is_nan() || qty <= 0.0 || price.is_nan() || price <= 0.0 {
        return 0.0;
    }
    match trailing_stop {
        Some(stop) if stop > 0.0 && stop.is_finite() => qty * (price - stop).max(0.0),
        _ => qty * price * p.max_stop_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> RiskParams {
        RiskParams::default()
    }

    fn book() -> Book {
        Book { equity: 100_000.0, buying_power: 100_000.0, drawdown: 0.0, open_risk: 0.0 }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn a_trade_risks_its_budget_at_the_stop() {
        // $100 stock, ATR $2 -> 3 ATR = 6% stop. 0.5% of $100k = $500 risk.
        let s = size_order(&book(), 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        assert!(close(s.stop_pct, 0.06));
        assert!(close(s.notional, 500.0 / 0.06));
        assert!(close(s.risk, 500.0));
    }

    #[test]
    fn a_volatile_name_gets_fewer_dollars_for_the_same_risk() {
        // Both stops wide enough (6%, 12%) that the 10% ticker cap does not bind.
        let quiet = size_order(&book(), 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        let wild = size_order(&book(), 100.0, 4.0, 0.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        assert!(wild.notional < quiet.notional);
        assert!(close(wild.risk, quiet.risk));
    }

    #[test]
    fn stop_distance_is_clamped_and_falls_back_without_atr() {
        assert!(close(stop_distance_pct(100.0, 0.1, &p()), 0.03), "tiny ATR floors at 3%");
        assert!(close(stop_distance_pct(100.0, 20.0, &p()), 0.15), "huge ATR caps at the hard stop");
        assert!(close(stop_distance_pct(100.0, 0.0, &p()), 0.10), "no ATR uses the fixed stop");
        assert!(close(stop_distance_pct(100.0, f64::NAN, &p()), 0.10));
    }

    #[test]
    fn the_ticker_cap_counts_what_is_already_held() {
        // 3% stop wants $16.7k; cap is $10k, $7k already held -> $3k.
        let s = size_order(&book(), 100.0, 0.5, 7_000.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        assert!(close(s.notional, 3_000.0));
        assert!(size_order(&book(), 100.0, 0.5, 9_990.0, &EdgeStats::default(), 1.0, &p()).is_none());
    }

    #[test]
    fn heat_trims_the_last_trade_and_blocks_the_next() {
        let mut b = book();
        b.open_risk = 9_800.0; // $200 of the $10k heat left
        let s = size_order(&b, 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        assert!(close(s.risk, 200.0));
        b.open_risk = 10_000.0;
        assert!(size_order(&b, 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).is_none());
    }

    #[test]
    fn buying_power_bounds_the_order() {
        let mut b = book();
        b.buying_power = 1_000.0;
        let s = size_order(&b, 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        assert!(close(s.notional, 1_000.0));
    }

    #[test]
    fn drawdown_shrinks_size_linearly_then_floors() {
        assert!(close(drawdown_multiplier(0.03, &p()), 1.0));
        assert!(close(drawdown_multiplier(0.125, &p()), 0.625), "halfway between 5% and 20%");
        assert!(close(drawdown_multiplier(0.40, &p()), 0.25));
        assert!(close(drawdown_multiplier(-0.01, &p()), 1.0));
    }

    #[test]
    fn today_s_book_sizes_down_for_its_drawdown() {
        // 2026-09-29: 8.5% below the peak.
        assert!(close(drawdown_multiplier(0.085, &p()), 1.0 - (0.035 / 0.15) * 0.75));
    }

    #[test]
    fn too_few_trades_leave_sizing_at_baseline() {
        let lucky = EdgeStats { trades: 10, wins: 9, avg_win: 10.0, avg_loss: 2.0 };
        assert!(close(edge_multiplier(&lucky, &p()), 1.0));
    }

    #[test]
    fn a_losing_record_halves_the_risk() {
        // The live ledger on 2026-09-29: 30 trades, 10 wins, avg win 451 vs loss 531.
        let live = EdgeStats { trades: 30, wins: 10, avg_win: 451.0, avg_loss: 531.0 };
        assert!(close(edge_multiplier(&live, &p()), 0.5));
    }

    #[test]
    fn a_proven_edge_grows_size_up_to_the_cap() {
        // 60 trades, 50% wins, 2:1 payoff. Kelly on the lowered win rate
        // (0.5 - 0.0645) is ~0.218; half of that is 21.8x the 0.5% budget -> cap.
        let strong = EdgeStats { trades: 60, wins: 30, avg_win: 2.0, avg_loss: 1.0 };
        assert!(close(edge_multiplier(&strong, &p()), 3.0));
    }

    #[test]
    fn a_marginal_edge_that_luck_could_explain_does_not_grow_size() {
        // 52% wins, 1:1 payoff over 40 trades: Kelly on the observed rate is
        // positive, but on the lowered rate it is not.
        let thin = EdgeStats { trades: 40, wins: 21, avg_win: 1.0, avg_loss: 1.0 };
        assert!(close(edge_multiplier(&thin, &p()), 1.0));
    }

    #[test]
    fn a_small_proven_edge_scales_in_proportion() {
        let mut params = p();
        params.min_trades_for_edge = 30;
        // 200 trades, 55% wins, 1.1 payoff. q_low = 0.55 - sqrt(.2475/200) = 0.5148.
        // Kelly = 0.5148 - 0.4852/1.1 = 0.0737; half = 0.0369 -> 7.4x -> cap 3.0.
        // Lower the cap to see the proportion.
        params.max_edge_mult = 100.0;
        let s = EdgeStats { trades: 200, wins: 110, avg_win: 1.1, avg_loss: 1.0 };
        let q_low = 0.55 - (0.55f64 * 0.45 / 200.0).sqrt();
        let expected = 0.5 * (q_low - (1.0 - q_low) / 1.1) / 0.005;
        assert!(close(edge_multiplier(&s, &params), expected));
    }

    #[test]
    fn edge_stats_count_a_flat_trade_as_a_loss() {
        let s = EdgeStats::from_returns(&[5.0, -2.0, 0.0, 3.0]);
        assert_eq!((s.trades, s.wins), (4, 2));
        assert!(close(s.avg_win, 4.0));
        assert!(close(s.avg_loss, 1.0));
    }

    #[test]
    fn multipliers_compound_into_the_budget() {
        let mut b = book();
        b.drawdown = 0.125; // 0.625x
        let strong = EdgeStats { trades: 60, wins: 30, avg_win: 2.0, avg_loss: 1.0 }; // 3x
        // 15% stop keeps the $6,250 order under the $10k ticker cap.
        let s = size_order(&b, 100.0, 5.0, 0.0, &strong, 1.0, &p()).unwrap();
        assert!(close(s.risk, 100_000.0 * 0.005 * 0.625 * 3.0));
    }

    #[test]
    fn a_scale_in_risks_half_an_entry() {
        let full = size_order(&book(), 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).unwrap();
        let half = size_order(&book(), 100.0, 2.0, 0.0, &EdgeStats::default(), p().scale_in_fraction, &p()).unwrap();
        assert!(close(half.risk * 2.0, full.risk));
    }

    #[test]
    fn open_risk_is_the_distance_to_the_trailing_stop() {
        assert!(close(position_risk(10.0, 100.0, Some(94.0), &p()), 60.0));
        assert!(close(position_risk(10.0, 100.0, Some(101.0), &p()), 0.0), "stop above price locks a gain");
        assert!(close(position_risk(10.0, 100.0, None, &p()), 150.0), "no stop: the 15% hard stop");
        assert!(close(position_risk(0.0, 100.0, None, &p()), 0.0));
    }

    #[test]
    fn nonsense_inputs_size_nothing() {
        let mut b = book();
        b.equity = 0.0;
        assert!(size_order(&b, 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).is_none());
        assert!(size_order(&book(), 0.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).is_none());
        let mut b = book();
        b.equity = f64::NAN;
        assert!(size_order(&b, 100.0, 2.0, 0.0, &EdgeStats::default(), 1.0, &p()).is_none());
    }
}
