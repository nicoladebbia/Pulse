//! Shared entity-noise filter.
//!
//! Federal filings, PACs, LLC/fund vehicles, and ALL-CAPS gov-payload names leak into
//! the entity tables via SEC/USASpending/FedReg mentions. They are not real news/market
//! subjects and must be dropped from user-facing lists (Trends radar, Signals convergence
//! watchlist). This is the SINGLE source of truth for that filter — do not paste divergent
//! copies (a duplicated filter is exactly how the two convergence engines drifted apart).

/// Generic garbage placeholder names.
const GARBAGE: &[&str] = &["n/a", "tbd", "unknown", "none", "other", "null", "test"];

/// Substring blocklist for federal/political/financial-filing entities.
const NOISE_SUBSTRINGS: &[&str] = &[
    "actblue",
    "national aeronautics",
    "general services administration",
    "general service administration",
    "federal register",
    "department of the treasury",
    "internal revenue service",
    "securities and exchange commission",
    " pac ",
    " pac,",
    "victory fund",
    "for michigan",
    "for america",
    "campaign committee",
    "(cik ",
    " llc ",
    " llp ",
    " l.p.",
    ", lp",
    ", inc.",
    ", inc ",
    " ventures",
    "fund i",
    "fund ii",
    "fund iii",
    "grassroots",
];

/// True if `name` is a noise entity that should be dropped from user-facing lists.
///
/// Drops: too-short/garbage placeholders, federal-filing/PAC/fund substrings, and
/// ALL-CAPS names of ≥3 tokens (the typical shape of USASpending/SEC payload names).
pub fn is_noise_entity(name: &str) -> bool {
    let t = name.to_lowercase();

    if t.len() < 2 || GARBAGE.contains(&t.as_str()) || t.contains("n/a") {
        return true;
    }

    // Federal filings / PACs / fund vehicles.
    let padded = format!(" {} ", t);
    if NOISE_SUBSTRINGS.iter().any(|s| padded.contains(s) || t.contains(s)) {
        return true;
    }

    // ALL-CAPS names of ≥3 tokens (USASpending/SEC payloads, e.g. "AIR INDUSTRIES GROUP").
    let alpha: String = name.chars().filter(|c| c.is_alphabetic()).collect();
    let is_all_caps = !alpha.is_empty() && alpha.chars().all(|c| c.is_uppercase());
    let token_count = name.split_whitespace().count();
    if is_all_caps && token_count >= 3 {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_the_audit_leak_entities() {
        // The exact entities that leaked into the Signals convergence watchlist.
        assert!(is_noise_entity("LS TECHNOLOGIES LLC"));
        assert!(is_noise_entity("AIR INDUSTRIES GROUP"));
        assert!(is_noise_entity("HUDSON TECHNOLOGIES INC /NY"));
        assert!(is_noise_entity("AIR INDUSTRIES GROUP  (CIK 0001009891)"));
    }

    #[test]
    fn drops_garbage_and_short() {
        assert!(is_noise_entity("n/a"));
        assert!(is_noise_entity("tbd"));
        assert!(is_noise_entity("x"));
        assert!(is_noise_entity("Some Fund II"));
    }

    #[test]
    fn keeps_real_companies_and_people() {
        // Real subjects must survive — including legitimately short names and
        // two-token ALL-CAPS (which the ≥3-token rule intentionally allows).
        assert!(!is_noise_entity("Arm"));
        assert!(!is_noise_entity("Giorgia Meloni"));
        assert!(!is_noise_entity("OpenAI"));
        assert!(!is_noise_entity("Nvidia"));
        assert!(!is_noise_entity("Inter Milan"));
        assert!(!is_noise_entity("Donald Trump"));
    }
}
