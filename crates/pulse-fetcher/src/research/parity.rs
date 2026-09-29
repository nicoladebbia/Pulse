//! `StrategyParams::live()` must describe the strategy that actually trades.
//!
//! The what-if backtester and the research lane's system card both read
//! `live()`; live scoring/trading read their own constants and literals. If
//! someone retunes live code and not `live()`, every paper would be judged — and
//! every backtest baselined — against a strategy that no longer exists. These
//! tests fail the build at that moment. Some checks read the live SOURCE because
//! the values are inline literals there, not named constants.

#[cfg(test)]
mod tests {
    use pulse_weights::StrategyParams;

    const SIGNALS_RS: &str = include_str!("../pipeline/signals.rs");
    const SIZING_RS: &str = include_str!("../position_sizing.rs");
    const TRADING_RS: &str = include_str!("../pipeline/trading.rs");

    #[test]
    fn votes_match_positive_dimensions() {
        let p = StrategyParams::live();
        // Deterministic sweep across the vote boundary for every dimension.
        let grid = [0.0, 0.29, 0.3, 0.3000001, 0.31, 0.8, 1.0];
        for i in 0..8 {
            for &a in &grid {
                for &b in &grid {
                    let mut norms = [0.0; 8];
                    norms[i] = a;
                    norms[(i + 2) % 8] = b;
                    let live = crate::pipeline::signals::positive_dimensions(&norms, &p.weights);
                    // Recreate the count via score(): converged with diversity 99
                    // and override disabled <=> positive >= min_positive.
                    let mut q = p.clone();
                    q.compound_override = 2.0;
                    for need in 1..=3 {
                        q.min_positive = need;
                        let (_, conv) = q.score(&norms, 99);
                        assert_eq!(conv, live >= need, "dim {i} a={a} b={b} need={need}");
                    }
                }
            }
        }
    }

    #[test]
    fn convergence_literals_match() {
        let p = StrategyParams::live();
        assert!(SIGNALS_RS.contains(".filter(|&(&v, &w)| w > 0.0 && v > 0.3)"));
        assert_eq!(p.positive_threshold, 0.3);
        assert!(SIGNALS_RS.contains(
            "let convergence = (positive >= 2 && *src_diversity >= 2) || compound >= 0.40;"
        ));
        assert_eq!((p.min_positive, p.min_diversity, p.compound_override), (2, 2, 0.40));
    }

    #[test]
    fn entry_and_sizing_literals_match() {
        let p = StrategyParams::live();
        assert!(TRADING_RS.contains("AND cs.compound_score > 0.3"));
        assert_eq!(p.min_score, 0.30);
        assert!(SIZING_RS.contains("pub const TOP_TIER_PCT: f64 = 0.10;"));
        assert!(SIZING_RS.contains("if score > 0.6 {\n        TOP_TIER_PCT\n    } else if score > 0.4 {\n        0.05\n    } else {\n        0.02\n    }"));
        let tiers: Vec<(f64, f64)> = p.sizing_tiers.iter().map(|t| (t.above_score, t.pct)).collect();
        assert_eq!(tiers[0], (0.6, 10.0));
        assert_eq!(tiers[1], (0.4, 5.0));
        assert_eq!(tiers[2].1, 2.0);
    }
}
