//! Deep read: the whole paper, one Opus call via the Batch API, returning a
//! study note (to learn from) and proposals (to test). Submitted in one fetcher
//! run, collected in a later one.

use super::anthropic::{self, BatchState, READ_MODEL};
use super::fulltext::{self, PaperText};
use super::ledger;
use base64::Engine;
use pulse_weights::{StrategyDelta, DIMENSIONS};
use serde_json::{json, Value};

/// Output budget for one read: thinking + a long structured note.
const MAX_TOKENS: u32 = 32_000;
/// Priced into the reservation. Real reads are logged at their true usage; this
/// only has to be a safe upper-middle guess so the cap is not overrun by a batch.
// (removed: reservations now price the MAX_TOKENS ceiling, see `estimate`)
/// Reads submitted per run.
pub const MAX_READS_PER_RUN: usize = 5;

fn nullable(t: Value) -> Value {
    json!({ "anyOf": [t, { "type": "null" }] })
}

fn obj(required: &[&str], properties: Value) -> Value {
    json!({ "type": "object", "additionalProperties": false, "required": required, "properties": properties })
}

fn str_list(fields: &[&str]) -> Value {
    let props: serde_json::Map<String, Value> =
        fields.iter().map(|f| (f.to_string(), json!({ "type": "string" }))).collect();
    json!({ "type": "array", "items": obj(fields, Value::Object(props)) })
}

fn delta_schema() -> Value {
    let num = || nullable(json!({ "type": "number" }));
    let int = || nullable(json!({ "type": "integer" }));
    obj(
        &["weights", "positive_threshold", "min_positive", "min_diversity", "compound_override",
          "min_score", "max_positions", "sizing_tiers", "stop_loss_pct", "take_profit_pct", "max_hold_days"],
        json!({
            "weights": nullable(json!({ "type": "array", "items": obj(&["dimension", "weight"], json!({
                "dimension": { "type": "string", "enum": DIMENSIONS },
                "weight": { "type": "number" }
            }))})),
            "positive_threshold": num(),
            "min_positive": int(),
            "min_diversity": int(),
            "compound_override": num(),
            "min_score": num(),
            "max_positions": int(),
            "sizing_tiers": nullable(json!({ "type": "array", "items": obj(&["above_score", "pct"], json!({
                "above_score": { "type": "number" },
                "pct": { "type": "number" }
            }))})),
            "stop_loss_pct": num(),
            "take_profit_pct": num(),
            "max_hold_days": int(),
        }),
    )
}

pub fn schema() -> Value {
    let severity = json!({ "type": "string", "enum": ["low", "medium", "high"] });
    obj(
        &["one_line", "problem", "method", "key_equations", "data", "results", "robustness",
          "critique", "evidence_strength", "transfers_to_pulse", "glossary", "proposals"],
        json!({
            "one_line": { "type": "string" },
            "problem": { "type": "string" },
            "method": { "type": "string" },
            "key_equations": str_list(&["latex", "meaning"]),
            "data": { "type": "string" },
            "results": str_list(&["claim", "number", "where"]),
            "robustness": { "type": "string" },
            "critique": { "type": "array", "items": obj(&["issue", "detail", "severity"], json!({
                "issue": { "type": "string", "enum": ["look_ahead_bias", "survivorship_bias",
                    "transaction_costs", "in_sample_only", "small_sample", "data_snooping",
                    "unrealistic_execution", "other"] },
                "detail": { "type": "string" },
                "severity": severity
            }))},
            "evidence_strength": { "type": "string", "enum": ["strong", "moderate", "weak"] },
            "transfers_to_pulse": { "type": "string" },
            "glossary": str_list(&["term", "definition"]),
            "proposals": { "type": "array", "items": obj(
                &["kind", "component", "title", "rationale", "falsifier", "delta", "spec_md"],
                json!({
                    "kind": { "type": "string", "enum": ["param", "new_signal", "data", "other"] },
                    "component": { "type": "string" },
                    "title": { "type": "string" },
                    "rationale": { "type": "string" },
                    "falsifier": { "type": "string" },
                    "delta": nullable(delta_schema()),
                    "spec_md": nullable(json!({ "type": "string" }))
                })
            )}
        }),
    )
}

