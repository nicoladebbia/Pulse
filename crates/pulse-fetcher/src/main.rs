mod pipeline;
mod sources;
mod claude;
mod contextual;
pub(crate) mod db;
mod dedup;
mod embeddings;
pub(crate) mod market_prices;
pub(crate) mod calibration;
pub(crate) mod position_management;
pub(crate) mod position_sizing;

use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "pulse-fetcher", about = "Daily news fetch pipeline for Pulse")]
struct Args {
    /// Fetch mode: 'daily' for scheduled fetch, 'manual' for on-demand
    #[arg(long, default_value = "daily")]
    mode: String,

    /// Path to the SQLite database
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Force re-fetch even if today's briefing already exists
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("pulse_fetcher=info".parse()?))
        .init();

    let args = Args::parse();

    // Auto-load .env from project directory
    let env_path = dirs::home_dir()
        .unwrap_or_default()
        .join("Projects/Pulse/.env");
    if env_path.exists() {
        dotenvy::from_path(&env_path).ok();
        tracing::info!("Loaded API keys from {}", env_path.display());
    }

    let db_path = args.db_path.unwrap_or_else(|| {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.pulse.app")
            .join("pulse.db")
    });

    tracing::info!("Pulse fetcher starting in {} mode", args.mode);
    tracing::info!("Database: {}", db_path.display());

    match args.mode.as_str() {
        "notify-test" => {
            // Self-test for the failure-alert path. Fires the FAILED and DEGRADED
            // banners exactly as a real failure would, so delivery can be verified
            // from the launchd context without waiting for an actual outage.
            // (Proved the fire-and-exit timing bug + the .status() fix on 2026-06-22.)
            tracing::info!("Firing test failure + degraded notifications (blocking delivery)...");
            notify_failure("TEST: this is what a failed daily run looks like.");
            crate::pipeline::notify_degraded_test("TEST: this is what a degraded run looks like.");
            tracing::info!("Test notifications fired");
        }
        "backfill-embeddings" => {
            tracing::info!("Backfilling embeddings for stories without them...");
            backfill_embeddings(&db_path).await?;
        }
        "extract-entities" => {
            tracing::info!("Extracting entities from all stories...");
            extract_entities(&db_path).await?;
        }
        "daily" => {
            // Idempotent skip: a day is "done" only if today's daily briefing already
            // has NEWS stories. This is deliberately NOT `status='complete'` — a run
            // blocked at the Groq summarize step still writes financial stories (they
            // skip summarization) and marks the briefing complete with ZERO news, which
            // is exactly the degraded state we want a LATER slot to retry, not skip.
            // Success == news present (source_type='news'). See groq-vpn-block-plan.md.
            if !args.force && db_path.exists() {
                let conn = rusqlite::Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?;
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let news_today: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM stories s \
                         JOIN briefings b ON s.briefing_id = b.id \
                         WHERE b.date = ?1 AND b.briefing_type = 'daily' AND s.source_type = 'news'",
                        [&today],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                if news_today > 0 {
                    tracing::info!("Already fetched {} news stories for {} — skipping (use --force to override)", news_today, today);
                    return Ok(());
                }
            }

            // Preflight reachability probe: if the source IP is on Groq's blocklist
            // (VPN exit node / school network) the whole run would 403 and abort after
            // ~90 min, firing the daily "not working" alert. A blocked slot is EXPECTED,
            // not a failure — exit 0 SILENTLY (no notification) so a later slot retries.
            // With 4 daily slots (7am/12pm/6pm/10pm) one usually catches a clean window.
            if !crate::claude::client::groq_reachable().await {
                tracing::info!("Groq unreachable (blocked source IP) — skipping this slot silently, a later slot will retry.");
                return Ok(());
            }

            // Notify-on-failure safety net: a scheduled daily run that errors AFTER a
            // passing preflight (so the network was reachable) must not fail silently.
            if let Err(e) = pipeline::run(&db_path).await {
                // Durable failure record for the always-visible progress bar: a clean
                // bail (cost cap, mid-run block) can happen before any ProgressWriter
                // stage write, so main.rs stamps the terminal "failed" state here. This
                // is what makes "run crashed 2h ago" queryable by the app.
                pipeline::write_failed_state(
                    &pipeline::progress_file_path(&db_path),
                    &format!("{}", e),
                );
                notify_failure(&format!("Daily run aborted: {}", e));
                return Err(e);
            }

            // Also run freedoms pipeline after daily
            tracing::info!("Running Four Freedoms pipeline...");
            if let Err(e) = pipeline::run_freedoms(&db_path).await {
                tracing::warn!("Freedoms pipeline failed (non-fatal): {}", e);
            }
        }
        "freedoms" => {
            tracing::info!("Running Four Freedoms pipeline...");
            pipeline::run_freedoms(&db_path).await?;
        }
        "fetch-form4" => {
            tracing::info!("Fetching targeted Form 4 filings for tracked companies...");
            match pipeline::run_targeted_form4(&db_path).await {
                Ok(n) => tracing::info!("Targeted Form 4 complete: {} new filings inserted", n),
                Err(e) => tracing::error!("Targeted Form 4 fetch failed: {}", e),
            }
        }
        "backfill-tickers" => {
            // Apply the CIK-mapping fix to ALL unmapped entities in one pass
            // (the daily pipeline caps at 200/run). Expands the tradeable/watchlist universe.
            tracing::info!("Backfilling ticker mappings for all unmapped entities...");
            match pipeline::backfill_tickers(&db_path) {
                Ok(n) => tracing::info!("Backfill complete: {} new ticker mappings", n),
                Err(e) => tracing::error!("Ticker backfill failed: {}", e),
            }
        }
        "manage-positions" => {
            // Run ONLY the exit-evaluation phase. Honors EXIT_DRY_RUN (default true).
            tracing::info!("Running position management (exit evaluation) only...");
            match pipeline::run_position_management(&db_path).await {
                Ok(n) => tracing::info!("Position management complete: {} action(s)", n),
                Err(e) => tracing::error!("Position management failed: {}", e),
            }
        }
        other => {
            tracing::info!("Running in {} mode", other);
            pipeline::run(&db_path).await?;
        }
    }

    tracing::info!("Pulse fetch complete");
    Ok(())
}

