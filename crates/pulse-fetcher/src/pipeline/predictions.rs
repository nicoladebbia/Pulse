use super::*;

/// Validate active predictions via hybrid router (Push 2 rebuild).
///
/// Routing:
///   - target_metric has ticker → market-based resolution from entity_prices
///   - else → Sonnet LLM outcome check (capped at 15/day)
///   - LLM "unclear" → increments resolution_attempts; after 3 → needs_review
///
/// Returns (resolved, expired).
pub(crate) async fn validate_and_expire_predictions(db_path: &std::path::Path) -> anyhow::Result<(usize, usize)> {
    let conn = rusqlite::Connection::open(db_path)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // 1. Load active predictions whose target_date is today or past
    //    (future-dated predictions wait until their deadline).
    let mut pred_stmt = conn.prepare(
        "SELECT id, title, content, confidence, target_metric, target_date, resolution_attempts
         FROM insights
         WHERE insight_type = 'prediction'
           AND status = 'active'
           AND (target_date IS NULL OR target_date <= date('now'))
         ORDER BY target_date ASC NULLS LAST"
    )?;
    let predictions: Vec<PredToResolve> = pred_stmt
        .query_map([], |row| Ok(PredToResolve {
            id: row.get(0)?,
            title: row.get(1)?,
            content: row.get(2)?,
            confidence: row.get(3)?,
            target_metric: row.get::<_, Option<String>>(4)?,
            target_date: row.get::<_, Option<String>>(5)?,
            resolution_attempts: row.get::<_, i64>(6).unwrap_or(0),
        }))?
        .filter_map(|r| r.ok())
        .collect();

    if predictions.is_empty() {
        // Still run expiry pass below — some legacy predictions without target_date
        // may have predicted_date in the past.
    }

    let mut resolved = 0usize;
    let mut llm_checks_used = 0usize;
    const LLM_DAILY_CAP: usize = 15;

    for p in &predictions {
        // Route: market-based if ticker, LLM check otherwise
        let outcome = if let Some(tm_str) = &p.target_metric {
            match serde_json::from_str::<serde_json::Value>(tm_str) {
                Ok(tm) if tm.get("ticker").and_then(|v| v.as_str()).is_some() => {
                    resolve_market_prediction(&conn, &tm, p.target_date.as_deref())
                }
                _ => ResolutionOutcome::Unclear,
            }
        } else {
            // Qualitative → LLM check, respecting daily cap
            if llm_checks_used >= LLM_DAILY_CAP {
                tracing::info!("Prediction #{}: LLM cap reached ({}), try tomorrow", p.id, LLM_DAILY_CAP);
                continue;
            }
            llm_checks_used += 1;
            resolve_llm_prediction(db_path, &conn, p, &today).await
                .unwrap_or(ResolutionOutcome::Unclear)
        };

        match outcome {
            ResolutionOutcome::Validated { summary, method } => {
                apply_resolution(&conn, p, "validated", 1.0, &summary, &method, &today)?;
                resolved += 1;
            }
            ResolutionOutcome::Invalidated { summary, method } => {
                apply_resolution(&conn, p, "invalidated", 0.0, &summary, &method, &today)?;
                resolved += 1;
            }
            ResolutionOutcome::Partial { summary, method } => {
                apply_resolution(&conn, p, "partially_validated", 0.5, &summary, &method, &today)?;
                resolved += 1;
            }
            ResolutionOutcome::Unclear => {
                let attempts = p.resolution_attempts + 1;
                if attempts >= 3 {
                    conn.execute(
                        "UPDATE insights SET status = 'needs_review', resolution_attempts = ?1, updated_at = datetime('now') WHERE id = ?2",
                        rusqlite::params![attempts, p.id],
                    ).ok();
                    tracing::info!("Prediction #{} → needs_review (3 unclear attempts)", p.id);
                } else {
                    conn.execute(
                        "UPDATE insights SET resolution_attempts = ?1, updated_at = datetime('now') WHERE id = ?2",
                        rusqlite::params![attempts, p.id],
                    ).ok();
                }
            }
        }
    }

    // 2. Expire legacy predictions that have predicted_date in the past but
    //    no target_date AND no target_metric (can't be resolved by us).
    let expired: usize = conn.execute(
        "UPDATE insights SET status = 'expired'
         WHERE insight_type = 'prediction' AND status = 'active'
           AND target_date IS NULL AND target_metric IS NULL
           AND predicted_date IS NOT NULL AND predicted_date < date('now')",
        [],
    ).unwrap_or(0);

    // 3. Drain the terminal-state backlog.
    //
    // Measured 2026-08-15: 324 predictions sat in `expired` (56 of them never attempted
    // even once) and 118 in `needs_review`, against only 39 ever resolved. Both states
    // were dead ends — nothing in the codebase ever looked at them again — so the
    // prediction feature could not calibrate itself BY CONSTRUCTION: the calibration
    // injection above needs 50 resolved and had been stuck at 39 for months.
    //
    // An expired prediction is not unresolvable; its deadline has simply passed, which is
    // exactly when the outcome IS knowable. So spend whatever LLM budget the active pass
    // left over on the oldest stuck ones. At ~15/day the 442-deep backlog drains in about
    // a month, and calibration crosses its threshold within days.
    let backlog_resolved = drain_resolution_backlog(
        db_path,
        &conn,
        LLM_DAILY_CAP.saturating_sub(llm_checks_used),
        &today,
    )
    .await;

    tracing::info!("Predictions v2: {} resolved, {} expired, {} from backlog, {} LLM checks used",
        resolved, expired, backlog_resolved, llm_checks_used);

    Ok((resolved + backlog_resolved, expired))
}