const SYSTEM: &str = "You are reading a full quantitative-finance research paper for the owner of \
the trading system described below. He wants two things: to genuinely LEARN the paper (he is a \
student and builder, strong in code, still learning the finance and statistics), and to find \
changes that would improve HIS system. Read the whole paper, including appendices.\n\n\
Study note fields:\n\
- one_line: what the paper shows, in one plain sentence.\n\
- problem: the question and why it matters.\n\
- method: how it works, explained so a smart newcomer can follow — define every symbol you use.\n\
- key_equations: the 2-6 equations that carry the method, as LaTeX, each with its meaning in words.\n\
- data: universe, period, frequency, sources, sample size.\n\
- results: the headline results WITH the paper's numbers and where they appear (table/section).\n\
- robustness: what checks the authors ran and what they did not.\n\
- critique: be a hostile referee. Look-ahead bias, survivorship, costs, in-sample-only, small \
samples, data snooping, execution realism. Do not invent problems the paper does not have.\n\
- evidence_strength: how much to believe the headline result.\n\
- transfers_to_pulse: what, if anything, carries over to the system below, and what does not.\n\
- glossary: terms a newcomer would not know.\n\n\
Proposals: 0-3 concrete changes to the system below that the PAPER'S EVIDENCE supports. Zero is \
a correct answer for most papers — never propose a change the paper does not support. \
kind='param' only if expressible as a delta the backtester can test (see the card); then fill \
`delta` with ONLY the fields that change (others null; weights may list a subset of dimensions) \
and set spec_md null. For new_signal/data/other, delta is null and spec_md is an implementation \
spec a developer could act on: data source (must be free), computation, where it plugs in, how \
to validate it. `falsifier`: the backtest result that would show the idea does not work here.\n\n";

pub fn params(card: &str, title: &str, text: &PaperText) -> Value {
    let content = match text {
        PaperText::Html(body) => json!([
            { "type": "text", "text": format!("PAPER: {title}\n\n{body}") }
        ]),
        PaperText::Pdf(bytes) => json!([
            { "type": "document", "source": {
                "type": "base64", "media_type": "application/pdf",
                "data": base64::engine::general_purpose::STANDARD.encode(bytes)
            }},
            { "type": "text", "text": format!("The attached PDF is the paper \"{title}\".") }
        ]),
    };
    anthropic::params(READ_MODEL, &format!("{SYSTEM}{card}"), content, &schema(), MAX_TOKENS)
}

/// Worst-case price of one read: the full MAX_TOKENS output ceiling, so the cap
/// holds even when a reply runs long. Collect replaces it with the real cost.
pub fn estimate(input_tokens: i64) -> f64 {
    ledger::cost(READ_MODEL, anthropic::Usage { input_tokens, output_tokens: MAX_TOKENS as i64 }, true)
}

fn custom_id(paper_id: i64) -> String {
    format!("paper-{paper_id}")
}

fn paper_id_of(custom_id: &str) -> Option<i64> {
    custom_id.strip_prefix("paper-")?.parse().ok()
}

/// Fetch, price and submit up to MAX_READS_PER_RUN triaged papers as one batch.
pub async fn submit(conn: &rusqlite::Connection, key: &str, card: &str, cap: f64) -> anyhow::Result<usize> {
    let queue: Vec<(i64, String, String)> = conn
        .prepare(
            "SELECT id, arxiv_id, title FROM research_papers WHERE status = 'triaged'
             ORDER BY triage_score DESC, published_at DESC LIMIT ?1",
        )?
        .query_map([MAX_READS_PER_RUN as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;

    let mut batch: Vec<(String, Value)> = Vec::new();
    let mut estimates: Vec<(i64, f64)> = Vec::new();
    let mut committed = 0.0;
    for (id, arxiv_id, title) in queue {
        let text = match fulltext::fetch(&arxiv_id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("research: full text for {arxiv_id} failed: {e}");
                mark_failed(conn, id, &format!("full text: {e}"))?;
                continue;
            }
        };
        let p = params(card, &title, &text);
        let tokens = match anthropic::count_tokens(key, &p).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("research: count_tokens for {arxiv_id} failed: {e}");
                continue; // stays triaged
            }
        };
        let est = estimate(tokens);
        if let Err(e) = ledger::reserve(conn, committed + est, cap) {
            // A cheaper paper further down may still fit.
            tracing::warn!("research: {arxiv_id} deferred: {e}");
            continue;
        }
        committed += est;
        let (source, body) = match &text {
            PaperText::Html(b) => ("html", Some(b.as_str())),
            PaperText::Pdf(_) => ("pdf", None),
        };
        conn.execute(
            "INSERT OR REPLACE INTO paper_texts (paper_id, source, body) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, source, body],
        )?;
        batch.push((custom_id(id), p));
        estimates.push((id, est));
        tracing::info!("research: queued {arxiv_id} ({source}, {tokens} tokens, est ${est:.3})");
    }
    if batch.is_empty() {
        return Ok(0);
    }
    let batch_id = anthropic::submit_batch(key, &batch).await?;
    mark_reading(conn, &batch_id, &estimates)?;
    tracing::info!("research: submitted batch {batch_id} with {} reads (est ${committed:.3})", batch.len());
    Ok(batch.len())
}

