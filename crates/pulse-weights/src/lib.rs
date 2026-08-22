//! The compound-score weight vector, shared by the fetcher and the app.
//!
//! This crate exists because two crates need to agree on one vector and neither
//! may depend on the other: `src-tauri` pulling in `pulse-fetcher` would drag
//! the whole pipeline (reqwest, feed-rs, the sources) into the app binary, and
//! a second hand-typed copy of the numbers has already drifted once
//! (calibration-backtest-universe audit, 2026-07-23). Same pattern, same reason
//! as `pulse-pricing`.

/// Canonical dimension order. Every `[f64; 8]` weight or norm array in either
/// crate is indexed by this, so it is the one place the order is defined.
pub const DIMENSIONS: [&str; 8] = [
    "insider_signal",
    "institutional_flow",
    "news_momentum",
    "government_signal",
    "search_trend",
    "patent_signal",
    "supply_chain",
    "political_signal",
];

/// Why three more dimensions were zeroed on 2026-08-17, and why that RAISES scores.
///
/// A dark dimension is not neutral. It contributes `0.0 * weight`, which is
/// arithmetically identical to a real measurement of zero, so the compound score
/// is dragged down by the dark weight while the gates that read it —
/// `compound >= 0.40` for convergence, `> 0.6` for the top sizing tier — stay
/// fixed. Measured on the live DB: with political (0.2391), patent (0.0435) and
/// search (0.0543) dark, only 0.6631 of the weight could still be earned, so
/// clearing 0.40 required 60% of what was achievable where the design asked for
/// 40%. The gate had silently tightened by about a third, and nobody chose that.
///
/// Redistributing their 0.3369 proportionally across the three dimensions that
/// still carry data restores the designed strictness. It is a recalibration back
/// to intent, not a loosening.
///
/// It is NOT a throughput fix, and an earlier version of this comment claimed it
/// was. Recomputed from the stored dimension values of all 16,871 rows in the 30
/// days to 2026-08-17, convergence at the 0.40 gate goes 153 -> **62** (down),
/// and the `> 0.6` top sizing tier stays at 0 because the maximum achievable
/// compound under either vector is 0.4855. The superseded "100 -> 219, tier
/// 0 -> 2" figures were derived by multiplying every score by 1/0.6631 rather
/// than recomputing; a renormalisation multiplier is only valid for a candidate
/// whose whole score sits in the surviving dimensions, and `political_signal`
/// carried the joint-largest weight AND read high most often, so zeroing it
/// costs the top of the distribution more than renormalisation returns.
/// `AUTO_TRADE_ENABLED` is true, so the direction of this number matters.
///
/// Evidence for each, from `cross_signals` over the 30 days to 2026-08-17
/// (16,871 rows):
///
/// - `political_signal` — Senate LDA has 403'd on every run since 2026-08-03.
///   The cause is an Akamai edge policy covering every senate.gov host, which
///   reproduces from any network and ignores an API token, so it cannot be fixed
///   here. Stage 5 reads a 90-day window, so this one is still nonzero for ~18%
///   of rows and decays to exactly 0.0 around 2026-11-01. Zeroing it now simply
///   stops that decay from quietly re-tightening the gate week by week.
/// - `patent_signal` — nonzero in **0** rows. Google Patents still fetches
///   (20 articles on 2026-08-16) but stores no new stories, so the dimension has
///   been dark for longer than the LDA outage.
/// - `search_trend` — nonzero in 1–3 rows per day out of ~500. This is the one
///   that is genuinely **fixable**: Wikimedia Pageviews answered a live probe
///   (Nvidia 2024-06-01 = 11,432 views, daily granularity, no auth). Restoring it
///   is a backfill job, and its weight should come back with it.
///
/// Restore any of these the same way `institutional_flow` and `supply_chain` are
/// meant to be restored: fix the source first, confirm non-zero rows appear, then
/// put the weight back and re-normalise. `weights_sum_to_one` guards the sum.
///
/// Calibration weights stored in DB for persistence across runs.
/// Zeroed dimensions (no data source) excluded from compound score.
/// Active weights sum to 1.0.
/// Single source of truth for BOTH crates. `pulse-fetcher` derives its
/// positional array from this rather than hand-duplicating the numbers, and
/// `src-tauri` reads it to refuse a calibration batch that would resurrect a
/// dimension deliberately zeroed here. It lives in its own crate for exactly
/// that reason: the app must not depend on the whole fetcher to know one vector.
pub const DEFAULT_WEIGHTS: &[(&str, f64)] = &[
    ("insider_signal", 0.3606),
    ("institutional_flow", 0.0),    // ZEROED 2026-06-05: substring-match bug (ticker "X" = 61 funds); restore when fixed
    ("news_momentum", 0.3606),
    ("government_signal", 0.2787),
    ("search_trend", 0.0),          // ZEROED 2026-08-17: see below. FIXABLE — Wikimedia Pageviews verified live
    ("patent_signal", 0.0),         // ZEROED 2026-08-17: no entity has scored non-zero in 16,871 rows
    ("supply_chain", 0.0),          // ZEROED 2026-06-05: market-wide constant, no per-entity discriminative power
    ("political_signal", 0.0),      // ZEROED 2026-08-17: Senate LDA 403 since 2026-08-03, see below
];