#[derive(Debug)]
pub(crate) struct PredToResolve {
    id: i64,
    title: String,
    content: String,
    confidence: f64,
    target_metric: Option<String>,
    target_date: Option<String>,
    resolution_attempts: i64,
}

#[derive(Debug)]
pub(crate) enum ResolutionOutcome {
    Validated { summary: String, method: String },
    Invalidated { summary: String, method: String },
    Partial { summary: String, method: String },
    Unclear,
}

/// Resolve a ticker-grounded prediction using entity_prices.
/// target_metric is JSON {ticker, operator, value, unit, baseline_date?}
pub(crate) fn resolve_market_prediction(
    conn: &rusqlite::Connection,
    tm: &serde_json::Value,
    target_date: Option<&str>,
) -> ResolutionOutcome {
    let ticker = match tm.get("ticker").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return ResolutionOutcome::Unclear,
    };
    let operator = tm.get("operator").and_then(|v| v.as_str()).unwrap_or(">=");
    let value = match tm.get("value").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None => return ResolutionOutcome::Unclear,
    };
    let unit = tm.get("unit").and_then(|v| v.as_str()).unwrap_or("price_usd");

    // Look up price on or after the target_date (closest)
    let t_date = target_date.unwrap_or("");
    let row: Option<(f64, String)> = conn.query_row(
        "SELECT close, date FROM entity_prices
         WHERE ticker = ?1 AND date >= ?2
         ORDER BY date ASC LIMIT 1",
        rusqlite::params![ticker, t_date],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).ok();

    let (close, matched_date) = match row {
        Some(r) => r,
        None => return ResolutionOutcome::Unclear, // no price data available yet
    };

    // Compute comparison value
    let observed = match unit {
        "price_usd" => close,
        "pct_change" => {
            let baseline_date = tm.get("baseline_date").and_then(|v| v.as_str()).unwrap_or("");
            let baseline: Option<f64> = conn.query_row(
                "SELECT close FROM entity_prices WHERE ticker = ?1 AND date <= ?2 ORDER BY date DESC LIMIT 1",
                rusqlite::params![ticker, baseline_date],
                |row| row.get(0),
            ).ok();
            match baseline {
                Some(b) if b > 0.0 => ((close - b) / b) * 100.0,
                _ => return ResolutionOutcome::Unclear,
            }
        }
        _ => return ResolutionOutcome::Unclear,
    };

    let hit = match operator {
        ">=" => observed >= value,
        "<=" => observed <= value,
        _ => return ResolutionOutcome::Unclear,
    };

    let summary = format!("{} close on {} was {:.2} {} — target {} {:.2} ({})",
        ticker, matched_date, observed, unit, operator, value,
        if hit { "MET" } else { "MISSED" });

    if hit {
        ResolutionOutcome::Validated { summary, method: "market".to_string() }
    } else {
        ResolutionOutcome::Invalidated { summary, method: "market".to_string() }
    }
}

/// Resolve a qualitative prediction using Sonnet LLM outcome check.
pub(crate) async fn resolve_llm_prediction(
    db_path: &std::path::Path,
    conn: &rusqlite::Connection,
    p: &PredToResolve,
    today: &str,
) -> anyhow::Result<ResolutionOutcome> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    // Gather stories near the target_date (+/- 7 days) that mention keywords
    // from the prediction. Simple keyword approach: split title by whitespace,
    // take tokens > 4 chars as candidates.
    let keywords: Vec<String> = p.title
        .split_whitespace()
        .filter(|w| w.len() >= 5)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .take(4)
        .collect();

    let like_clause = keywords.iter()
        .map(|k| format!("LOWER(headline) LIKE '%{}%'", k.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" OR ");

    let query = if like_clause.is_empty() {
        "SELECT headline, summary FROM stories WHERE DATE(created_at) >= date('now', '-7 days') ORDER BY created_at DESC LIMIT 10".to_string()
    } else {
        format!("SELECT headline, summary FROM stories WHERE DATE(created_at) >= date('now', '-7 days') AND ({}) ORDER BY created_at DESC LIMIT 10", like_clause)
    };

    let mut stmt = conn.prepare(&query)?;
    let stories: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if stories.is_empty() {
        return Ok(ResolutionOutcome::Unclear);
    }

    // Judge the FULL prediction, not the title.
    //
    // `title` is `text.chars().take(100)` — the prediction truncated to 100 characters,
    // frequently mid-sentence. The fact-checker was being asked to rule on a fragment,
    // which is a direct cause of "unclear" verdicts and therefore of the 118 predictions
    // that piled up in `needs_review` after three unclear attempts each.
    //
    // The stored `content` is `text + "\n\nReasoning: ..."`. Only the text half is used:
    // showing the fact-checker the original rationale invites it to grade the argument
    // rather than the outcome.
    let prediction_text = p
        .content
        .split("\n\nReasoning:")
        .next()
        .unwrap_or(&p.content)
        .trim();
    let prediction_text = if prediction_text.is_empty() { p.title.as_str() } else { prediction_text };

    let mut input = format!("PREDICTION (made earlier): {}\n\nTARGET DATE: {}\n\nRECENT STORIES:\n",
        prediction_text, p.target_date.as_deref().unwrap_or("unknown"));
    for (i, (h, s)) in stories.iter().enumerate() {
        input.push_str(&format!("[{}] {} — {}\n", i, h, s.chars().take(200).collect::<String>()));
    }
    input.push_str("\nReturn your verdict as strict JSON only.");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 500,
        "system": crate::claude::prompts::PREDICTION_OUTCOME_CHECK_SYSTEM,
        "messages": [{"role": "user", "content": input}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        tracing::warn!("LLM outcome check returned {}: prediction {}", status, p.id);
        return Ok(ResolutionOutcome::Unclear);
    }

    let parsed: serde_json::Value = resp.json().await?;
    let text = parsed["content"][0]["text"].as_str().unwrap_or("").to_string();

    // Log token usage (rough estimate: 1k in, 200 out per outcome check)
    log_usage(db_path, "anthropic", "claude-sonnet-4-6", "predictions_outcome_check", 1000, 200);

    let _ = today;  // reserved for future use
    let json_str = extract_json_str(&text);
    let verdict: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Ok(ResolutionOutcome::Unclear),
    };

    let verdict_str = verdict.get("verdict").and_then(|v| v.as_str()).unwrap_or("unclear");
    let summary = verdict.get("outcome_summary").and_then(|v| v.as_str()).unwrap_or("").to_string();

    Ok(match verdict_str {
        "validated" => ResolutionOutcome::Validated { summary, method: "llm".to_string() },
        "invalidated" => ResolutionOutcome::Invalidated { summary, method: "llm".to_string() },
        "partial" => ResolutionOutcome::Partial { summary, method: "llm".to_string() },
        _ => ResolutionOutcome::Unclear,
    })
}