/// All or none: a paper left 'triaged' after its batch was accepted would be
/// submitted, and paid for, a second time. `error` is deliberately kept: it is
/// how `collect` knows this is the retry, so the next failure is the last.
fn mark_reading(conn: &rusqlite::Connection, batch_id: &str, estimates: &[(i64, f64)]) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (id, est) in estimates {
        tx.execute(
            "UPDATE research_papers SET status = 'reading', read_batch_id = ?2, read_cost_estimate = ?3,
                    read_submitted_at = datetime('now'), updated_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id, batch_id, est],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn mark_failed(conn: &rusqlite::Connection, id: i64, err: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE research_papers SET status = 'failed', error = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, err],
    )?;
    Ok(())
}

/// Store one successful read and its proposals. Proposals whose delta does not
/// validate against `StrategyParams` are kept as spec-only with the reason
/// appended, never dropped.
pub fn store(conn: &rusqlite::Connection, paper_id: i64, study: &Value, usage: anthropic::Usage, cost: f64) -> anyhow::Result<i64> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO paper_reads (paper_id, model, study_json, input_tokens, output_tokens, cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![paper_id, READ_MODEL, study.to_string(), usage.input_tokens, usage.output_tokens, cost],
    )?;
    let read_id = tx.last_insert_rowid();
    let live = pulse_weights::StrategyParams::live();
    for p in study["proposals"].as_array().cloned().unwrap_or_default() {
        let mut kind = p["kind"].as_str().unwrap_or("other").to_string();
        let mut spec = p["spec_md"].as_str().map(str::to_string);
        let mut delta_json: Option<String> = None;
        if kind == "param" {
            match serde_json::from_value::<StrategyDelta>(p["delta"].clone()) {
                Ok(d) if d.is_noop() => {
                    kind = "other".into();
                    spec = Some(format!("{}\n\n(Marked param, but its delta changed nothing.)", spec.unwrap_or_default()));
                }
                Ok(d) => match live.apply(&d) {
                    Ok(_) => delta_json = Some(serde_json::to_string(&d)?),
                    Err(e) => {
                        kind = "other".into();
                        spec = Some(format!(
                            "{}\n\n(Proposed delta was invalid and cannot be backtested: {e}. Raw: {})",
                            spec.unwrap_or_default(),
                            p["delta"]
                        ));
                    }
                },
                Err(e) => {
                    kind = "other".into();
                    spec = Some(format!("{}\n\n(Unparseable delta: {e})", spec.unwrap_or_default()));
                }
            }
        }
        tx.execute(
            "INSERT INTO paper_proposals (paper_id, read_id, kind, component, title, rationale, falsifier, delta_json, spec_md)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                paper_id,
                read_id,
                kind,
                p["component"].as_str().unwrap_or(""),
                p["title"].as_str().unwrap_or("(untitled)"),
                p["rationale"].as_str().unwrap_or(""),
                p["falsifier"].as_str().unwrap_or(""),
                delta_json,
                spec,
            ],
        )?;
    }
    tx.execute(
        "UPDATE research_papers SET status = 'read', error = NULL, updated_at = datetime('now') WHERE id = ?1",
        [paper_id],
    )?;
    tx.commit()?;
    Ok(read_id)
}