/// Fire a macOS notification when a scheduled fetch fails or produces a degraded
/// briefing. This is the safety net for silent failures: the June 2026 incident
/// (NordVPN routing Groq through a blocked VPN exit IP -> every summary 403'd ->
/// empty briefing aborted) ran for ~2.5 days unnoticed because nothing alerted.
///
/// Mirrors the existing success notification in pipeline::send_notification, which
/// already delivers reliably from the launchd context. This is DETECTION, not
/// prevention: it tells Nicola the morning after instead of days later. True
/// prevention is NordVPN split-tunnel (free, GUI) or a provider fallback (costs $).
fn notify_failure(summary: &str) {
    // Must use .status() (blocks until osascript finishes), NOT .spawn(): this is
    // called right before the process exits with Err, and a fire-and-forget spawn
    // gets killed before notificationd delivers the banner. Measured 2026-06-22:
    // fire-and-exit dropped the banner silently; blocking delivers it.
    // best-effort; never let a notification failure mask the real error.
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display notification "{}" with title "Pulse fetch FAILED" sound name "Basso""#,
            summary.replace('"', "'").replace('\\', "")
        ))
        .status();
}

async fn backfill_embeddings(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    // Find stories without embeddings
    let stories: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.headline, s.summary, s.key_facts
             FROM stories s
             LEFT JOIN story_embeddings se ON se.story_id = s.id
             WHERE se.story_id IS NULL
             ORDER BY s.id"
        )?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?.collect::<Result<Vec<_>, _>>()?
    };

    if stories.is_empty() {
        tracing::info!("All stories already have embeddings");
        return Ok(());
    }

    tracing::info!("Found {} stories without embeddings", stories.len());

    // Process in batches of 10 (Voyage free tier: 10K TPM limit)
    for chunk in stories.chunks(10) {
        let texts: Vec<String> = chunk.iter().map(|(_, headline, summary, key_facts)| {
            format!("{}. {}. {}", headline, summary, key_facts)
        }).collect();

        let ids: Vec<i64> = chunk.iter().map(|(id, _, _, _)| *id).collect();

        match embeddings::generate_from_texts(&texts).await {
            Ok(embeddings) => {
                for (i, emb) in embeddings.iter().enumerate() {
                    let blob: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
                    conn.execute(
                        "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                        rusqlite::params![ids[i], blob],
                    )?;
                }
                tracing::info!("Embedded batch of {} stories (IDs {}-{})", chunk.len(), ids[0], ids[ids.len()-1]);
            }
            Err(e) => {
                tracing::warn!("Embedding batch failed: {}", e);
            }
        }

        // Rate limit pause (Voyage free tier = 3 RPM)
        tokio::time::sleep(std::time::Duration::from_secs(21)).await;
    }

    let count: i64 = conn.query_row("SELECT COUNT(*) FROM story_embeddings", [], |r| r.get(0))?;
    tracing::info!("Backfill complete. Total embeddings in DB: {}", count);
    Ok(())
}

