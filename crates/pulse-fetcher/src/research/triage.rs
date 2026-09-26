//! Triage: Haiku reads each new abstract against the system card and scores how
//! much the paper could change THIS trading system. Every paper gets a verdict,
//! including the skipped ones, so nothing disappears silently.

use super::anthropic::{self, TRIAGE_MODEL};
use super::ledger;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Papers at or above this get a deep read. 0-10 scale.
pub const READ_THRESHOLD: i64 = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub relevance: i64,
    pub component: String,
    pub testable_here: bool,
    pub data_feasible: bool,
    pub reason: String,
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["relevance", "component", "testable_here", "data_feasible", "reason"],
        "properties": {
            "relevance": { "type": "integer", "description": "0-10" },
            "component": {
                "type": "string",
                "enum": ["weights", "convergence_rule", "entry", "sizing", "exits",
                         "new_signal", "new_data", "evaluation_method", "none"]
            },
            "testable_here": { "type": "boolean" },
            "data_feasible": { "type": "boolean" },
            "reason": { "type": "string" }
        }
    })
}

const SYSTEM: &str = "You triage new quantitative-finance papers for one specific trading system, \
described below. Score RELEVANCE 0-10 = how likely a careful reading of this paper yields a \
concrete, evidence-backed change to THIS system (its signals, weights, convergence rule, entry, \
sizing, exits, or how it evaluates itself). Generic importance does not count: a famous \
result about price momentum scores low here because this system uses no price signals and \
cannot compute them. Score high only when the paper's idea maps onto what this system does or \
the data it has (insider filings, government contracts, regulatory events, news-mention \
acceleration, pageviews, multi-source convergence, daily bars for exits and sizing), or when \
it offers a method to evaluate such a system honestly on little data.\n\
Calibration: 0-2 unrelated (insurance, option pricing, crypto microstructure, macro theory); \
3-5 same broad area but needs data or machinery this system lacks; 6-7 a testable idea for \
one component; 8-10 directly about alternative-data / event-driven equity signals or their \
combination, with a change this system could test.\n\
`testable_here` = the change fits what the what-if backtester can express. \
`data_feasible` = this system already has, or could get for free, the data needed. \
`reason`: one or two sentences naming the specific component and why.\n\n";

pub fn params(card: &str, title: &str, categories: &str, abstract_text: &str) -> Value {
    let user = format!("Title: {title}\nCategories: {categories}\nAbstract: {abstract_text}");
    anthropic::params(
        TRIAGE_MODEL,
        &format!("{SYSTEM}{card}"),
        json!([{ "type": "text", "text": user }]),
        &schema(),
        600,
    )
}

/// Worst-case cost of one triage call, for the ledger reservation: the card is
/// ~1.5k tokens and abstracts ~300; priced at 3k in + 600 out, generously.
pub fn estimate() -> f64 {
    ledger::cost(TRIAGE_MODEL, anthropic::Usage { input_tokens: 3_000, output_tokens: 600 }, false)
}

/// Triage every paper still `new`. Returns (triaged, skipped). Stops early — not
/// an error — when the spend cap is reached; the rest wait for the next run.
pub async fn run(conn: &rusqlite::Connection, key: &str, card: &str, cap: f64) -> anyhow::Result<(usize, usize)> {
    let pending: Vec<(i64, String, String, String)> = conn
        .prepare("SELECT id, title, categories, abstract FROM research_papers WHERE status = 'new' ORDER BY published_at DESC")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;

    let (mut read, mut skipped) = (0, 0);
    for (id, title, cats, abs) in pending {
        if let Err(e) = ledger::reserve(conn, estimate(), cap) {
            tracing::warn!("research triage paused: {e}");
            break;
        }
        let reply = match anthropic::create(key, &params(card, &title, &cats, &abs)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("research triage failed for paper {id}: {e}");
                continue; // stays 'new', retried next run
            }
        };
        ledger::log(conn, TRIAGE_MODEL, "research_triage", reply.usage, false);
        let verdict: Verdict = match anthropic::json_of(&reply).and_then(|v| Ok(serde_json::from_value(v)?)) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("research triage unusable for paper {id}: {e}");
                continue;
            }
        };
        let relevance = verdict.relevance.clamp(0, 10);
        let status = if relevance >= READ_THRESHOLD { "triaged" } else { "skipped" };
        conn.execute(
            "UPDATE research_papers SET status = ?2, triage_score = ?3, triage_json = ?4, updated_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id, status, relevance, serde_json::to_string(&verdict)?],
        )?;
        if status == "triaged" {
            read += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((read, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_strict() {
        let s = schema();
        assert_eq!(s["additionalProperties"], false);
        let props: Vec<&str> = s["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        let req: Vec<&str> = s["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        for p in props {
            assert!(req.contains(&p), "{p} not required");
        }
    }

    #[test]
    fn triage_estimate_is_a_fraction_of_a_cent() {
        let e = estimate();
        assert!(e > 0.0 && e < 0.01, "{e}");
    }
}
