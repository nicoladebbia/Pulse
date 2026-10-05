//! The trading strategy's tunable knobs as ONE value, for the what-if backtester
//! and the research lane.
//!
//! Deliberately NOT read by live scoring or live trading: those keep their own
//! constants (signals.rs, position_sizing.rs, position_management.rs), and a
//! parity test in pulse-fetcher asserts `StrategyParams::live()` still equals
//! them. A paper proposal is expressed as a `StrategyDelta` over `live()` and
//! can only ever reach a backtest — never an order.

use crate::{default_vector, DIMENSIONS};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SizingTier {
    /// Tier applies when compound score is strictly above this.
    pub above_score: f64,
    /// Percent of equity, e.g. 10.0.
    pub pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyParams {
    /// `DIMENSIONS` order.
    pub weights: [f64; 8],
    /// A dimension votes for convergence when its norm is above this (and its weight > 0).
    pub positive_threshold: f64,
    pub min_positive: usize,
    pub min_diversity: i64,
    /// Compound at or above this converges on its own.
    pub compound_override: f64,
    /// Entry needs compound >= this (live: `> 0.3`; the backtester's `>=` differs
    /// only on exact ties).
    pub min_score: f64,
    pub max_positions: usize,
    /// Highest threshold first; the last tier catches everything else.
    pub sizing_tiers: Vec<SizingTier>,
    /// The backtester's exits are a fixed-% PROXY of the live ATR trail (see
    /// src-tauri commands/trading.rs auto_backtest_if_due). Negative.
    pub stop_loss_pct: f64,
    pub take_profit_pct: f64,
    pub max_hold_days: i64,
}

impl StrategyParams {
    /// The live strategy, as the backtester can express it.
    pub fn live() -> Self {
        StrategyParams {
            weights: default_vector(),
            positive_threshold: 0.3,
            min_positive: 2,
            min_diversity: 2,
            compound_override: 0.40,
            min_score: 0.30,
            max_positions: 10,
            sizing_tiers: vec![
                SizingTier { above_score: 0.6, pct: 8.0 },
                SizingTier { above_score: 0.4, pct: 5.0 },
                SizingTier { above_score: f64::NEG_INFINITY, pct: 2.0 },
            ],
            stop_loss_pct: -10.0,
            take_profit_pct: 15.0,
            max_hold_days: 90,
        }
    }

    /// (compound, converged) for one stored cross_signals row. Mirrors
    /// pipeline/signals.rs compute_cross_signals: weighted sum flattened to
    /// [0,1] with NaN -> 0, convergence by live-dimension votes or override.
    pub fn score(&self, norms: &[f64; 8], source_diversity: i64) -> (f64, bool) {
        #[allow(clippy::manual_clamp)]
        let compound = norms
            .iter()
            .zip(self.weights.iter())
            .map(|(n, w)| n * w)
            .sum::<f64>()
            .max(0.0)
            .min(1.0);
        let positive = norms
            .iter()
            .zip(self.weights.iter())
            .filter(|&(&n, &w)| w > 0.0 && n > self.positive_threshold)
            .count();
        let converged = (positive >= self.min_positive && source_diversity >= self.min_diversity)
            || compound >= self.compound_override;
        (compound, converged)
    }

    pub fn size_pct(&self, compound: f64) -> f64 {
        self.sizing_tiers
            .iter()
            .find(|t| compound > t.above_score)
            .map(|t| t.pct)
            .unwrap_or(0.0)
    }

    /// Apply a delta. Returns the new params plus human-readable notes about
    /// anything adjusted (renormalisation) — never silently.
    pub fn apply(&self, d: &StrategyDelta) -> Result<(StrategyParams, Vec<String>), String> {
        let mut p = self.clone();
        let mut notes = Vec::new();

        if let Some(ws) = &d.weights {
            for dw in ws {
                let i = DIMENSIONS
                    .iter()
                    .position(|n| *n == dw.dimension)
                    .ok_or_else(|| format!("unknown dimension '{}'", dw.dimension))?;
                if !dw.weight.is_finite() || dw.weight < 0.0 {
                    return Err(format!("weight for {} must be a finite non-negative number", dw.dimension));
                }
                if p.weights[i] == 0.0 && dw.weight > 0.0 {
                    notes.push(format!(
                        "{} is zeroed live (its data is broken or constant); reviving it tests stored values that may be meaningless",
                        dw.dimension
                    ));
                }
                p.weights[i] = dw.weight;
            }
            let sum: f64 = p.weights.iter().sum();
            if sum <= 0.0 {
                return Err("weights sum to zero".into());
            }
            if (sum - 1.0).abs() > 1e-6 {
                for w in p.weights.iter_mut() {
                    *w /= sum;
                }
                notes.push(format!("weights summed to {sum:.4}; renormalised to 1"));
            }
        }
        let unit = |name: &str, v: f64| -> Result<f64, String> {
            if (0.0..=1.0).contains(&v) {
                Ok(v)
            } else {
                Err(format!("{name} must be in [0, 1], got {v}"))
            }
        };
        if let Some(v) = d.positive_threshold {
            p.positive_threshold = unit("positive_threshold", v)?;
        }
        if let Some(v) = d.compound_override {
            p.compound_override = unit("compound_override", v)?;
        }
        if let Some(v) = d.min_score {
            p.min_score = unit("min_score", v)?;
        }
        if let Some(v) = d.min_positive {
            p.min_positive = v.clamp(1, 8) as usize;
        }
        if let Some(v) = d.min_diversity {
            p.min_diversity = v.max(0);
        }
        if let Some(v) = d.max_positions {
            if v < 1 {
                return Err("max_positions must be >= 1".into());
            }
            p.max_positions = v as usize;
        }
        if let Some(v) = d.stop_loss_pct {
            if !(v < 0.0 && v > -100.0) {
                return Err(format!("stop_loss_pct must be in (-100, 0), got {v}"));
            }
            p.stop_loss_pct = v;
        }
        if let Some(v) = d.take_profit_pct {
            if v <= 0.0 {
                return Err(format!("take_profit_pct must be > 0, got {v}"));
            }
            p.take_profit_pct = v;
        }
        if let Some(v) = d.max_hold_days {
            if v < 1 {
                return Err("max_hold_days must be >= 1".into());
            }
            p.max_hold_days = v;
        }
        if let Some(tiers) = &d.sizing_tiers {
            if tiers.is_empty() || tiers.iter().any(|t| !(t.pct > 0.0 && t.pct <= 25.0)) {
                return Err("sizing tiers need 1+ tiers with pct in (0, 25]".into());
            }
            let mut t = tiers.clone();
            t.sort_by(|a, b| b.above_score.partial_cmp(&a.above_score).unwrap_or(std::cmp::Ordering::Equal));
            // The lowest tier must catch everything the entry gate lets through.
            if let Some(last) = t.last_mut() {
                last.above_score = f64::NEG_INFINITY;
            }
            p.sizing_tiers = t;
        }
        Ok((p, notes))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionWeight {
    pub dimension: String,
    pub weight: f64,
}

/// A change to `StrategyParams`. Every field optional; None = unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StrategyDelta {
    #[serde(default)]
    pub weights: Option<Vec<DimensionWeight>>,
    #[serde(default)]
    pub positive_threshold: Option<f64>,
    #[serde(default)]
    pub min_positive: Option<i64>,
    #[serde(default)]
    pub min_diversity: Option<i64>,
    #[serde(default)]
    pub compound_override: Option<f64>,
    #[serde(default)]
    pub min_score: Option<f64>,
    #[serde(default)]
    pub max_positions: Option<i64>,
    #[serde(default)]
    pub sizing_tiers: Option<Vec<SizingTier>>,
    #[serde(default)]
    pub stop_loss_pct: Option<f64>,
    #[serde(default)]
    pub take_profit_pct: Option<f64>,
    #[serde(default)]
    pub max_hold_days: Option<i64>,
}

impl StrategyDelta {
    pub fn is_noop(&self) -> bool {
        *self == StrategyDelta::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_score_matches_the_pipeline_formula() {
        let p = StrategyParams::live();
        // insider .8, news .5, gov .2 → only insider and news vote (> 0.3, weight > 0).
        let norms = [0.8, 0.9, 0.5, 0.2, 0.0, 0.9, 0.9, 0.9];
        let (c, conv) = p.score(&norms, 2);
        let expected = 0.8 * 0.3411 + 0.5 * 0.3411 + 0.2 * 0.2636;
        assert!((c - expected).abs() < 1e-12);
        assert!(conv, "2 live votes + diversity 2");
        // Zeroed dims at 0.9 do not vote: with diversity 1, no convergence unless override.
        let (c2, conv2) = p.score(&[0.0, 0.9, 0.0, 0.0, 0.0, 0.9, 0.9, 0.9], 5);
        assert_eq!(c2, 0.0);
        assert!(!conv2);
        // NaN flattens to 0, never propagates.
        let (c3, _) = p.score(&[f64::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0);
        assert_eq!(c3, 0.0);
    }

    #[test]
    fn noop_delta_is_identity() {
        let p = StrategyParams::live();
        let (q, notes) = p.apply(&StrategyDelta::default()).unwrap();
        assert_eq!(p, q);
        assert!(notes.is_empty());
    }

    #[test]
    fn weight_delta_renormalises_and_says_so() {
        let p = StrategyParams::live();
        let d = StrategyDelta {
            weights: Some(vec![DimensionWeight { dimension: "news_momentum".into(), weight: 0.6 }]),
            ..Default::default()
        };
        let (q, notes) = p.apply(&d).unwrap();
        assert!((q.weights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(q.weights[2] > p.weights[2]);
        assert!(notes.iter().any(|n| n.contains("renormalised")));
    }

    #[test]
    fn rejects_nonsense() {
        let p = StrategyParams::live();
        let bad = |d: StrategyDelta| p.apply(&d).is_err();
        assert!(bad(StrategyDelta {
            weights: Some(vec![DimensionWeight { dimension: "momentum".into(), weight: 0.5 }]),
            ..Default::default()
        }));
        assert!(bad(StrategyDelta { stop_loss_pct: Some(5.0), ..Default::default() }));
        assert!(bad(StrategyDelta { min_score: Some(1.5), ..Default::default() }));
        assert!(bad(StrategyDelta { sizing_tiers: Some(vec![]), ..Default::default() }));
    }

    #[test]
    fn reviving_a_zeroed_dimension_is_flagged() {
        let p = StrategyParams::live();
        let d = StrategyDelta {
            weights: Some(vec![DimensionWeight { dimension: "patent_signal".into(), weight: 0.1 }]),
            ..Default::default()
        };
        let (_, notes) = p.apply(&d).unwrap();
        assert!(notes.iter().any(|n| n.contains("patent_signal is zeroed live")));
    }

    #[test]
    fn sizing_tiers_follow_live() {
        let p = StrategyParams::live();
        assert_eq!(p.size_pct(0.61), 8.0);
        assert_eq!(p.size_pct(0.6), 5.0);
        assert_eq!(p.size_pct(0.41), 5.0);
        assert_eq!(p.size_pct(0.31), 2.0);
    }
}