async fn extract_entities(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    // Run migration 003 if needed
    let applied: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
        stmt.query_map([], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?
    };
    if !applied.contains(&3) {
        conn.execute_batch(include_str!("../../../migrations/003_intelligence.sql"))?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (3)", [])?;
    }

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    // Get all stories that don't have entity_mentions yet
    let stories: Vec<(i64, String, String, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.headline, s.summary, s.sector, b.date, s.why_it_matters
             FROM stories s
             JOIN briefings b ON b.id = s.briefing_id
             LEFT JOIN entity_mentions em ON em.story_id = s.id
             WHERE em.id IS NULL
             ORDER BY s.id"
        )?;
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
        })?.collect::<Result<Vec<_>, _>>()?
    };

    if stories.is_empty() {
        tracing::info!("All stories already have entities extracted");
        return Ok(());
    }

    tracing::info!("Found {} stories to extract entities from", stories.len());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Process in batches of 15 stories per Haiku call
    for (batch_idx, chunk) in stories.chunks(30).enumerate() {
        let mut stories_text = String::new();
        for (id, headline, summary, sector, date, why) in chunk {
            stories_text.push_str(&format!(
                "\n[Story {}] [{}] [{}] {}\n{}\n{}\n",
                id, sector, date, headline, summary, why
            ));
        }

        let body = serde_json::json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 2000,
            "system": r#"Extract named entities from these news stories. Return valid JSON only.

For each entity provide:
- name: The canonical name (e.g., "OpenAI" not "openai")
- entity_type: one of "company", "person", "topic", "product", "regulation"
- sentiment: -1.0 to 1.0 (how the story portrays this entity)
- context: One brief sentence about the mention
- story_id: The story ID number from the [Story N] tag

Return: {"entities": [{"name": "...", "entity_type": "...", "sentiment": 0.5, "context": "...", "story_id": 123}]}

Focus on the MOST important entities (max 5 per story). Prioritize companies, key people, and products over generic topics."#,
            "messages": [{"role": "user", "content": stories_text}]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            tracing::warn!("Haiku entity extraction failed for batch {}: {}", batch_idx, err);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        let response: serde_json::Value = resp.json().await?;
        let text = response["content"][0]["text"].as_str().unwrap_or("{}");

        // Extract JSON from response
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else { text }
        } else { text };

        #[derive(serde::Deserialize)]
        struct ExtractedEntity {
            name: String,
            entity_type: String,
            sentiment: f64,
            context: Option<String>,
            story_id: i64,
        }
        #[derive(serde::Deserialize)]
        struct ExtractionResult {
            entities: Vec<ExtractedEntity>,
        }

        match serde_json::from_str::<ExtractionResult>(json_str) {
            Ok(result) => {
                let mut stored = 0;
                let valid_types = ["company", "person", "topic", "product", "regulation"];
                for ent in &result.entities {
                    // Validate entity_type against CHECK constraint
                    let entity_type = ent.entity_type.to_lowercase();
                    let entity_type = entity_type.trim();
                    if !valid_types.contains(&entity_type) {
                        continue; // Skip invalid types
                    }
                    let name_normalized = ent.name.to_lowercase().trim().to_string();
                    if name_normalized.is_empty() { continue; }

                    // Find the sector and date for this story
                    let story_meta = chunk.iter().find(|(id, _, _, _, _, _)| *id == ent.story_id);
                    let (sector, date) = match story_meta {
                        Some((_, _, _, s, d, _)) => (s.as_str(), d.as_str()),
                        None => continue,
                    };

                    // Upsert entity
                    conn.execute(
                        "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6)
                         ON CONFLICT(name_normalized, entity_type) DO UPDATE SET
                           last_seen = MAX(last_seen, ?5),
                           sentiment_avg = (sentiment_avg * mention_count + ?6) / (mention_count + 1),
                           mention_count = mention_count + 1",
                        rusqlite::params![ent.name, name_normalized, entity_type, sector, date, ent.sentiment],
                    )?;

                    let entity_id: i64 = conn.query_row(
                        "SELECT id FROM entities WHERE name_normalized = ?1 AND entity_type = ?2",
                        rusqlite::params![name_normalized, entity_type],
                        |row| row.get(0),
                    )?;

                    conn.execute(
                        "INSERT INTO entity_mentions (entity_id, story_id, sentiment, context, mentioned_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![entity_id, ent.story_id, ent.sentiment, ent.context, date],
                    )?;

                    stored += 1;
                }
                tracing::info!("Batch {}: extracted {} entities from {} stories", batch_idx + 1, stored, chunk.len());
            }
            Err(e) => {
                tracing::warn!("Failed to parse entity extraction result for batch {}: {}", batch_idx, e);
            }
        }

        // Rate limit
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // Recompute signals
    tracing::info!("Recomputing signals...");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let signal_count = recompute_signals(&conn, &today)?;
    tracing::info!("Computed {} signals", signal_count);

    let entity_count: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;
    let mention_count: i64 = conn.query_row("SELECT COUNT(*) FROM entity_mentions", [], |r| r.get(0))?;
    tracing::info!("Entity extraction complete. {} entities, {} mentions", entity_count, mention_count);
    Ok(())
}