/// Compute calibration stats over all resolved predictions.
/// Bucketed by confidence (0.1 buckets), topic (sector), timeframe (7/14/30/60/90d),
/// and source (aggregated from source_story_ids → stories.source_name).
/// Writes one row to prediction_calibration.
pub(crate) async fn compute_calibration_stats(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;

    // Load resolved predictions only
    let mut stmt = conn.prepare(
        "SELECT id, confidence, status, sector, target_date, created_at, source_story_ids, brier_score
         FROM insights
         WHERE insight_type = 'prediction'
           AND status IN ('validated', 'partially_validated', 'invalidated')"
    )?;

    struct ResolvedPred {
        _id: i64,
        confidence: f64,
        outcome: f64,
        sector: Option<String>,
        days_horizon: Option<i64>,
        source_ids: Vec<i64>,
        brier: Option<f64>,
    }

    let resolved: Vec<ResolvedPred> = stmt.query_map([], |row| {
        let status: String = row.get(2)?;
        let outcome = match status.as_str() {
            "validated" => 1.0,
            "partially_validated" => 0.5,
            _ => 0.0,
        };
        let target_date: Option<String> = row.get(4)?;
        let created_at: Option<String> = row.get(5)?;
        let days_horizon = match (target_date.as_deref(), created_at.as_deref()) {
            (Some(td), Some(ca)) => {
                let t = chrono::NaiveDate::parse_from_str(td, "%Y-%m-%d").ok();
                let c = chrono::NaiveDateTime::parse_from_str(ca, "%Y-%m-%d %H:%M:%S").ok()
                    .map(|dt| dt.date());
                match (t, c) {
                    (Some(t), Some(c)) => Some((t - c).num_days()),
                    _ => None,
                }
            }
            _ => None,
        };
        let source_ids_json: Option<String> = row.get(6)?;
        let source_ids: Vec<i64> = source_ids_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        Ok(ResolvedPred {
            _id: row.get(0)?,
            confidence: row.get(1)?,
            outcome,
            sector: row.get(3)?,
            days_horizon,
            source_ids,
            brier: row.get(7)?,
        })
    })?
    .filter_map(|r| r.ok())
    .collect();

    let total_resolved = resolved.len();
    if total_resolved == 0 {
        tracing::info!("Calibration: no resolved predictions yet, skipping");
        return Ok(());
    }

    let accuracy_overall = resolved.iter().map(|r| r.outcome).sum::<f64>() / total_resolved as f64;
    let avg_brier = {
        let briers: Vec<f64> = resolved.iter().filter_map(|r| r.brier).collect();
        if briers.is_empty() { None } else { Some(briers.iter().sum::<f64>() / briers.len() as f64) }
    };

    // Bucket by confidence (0.5-0.6, 0.6-0.7, ..., 0.9-1.0)
    let mut confidence_buckets: std::collections::BTreeMap<String, (f64, usize)> = std::collections::BTreeMap::new();
    for r in &resolved {
        let bucket = if r.confidence < 0.6 { "0.5-0.6" }
            else if r.confidence < 0.7 { "0.6-0.7" }
            else if r.confidence < 0.8 { "0.7-0.8" }
            else if r.confidence < 0.9 { "0.8-0.9" }
            else { "0.9-1.0" };
        let entry = confidence_buckets.entry(bucket.to_string()).or_insert((0.0, 0));
        entry.0 += r.outcome;
        entry.1 += 1;
    }
    let confidence_map: serde_json::Map<String, serde_json::Value> = confidence_buckets.iter()
        .filter(|(_, (_, n))| *n >= 5)  // min sample size
        .map(|(k, (sum, n))| (k.clone(), serde_json::json!({"accuracy": sum / *n as f64, "n": n})))
        .collect();

    // Bucket by topic (sector)
    let mut topic_buckets: std::collections::BTreeMap<String, (f64, usize)> = std::collections::BTreeMap::new();
    for r in &resolved {
        if let Some(s) = &r.sector {
            let entry = topic_buckets.entry(s.clone()).or_insert((0.0, 0));
            entry.0 += r.outcome;
            entry.1 += 1;
        }
    }
    let topic_map: serde_json::Map<String, serde_json::Value> = topic_buckets.iter()
        .filter(|(_, (_, n))| *n >= 5)
        .map(|(k, (sum, n))| (k.clone(), serde_json::json!({"accuracy": sum / *n as f64, "n": n})))
        .collect();

    // Bucket by timeframe (7/14/30/60/90 days)
    let mut time_buckets: std::collections::BTreeMap<i64, (f64, usize)> = std::collections::BTreeMap::new();
    for r in &resolved {
        if let Some(d) = r.days_horizon {
            let bucket = if d <= 7 { 7 }
                else if d <= 14 { 14 }
                else if d <= 30 { 30 }
                else if d <= 60 { 60 }
                else { 90 };
            let entry = time_buckets.entry(bucket).or_insert((0.0, 0));
            entry.0 += r.outcome;
            entry.1 += 1;
        }
    }
    let time_map: serde_json::Map<String, serde_json::Value> = time_buckets.iter()
        .filter(|(_, (_, n))| *n >= 5)
        .map(|(k, (sum, n))| (k.to_string(), serde_json::json!({"accuracy": sum / *n as f64, "n": n})))
        .collect();

    // Bucket by source — aggregate by source_name from stories
    let mut source_accuracies: std::collections::BTreeMap<String, (f64, usize)> = std::collections::BTreeMap::new();
    for r in &resolved {
        if r.source_ids.is_empty() { continue; }
        // For each source_id, look up source_name
        let placeholders = r.source_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!("SELECT DISTINCT source_name FROM stories WHERE id IN ({})", placeholders);
        let params: Vec<&dyn rusqlite::ToSql> = r.source_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let mut stmt = match conn.prepare(&query) { Ok(s) => s, Err(_) => continue };
        let names: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(params), |row| row.get::<_, String>(0))
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        for name in names {
            let entry = source_accuracies.entry(name).or_insert((0.0, 0));
            entry.0 += r.outcome;
            entry.1 += 1;
        }
    }
    let source_map: serde_json::Map<String, serde_json::Value> = source_accuracies.iter()
        .filter(|(_, (_, n))| *n >= 5)
        .map(|(k, (sum, n))| (k.clone(), serde_json::json!({"accuracy": sum / *n as f64, "n": n})))
        .collect();

    conn.execute(
        "INSERT INTO prediction_calibration
            (total_resolved, accuracy_overall, accuracy_by_confidence,
             accuracy_by_topic, accuracy_by_timeframe, accuracy_by_source, avg_brier)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            total_resolved as i64,
            accuracy_overall,
            serde_json::Value::Object(confidence_map).to_string(),
            serde_json::Value::Object(topic_map).to_string(),
            serde_json::Value::Object(time_map).to_string(),
            serde_json::Value::Object(source_map).to_string(),
            avg_brier,
        ],
    )?;

    tracing::info!("Calibration: {} resolved, overall accuracy {:.1}%",
        total_resolved, accuracy_overall * 100.0);

    Ok(())
}

