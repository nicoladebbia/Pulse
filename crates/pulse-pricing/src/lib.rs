//! Single source of truth for API pricing, shared by the Tauri app and the fetcher.
//!
//! Prices live in `pricing.json` next to `pulse.db` in the app data dir. The file is
//! read on EVERY cost estimate, so editing it takes effect on the next API call — no
//! rebuild. If the file is missing it is seeded from the compiled-in defaults below;
//! if it is unreadable/corrupt the defaults are used (the file is never overwritten
//! once it exists, so manual edits are safe).
//!
//! This crate replaced two hand-maintained duplicate tables (src-tauri
//! `api_usage.rs` + fetcher `db.rs`) that had drifted apart — one priced Haiku 4.5
//! at Haiku-3 rates (4x undercount), the other had no opus arm and logged $0.00.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceEntry {
    pub provider: String,
    /// Matched with `model.contains(model_prefix)`; first match wins, so keep
    /// more specific prefixes before generic ones.
    pub model_prefix: String,
    pub input_per_1m: f64,
    pub output_per_1m: f64,
}

fn defaults() -> Vec<PriceEntry> {
    let e = |provider: &str, prefix: &str, i: f64, o: f64| PriceEntry {
        provider: provider.into(),
        model_prefix: prefix.into(),
        input_per_1m: i,
        output_per_1m: o,
    };
    vec![
        // Anthropic (per 1M tokens, 2026-08)
        e("anthropic", "claude-haiku", 1.0, 5.0),
        e("anthropic", "claude-sonnet", 3.0, 15.0),
        e("anthropic", "claude-opus", 5.0, 25.0),
        // Groq. gpt-oss prices confirmed against console.groq.com/docs/models.md
        // 2026-08-22, the day Groq's Llama family stopped existing. The llama
        // entries are KEPT deliberately: no live call can hit them any more, but
        // `api_usage` rows written before 2026-08-17 still need to price.
        e("groq", "gpt-oss-120b", 0.15, 0.60),
        e("groq", "gpt-oss-20b", 0.075, 0.30),
        e("groq", "qwen", 0.15, 0.60),
        e("groq", "8b", 0.05, 0.08),
        e("groq", "scout", 0.11, 0.34),
        e("groq", "llama", 0.59, 0.79), // catch-all for other llama models (70B rates)
        // Voyage (embeddings: input only)
        e("voyage", "voyage", 0.01, 0.0),
    ]
}

/// Canonical location: `<data_dir>/com.pulse.app/pricing.json` (next to pulse.db).
pub fn pricing_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("com.pulse.app").join("pricing.json"))
}

/// Load the live price table. Seeds the file with defaults on first use.
pub fn load() -> Vec<PriceEntry> {
    let Some(path) = pricing_path() else {
        return defaults();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Vec<PriceEntry>>(&text) {
            Ok(entries) if !entries.is_empty() => entries,
            Ok(_) => {
                tracing::warn!("pricing.json is empty — using compiled-in defaults");
                defaults()
            }
            Err(e) => {
                tracing::warn!("pricing.json unparseable ({}) — using compiled-in defaults", e);
                defaults()
            }
        },
        Err(_) => {
            // First run: seed the file so there is something to edit.
            let d = defaults();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&d) {
                if std::fs::write(&path, json).is_ok() {
                    tracing::info!("seeded {} with default prices", path.display());
                }
            }
            d
        }
    }
}

/// Estimate cost in USD for a call, using the live table. Unknown provider/model
/// pairs cost 0.0 (free APIs are intentionally absent from the table).
pub fn estimate_cost(provider: &str, model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
    for p in load() {
        if p.provider == provider && model.contains(&p.model_prefix) {
            return (input_tokens as f64 * p.input_per_1m
                + output_tokens as f64 * p.output_per_1m)
                / 1_000_000.0;
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_price_current_models() {
        // Haiku 4.5 at $1/$5 — the exact undercount bug this crate replaced.
        let d = defaults();
        let cost = |prov: &str, model: &str| {
            d.iter()
                .find(|p| p.provider == prov && model.contains(&p.model_prefix))
                .map(|p| (p.input_per_1m, p.output_per_1m))
        };
        assert_eq!(cost("anthropic", "claude-haiku-4-5"), Some((1.0, 5.0)));
        assert_eq!(cost("anthropic", "claude-opus-4-7"), Some((5.0, 25.0)));
        assert_eq!(cost("groq", "llama-3.1-8b-instant"), Some((0.05, 0.08)));
        assert_eq!(cost("groq", "llama-3.3-70b-versatile"), Some((0.59, 0.79)));
        assert_eq!(cost("groq", "meta-llama/llama-4-scout-17b-16e-instruct"), Some((0.11, 0.34)));
        // The models that replaced the decommissioned Llama family. Without
        // these, every Groq call after 2026-08-22 logs $0.00 — the silent
        // undercount this crate exists to prevent, in a new coat.
        assert_eq!(cost("groq", "openai/gpt-oss-20b"), Some((0.075, 0.30)));
        assert_eq!(cost("groq", "openai/gpt-oss-120b"), Some((0.15, 0.60)));
        assert_eq!(cost("groq", "qwen/qwen3.6-27b"), Some((0.15, 0.60)));
        assert_eq!(cost("voyage", "voyage-3-lite"), Some((0.01, 0.0)));
        // Free APIs fall through to no entry.
        assert_eq!(cost("fred", "economic"), None);
    }

    /// The 20b and 120b prefixes differ by one character in the middle of the
    /// string, and 20b is 2x cheaper. A prefix-ordering slip here misprices
    /// every call rather than failing loudly.
    #[test]
    fn the_two_gpt_oss_sizes_do_not_match_each_other() {
        assert_eq!(estimate_from(&defaults(), "groq", "openai/gpt-oss-120b", 1_000_000, 0), 0.15);
        assert_eq!(estimate_from(&defaults(), "groq", "openai/gpt-oss-20b", 1_000_000, 0), 0.075);
    }

    #[test]
    fn first_match_wins_specific_before_generic() {
        // "8b" and "scout" must match before the generic "llama" catch-all.
        assert_eq!(
            estimate_from(&defaults(), "groq", "llama-3.1-8b-instant", 1_000_000, 0),
            0.05
        );
        assert_eq!(
            estimate_from(&defaults(), "groq", "meta-llama/llama-4-scout-17b-16e-instruct", 1_000_000, 0),
            0.11
        );
    }

    fn estimate_from(entries: &[PriceEntry], provider: &str, model: &str, i: i64, o: i64) -> f64 {
        for p in entries {
            if p.provider == provider && model.contains(&p.model_prefix) {
                return (i as f64 * p.input_per_1m + o as f64 * p.output_per_1m) / 1_000_000.0;
            }
        }
        0.0
    }
}