fn recompute_signals(conn: &rusqlite::Connection, today: &str) -> anyhow::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT e.name, e.sector,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-7 days') THEN 1 ELSE 0 END) as w7,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-30 days') THEN 1 ELSE 0 END) as w30,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-90 days') THEN 1 ELSE 0 END) as w90,
            COUNT(DISTINCT em.mentioned_at) as days_active
         FROM entities e
         JOIN entity_mentions em ON em.entity_id = e.id
         GROUP BY e.name, e.sector"
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, i64, i64)> = stmt.query_map(
        [today],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
    )?.collect::<Result<Vec<_>, _>>()?;

    let mut count = 0;
    for (topic, sector, w7, w30, w90, days_active) in &rows {
        let acceleration = if *w30 == 0 {
            if *w7 > 0 { 10.0 } else { 0.0 }
        } else {
            let r7 = *w7 as f64 / 7.0;
            let r30 = *w30 as f64 / 30.0;
            if r30 < 0.001 { if *w7 > 0 { 10.0 } else { 0.0 } } else { r7 / r30 }
        };

        let total = (*w30).max(*w7);
        let trajectory = if *w7 == 0 && *w30 == 0 { "dormant" }
            else if total >= 14 && *days_active >= 10 { "dominant" }
            else if total >= 7 && *days_active >= 5 { "hot" }
            else if acceleration < 0.8 && total >= 3 { "fading" }
            else if total >= 3 || *days_active >= 2 { "rising" }
            else if *w7 > 0 { "rising" }
            else { "dormant" };

        conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(topic, sector) DO UPDATE SET
               window_7d = ?3, window_30d = ?4, window_90d = ?5,
               acceleration = ?6, trajectory = ?7, updated_at = datetime('now')",
            rusqlite::params![topic, sector, w7, w30, w90, acceleration, trajectory],
        )?;
        count += 1;
    }

    Ok(count)
}