/// Persist a resolution: set status, actual_outcome, resolution_method, compute Brier.
/// SQL selecting predictions stuck in a terminal state that are nonetheless resolvable.
///
/// `expired` and `needs_review` were both dead ends. A prediction is eligible to come back
/// only if its deadline has actually passed — otherwise there is no outcome to grade yet —
/// and the oldest are drained first so the backlog empties FIFO instead of thrashing the
/// same few rows. `resolution_attempts` ascending keeps never-attempted rows at the front:
/// 56 of the 324 expired had never been tried even once.
pub(crate) const BACKLOG_QUERY: &str = "SELECT id, title, content, confidence, target_metric, target_date, resolution_attempts
     FROM insights
     WHERE insight_type = 'prediction'
       AND status IN ('expired', 'needs_review')
       AND COALESCE(target_date, predicted_date) IS NOT NULL
       AND COALESCE(target_date, predicted_date) <= date('now')
     ORDER BY resolution_attempts ASC, COALESCE(target_date, predicted_date) ASC
     LIMIT ?1";

/// Re-attempt resolution for predictions abandoned in `expired` / `needs_review`.
///
/// Returns how many reached a decisive outcome. Anything still unclear keeps its terminal
/// status and gets its attempt counter bumped, so the ORDER BY naturally deprioritises
/// rows that have already resisted several tries rather than retrying them forever.
pub(crate) async fn drain_resolution_backlog(
    db_path: &std::path::Path,
    conn: &rusqlite::Connection,
    budget: usize,
    today: &str,
) -> usize {
    if budget == 0 {
        return 0;
    }

    let stuck: Vec<PredToResolve> = match conn.prepare(BACKLOG_QUERY) {
        Ok(mut stmt) => stmt
            .query_map([budget as i64], |row| {
                Ok(PredToResolve {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    confidence: row.get(3)?,
                    target_metric: row.get::<_, Option<String>>(4)?,
                    target_date: row.get::<_, Option<String>>(5)?,
                    resolution_attempts: row.get::<_, i64>(6).unwrap_or(0),
                })
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!("Backlog drain query failed: {}", e);
            return 0;
        }
    };

    if stuck.is_empty() {
        return 0;
    }
    tracing::info!("Predictions: draining {} stuck predictions (budget {})", stuck.len(), budget);

    let mut resolved = 0usize;
    for p in &stuck {
        // Prefer the free market-based route when the prediction carries a ticker;
        // only fall back to the paid LLM check when it does not.
        let outcome = match p.target_metric.as_deref().and_then(|tm| {
            serde_json::from_str::<serde_json::Value>(tm).ok()
        }) {
            Some(tm) if tm.get("ticker").and_then(|v| v.as_str()).is_some() => {
                resolve_market_prediction(conn, &tm, p.target_date.as_deref())
            }
            _ => resolve_llm_prediction(db_path, conn, p, today)
                .await
                .unwrap_or(ResolutionOutcome::Unclear),
        };

        let (status, value, summary, method) = match outcome {
            ResolutionOutcome::Validated { summary, method } => ("validated", 1.0, summary, method),
            ResolutionOutcome::Invalidated { summary, method } => ("invalidated", 0.0, summary, method),
            ResolutionOutcome::Partial { summary, method } => ("partially_validated", 0.5, summary, method),
            ResolutionOutcome::Unclear => {
                // Still unclear — keep the terminal status but record the attempt so the
                // ORDER BY sends it to the back of the queue instead of re-picking it.
                conn.execute(
                    "UPDATE insights SET resolution_attempts = ?1, updated_at = datetime('now') WHERE id = ?2",
                    rusqlite::params![p.resolution_attempts + 1, p.id],
                )
                .ok();
                continue;
            }
        };

        match apply_resolution(conn, p, status, value, &summary, &method, today) {
            Ok(()) => {
                resolved += 1;
                tracing::info!("Predictions: backlog #{} resolved as {}", p.id, status);
            }
            Err(e) => tracing::warn!("Predictions: backlog #{} resolution failed to save: {}", p.id, e),
        }
    }

    resolved
}

pub(crate) fn apply_resolution(
    conn: &rusqlite::Connection,
    p: &PredToResolve,
    status: &str,
    outcome_value: f64,
    summary: &str,
    method: &str,
    today: &str,
) -> anyhow::Result<()> {
    let brier = (p.confidence - outcome_value).powi(2);

    // Append to confidence_history (Vec<f64>)
    let history: String = conn.query_row(
        "SELECT COALESCE(confidence_history, '[]') FROM insights WHERE id = ?1",
        [p.id], |row| row.get(0),
    ).unwrap_or_else(|_| "[]".to_string());
    let mut entries: Vec<f64> = serde_json::from_str(&history).unwrap_or_default();
    entries.push(p.confidence);  // record confidence at resolution

    conn.execute(
        "UPDATE insights
            SET status = ?1,
                actual_outcome = ?2,
                resolution_method = ?3,
                brier_score = ?4,
                confidence_history = ?5,
                updated_at = datetime('now')
          WHERE id = ?6",
        rusqlite::params![
            status,
            summary,
            method,
            brier,
            serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string()),
            p.id,
        ],
    )?;

    tracing::info!("Prediction #{} → {} ({}), Brier={:.3}", p.id, status, method, brier);
    let _ = today;
    Ok(())
}

