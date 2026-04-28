//! Position sizing for auto-trades.
//!
//! Single source of truth for converting a compound signal score into a
//! dollar notional. Used by both the entry path (`auto_trade_on_convergence`)
//! and the scale-in path so that the two cannot drift apart.

const ENTRY_FLOOR: f64 = 50.0;
const ENTRY_CAP: f64 = 10_000.0;
const SCALE_IN_PCT: f64 = 0.01;
const SCALE_IN_CAP: f64 = 5_000.0;

/// Tier the compound score into a portfolio-percentage allocation.
fn tier_pct(score: f64) -> f64 {
    if score > 0.6 {
        0.05
    } else if score > 0.4 {
        0.02
    } else {
        0.01
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_high_score_uses_5pct() {
        let n = entry_notional(100_000.0, 0.7).unwrap();
        assert!((n - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_mid_score_uses_2pct() {
        let n = entry_notional(100_000.0, 0.5).unwrap();
        assert!((n - 2_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_low_score_uses_1pct() {
        let n = entry_notional(100_000.0, 0.35).unwrap();
        assert!((n - 1_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_respects_cap() {
        // 5% of $1M = $50K, must clamp to $10K
        let n = entry_notional(1_000_000.0, 0.7).unwrap();
        assert!((n - 10_000.0).abs() < 0.01);
    }

    #[test]
    fn entry_respects_floor() {
        // 1% of $200 = $2, must clamp up to $50
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
