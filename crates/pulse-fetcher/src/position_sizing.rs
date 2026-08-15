//! Position sizing for auto-trades.
//!
//! Single source of truth for converting a compound signal score into a
//! dollar notional. Used by both the entry path (`auto_trade_on_convergence`)
//! and the scale-in path so that the two cannot drift apart.

const ENTRY_FLOOR: f64 = 50.0;
const ENTRY_CAP: f64 = 20_000.0;
const SCALE_IN_PCT: f64 = 0.01;
const SCALE_IN_CAP: f64 = 5_000.0;

/// Tier the compound score into a portfolio-percentage allocation.
/// Long-term design (2026-07-23): doubled from 1%/2%/5% — fewer, bigger,
/// longer-held positions fit a conviction-based long-term approach better
/// than many small short-term-sized trades.
fn tier_pct(score: f64) -> f64 {
    if score > 0.6 {
        0.10
    } else if score > 0.4 {
        0.05
    } else {
        0.02
    }
}

/// Notional dollars to allocate for a new entry given buying power and the
/// compound score that gated the trade.
///
/// Returns `None` if buying power is below the minimum entry floor — the
/// caller should skip the trade rather than place an undersized order.
pub fn entry_notional(buying_power: f64, compound_score: f64) -> Option<f64> {
    if buying_power < ENTRY_FLOOR {
        return None;
    }
    let raw = buying_power * tier_pct(compound_score);
    Some(raw.clamp(ENTRY_FLOOR, ENTRY_CAP))
}

/// Notional dollars for a scale-in into an already-open winning position.
/// Conservative by design — fixed 1% of buying power, lower cap.
pub fn scale_in_notional(buying_power: f64) -> Option<f64> {
    if buying_power < ENTRY_FLOOR {
        return None;
    }
    let raw = buying_power * SCALE_IN_PCT;
    Some(raw.clamp(ENTRY_FLOOR, SCALE_IN_CAP))
}

/// Share-weighted average cost after adding `add_notional` at `add_price` to a
/// position of `held_notional` opened at `held_entry`.
///
/// Scale-in only fires on a position that is already up, so the added shares
/// always cost more than the original ones and the true basis always rises.
/// Leaving `entry_price` at the first fill is not a rounding error:
/// `pnl_pct` is measured off it, `pnl` multiplies that percentage by the
/// grown `position_size`, and the -15% hard stop and 3x-ATR profit target are
/// both struck from it. An understated basis therefore overstates the reported
/// gain and pushes the stop further away than the money at risk allows.
///
/// Returns `None` when either leg's price is unusable — the caller must then
/// leave the recorded basis alone rather than write a wrong one.
pub fn blended_entry_price(
    held_notional: f64,
    held_entry: f64,
    add_notional: f64,
    add_price: f64,
) -> Option<f64> {
    if held_notional <= 0.0 || held_entry <= 0.0 || add_notional <= 0.0 || add_price <= 0.0 {
        return None;
    }
    let shares = held_notional / held_entry + add_notional / add_price;
    if shares <= 0.0 || !shares.is_finite() {
        return None;
    }
    let basis = (held_notional + add_notional) / shares;
    basis.is_finite().then_some(basis)
}

#[cfg(test)]
mod basis_tests {
    use super::*;

    #[test]
    fn adding_higher_priced_shares_raises_the_basis() {
        // $5,000 at $100 = 50 shares. Add $2,500 at $110 = 22.727 shares.
        // $7,500 over 72.727 shares = $103.125.
        let basis = blended_entry_price(5_000.0, 100.0, 2_500.0, 110.0).unwrap();
        assert!((basis - 103.125).abs() < 0.001, "got {basis}");
    }

    #[test]
    fn the_understated_basis_is_worth_real_money() {
        // Same position, marked at $110. Reporting the original $100 entry
        // claims +10% on $7,500 = $750. The truth is +6.67% = $500.
        let basis = blended_entry_price(5_000.0, 100.0, 2_500.0, 110.0).unwrap();
        let honest_pct = (110.0 - basis) / basis * 100.0;
        let stale_pct = (110.0 - 100.0) / 100.0 * 100.0;
        assert!((honest_pct - 6.6667).abs() < 0.01, "got {honest_pct}");
        assert!(stale_pct - honest_pct > 3.0, "the gap is the overstatement");

        // And the -15% hard stop moves up with it: $85.00 off the stale entry,
        // $87.66 off the real one — 2.66 dollars of unintended risk per share.
        assert!((basis * 0.85 - 87.656).abs() < 0.01);
    }

    #[test]
    fn adding_at_the_same_price_leaves_the_basis_alone() {
        let basis = blended_entry_price(5_000.0, 100.0, 2_500.0, 100.0).unwrap();
        assert!((basis - 100.0).abs() < 0.001, "got {basis}");
    }

    #[test]
    fn an_unusable_price_yields_no_basis_rather_than_a_wrong_one() {
        assert!(blended_entry_price(5_000.0, 100.0, 2_500.0, 0.0).is_none());
        assert!(blended_entry_price(5_000.0, 0.0, 2_500.0, 110.0).is_none());
        assert!(blended_entry_price(0.0, 100.0, 2_500.0, 110.0).is_none());
        assert!(blended_entry_price(5_000.0, 100.0, 0.0, 110.0).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_high_score_uses_10pct() {
        let n = entry_notional(100_000.0, 0.7).unwrap();
        assert!((n - 10_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_mid_score_uses_5pct() {
        let n = entry_notional(100_000.0, 0.5).unwrap();
        assert!((n - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_low_score_uses_2pct() {
        let n = entry_notional(100_000.0, 0.35).unwrap();
        assert!((n - 2_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_respects_cap() {
        // 10% of $1M = $100K, must clamp to $20K
        let n = entry_notional(1_000_000.0, 0.7).unwrap();
        assert!((n - 20_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_respects_floor() {
        // 2% of $200 = $4, must clamp up to $50
        let n = entry_notional(200.0, 0.3).unwrap();
        assert!((n - 50.0).abs() < 0.01);
    }

    #[test]
    fn entry_skipped_when_underfunded() {
        assert!(entry_notional(49.99, 0.9).is_none());
    }

    #[test]
    fn scale_in_caps_at_5k() {
        let n = scale_in_notional(1_000_000.0).unwrap();
        assert!((n - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn scale_in_floored_at_50() {
        let n = scale_in_notional(500.0).unwrap();
        assert!((n - 50.0).abs() < 0.01);
    }
}