#[derive(serde::Deserialize)]
pub(crate) struct GeneratedPrediction {
    text: String,
    #[serde(default)]
    target_metric: Option<serde_json::Value>,
    target_date: String,
    confidence: f64,
    #[serde(default)]
    source_story_ids: Vec<i64>,
    #[serde(default)]
    source_signal_ids: Vec<i64>,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    sector: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct PredictionGenResponse {
    predictions: Vec<GeneratedPrediction>,
}

/// Outcome of validating one prediction's story references against the stories that were
/// actually shown to the model.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct RefResolution {
    /// Real `stories.id` values, deduped, in the order the model gave them.
    pub ids: Vec<i64>,
    /// Refs that were a positional index into the presented list and got remapped.
    pub remapped: usize,
    /// Refs that matched neither a presented id nor a valid position — dropped.
    pub dropped: usize,
}

/// Map the model's story references onto real `stories.id` values.
///
/// THE BUG THIS FIXES: the prompt presents each story as `[{i}] story_id={id} ...`, giving
/// the model two numbers for the same story. It overwhelmingly returned the bracket INDEX.
/// Measured on the live DB 2026-08-15: 1,146 of 1,641 stored refs were <= 200 against a
/// table whose max id is 44,923, and only 32.5% resolved to a real story — so roughly
/// two-thirds of every prediction's citations pointed at an unrelated article or nothing.
/// Same index-space confusion as the cross-sector connections bug (`build_analysis_result`).
///
/// Precedence is deliberate: an exact `stories.id` match wins over a positional read, so a
/// model that does the right thing is never "corrected" into the wrong story. A ref that is
/// neither is DROPPED — a prediction with no citation is honest; one with a wrong citation
/// is a fabrication wearing a citation's clothes.
pub(crate) fn resolve_story_refs(model_refs: &[i64], presented_ids: &[i64]) -> RefResolution {
    let mut out = RefResolution::default();
    for &r in model_refs {
        let resolved = if presented_ids.contains(&r) {
            Some(r)
        } else if r >= 0 && (r as usize) < presented_ids.len() {
            out.remapped += 1;
            Some(presented_ids[r as usize])
        } else {
            out.dropped += 1;
            None
        };
        if let Some(id) = resolved {
            if !out.ids.contains(&id) {
                out.ids.push(id);
            }
        }
    }
    out
}