/// Collect every ended batch. A failed read goes back to `triaged` once (retried
/// next run); a second failure marks it `failed` with the reason.
pub async fn collect(conn: &rusqlite::Connection, key: &str) -> anyhow::Result<usize> {
    let batches: Vec<String> = conn
        .prepare("SELECT DISTINCT read_batch_id FROM research_papers WHERE status = 'reading' AND read_batch_id IS NOT NULL")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut stored = 0;
    for batch_id in batches {
        // A failed poll skips this batch until the next run; it must not abort
        // the run (and with it every new submission).
        let url = match anthropic::batch_state(key, &batch_id).await {
            Ok(BatchState::InProgress) => continue,
            Ok(BatchState::Ended(url)) => url,
            Err(e) => {
                tracing::warn!("research: polling {batch_id} failed: {e}");
                continue;
            }
        };
        let results = match anthropic::batch_results(key, &url).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("research: results of {batch_id} failed: {e}");
                continue;
            }
        };
        for (cid, outcome) in results {
            let Some(id) = paper_id_of(&cid) else { continue };
            let parsed = outcome.map_err(|e| anyhow::anyhow!(e)).and_then(|reply| {
                let cost = ledger::log(conn, READ_MODEL, "research_read", reply.usage, true);
                anthropic::json_of(&reply).map(|v| (v, reply.usage, cost))
            });
            match parsed {
                Ok((study, usage, cost)) => {
                    store(conn, id, &study, usage, cost)?;
                    stored += 1;
                }
                Err(e) => {
                    retry_or_fail(conn, id, &e.to_string())?;
                    tracing::warn!("research: read of paper {id} failed: {e}");
                }
            }
        }
        // Anything still 'reading' in an ended batch had no result line at all.
        let orphans: Vec<i64> = conn
            .prepare("SELECT id FROM research_papers WHERE status = 'reading' AND read_batch_id = ?1")?
            .query_map([&batch_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in orphans {
            retry_or_fail(conn, id, "batch ended without a result for this paper")?;
        }
    }
    fail_stale(conn)?;
    Ok(stored)
}

/// A batch that could not be polled for STALE_READING_DAYS (deleted, key or
/// workspace changed) will never be collected. Fail it rather than resubmit:
/// its results may still exist, and a resubmit would pay twice. Re-queue by hand.
fn fail_stale(conn: &rusqlite::Connection) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "UPDATE research_papers SET status = 'failed',
                error = 'batch not collectable after ' || ?1 || ' days (' || read_batch_id || ')',
                updated_at = datetime('now')
         WHERE status = 'reading' AND read_submitted_at <= datetime('now', '-' || ?1 || ' days')",
        [STALE_READING_DAYS],
    )?)
}

/// Batch results are kept 29 days; a batch finishes within 1. A week unpolled is lost.
const STALE_READING_DAYS: i64 = 7;