/// Weight equality tolerance. These numbers round-trip through SQLite REAL and
/// through a JSON blob, so an exact `==` would flag a batch as stale over the
/// last bit of a float.
const EPS: f64 = 1e-9;

/// The code defaults as a positional array in `DIMENSIONS` order.
pub fn default_vector() -> [f64; 8] {
    let mut out = [0.0; 8];
    for (i, dim) in DIMENSIONS.iter().enumerate() {
        out[i] = DEFAULT_WEIGHTS
            .iter()
            .find(|(k, _)| k == dim)
            .map(|(_, v)| *v)
            .unwrap_or(0.0);
    }
    out
}

/// The effective weights after overlaying a stored override, plus the names of
/// any dimensions whose override was refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedWeights {
    pub weights: [f64; 8],
    /// Dimensions the override tried to give weight to that are zeroed in code.
    /// Non-empty means the stored override is stale and something should say so.
    pub clamped: Vec<String>,
}

/// Overlay a stored `calibrated_weights` override on the code defaults.
///
/// The invariant that makes this safe: **an override may never give non-zero
/// weight to a dimension the code has zeroed.** A zero in `DEFAULT_WEIGHTS` is
/// not a tuning choice, it is a statement that the dimension has no working data
/// source — `patent_signal` was nonzero in 0 of 16,871 rows, Senate LDA has
/// 403'd since 2026-08-03. Without the clamp a single stored override pins those
/// dimensions on forever, because the overlay is applied on top of the defaults
/// and so silently outranks any later decision to zero one in code.
///
/// The clamp is deliberately one-directional. An override may lower or raise a
/// dimension that is alive; it may not raise one that is dead.
pub fn resolve_overrides(pairs: &[(String, f64)]) -> ResolvedWeights {
    let defaults = default_vector();
    let mut weights = defaults;
    let mut clamped = Vec::new();
    for (key, val) in pairs {
        let Some(i) = DIMENSIONS.iter().position(|d| d == key) else {
            continue;
        };
        if defaults[i].abs() < EPS && val.abs() >= EPS {
            clamped.push(key.clone());
            continue;
        }
        weights[i] = *val;
    }
    ResolvedWeights { weights, clamped }
}

/// Why one dimension of a pending calibration batch cannot be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// The batch's `old_weight` snapshot no longer matches the code default, so
    /// the whole proposal was computed against a weight vector that has since
    /// been superseded — every number in it is scaled off a stale baseline.
    SupersededSnapshot,
    /// The batch would give weight to a dimension the code has zeroed.
    ResurrectsZeroed,
}

/// One dimension's objection, carrying the numbers needed to explain it.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleDimension {
    pub dimension: String,
    pub batch_old: f64,
    pub batch_new: f64,
    pub code_default: f64,
    pub reason: StaleReason,
}

/// Decide whether a pending calibration batch may be applied.
///
/// `rows` is `(dimension, old_weight, new_weight)` straight out of
/// `pending_calibration`. An empty return means the batch is applicable.
///
/// The comparand is `DEFAULT_WEIGHTS`, **not** the currently effective weights.
/// That is deliberate and it is the whole correctness of this check:
/// `calibration::adjust_weights` seeds its proposal from `DEFAULT_WEIGHTS` and
/// snapshots `old_weight` from the same place, never from a stored override. So
/// "does this batch's snapshot match the code defaults" is exactly "was this
/// batch computed under the rules in force now". Comparing against the effective
/// vector instead would false-flag every batch computed after any legitimate
/// apply — the guard would park calibration permanently.
///
/// Note that a batch computed AFTER a dimension is zeroed passes cleanly:
/// `adjust_weights` multiplies the seeded weight by `hit_rate / 0.5`, so a
/// zeroed dimension proposes `0.0 * adjustment = 0.0` and its snapshot is 0.0
/// too. This guard rejects superseded proposals, not all future ones.
pub fn stale_dimensions(rows: &[(String, f64, f64)]) -> Vec<StaleDimension> {
    let defaults = default_vector();
    let mut out = Vec::new();
    for (dimension, old, new) in rows {
        let Some(i) = DIMENSIONS.iter().position(|d| d == dimension) else {
            continue;
        };
        let code_default = defaults[i];
        let reason = if code_default.abs() < EPS && new.abs() >= EPS {
            Some(StaleReason::ResurrectsZeroed)
        } else if (old - code_default).abs() >= EPS {
            Some(StaleReason::SupersededSnapshot)
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(StaleDimension {
                dimension: dimension.clone(),
                batch_old: *old,
                batch_new: *new,
                code_default,
                reason,
            });
        }
    }
    out
}