/// Generate fresh predictions from today's top stories + cross-signals.
/// Runs once per daily pipeline. Uses Sonnet by default; on Sunday, uses Opus
/// with a bigger input for a "weekly deep-dive" run (see plan Q2).
pub(crate) async fn generate_predictions(
    db_path: &Path,
    top_stories: &[(i64, String, String, String)],  // (id, headline, summary, sector)
    top_signals: &[(i64, String, Option<String>, f64)],  // (entity_id, name, ticker, score)
) -> anyhow::Result<usize> {
    use chrono::{Datelike, Local, Weekday};

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    // Sunday = weekly Opus deep-dive. Other days = Sonnet.
    let is_sunday = Local::now().weekday() == Weekday::Sun;
    let (model, max_stories, max_signals) = if is_sunday {
        ("claude-opus-4-7", 40usize, 20usize)
    } else {
        ("claude-sonnet-4-6", 20usize, 10usize)
    };

    if top_stories.is_empty() {
        tracing::info!("Predictions: no top stories, skipping generation");
        return Ok(0);
    }

    // Build input block.
    //
    // NO bracket index. The old format was `[{i}] story_id={id} ...`, which handed the
    // model two different numbers for the same story; it returned the index roughly
    // two-thirds of the time, so stored citations pointed at unrelated articles. The
    // presented ids are captured below and `resolve_story_refs` still repairs an index if
    // the model produces one anyway — belt and braces, because the prompt alone is not a
    // guarantee.
    let presented_story_ids: Vec<i64> =
        top_stories.iter().take(max_stories).map(|(id, ..)| *id).collect();
    let presented_signal_ids: Vec<i64> =
        top_signals.iter().take(max_signals).map(|(id, ..)| *id).collect();

    let mut input = String::from(
        "Today's top stories (cite ONLY by the story_id value shown):\n",
    );
    for (id, headline, summary, sector) in top_stories.iter().take(max_stories) {
        input.push_str(&format!(
            "story_id={} sector={} | {} — {}\n",
            id, sector, headline,
            summary.chars().take(150).collect::<String>()
        ));
    }
    input.push_str("\nTop cross-signals today (cite ONLY by the signal_id value shown):\n");
    for (eid, name, ticker, score) in top_signals.iter().take(max_signals) {
        input.push_str(&format!(
            "signal_id={} entity=\"{}\" ticker={} score={:.2}\n",
            eid, name, ticker.as_deref().unwrap_or("-"), score
        ));
    }
    input.push_str(&format!(
        "\nToday's date: {}. Return 5-10 predictions as strict JSON.",
        Local::now().format("%Y-%m-%d")
    ));

    // Feedback loop (Task 2.11): inject calibration stats when ≥50 resolved.
    // Give the model its own track record to calibrate new predictions against.
    let calibration_injection = {
        let conn = rusqlite::Connection::open(db_path)?;
        let total_resolved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM insights
             WHERE insight_type = 'prediction'
               AND status IN ('validated', 'partially_validated', 'invalidated')",
            [],
            |row| row.get(0),
        ).unwrap_or(0);

        if total_resolved >= 50 {
            // Fetch latest calibration row
            let row: Option<(f64, Option<f64>, String, String, String)> = conn.query_row(
                "SELECT accuracy_overall, avg_brier, accuracy_by_confidence,
                        accuracy_by_topic, accuracy_by_timeframe
                 FROM prediction_calibration
                 ORDER BY computed_at DESC LIMIT 1",
                [],
                |row| Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2).unwrap_or_else(|_| "{}".to_string()),
                    row.get(3).unwrap_or_else(|_| "{}".to_string()),
                    row.get(4).unwrap_or_else(|_| "{}".to_string()),
                )),
            ).ok();
            if let Some((acc, brier, by_conf, by_topic, by_time)) = row {
                Some(format!(
                    "\n\n=== YOUR TRACK RECORD (last {} resolved predictions) ===\nOverall accuracy: {:.0}%\nAvg Brier score: {}\nBy confidence bucket: {}\nBy topic: {}\nBy timeframe (days): {}\n\nCalibrate your confidence accordingly — if 80% confidence = 63% actual, be less certain.",
                    total_resolved,
                    acc * 100.0,
                    brier.map(|b| format!("{:.3}", b)).unwrap_or_else(|| "n/a".to_string()),
                    by_conf, by_topic, by_time,
                ))
            } else { None }
        } else {
            tracing::info!("Predictions: calibration injection OFF ({}/50 resolved)", total_resolved);
            None
        }
    };

    // Build full system prompt with optional calibration suffix
    let system_prompt = match calibration_injection {
        Some(s) => format!("{}{}", crate::claude::prompts::PREDICTION_GENERATOR_SYSTEM, s),
        None => crate::claude::prompts::PREDICTION_GENERATOR_SYSTEM.to_string(),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4000,
        "system": system_prompt,
        "messages": [{"role": "user", "content": input}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Prediction generator API {}: {}", status, text.chars().take(500).collect::<String>());
    }

    let parsed: serde_json::Value = resp.json().await?;
    let raw_text = parsed["content"][0]["text"].as_str()
        .ok_or_else(|| anyhow::anyhow!("No content in prediction generator response"))?
        .to_string();

    // Parse JSON — extract from potential markdown fences.
    let json_str = extract_json_str(&raw_text);
    let parsed: PredictionGenResponse = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse predictions: {} — raw: {}", e, json_str.chars().take(300).collect::<String>()))?;

    tracing::info!("Predictions: generated {} predictions via {}", parsed.predictions.len(), model);

    // Log cost. Sonnet: ~2k in + 1k out. Opus: ~4k in + 2k out.
    let (in_tokens, out_tokens) = if is_sunday { (4000, 2000) } else { (2000, 1000) };
    log_usage(db_path, "anthropic", model, "predictions_generate", in_tokens, out_tokens);

    // Insert into insights table
    let conn = rusqlite::Connection::open(db_path)?;
    let mut inserted = 0;
    let mut total_remapped = 0usize;
    let mut total_dropped = 0usize;
    for p in &parsed.predictions {
        // Confidence validation
        let confidence = p.confidence.clamp(0.5, 0.95);

        // Validate every citation against what the model was actually shown. Anything
        // that resolves to no real story is dropped rather than stored — see
        // `resolve_story_refs` for why a wrong citation is worse than none.
        let stories_res = resolve_story_refs(&p.source_story_ids, &presented_story_ids);
        let signals_res = resolve_story_refs(&p.source_signal_ids, &presented_signal_ids);
        total_remapped += stories_res.remapped + signals_res.remapped;
        total_dropped += stories_res.dropped + signals_res.dropped;

        // Build "evidence" JSON in the legacy shape for back-compat
        let evidence = serde_json::json!(
            stories_res.ids.iter().map(|sid| serde_json::json!({
                "story_id": sid,
                "reasoning": "Evidence type: source"
            })).collect::<Vec<_>>()
        );

        // Seed confidence_history with the initial confidence
        let confidence_history = serde_json::json!([confidence]);

        let target_metric_str = p.target_metric.as_ref().map(|v| v.to_string());
        let source_story_ids_str = serde_json::to_string(&stories_res.ids).unwrap_or_else(|_| "[]".to_string());
        let source_signal_ids_str = serde_json::to_string(&signals_res.ids).unwrap_or_else(|_| "[]".to_string());

        let title = p.text.chars().take(100).collect::<String>();
        let content = if p.reasoning.is_empty() {
            p.text.clone()
        } else {
            format!("{}\n\nReasoning: {}", p.text, p.reasoning)
        };

        let result = conn.execute(
            "INSERT INTO insights (
                insight_type, title, content, confidence, evidence, sector, status,
                predicted_date, target_metric, target_date, source_story_ids,
                source_signal_ids, model_used, confidence_history
             ) VALUES ('prediction', ?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                title,
                content,
                confidence,
                evidence.to_string(),
                p.sector.as_deref(),
                p.target_date,       // predicted_date (back-compat)
                target_metric_str,
                p.target_date,
                source_story_ids_str,
                source_signal_ids_str,
                model,
                confidence_history.to_string(),
            ],
        );

        match result {
            Ok(_) => {
                inserted += 1;
                // Mirror the citations into the NORMALISED table as well.
                //
                // Pulse had two parallel evidence representations: the fetcher wrote the
                // denormalised `insights.source_story_ids` / `insights.evidence`, while
                // `patterns.rs` and `predictions.rs` READ `insight_evidence` — a table
                // with 0 rows. Every reader therefore saw "no evidence" for all 902
                // predictions, and any logic gated on that count was silently dead.
                // Writing both keeps the readers honest without a risky schema migration;
                // the ids here are already validated by `resolve_story_refs`.
                let insight_id = conn.last_insert_rowid();
                for sid in &stories_res.ids {
                    if let Err(e) = conn.execute(
                        "INSERT OR IGNORE INTO insight_evidence (insight_id, story_id, role)
                         VALUES (?1, ?2, 'support')",
                        rusqlite::params![insight_id, sid],
                    ) {
                        // Non-fatal: the denormalised copy above is still authoritative.
                        tracing::warn!("insight_evidence link {}->{} failed: {}", insight_id, sid, e);
                    }
                }
            }
            Err(e) => tracing::warn!("Prediction insert failed: {}", e),
        }
    }

    tracing::info!("Predictions: inserted {} of {} generated predictions", inserted, parsed.predictions.len());
    if total_remapped > 0 || total_dropped > 0 {
        // Watch this line: after the prompt change the model should cite real story_ids,
        // so a persistently high `remapped` means the prompt fix is not landing and the
        // model is still emitting positional indices.
        tracing::warn!(
            "Predictions: citation repair — {} refs remapped from positional index, {} dropped as unresolvable",
            total_remapped, total_dropped
        );
    }
    Ok(inserted)
}