/// First failed read goes back to `triaged` for one retry; the second is final.
/// `error` doubles as the "already retried" marker: `submit` never clears it and
/// only a successful `store` (or a manual re-queue) does.
fn retry_or_fail(conn: &rusqlite::Connection, id: i64, err: &str) -> anyhow::Result<()> {
    let prior: Option<String> = conn.query_row("SELECT error FROM research_papers WHERE id = ?1", [id], |r| r.get(0))?;
    if prior.is_some() {
        mark_failed(conn, id, err)
    } else {
        conn.execute(
            "UPDATE research_papers SET status = 'triaged', read_batch_id = NULL, read_cost_estimate = NULL,
                    error = ?2, updated_at = datetime('now') WHERE id = ?1",
            rusqlite::params![id, err],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_triaged() -> (rusqlite::Connection, i64) {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&c).unwrap();
        c.execute(
            "INSERT INTO research_papers (arxiv_id, title, published_at, status) VALUES ('x', 't', '2026-09-24', 'triaged')",
            [],
        )
        .unwrap();
        let id = c.last_insert_rowid();
        (c, id)
    }

    fn status(c: &rusqlite::Connection, id: i64) -> String {
        c.query_row("SELECT status FROM research_papers WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    /// The paid path, in order: submit, fail, resubmit, fail again. The second
    /// failure must be final; before the fix, resubmitting cleared the marker and
    /// a deterministically failing paper was billed on every run forever.
    #[test]
    fn a_read_that_fails_twice_is_not_resubmitted() {
        let (c, id) = db_with_triaged();
        mark_reading(&c, "b1", &[(id, 0.3)]).unwrap();
        retry_or_fail(&c, id, "max_tokens").unwrap();
        assert_eq!(status(&c, id), "triaged");
        mark_reading(&c, "b2", &[(id, 0.3)]).unwrap();
        retry_or_fail(&c, id, "max_tokens").unwrap();
        assert_eq!(status(&c, id), "failed");
    }

    #[test]
    fn unpollable_reading_ages_out_to_failed_not_triaged() {
        let (c, id) = db_with_triaged();
        mark_reading(&c, "b1", &[(id, 0.3)]).unwrap();
        assert_eq!(fail_stale(&c).unwrap(), 0, "fresh batch must be left alone");
        c.execute("UPDATE research_papers SET read_submitted_at = datetime('now', '-8 days')", []).unwrap();
        assert_eq!(fail_stale(&c).unwrap(), 1);
        assert_eq!(status(&c, id), "failed");
    }

    #[test]
    fn reservation_prices_the_output_ceiling() {
        let worst = ledger::cost(READ_MODEL, anthropic::Usage { input_tokens: 30_000, output_tokens: MAX_TOKENS as i64 }, true);
        assert!((estimate(30_000) - worst).abs() < 1e-12);
    }

    /// Structured outputs require every object to list all its properties as
    /// required and forbid extra ones — walk the whole schema.
    fn assert_strict(v: &Value, path: &str) {
        match v {
            Value::Object(m) => {
                if m.get("type") == Some(&json!("object")) {
                    assert_eq!(m.get("additionalProperties"), Some(&json!(false)), "{path}");
                    let props: Vec<&String> = m["properties"].as_object().unwrap().keys().collect();
                    let req: Vec<&str> = m["required"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
                    for p in props {
                        assert!(req.contains(&p.as_str()), "{path}.{p} not required");
                    }
                }
                for (k, child) in m {
                    assert_strict(child, &format!("{path}.{k}"));
                }
            }
            Value::Array(a) => a.iter().for_each(|c| assert_strict(c, path)),
            _ => {}
        }
    }

    #[test]
    fn schema_is_strict_everywhere() {
        assert_strict(&schema(), "$");
    }

    #[test]
    fn delta_schema_round_trips_into_strategy_delta() {
        // What the model returns for "raise news to 0.45, stop at -8%": every
        // field present, unused ones null.
        let v = json!({
            "weights": [{"dimension": "news_momentum", "weight": 0.45}],
            "positive_threshold": null, "min_positive": null, "min_diversity": null,
            "compound_override": null, "min_score": null, "max_positions": null,
            "sizing_tiers": null, "stop_loss_pct": -8.0, "take_profit_pct": null, "max_hold_days": null
        });
        let d: StrategyDelta = serde_json::from_value(v).unwrap();
        assert_eq!(d.stop_loss_pct, Some(-8.0));
        assert!(pulse_weights::StrategyParams::live().apply(&d).is_ok());
    }

    fn db_with_paper() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&c).unwrap();
        c.execute(
            "INSERT INTO research_papers (id, arxiv_id, title, published_at, status) VALUES (7, 'x', 't', '2026-09-24', 'reading')",
            [],
        )
        .unwrap();
        c
    }

    #[test]
    fn store_keeps_invalid_deltas_as_spec_only() {
        let c = db_with_paper();
        let study = json!({
            "one_line": "x",
            "proposals": [
                {"kind": "param", "component": "exits", "title": "tighter stop", "rationale": "r",
                 "falsifier": "f", "delta": {"stop_loss_pct": -8.0}, "spec_md": null},
                {"kind": "param", "component": "exits", "title": "positive stop", "rationale": "r",
                 "falsifier": "f", "delta": {"stop_loss_pct": 8.0}, "spec_md": null},
                {"kind": "param", "component": "x", "title": "noop", "rationale": "r",
                 "falsifier": "f", "delta": {}, "spec_md": null},
                {"kind": "new_signal", "component": "signals", "title": "earnings", "rationale": "r",
                 "falsifier": "f", "delta": null, "spec_md": "build it"}
            ]
        });
        store(&c, 7, &study, anthropic::Usage { input_tokens: 10, output_tokens: 5 }, 0.1).unwrap();
        let rows: Vec<(String, String, Option<String>, Option<String>)> = c
            .prepare("SELECT title, kind, delta_json, spec_md FROM paper_proposals ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 4, "nothing dropped");
        assert_eq!(rows[0].1, "param");
        assert!(rows[0].2.as_deref().unwrap().contains("-8"));
        assert_eq!(rows[1].1, "other");
        assert!(rows[1].3.as_deref().unwrap().contains("invalid"));
        assert_eq!(rows[2].1, "other");
        assert!(rows[2].3.as_deref().unwrap().contains("changed nothing"));
        assert_eq!(rows[3].1, "new_signal");
        let status: String = c.query_row("SELECT status FROM research_papers WHERE id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "read");
    }

    #[test]
    fn custom_id_round_trip() {
        assert_eq!(paper_id_of(&custom_id(42)), Some(42));
        assert_eq!(paper_id_of("other-1"), None);
    }
}