/// Human-readable refusal, for a Tauri error string or a log line.
pub fn refusal_message(batch_id: &str, stale: &[StaleDimension]) -> String {
    let mut msg = format!(
        "Calibration batch '{}' was computed against a superseded weight vector and was NOT applied.\n",
        batch_id
    );
    for s in stale {
        match s.reason {
            StaleReason::ResurrectsZeroed => msg.push_str(&format!(
                "  {} — batch proposes {:.4}, but this dimension is zeroed in code (no working data source). Applying would silently restore it.\n",
                s.dimension, s.batch_new
            )),
            StaleReason::SupersededSnapshot => msg.push_str(&format!(
                "  {} — batch was computed from an old weight of {:.4}; the current default is {:.4}.\n",
                s.dimension, s.batch_old, s.code_default
            )),
        }
    }
    msg.push_str("Reject this batch. A fresh proposal will be computed on the next run that clears the resolved-trade threshold.");
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five batches sitting in `pending_calibration` on 2026-08-22, verbatim
    /// from batch `cal-1786920614-928938000`. This is the job the guard exists to
    /// refuse — a synthetic fixture would not have caught that `supply_chain` and
    /// `institutional_flow` are 0.0 -> 0.0 and must NOT be objected to.
    fn the_real_stale_batch() -> Vec<(String, f64, f64)> {
        [
            ("insider_signal", 0.2391, 0.284473527662106),
            ("institutional_flow", 0.0, 0.0),
            ("news_momentum", 0.2391, 0.189649018441404),
            ("government_signal", 0.1848, 0.219869125520524),
            ("search_trend", 0.0543, 0.0646044021415824),
            ("patent_signal", 0.0435, 0.0517549077929804),
            ("supply_chain", 0.0, 0.0),
            ("political_signal", 0.2391, 0.189649018441404),
        ]
        .iter()
        .map(|(d, o, n)| (d.to_string(), *o, *n))
        .collect()
    }

    #[test]
    fn the_real_pending_batch_is_refused() {
        let stale = stale_dimensions(&the_real_stale_batch());
        assert!(!stale.is_empty(), "the live batch must not be applicable");

        let named: Vec<&str> = stale.iter().map(|s| s.dimension.as_str()).collect();
        for dim in ["political_signal", "patent_signal", "search_trend"] {
            assert!(named.contains(&dim), "{} must be objected to", dim);
        }
        // These three would be resurrected outright — the expensive failure.
        for s in &stale {
            if ["political_signal", "patent_signal", "search_trend"].contains(&s.dimension.as_str()) {
                assert_eq!(s.reason, StaleReason::ResurrectsZeroed);
            }
        }
    }

    /// The batch's numbers sum to 1.0, which is why a sum check never caught it.
    /// If this ever fails the guard is being credited with work a cheaper check
    /// could have done.
    #[test]
    fn a_sum_check_would_not_have_caught_it() {
        let total: f64 = the_real_stale_batch().iter().map(|(_, _, n)| n).sum();
        assert!((total - 1.0).abs() < 1e-6, "sum was {}", total);
    }

    /// Dimensions that are dead in BOTH the batch and the code are not an
    /// objection. Without this the guard would refuse every batch forever, since
    /// `institutional_flow` and `supply_chain` have been 0.0 since 2026-06-05.
    #[test]
    fn a_dimension_dead_on_both_sides_is_not_an_objection() {
        let named: Vec<String> = stale_dimensions(&the_real_stale_batch())
            .into_iter()
            .map(|s| s.dimension)
            .collect();
        assert!(!named.contains(&"institutional_flow".to_string()));
        assert!(!named.contains(&"supply_chain".to_string()));
    }

    /// The positive case, and the one that proves the guard is not simply
    /// refusing everything: a proposal computed under the CURRENT defaults, the
    /// shape `adjust_weights` produces after the reweight — zeroed dimensions
    /// snapshot 0.0 and propose 0.0, live ones move.
    #[test]
    fn a_batch_computed_under_the_current_defaults_applies() {
        let d = default_vector();
        let rows: Vec<(String, f64, f64)> = DIMENSIONS
            .iter()
            .enumerate()
            .map(|(i, dim)| {
                let proposed = if d[i] > 0.0 { d[i] * 1.1 } else { 0.0 };
                (dim.to_string(), d[i], proposed)
            })
            .collect();
        assert!(
            stale_dimensions(&rows).is_empty(),
            "a fresh proposal must still be applicable — a guard that never \
             passes anything parks calibration forever"
        );
    }

    /// `adjust_weights` multiplies the seeded weight by `hit_rate / 0.5`. Zero
    /// times anything is zero, so this is the arithmetic the test above relies
    /// on, asserted directly.
    #[test]
    fn a_zeroed_dimension_cannot_propose_itself_back() {
        for adjustment in [0.0, 0.5, 1.0, 1.9] {
            assert_eq!(0.0_f64 * adjustment, 0.0);
        }
    }

    #[test]
    fn an_override_may_not_raise_a_dimension_the_code_zeroed() {
        let r = resolve_overrides(&[
            ("political_signal".to_string(), 0.2391),
            ("news_momentum".to_string(), 0.30),
        ]);
        let i_pol = DIMENSIONS.iter().position(|d| *d == "political_signal").unwrap();
        let i_news = DIMENSIONS.iter().position(|d| *d == "news_momentum").unwrap();
        assert_eq!(r.weights[i_pol], 0.0, "a dead dimension stays dead");
        assert_eq!(r.weights[i_news], 0.30, "a live one is still tunable");
        assert_eq!(r.clamped, vec!["political_signal".to_string()]);
    }

    /// The clamp is one-directional: an override may still turn a live dimension
    /// down, including to zero. Only raising a dead one is refused.
    #[test]
    fn an_override_may_still_lower_a_live_dimension() {
        let r = resolve_overrides(&[("insider_signal".to_string(), 0.0)]);
        let i = DIMENSIONS.iter().position(|d| *d == "insider_signal").unwrap();
        assert_eq!(r.weights[i], 0.0);
        assert!(r.clamped.is_empty());
    }

    #[test]
    fn no_override_is_the_defaults() {
        assert_eq!(resolve_overrides(&[]).weights, default_vector());
    }

    #[test]
    fn an_unknown_dimension_name_is_ignored_not_panicked_on() {
        let r = resolve_overrides(&[("nonexistent_signal".to_string(), 0.9)]);
        assert_eq!(r.weights, default_vector());
        assert!(stale_dimensions(&[("nonexistent_signal".to_string(), 0.9, 0.9)]).is_empty());
    }

    /// The live vector sums to 0.9999, not exactly 1.0 — the redistribution of
    /// the zeroed dimensions' weight was rounded to four places. A 1-in-10,000
    /// shortfall is far below anything the 0.40 gate can feel, and the tolerance
    /// here is the one the project already chose in `calibration.rs`. Tightening
    /// it would fail on the shipped vector.
    #[test]
    fn active_weights_sum_to_one() {
        let total: f64 = default_vector().iter().sum();
        assert!(
            (total - 1.0).abs() < 1e-3,
            "DEFAULT_WEIGHTS sum to {total}, not 1.0 — every gate is now quoted against the wrong scale"
        );
    }

    /// A dark dimension left at a small weight still drags the compound score
    /// down while the gates stay fixed, so zeroed must mean exactly 0.0.
    #[test]
    fn a_zeroed_dimension_is_exactly_zero() {
        for dim in ["institutional_flow", "search_trend", "patent_signal", "supply_chain", "political_signal"] {
            let i = DIMENSIONS.iter().position(|d| *d == dim).unwrap();
            assert_eq!(default_vector()[i], 0.0, "{dim} must be exactly zero");
        }
    }

    #[test]
    fn every_default_weight_key_is_a_known_dimension() {
        for (k, _) in DEFAULT_WEIGHTS {
            assert!(DIMENSIONS.contains(k), "{} is not in DIMENSIONS", k);
        }
        assert_eq!(DEFAULT_WEIGHTS.len(), DIMENSIONS.len());
    }

    #[test]
    fn the_refusal_names_the_dimension_and_both_numbers() {
        let msg = refusal_message("cal-1786920614-928938000", &stale_dimensions(&the_real_stale_batch()));
        assert!(msg.contains("political_signal"));
        assert!(msg.contains("cal-1786920614-928938000"));
        assert!(msg.contains("NOT applied"));
    }
}