#[cfg(test)]
mod backlog_tests {
    use super::*;

    fn seed() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("t.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE insights (
                id INTEGER PRIMARY KEY, insight_type TEXT, title TEXT, content TEXT,
                confidence REAL, target_metric TEXT, target_date TEXT,
                predicted_date TEXT, status TEXT, resolution_attempts INTEGER DEFAULT 0);",
        )
        .unwrap();
        (dir, conn)
    }

    fn add(conn: &rusqlite::Connection, id: i64, status: &str, target: Option<&str>, attempts: i64) {
        conn.execute(
            "INSERT INTO insights (id, insight_type, title, content, confidence,
                target_date, predicted_date, status, resolution_attempts)
             VALUES (?1, 'prediction', 't', 'c', 0.6, ?2, ?2, ?3, ?4)",
            rusqlite::params![id, target, status, attempts],
        )
        .unwrap();
    }

    fn selected(conn: &rusqlite::Connection, limit: i64) -> Vec<i64> {
        let mut stmt = conn.prepare(BACKLOG_QUERY).unwrap();
        stmt.query_map([limit], |r| r.get::<_, i64>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// The 442 rows the drain exists for: both dead-end states must be reachable again.
    #[test]
    fn both_terminal_states_are_drained() {
        let (_d, conn) = seed();
        add(&conn, 1, "expired", Some("2020-01-01"), 0);
        add(&conn, 2, "needs_review", Some("2020-01-02"), 0);
        let got = selected(&conn, 10);
        assert!(got.contains(&1) && got.contains(&2), "got {got:?}");
    }

    /// Already-graded predictions must never be re-opened — that would corrupt the
    /// calibration set the whole feature depends on.
    #[test]
    fn resolved_predictions_are_never_reopened() {
        let (_d, conn) = seed();
        for (i, st) in ["validated", "invalidated", "partially_validated", "active"]
            .iter()
            .enumerate()
        {
            add(&conn, i as i64 + 1, st, Some("2020-01-01"), 0);
        }
        assert!(selected(&conn, 10).is_empty());
    }

    /// A deadline that has not passed has no outcome to grade yet.
    #[test]
    fn future_deadlines_are_not_drained() {
        let (_d, conn) = seed();
        add(&conn, 1, "expired", Some("2999-01-01"), 0);
        assert!(selected(&conn, 10).is_empty());
    }

    /// 56 of the 324 expired had never been attempted once. Those go first.
    #[test]
    fn never_attempted_are_drained_before_repeatedly_failed() {
        let (_d, conn) = seed();
        add(&conn, 1, "expired", Some("2019-01-01"), 5); // oldest, but tried 5x
        add(&conn, 2, "expired", Some("2020-01-01"), 0); // newer, never tried
        assert_eq!(selected(&conn, 1), vec![2], "never-attempted must win");
    }

    /// Unclear rows keep their status and bump attempts, so the ORDER BY pushes them
    /// back — without that the drain would re-pick the same rows every single day.
    #[test]
    fn bumped_attempts_move_a_row_to_the_back_of_the_queue() {
        let (_d, conn) = seed();
        add(&conn, 1, "expired", Some("2020-01-01"), 0);
        add(&conn, 2, "expired", Some("2020-01-02"), 0);
        assert_eq!(selected(&conn, 1), vec![1]);
        conn.execute("UPDATE insights SET resolution_attempts = 1 WHERE id = 1", [])
            .unwrap();
        assert_eq!(selected(&conn, 1), vec![2], "row 1 must yield to row 2");
    }

    /// Legacy rows carry predicted_date instead of target_date; they must still drain.
    #[test]
    fn falls_back_to_predicted_date_when_target_date_is_null() {
        let (_d, conn) = seed();
        conn.execute(
            "INSERT INTO insights (id, insight_type, title, content, confidence,
                target_date, predicted_date, status, resolution_attempts)
             VALUES (1, 'prediction', 't', 'c', 0.6, NULL, '2020-01-01', 'expired', 0)",
            [],
        )
        .unwrap();
        assert_eq!(selected(&conn, 10), vec![1]);
    }

    /// No deadline at all = nothing to grade against. Must not be picked.
    #[test]
    fn rows_with_no_deadline_are_skipped() {
        let (_d, conn) = seed();
        conn.execute(
            "INSERT INTO insights (id, insight_type, title, content, confidence,
                target_date, predicted_date, status) 
             VALUES (1, 'prediction', 't', 'c', 0.6, NULL, NULL, 'expired')",
            [],
        )
        .unwrap();
        assert!(selected(&conn, 10).is_empty());
    }
}

