//! Research tab: papers the fetcher triaged and read, their study notes, and the
//! proposals they produced — plus the what-if backtest of a proposal.
//!
//! Nothing here calls a paid API. Deep reads happen only in the fetcher, under
//! its research spend cap (`PULSE_RESEARCH_DAILY_CAP`); "read anyway" just
//! queues a paper for the fetcher's next hourly wake.

use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::db::DbState;
use crate::services::what_if;
use pulse_weights::StrategyDelta;

#[derive(Debug, Serialize)]
pub struct ResearchPaperRow {
    pub id: i64,
    pub arxiv_id: String,
    pub title: String,
    pub authors: String,
    pub categories: String,
    pub published_at: String,
    pub pages: Option<i64>,
    pub status: String,
    pub triage_score: Option<i64>,
    pub triage_reason: Option<String>,
    pub component: Option<String>,
    pub one_line: Option<String>,
    pub proposal_count: i64,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResearchStats {
    pub total: i64,
    pub read: i64,
    pub queued: i64,
    pub skipped: i64,
    pub proposals: i64,
    pub tested: i64,
    pub spent_today_usd: f64,
    pub spent_30d_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct Proposal {
    pub id: i64,
    pub kind: String,
    pub component: String,
    pub title: String,
    pub rationale: String,
    pub falsifier: String,
    pub delta: Option<Value>,
    pub spec_md: Option<String>,
    pub status: String,
    pub result: Option<Value>,
    pub tested_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResearchPaperDetail {
    pub paper: ResearchPaperRow,
    pub abstract_text: String,
    pub triage: Option<Value>,
    pub study: Option<Value>,
    pub read_model: Option<String>,
    pub read_cost_usd: Option<f64>,
    pub read_at: Option<String>,
    pub text_source: Option<String>,
    pub proposals: Vec<Proposal>,
}

const ROW_SELECT: &str = "SELECT p.id, p.arxiv_id, p.title, p.authors, p.categories, p.published_at, p.pages,
        p.status, p.triage_score, json_extract(p.triage_json, '$.reason'), json_extract(p.triage_json, '$.component'),
        (SELECT json_extract(r.study_json, '$.one_line') FROM paper_reads r WHERE r.paper_id = p.id ORDER BY r.id DESC LIMIT 1),
        (SELECT COUNT(*) FROM paper_proposals pp WHERE pp.paper_id = p.id),
        p.error
     FROM research_papers p";

fn row(r: &rusqlite::Row) -> rusqlite::Result<ResearchPaperRow> {
    Ok(ResearchPaperRow {
        id: r.get(0)?,
        arxiv_id: r.get(1)?,
        title: r.get(2)?,
        authors: r.get(3)?,
        categories: r.get(4)?,
        published_at: r.get(5)?,
        pages: r.get(6)?,
        status: r.get(7)?,
        triage_score: r.get(8)?,
        triage_reason: r.get(9)?,
        component: r.get(10)?,
        one_line: r.get(11)?,
        proposal_count: r.get(12)?,
        error: r.get(13)?,
    })
}

/// Read papers first, then queued, then the rest; newest first within each.
#[tauri::command]
pub fn get_research_papers(db: State<'_, DbState>, limit: Option<i64>) -> Result<Vec<ResearchPaperRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let sql = format!(
        "{ROW_SELECT}
         ORDER BY CASE p.status WHEN 'read' THEN 0 WHEN 'reading' THEN 1 WHEN 'triaged' THEN 1
                                WHEN 'failed' THEN 2 WHEN 'new' THEN 3 ELSE 4 END,
                  p.published_at DESC
         LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit.unwrap_or(300).clamp(1, 2000)], row)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
pub fn get_research_stats(db: State<'_, DbState>) -> Result<ResearchStats, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let (total, read, queued, skipped): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(status = 'read'), 0),
                    COALESCE(SUM(status IN ('triaged', 'reading')), 0), COALESCE(SUM(status = 'skipped'), 0)
             FROM research_papers",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| e.to_string())?;
    let (proposals, tested): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(tested_at IS NOT NULL), 0) FROM paper_proposals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let spend = |window: &str| -> f64 {
        conn.query_row(
            "SELECT COALESCE(SUM(estimated_cost_usd), 0.0) FROM api_usage
             WHERE endpoint LIKE 'research_%' AND created_at >= date('now', ?1)",
            [window],
            |r| r.get(0),
        )
        .unwrap_or(0.0)
    };
    Ok(ResearchStats {
        total,
        read,
        queued,
        skipped,
        proposals,
        tested,
        spent_today_usd: spend("+0 days"),
        spent_30d_usd: spend("-30 days"),
    })
}

fn parse_json(s: Option<String>) -> Option<Value> {
    s.and_then(|t| serde_json::from_str(&t).ok())
}

#[tauri::command]
pub fn get_research_paper(db: State<'_, DbState>, id: i64) -> Result<ResearchPaperDetail, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let paper = conn
        .query_row(&format!("{ROW_SELECT} WHERE p.id = ?1"), [id], row)
        .map_err(|e| format!("paper {id}: {e}"))?;
    let (abstract_text, triage): (String, Option<String>) = conn
        .query_row("SELECT abstract, triage_json FROM research_papers WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?;
    let read: Option<(String, String, f64, String)> = conn
        .query_row(
            "SELECT study_json, model, cost_usd, created_at FROM paper_reads WHERE paper_id = ?1 ORDER BY id DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok();
    let text_source: Option<String> = conn
        .query_row("SELECT source FROM paper_texts WHERE paper_id = ?1", [id], |r| r.get(0))
        .ok();
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, component, title, rationale, falsifier, delta_json, spec_md, status, result_json, tested_at
             FROM paper_proposals WHERE paper_id = ?1 ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let proposals = stmt
        .query_map([id], |r| {
            Ok(Proposal {
                id: r.get(0)?,
                kind: r.get(1)?,
                component: r.get(2)?,
                title: r.get(3)?,
                rationale: r.get(4)?,
                falsifier: r.get(5)?,
                delta: parse_json(r.get(6)?),
                spec_md: r.get(7)?,
                status: r.get(8)?,
                result: parse_json(r.get(9)?),
                tested_at: r.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(ResearchPaperDetail {
        paper,
        abstract_text,
        triage: parse_json(triage),
        study: read.as_ref().and_then(|r| serde_json::from_str(&r.0).ok()),
        read_model: read.as_ref().map(|r| r.1.clone()),
        read_cost_usd: read.as_ref().map(|r| r.2),
        read_at: read.map(|r| r.3),
        text_source,
        proposals,
    })
}

/// Queue a skipped or failed paper for a deep read on the fetcher's next hourly
/// wake. Spends nothing here; the fetcher prices it and checks the cap first.
#[tauri::command]
pub fn queue_paper_read(db: State<'_, DbState>, id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let n = conn
        .execute(
            "UPDATE research_papers SET status = 'triaged', error = NULL, updated_at = datetime('now')
             WHERE id = ?1 AND status IN ('skipped', 'failed')",
            [id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("Only skipped or failed papers can be queued".into());
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ProposalBacktest {
    pub result: what_if::WhatIfResult,
    /// Distinct proposals tested on this same history, this one included. Shown
    /// next to every verdict: try enough ideas on 145 days and one will "win".
    pub hypotheses_tested: i64,
}

/// Backtest a param proposal: baseline vs variant over all stored history.
/// Stores the result on the proposal. Pure computation, no API calls.
#[tauri::command]
pub fn backtest_proposal(db: State<'_, DbState>, proposal_id: i64) -> Result<ProposalBacktest, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    backtest_proposal_on(&conn, proposal_id)
}

fn backtest_proposal_on(conn: &rusqlite::Connection, proposal_id: i64) -> Result<ProposalBacktest, String> {
    let (kind, delta_json): (String, Option<String>) = conn
        .query_row("SELECT kind, delta_json FROM paper_proposals WHERE id = ?1", [proposal_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| format!("proposal {proposal_id}: {e}"))?;
    let delta: StrategyDelta = match (kind.as_str(), delta_json) {
        ("param", Some(j)) => serde_json::from_str(&j).map_err(|e| format!("stored delta unreadable: {e}"))?,
        _ => return Err("Only 'param' proposals can be backtested; this one is an implementation spec".into()),
    };
    let (start, end): (Option<String>, Option<String>) = conn
        .query_row("SELECT MIN(date(computed_at)), MAX(date(computed_at)) FROM cross_signals", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?;
    let (start, end) = start.zip(end).ok_or("no scored history to backtest on")?;

    let result = what_if::run(conn, &delta, &start, &end)?;
    conn.execute(
        "UPDATE paper_proposals SET result_json = ?2, tested_at = datetime('now'),
                status = CASE WHEN status = 'proposed' THEN 'tested' ELSE status END
         WHERE id = ?1",
        rusqlite::params![proposal_id, serde_json::to_string(&result).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    let hypotheses_tested: i64 = conn
        .query_row("SELECT COUNT(*) FROM paper_proposals WHERE tested_at IS NOT NULL", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(ProposalBacktest { result, hypotheses_tested })
}

/// Record a human decision on a proposal. 'kept' records intent only — it never
/// changes live weights or trading; that stays a reviewed code change.
#[tauri::command]
pub fn set_proposal_status(db: State<'_, DbState>, proposal_id: i64, status: String) -> Result<(), String> {
    if !["kept", "rejected", "proposed", "tested"].contains(&status.as_str()) {
        return Err(format!("invalid status '{status}'"));
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let n = conn
        .execute("UPDATE paper_proposals SET status = ?2 WHERE id = ?1", rusqlite::params![proposal_id, status])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("proposal {proposal_id} not found"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end on a COPY of the real DB (never the live one):
    /// `PULSE_WHATIF_DB=/path/to/copy.db cargo test -p pulse backtest_proposal_real_db -- --ignored`.
    /// Runs inside a transaction that is rolled back, so the copy is left untouched.
    #[test]
    #[ignore]
    fn backtest_proposal_real_db() {
        let path = std::env::var("PULSE_WHATIF_DB").expect("PULSE_WHATIF_DB");
        let mut conn = rusqlite::Connection::open(path).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO research_papers (arxiv_id, version, title, authors, categories, abstract, published_at, status)
             VALUES ('test.00001', 1, 't', '', '', 'a', '2026-09-01', 'read')",
            [],
        )
        .unwrap();
        let paper = tx.last_insert_rowid();
        tx.execute("INSERT INTO paper_reads (paper_id, model, study_json) VALUES (?1, 'test', '{}')", [paper])
            .unwrap();
        let read = tx.last_insert_rowid();
        let insert = |kind: &str, delta: Option<&str>| {
            tx.execute(
                "INSERT INTO paper_proposals (paper_id, read_id, kind, component, title, rationale, falsifier, delta_json)
                 VALUES (?1, ?2, ?3, 'exits', 't', 'r', 'f', ?4)",
                rusqlite::params![paper, read, kind, delta],
            )
            .unwrap();
            tx.last_insert_rowid()
        };
        let param = insert("param", Some(r#"{"stop_loss_pct": -6.0}"#));
        let spec = insert("new_signal", None);

        let out = backtest_proposal_on(&tx, param).unwrap();
        assert!(out.result.train.baseline.trades > 0, "baseline traded nothing");
        assert!(out.result.fidelity.ok, "stored compounds not reproduced");
        assert!(out.hypotheses_tested >= 1);
        let (status, stored): (String, Option<String>) = tx
            .query_row("SELECT status, result_json FROM paper_proposals WHERE id = ?1", [param], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "tested");
        assert!(stored.is_some());
        assert!(backtest_proposal_on(&tx, spec).unwrap_err().contains("Only 'param'"));
        eprintln!("{}", serde_json::to_string_pretty(&out.result).unwrap());
        tx.rollback().unwrap();
    }
}