#[cfg(test)]
mod citation_tests {
    use super::*;

    // Realistic ids: the live table's max is ~44,923, so nothing collides with an index.
    const SHOWN: [i64; 5] = [44923, 44900, 44888, 44870, 44851];

    /// The live bug, reproduced: the model returned bracket indices like [1,5,0] / [16].
    /// Those must land on the RIGHT stories, not be stored raw.
    #[test]
    fn positional_indices_are_remapped_to_real_story_ids() {
        let r = resolve_story_refs(&[1, 3, 0], &SHOWN);
        assert_eq!(r.ids, vec![44900, 44870, 44923]);
        assert_eq!(r.remapped, 3);
        assert_eq!(r.dropped, 0);
    }

    /// A model that does the right thing must never be "corrected" into a wrong story.
    /// Exact id match takes precedence over reading the number as a position.
    #[test]
    fn real_story_ids_pass_through_untouched() {
        let r = resolve_story_refs(&[44888, 44851], &SHOWN);
        assert_eq!(r.ids, vec![44888, 44851]);
        assert_eq!(r.remapped, 0);
        assert_eq!(r.dropped, 0);
    }

    /// The honesty rule: a citation that resolves to nothing is DROPPED, never stored.
    /// 44999 was not shown and is not a valid position — it cites nothing real.
    #[test]
    fn unresolvable_refs_are_dropped_not_stored() {
        let r = resolve_story_refs(&[44999, 2], &SHOWN);
        assert_eq!(r.ids, vec![44888], "only the resolvable ref survives");
        assert_eq!(r.dropped, 1);
        assert_eq!(r.remapped, 1);
    }

    /// Position-derived keys break under insertion — my own rule says test that first.
    /// The SAME index must follow the story it named when the presented list changes.
    #[test]
    fn remap_follows_the_list_when_a_story_is_inserted_mid_sequence() {
        let before = resolve_story_refs(&[2], &SHOWN);
        assert_eq!(before.ids, vec![44888]);

        // A new story lands at position 1, pushing everything down.
        let shifted = [44923, 44999, 44900, 44888, 44870, 44851];
        let after = resolve_story_refs(&[2], &shifted);
        assert_eq!(after.ids, vec![44900], "index 2 now names a different story");
        // The point: the resolution is always taken against the list ACTUALLY presented
        // to the model, never a remembered one — which is how the connections bug bit.
    }

    /// Duplicate refs (index + real id for the same story) must not double-cite.
    #[test]
    fn duplicate_refs_are_deduped() {
        let r = resolve_story_refs(&[0, 44923, 0], &SHOWN);
        assert_eq!(r.ids, vec![44923]);
    }

    #[test]
    fn empty_and_negative_refs_are_safe() {
        assert_eq!(resolve_story_refs(&[], &SHOWN), RefResolution::default());
        let r = resolve_story_refs(&[-1], &SHOWN);
        assert!(r.ids.is_empty());
        assert_eq!(r.dropped, 1);
    }

    /// With nothing presented, every ref is unresolvable — no citation may be invented.
    #[test]
    fn nothing_presented_means_nothing_cited() {
        let r = resolve_story_refs(&[0, 1, 44923], &[]);
        assert!(r.ids.is_empty());
        assert_eq!(r.dropped, 3);
    }
}
