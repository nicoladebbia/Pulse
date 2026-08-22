mod cost;
mod form4;
pub(crate) mod notify;
mod signals;
mod tickers;
mod trading;
pub(crate) use form4::{classify_ambiguous_8ks, enrich_form4_stories, fetch_targeted_form4,
    run_targeted_form4};
pub(crate) use notify::{notify_degraded, notify_degraded_test, notify_info, send_notification};
pub(crate) use signals::{compute_cross_signals, recompute_signals_pipeline, run_backfill_signals};
pub(crate) use tickers::{backfill_tickers, populate_tickers, resolve_entities, run_recanonicalize};
pub(crate) use trading::{auto_trade_on_convergence, manage_open_positions, run_auto_trade,
    run_position_management, snapshot_portfolio};

mod predictions;
pub(crate) use predictions::{compute_calibration_stats, generate_predictions,
    validate_and_expire_predictions};
mod progress;
pub(crate) use cost::{API_USAGE_RETENTION_DAYS, EMBEDDING_COVERAGE_DROP_PCT,
    EMBEDDING_COVERAGE_FLOOR_PCT, check_daily_cost_cap, log_usage};
pub(crate) use progress::{HeartbeatGuard, ProgressWriter, progress_file_path, spawn_heartbeat, write_failed_state};

use std::path::Path;

// --- Progress reporting ---






















pub async fn run(db_path: &Path) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    let mut progress = ProgressWriter::new(db_path);
    // Stamp a fresh "running" record immediately (clears any prior failed/interrupted
    // state) so the UI reflects THIS run during the pre-Phase-1 network calls below,
    // which run for several seconds before the first start_stage().
    progress.start_run();

    // Keep `updated_at` moving for the whole run, so the app's 240s silence rule only
    // ever fires on a genuinely dead process. Without this, Phase 1 alone (one progress
    // write, then 9–120 minutes of collecting) painted a false "Fetch stopped
    // unexpectedly" on nearly every run. Aborted on every exit path below.
    let heartbeat = spawn_heartbeat(progress_file_path(db_path));
    let _heartbeat = HeartbeatGuard(heartbeat);

    // Cost guardrail: bail before any LLM call if today's spend already crossed the cap.
    check_daily_cost_cap(db_path)?;

    // Phase 0a: Entity-targeted Form 4 fetch — pull insider filings PER tracked CIK
    // (the global EDGAR fetch only catches the recent-50, leaving ~83% of converging
    // companies blind on insider, the largest-weighted signal). Runs before enrichment
    // so today's new filings get their transaction details filled in the same run.
    tracing::info!("Phase 0a: Fetching targeted Form 4 filings for tracked companies...");
    match fetch_targeted_form4(db_path).await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Targeted Form 4: inserted {} new insider filings", count);
            }
        }
        Err(e) => tracing::warn!("Targeted Form 4 fetch failed (non-fatal): {}", e),
    }
    progress.heartbeat_starting("Fetching insider filings");

    // Phase 0: Enrich Form 4 stories from previous runs (before EDGAR fetch burns SEC rate limit)
    tracing::info!("Phase 0: Enriching prior Form 4 filings with transaction data...");
    match enrich_form4_stories(db_path).await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Enriched {} Form 4 stories with transaction data", count);
            }
        }
        Err(e) => tracing::warn!("Form 4 enrichment failed (non-fatal): {}", e),
    }
    progress.heartbeat_starting("Preparing sources");

    // Phase 1: Collect from all sources
    progress.start_stage(1);
    tracing::info!("Phase 1: Collecting from sources...");
    sources::API_CALLS.reset();
    let mut all_articles = sources::collect_all().await?;

    // Wikipedia Pageviews — needs DB access for entity lookup (non-fatal)
    match sources::wikipedia::fetch(db_path).await {
        Ok(a) => {
            if !a.is_empty() {
                tracing::info!("Wikipedia Pageviews: {} search trend signals", a.len());
                all_articles.extend(a);
            }
        }
        Err(e) => tracing::warn!("Wikipedia Pageviews FAILED (non-fatal): {}", e),
    }

    let raw_articles: Vec<_> = all_articles.into_iter()
        .filter(|a| !a.sector.starts_with("freedom_"))
        .collect();
    let raw_count = raw_articles.len();
    tracing::info!("Collected {} raw articles (excluding freedom sources)", raw_count);

    // Log per-sector distribution at collection time
    {
        let sectors = ["ai", "miami", "italy", "tech"];
        for sector in &sectors {
            let count = raw_articles.iter().filter(|a| a.sector == *sector && a.source_type == "news").count();
            if count < 10 {
                tracing::warn!("Low source coverage: {} has only {} articles (expected 20+)", sector, count);
            } else {
                tracing::info!("Source coverage: {} = {} articles", sector, count);
            }
        }
    }

    // Split financial articles OUT before dedup — they have their own dedup mechanism
    // and should NOT go through the O(n²) trigram comparison designed for news
    let (raw_news, raw_financial): (Vec<_>, Vec<_>) = raw_articles
        .into_iter()
        .partition(|a| a.source_type != "financial");

    // Log API calls for quota tracking — 1 row per actual HTTP request,
    // not per batch. Each source module bumps its atomic counter inside
    // every fetch() call; we write that many rows here.
    {
        let snapshot = sources::API_CALLS.snapshot();
        for (provider, calls) in snapshot {
            for _ in 0..calls {
                log_usage(db_path, provider, "fetch", "collect", 0, 0);
            }
        }
    }

    // FINANCIAL PATH: dedup + convert + write to DB IMMEDIATELY (before news dedup)
    // This ensures financial data is stored even if the news pipeline times out
    let financial_articles = if !raw_financial.is_empty() && db_path.exists() {
        let pre_count = raw_financial.len();
        let deduped = dedup_financial_articles(db_path, raw_financial);
        tracing::info!("{} financial articles after dedup (from {})", deduped.len(), pre_count);
        deduped
    } else {
        raw_financial
    };

    let financial_stories: Vec<crate::claude::SummarizedStory> = financial_articles
        .into_iter()
        .map(|article| {
            let (key_facts, why_it_matters, what_to_watch) = generate_financial_fts_fields(&article);
            crate::claude::SummarizedStory {
                headline: article.title.clone(),
                summary: article.content_snippet.clone(),
                key_facts,
                why_it_matters,
                what_to_watch,
                importance_score: 5,
                sentiment: None,
                novelty: None,
                event_type: Some("financial_data".to_string()),
                article,
            }
        })
        .collect();

    // Financial stories will be written after the main briefing (Phase 8) to avoid creating a stub briefing

    // NEWS PATH: dedup (slow O(n²) trigram comparison)
    progress.start_stage(2);
    tracing::info!("Phase 2: Deduplicating {} news articles ({} financial already stored)...",
        raw_news.len(), financial_stories.len());
    let (historical_hashes, historical_titles) = if db_path.exists() {
        match rusqlite::Connection::open(db_path) {
            Ok(conn) => crate::dedup::load_recent_hashes(&conn, 7),
            Err(e) => {
                tracing::warn!("Could not open DB for historical dedup: {}", e);
                (std::collections::HashSet::new(), Vec::new())
            }
        }
    } else {
        (std::collections::HashSet::new(), Vec::new())
    };
    let news_articles = crate::dedup::deduplicate_with_history(raw_news, historical_hashes, historical_titles);
    tracing::info!("{} news articles after dedup", news_articles.len());

    // Abort if too few new articles (not worth API cost for a thin briefing)
    let total_unique = news_articles.len() + financial_stories.len();
    if total_unique < 15 && news_articles.len() < 15 {
        tracing::info!("Only {} new articles after dedup — too few for a quality briefing, skipping", total_unique);
        progress.finish();
        return Ok(());
    }

    // Phase 2.5: Pre-curate — pick the best ~90 NEWS articles BEFORE expensive summarization
    // (Financial articles skip this — they're already structured data)
    // Degradation flags for the post-run health alert: every fallback below is
    // deliberately non-fatal, which is exactly why each one must be COUNTED — the
    // Scout decommission (2026-07-17) ran the dumb pre-curation fallback for 3
    // weeks with nothing but a log line nobody reads.
    let mut pre_curate_fell_back = false;
    let mut analysis_degraded = false;
    let articles_to_summarize = if news_articles.len() > 100 {
        tracing::info!("Pre-curating: selecting best articles from {} candidates...", news_articles.len());
        let api_key = std::env::var("GROQ_API_KEY")
            .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
        let client = crate::claude::client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;
        match client.pre_curate(&news_articles).await {
            Ok(indices) => {
                let curated: Vec<_> = indices.into_iter()
                    .filter_map(|i| news_articles.get(i).cloned())
                    .collect();
                tracing::info!("Pre-curated to {} articles (saved {} summarization calls)",
                    curated.len(), news_articles.len() - curated.len());
                curated
            }
            Err(e) => {
                tracing::warn!("Pre-curation failed (non-fatal): {}", e);
                pre_curate_fell_back = true;
                // Sector-balanced cap: ensure each sector is represented
                let fallback = if news_articles.len() > 150 {
                    tracing::info!("Sector-balanced cap from {} to ~150 articles", news_articles.len());
                    let sectors = ["ai", "miami", "italy", "tech"];
                    let mut balanced = Vec::with_capacity(150);
                    let per_sector = 37; // ~150 / 4
                    for sector in &sectors {
                        let sector_articles: Vec<_> = news_articles.iter()
                            .filter(|a| a.sector == *sector)
                            .take(per_sector)
                            .cloned()
                            .collect();
                        tracing::info!("  {}: {} articles (of {} available)", sector, sector_articles.len(),
                            news_articles.iter().filter(|a| a.sector == *sector).count());
                        balanced.extend(sector_articles);
                    }
                    // Fill remaining slots with any sector
                    if balanced.len() < 150 {
                        let already: std::collections::HashSet<String> = balanced.iter().map(|a| a.url.clone()).collect();
                        for a in &news_articles {
                            if balanced.len() >= 150 { break; }
                            if !already.contains(&a.url) {
                                balanced.push(a.clone());
                            }
                        }
                    }
                    balanced
                } else {
                    news_articles
                };
                fallback
            }
        }
    } else {
        tracing::info!("Skipping pre-curation ({} articles, threshold 100)", news_articles.len());
        news_articles
    };

    // Phase 3: Summarize pre-curated NEWS articles (not financial — they're already structured)
    progress.start_stage(3);
    // Ground summaries in real article text (best-effort — fetch failures keep
    // the snippet). Without this, models fabricate specifics from title+snippet.
    tracing::info!("Phase 3: Fetching article bodies for {} stories...", articles_to_summarize.len());
    let articles_to_summarize = crate::article_text::enrich(articles_to_summarize).await;
    // Grounding coverage feeds the post-run health alert: 0 grounded articles
    // means summaries are back to snippet-only fabrication territory.
    let grounded_count = articles_to_summarize.iter()
        .filter(|a| a.content_snippet.contains("\n\nArticle text: "))
        .count();
    tracing::info!("Phase 3: Summarizing {} stories ({} grounded in article text)...",
        articles_to_summarize.len(), grounded_count);
    let outcome = crate::claude::summarize_stories(&articles_to_summarize, Some(&progress), db_path).await?;
    let summarize_failure = outcome.failure;
    let summaries = outcome.stories;
    let sum_count = summaries.len() as i64;
    let sum_failed = articles_to_summarize.len() as i64 - sum_count;
    // No hardcoded log_usage here — GroqClient now logs each summarize_story call
    // with REAL token counts from the API response.

    if sum_failed > 0 {
        tracing::warn!("Summarized {}/{} stories ({} failed)", sum_count, articles_to_summarize.len(), sum_failed);
    } else {
        tracing::info!("Summarized all {} stories successfully", sum_count);
    }

    // Bail if NEWS is empty — not just when both news AND financial are empty.
    // A block at the Groq summarize step leaves financial stories intact (they skip
    // summarization), which previously wrote a financial-only, news-EMPTY briefing
    // marked 'complete'. That gutted briefing became today's daily view (the "old
    // stories" the app shows) AND masked the last good briefing. Bailing here instead
    // means the run produces NO daily briefing this slot, the app keeps showing the
    // last GOOD day, and a later slot (7am/12pm/6pm/10pm) retries once Groq is
    // reachable — the news-based skip check in main.rs won't treat this as "done".
    // The preflight probe should catch most blocks before we get here; this is the
    // backstop for a block that starts mid-run (reachable at probe, blocked by summarize).
    if summaries.is_empty() {
        // Report the error that was actually observed. This line lands at the top
        // of fetch-progress.json and is the first thing read during an outage; it
        // used to assert "likely a blocked API (VPN/network)" no matter what the
        // upstream said, and in August 2026 that guess pointed at the network for
        // days while the real cause was Groq deleting the whole Llama family.
        anyhow::bail!(
            "No news stories could be summarized. {} Aborting so a later slot retries \
             instead of storing a news-empty briefing.",
            summarize_failure
                .as_deref()
                .map(|f| format!("{f}."))
                .unwrap_or_else(|| "No per-story errors were recorded, which is itself \
                     unexpected — check fetch-stdout.log for the upstream error."
                    .to_string())
        );
    }

    // Degraded-briefing alert: the run will continue (financial stories may have
    // saved it from a hard abort), but if the news summarizer mostly failed the
    // briefing is gutted. Without this, a Groq block that leaves financial intact
    // returns Ok and fails silently — the same class of bug as the June 2026
    // incident, just narrower. Threshold: <50% of attempted summaries succeeded.
    let attempted = articles_to_summarize.len() as i64;
    if attempted > 0 && sum_count * 2 < attempted {
        let msg = format!(
            "Only {}/{} news stories summarized. Briefing is degraded. {}",
            sum_count,
            attempted,
            summarize_failure.as_deref().unwrap_or("No per-story errors were recorded.")
        );
        tracing::error!("{}", msg);
        notify_degraded(&msg);
    }

    // Phase 4: Cross-sector analysis (news only) — BEST-EFFORT, must not abort the run.
    // The Groq block flips on/off mid-run: a run can pass the preflight probe and
    // summarize all news, then get 403'd here at analyze (observed 2026-07-13 22:12).
    // analyze sits UPSTREAM of the DB write (Phase 8), so a hard `?` here threw away
    // every summarized story AND fired the false "FAILED" alert — reproducing both of
    // the user's original symptoms on any intermittent-connectivity day. On failure we
    // fall back to a degraded analysis (same stories, no cross-sector enrichment) so the
    // news still persists. Also neutralizes the stochastic serde-on-200 abort inside
    // analyze. See .plans/groq-vpn-block-plan.md.
    progress.start_stage(4);
    tracing::info!("Phase 4: Cross-sector analysis...");
    let mut analysis = match crate::claude::analyze_cross_sector(&summaries, db_path).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Cross-sector analysis failed ({}) — persisting news WITHOUT cross-sector enrichment (degraded briefing).", e);
            analysis_degraded = true;
            crate::claude::degraded_analysis(&summaries)
        }
    };

    // Dedicated connections pass: the 4-task analyze prompt under-delivers
    // connections (measured 0-2/run). A single-task call over the curated list
    // complies much better. Run whenever analyze produced <3; keep the longer list.
    if !analysis_degraded && analysis.connections.len() < 3 {
        if let Ok(api_key) = std::env::var("GROQ_API_KEY") {
            if let Ok(client) = crate::claude::client::GroqClient::new(&api_key, Some(db_path.to_path_buf())) {
                match client.find_connections(&analysis.curated_stories).await {
                    Ok(conns) if conns.len() > analysis.connections.len() => {
                        tracing::info!("Connections upgraded {} -> {} via dedicated pass", analysis.connections.len(), conns.len());
                        analysis.connections = conns;
                    }
                    Ok(conns) => tracing::info!("Dedicated pass found {} connections — keeping analyze's {}", conns.len(), analysis.connections.len()),
                    Err(e) => tracing::warn!("Dedicated connections pass failed (non-fatal): {}", e),
                }
            }
        }
    }

    // Log sector distribution in curated stories
    {
        let sectors = ["ai", "miami", "italy", "tech"];
        for sector in &sectors {
            let count = analysis.curated_stories.iter().filter(|s| s.article.sector == *sector).count();
            tracing::info!("Briefing curation: {} = {} stories", sector, count);
        }
        let total = analysis.curated_stories.len();
        if total < 60 {
            tracing::warn!("Briefing has only {} stories (expected ~80)", total);
        }
    }

    // Phase 5: Executive summary (non-fatal)
    progress.start_stage(5);
    tracing::info!("Phase 5: Generating executive summary...");
    let executive_summary = match generate_executive_summary(&analysis, db_path).await {
        Ok(s) => {
            tracing::info!("Executive summary: {} chars", s.len());
            Some(s)
        }
        Err(e) => {
            tracing::warn!("Executive summary generation failed (non-fatal): {}", e);
            None
        }
    };

    // Phase 6: Contextual prefixes (non-fatal, only for stories without existing entity coverage)
    progress.start_stage(6);
    tracing::info!("Phase 6: Generating contextual prefixes...");
    let prefixes = if db_path.exists() {
        // Check which stories already have entity mentions (from previous fetches)
        let stories_needing_prefix: Vec<&crate::claude::SummarizedStory> = if let Ok(conn) = rusqlite::Connection::open(db_path) {
            analysis.curated_stories.iter()
                .filter(|s| {
                    // Story needs a prefix if it doesn't already have entity mentions
                    let has_mentions: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM entity_mentions em JOIN stories st ON st.id = em.story_id WHERE st.headline = ?1)",
                        [&s.headline],
                        |row| row.get(0),
                    ).unwrap_or(false);
                    !has_mentions
                })
                .collect()
        } else {
            analysis.curated_stories.iter().collect()
        };

        if stories_needing_prefix.is_empty() {
            tracing::info!("All stories have entity coverage, skipping prefix generation");
            None
        } else {
            tracing::info!("Generating prefixes for {} stories (skipping {} with entity coverage)",
                stories_needing_prefix.len(), analysis.curated_stories.len() - stories_needing_prefix.len());
            let day_context: String = analysis.curated_stories.iter()
                .map(|s| format!("[{}] {}", s.article.sector, s.headline))
                .collect::<Vec<_>>()
                .join("\n");
            // Generate prefixes for all stories but only send the ones that need it
            // (contextual::generate_prefixes expects the full array for cross-referencing)
            match crate::contextual::generate_prefixes(&analysis.curated_stories, &day_context).await {
                Ok(p) => {
                    let count = p.iter().filter(|x| x.is_some()).count();
                    tracing::info!("Generated {} contextual prefixes", count);
                    let prefix_batches = ((analysis.curated_stories.len() + 9) / 10) as i64;
                    log_usage(db_path, "anthropic", "claude-haiku", "contextual_prefixes", prefix_batches * 2500, prefix_batches * 500);
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!("Contextual prefix generation failed (non-fatal): {}", e);
                    None
                }
            }
        }
    } else {
        let day_context: String = analysis.curated_stories.iter()
            .map(|s| format!("[{}] {}", s.article.sector, s.headline))
            .collect::<Vec<_>>()
            .join("\n");
        match crate::contextual::generate_prefixes(&analysis.curated_stories, &day_context).await {
            Ok(p) => { tracing::info!("Generated {} contextual prefixes", p.iter().filter(|x| x.is_some()).count()); Some(p) }
            Err(e) => { tracing::warn!("Contextual prefix generation failed: {}", e); None }
        }
    };

    // Phase 7: Generate embeddings for NEWS stories only (financial already written)
    progress.start_stage(7);
    let news_count = analysis.curated_stories.len();
    tracing::info!("Phase 7.5: Generating embeddings for {} news stories...", news_count);
    let embeddings = match crate::embeddings::generate(&analysis.curated_stories, prefixes.as_deref(), |detail, pct| progress.update_detail(detail, pct)).await {
        Ok(embs) => {
            tracing::info!("Generated {} embeddings", embs.len());
            log_usage(db_path, "voyage", "voyage-3-lite", "embeddings", (embs.len() as i64) * 200, 0);
            Some(embs)
        }
        Err(e) => {
            tracing::warn!("Embedding generation failed (non-fatal): {}", e);
            None
        }
    };

    // Phase 8: Write NEWS stories to database (with embeddings)
    progress.start_stage(8);
    tracing::info!("Phase 8: Writing {} news stories to database...", analysis.curated_stories.len());
    write_to_db(db_path, &analysis, embeddings.as_deref(), prefixes.as_deref(), executive_summary.as_deref())?;

    // Phase 8.1: Write financial stories (after main briefing exists so they share the same briefing_id)
    if !financial_stories.is_empty() {
        tracing::info!("Writing {} financial stories to database...", financial_stories.len());
        match write_financial_stories(db_path, &financial_stories) {
            Ok(count) => tracing::info!("Stored {} financial stories successfully", count),
            Err(e) => tracing::warn!("Financial story write failed: {}", e),
        }
        record_financial_dedup(db_path, &financial_stories);
    }

    // Phase 8.5: Auto-backfill missing embeddings from previous failed runs (non-fatal)
    {
        let conn = rusqlite::Connection::open(db_path)?;
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM stories", [], |r| r.get(0))?;
        let embedded: i64 = conn.query_row("SELECT COUNT(*) FROM story_embeddings WHERE story_id > 0", [], |r| r.get(0))?;
        let missing = total - embedded;

        if missing > 0 {
            tracing::warn!("Embedding coverage: {}/{} stories ({:.0}%) — backfilling {} missing",
                embedded, total, (embedded as f64 / total as f64) * 100.0, missing);
            match backfill_missing_embeddings(db_path, 50).await {
                Ok(filled) => {
                    if filled > 0 {
                        tracing::info!("Auto-backfill: embedded {} previously missing stories", filled);
                    }
                }
                Err(e) => tracing::warn!("Auto-backfill failed (non-fatal): {}", e),
            }
        } else {
            tracing::info!("Embedding coverage: {}/{} stories (100%)", embedded, total);
        }
    }

    // Phase 9: Extract entities (non-fatal)
    // `entities_extracted` is captured for pipeline_health; it read 0 on every historical
    // row purely because it was never passed to the INSERT, not because extraction failed.
    let mut entities_extracted: i64 = 0;
    progress.start_stage(9);
    tracing::info!("Phase 9: Extracting entities...");
    match extract_entities_from_stories(db_path, &analysis, &progress).await {
        Ok(count) => {
            entities_extracted += count as i64;
            tracing::info!("Extracted {} entity mentions", count);
            // ~2000 tokens in, ~500 out per batch of 30 stories; ~3 batches for 80 stories
            let batches = ((analysis.curated_stories.len() + 29) / 30) as i64;
            log_usage(db_path, "anthropic", "claude-haiku", "entity_extraction", batches * 2000, batches * 500);
        }
        Err(e) => tracing::warn!("Entity extraction failed (non-fatal): {}", e),
    }

    // Phase 9.5: Extract entities from financial_metadata (no LLM, instant)
    tracing::info!("Phase 9.5: Extracting entities from financial stories...");
    match extract_entities_from_financial_metadata(db_path) {
        Ok(count) => {
            entities_extracted += count as i64;
            if count > 0 {
                tracing::info!("Extracted {} entity mentions from financial metadata", count);
            }
        }
        Err(e) => tracing::warn!("Financial entity extraction failed (non-fatal): {}", e),
    }

    // Phase 9.7: Classify ambiguous 8-K filings via Haiku (non-fatal)
    tracing::info!("Phase 9.7: Classifying ambiguous 8-K filings...");
    match classify_ambiguous_8ks(db_path).await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Classified {} ambiguous 8-K filings via Haiku", count);
            }
        }
        Err(e) => tracing::warn!("8-K classification failed (non-fatal): {}", e),
    }

    // Phase 10: Deep summaries for top stories (non-fatal)
    progress.start_stage(10);
    tracing::info!("Phase 10: Generating deep summaries for top stories...");
    match generate_deep_summaries(db_path).await {
        Ok(count) => tracing::info!("Generated {} deep summaries", count),
        Err(e) => tracing::warn!("Deep summary generation failed (non-fatal): {}", e),
    }

    // Phase 11: Resolve predictions (hybrid router: market/LLM/manual) (non-fatal)
    progress.update_detail("Resolving predictions", 100.0);
    tracing::info!("Phase 11: Resolving predictions...");
    // Captured for pipeline_health below — this column read 0 on every row ever written
    // because it was simply never passed to the INSERT, so the health table could not
    // show that the prediction loop had stalled at 39 resolved for months.
    let mut predictions_validated: i64 = 0;
    match validate_and_expire_predictions(db_path).await {
        Ok((resolved, expired)) => {
            predictions_validated = resolved as i64;
            if resolved > 0 || expired > 0 {
                tracing::info!("Predictions: {} resolved, {} expired", resolved, expired);
            }
        }
        Err(e) => tracing::warn!("Prediction resolution failed (non-fatal): {}", e),
    }

    // Phase 11.1: Compute calibration stats (daily aggregation) (non-fatal)
    tracing::info!("Phase 11.1: Computing prediction calibration...");
    if let Err(e) = compute_calibration_stats(db_path).await {
        tracing::warn!("Calibration computation failed (non-fatal): {}", e);
    }

    // Phase 11.5: Entity resolution — merge duplicate entities into canonical records
    progress.update_detail("Resolving entities to canonical records", 100.0);
    tracing::info!("Phase 11.5: Resolving entities...");
    match resolve_entities(db_path) {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Resolved {} entities to canonical records", count);
            }
        }
        Err(e) => tracing::warn!("Entity resolution failed (non-fatal): {}", e),
    }

    // Phase 12: Auto-populate ticker mappings + fetch market prices (non-fatal)
    progress.update_detail("Fetching market prices", 100.0);
    tracing::info!("Phase 12: Fetching market prices...");
    {
        // First populate ticker mappings from SEC data
        match populate_tickers(db_path) {
            Ok(count) => {
                if count > 0 {
                    tracing::info!("Mapped {} entities to tickers", count);
                }
            }
            Err(e) => tracing::warn!("Ticker mapping failed (non-fatal): {}", e),
        }
        // Then fetch prices for mapped entities
        match crate::market_prices::fetch_prices(db_path).await {
            Ok(count) => {
                if count > 0 {
                    tracing::info!("Fetched {} market prices", count);
                }
            }
            Err(e) => tracing::warn!("Market price fetch failed (non-fatal): {}", e),
        }
    }

    // Phase 12.5: Recompute entity signals before cross-signal detection
    progress.update_detail("Recomputing entity signals", 100.0);
    tracing::info!("Phase 12.5: Recomputing entity signals...");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    {
        let conn = rusqlite::Connection::open(db_path)?;
        match recompute_signals_pipeline(&conn, &today, 90) {
            Ok(count) => {
                if count > 0 {
                    tracing::info!("Recomputed {} entity signals", count);
                }
            }
            Err(e) => tracing::warn!("Signal recomputation failed (non-fatal): {}", e),
        }
    }

    // Phase 13: Cross-signal detection (non-fatal)
    progress.update_detail("Computing cross-signal scores", 100.0);
    tracing::info!("Phase 13: Computing cross-signal scores...");
    match compute_cross_signals(db_path, &today) {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Computed {} cross-signal scores", count);
            }
        }
        Err(e) => tracing::warn!("Cross-signal computation failed (non-fatal): {}", e),
    }

    // Phase 13.5: Auto paper trade on convergence (non-fatal)
    tracing::info!("Phase 13.5: Checking for auto-trade opportunities...");
    match auto_trade_on_convergence(db_path).await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Executed {} auto paper trades from convergence signals", count);
            } else {
                tracing::info!("No convergence signals meeting trade criteria");
            }
        }
        Err(e) => tracing::warn!("Auto-trade failed (non-fatal): {}", e),
    }

    // Phase 13.6: Manage open positions — evaluate exits (stops/targets/expiry).
    // Unlike auto-BUY (gated off by AUTO_TRADE_ENABLED), exits are a SAFETY
    // mechanism and run by default, but start in dry-run (EXIT_DRY_RUN=true)
    // until verified. This is the half that was missing: positions never closed.
    tracing::info!("Phase 13.6: Managing open positions (exit evaluation)...");
    match manage_open_positions(db_path).await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Position management: {} exit action(s) taken", count);
            } else {
                tracing::info!("Position management: no exits triggered");
            }
        }
        Err(e) => tracing::warn!("Position management failed (non-fatal): {}", e),
    }

    // Phase 13.7: Verify the realized-P&L WRITE on every close path (non-fatal).
    // Path-agnostic table query over resolved post-arm rows — catches a bad/NULL
    // pnl write from the sell branch OR the 404/reconcile no-fill branch, either of
    // which would silently corrupt the edge-report substrate. Fires a permanent
    // breakage alarm + a one-time "first close" runtime confirmation (rule #8).
    {
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        if let Err(e) = crate::edge_report::verify_pnl_writes(db_path, &now, &notify_info) {
            tracing::warn!("P&L-write verification failed (non-fatal): {}", e);
        }
    }

    // Phase 14: Automated calibration (non-fatal)
    progress.update_detail("Calibrating signal weights", 100.0);
    tracing::info!("Phase 14: Running signal calibration...");
    match crate::calibration::run_calibration(db_path).await {
        Ok(report) => {
            if report.positions_evaluated > 0 {
                tracing::info!("Calibration: evaluated {} positions", report.positions_evaluated);
            }
            if report.brier_scores_updated > 0 {
                tracing::info!("Calibration: updated {} Brier scores", report.brier_scores_updated);
            }
            if report.signal_analysis.total_resolved > 0 {
                tracing::info!("Calibration: {:.0}% overall hit rate ({} resolved trades)",
                    report.signal_analysis.overall_hit_rate * 100.0,
                    report.signal_analysis.total_resolved);
            }
            if report.weights_adjusted {
                tracing::warn!("Calibration: reweight PROPOSED and held in pending_calibration — not applied. Review + apply manually.");
            }
            if !report.signal_analysis.dead_signals.is_empty() {
                tracing::warn!("Calibration: dead signals detected: {}",
                    report.signal_analysis.dead_signals.join(", "));
            }
        }
        Err(e) => tracing::warn!("Calibration failed (non-fatal): {}", e),
    }

    // Phase 14.1: Daily portfolio snapshot (non-fatal). Runs after calibration
    // so the equity reflects any closes Phase 14 just produced.
    tracing::info!("Phase 14.1: Snapshotting portfolio...");
    match snapshot_portfolio(db_path).await {
        Ok(true) => tracing::info!("Portfolio snapshot recorded"),
        Ok(false) => tracing::info!("Portfolio snapshot skipped"),
        Err(e) => tracing::warn!("Portfolio snapshot failed (non-fatal): {}", e),
    }

    // Phase 14.2: Announce ONCE when the post-arm edge sample first reaches N=20
    // (non-fatal). No verdict, no flag flip — a human runs `--mode edge-report`.
    // Removes the "remember to check in weeks" dependency; latched, fires once.
    {
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        if let Err(e) = crate::edge_report::maybe_announce_edge_sample(db_path, &now, &notify_info) {
            tracing::warn!("Edge-sample announce check failed (non-fatal): {}", e);
        }
    }

    // Phase 14.5: Generate predictions (Sonnet daily, Opus Sunday) (non-fatal)
    progress.update_detail("Generating predictions", 100.0);
    // Runs AFTER Phase 13 cross-signals are computed so predictions can
    // reference them. See plan .plans/signals-predictions-rework-plan.md Push 2.
    tracing::info!("Phase 14.5: Generating predictions...");
    {
        // Gather top stories
        let top_stories: Vec<(i64, String, String, String)> = {
            let conn = rusqlite::Connection::open(db_path)?;
            let mut stmt = conn.prepare(
                "SELECT id, headline, summary, sector
                 FROM stories
                 WHERE DATE(created_at) = DATE('now')
                 ORDER BY importance_score DESC, created_at DESC
                 LIMIT 40"
            )?;
            stmt.query_map([], |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            )))?.filter_map(|r| r.ok()).collect()
        };

        // Gather top cross-signals
        let top_signals: Vec<(i64, String, Option<String>, f64)> = {
            let conn = rusqlite::Connection::open(db_path)?;
            let mut stmt = conn.prepare(
                "WITH latest AS (
                   SELECT cs.*, ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY computed_at DESC) AS rn
                   FROM cross_signals cs
                   WHERE cs.computed_at >= date('now', '-7 days')
                 )
                 SELECT cs.entity_id, e.name, cs.ticker, cs.compound_score
                 FROM latest cs
                 LEFT JOIN entities e ON e.id = cs.entity_id
                 WHERE cs.rn = 1 AND cs.compound_score > 0.3
                 ORDER BY cs.compound_score DESC
                 LIMIT 20"
            )?;
            stmt.query_map([], |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, Option<String>>(2)?,
                row.get::<_, f64>(3)?,
            )))?.filter_map(|r| r.ok()).collect()
        };

        match generate_predictions(db_path, &top_stories, &top_signals).await {
            Ok(count) => tracing::info!("Generated {} predictions", count),
            Err(e) => tracing::warn!("Prediction generation failed (non-fatal): {}", e),
        }
    }

    // Done
    progress.finish();
    // Best-effort: news is already persisted by now, so a flaky osascript must NOT
    // turn a successful run into an Err (which would fire notify_failure — a false
    // "FAILED" alert on a run that actually succeeded).
    if let Err(e) = send_notification(analysis.curated_stories.len()) {
        tracing::warn!("Success notification failed (non-fatal): {}", e);
    }

    let duration = start.elapsed();

    // Pipeline health summary — write to DB and log
    {
        let conn = rusqlite::Connection::open(db_path)?;
        crate::db::run_migrations(&conn)?;

        let total_stories: i64 = conn.query_row("SELECT COUNT(*) FROM stories", [], |r| r.get(0)).unwrap_or(0);
        let total_embeddings: i64 = conn.query_row("SELECT COUNT(*) FROM story_embeddings WHERE story_id > 0", [], |r| r.get(0)).unwrap_or(0);
        let emb_pct = if total_stories > 0 { (total_embeddings as f64 / total_stories as f64) * 100.0 } else { 0.0 };
        let feeds_failed =
            crate::sources::SOURCES_FAILED.load(std::sync::atomic::Ordering::Relaxed) as i64;

        // api_usage retention. Migration 031 does the one-time cleanup of the 329k rows
        // that accumulated with no policy since April; this keeps it that way. Cheap —
        // idx_api_usage_created makes it a range delete, and on a normal day it removes
        // ~2,500 rows.
        match conn.execute(
            "DELETE FROM api_usage WHERE created_at < datetime('now', ?1)",
            [format!("-{} day", API_USAGE_RETENTION_DAYS)],
        ) {
            Ok(n) if n > 0 => tracing::info!("Pruned {} api_usage rows older than {} days", n, API_USAGE_RETENTION_DAYS),
            Ok(_) => {}
            Err(e) => tracing::warn!("api_usage prune failed (non-fatal): {}", e),
        }

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            // entities_extracted / predictions_validated / feeds_failed were MISSING from
            // this INSERT, so all three read 0 on all 161 historical rows. The health
            // table was reporting three permanent zeros as if they were measurements.
            "INSERT INTO pipeline_health (run_date, stories_fetched, stories_summarized, stories_embedded, entities_extracted, predictions_validated, feeds_failed, embedding_coverage_pct, summary_failures, duration_secs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![today, raw_count, analysis.curated_stories.len(), total_embeddings, entities_extracted, predictions_validated, feeds_failed, emb_pct, sum_failed, duration.as_secs_f64()],
        ).ok();

        tracing::info!("╔══════════════════════════════════════════╗");
        tracing::info!("║         PIPELINE HEALTH SUMMARY          ║");
        tracing::info!("╠══════════════════════════════════════════╣");
        tracing::info!("║ Articles fetched:    {:>6}              ║", raw_count);
        tracing::info!("║ Stories summarized:  {:>6}              ║", analysis.curated_stories.len());
        tracing::info!("║ Summary failures:    {:>6}              ║", sum_failed);
        tracing::info!("║ Sources failed:      {:>6}              ║", feeds_failed);
        tracing::info!("║ Entities extracted:  {:>6}              ║", entities_extracted);
        tracing::info!("║ Predictions graded:  {:>6}              ║", predictions_validated);
        tracing::info!("║ Embedding coverage:  {:>5.1}%              ║", emb_pct);
        tracing::info!("║ Total stories in DB: {:>6}              ║", total_stories);
        tracing::info!("║ Duration:            {:>5.1}s              ║", duration.as_secs_f64());
        tracing::info!("╚══════════════════════════════════════════╝");

        // Silent-degradation alert: a run can end Ok while quietly worse than
        // yesterday's — every check below corresponds to a fallback that once
        // masked a real outage for days/weeks. One notification, all findings.
        let mut degradations: Vec<String> = Vec::new();
        if pre_curate_fell_back {
            degradations.push("pre-curation fell back to sector cap (LLM call failed)".into());
        }
        if analysis_degraded {
            degradations.push("analyze failed entirely — no relevance/connections/trends".into());
        }
        if !analysis_degraded && analysis.connections.is_empty() {
            degradations.push("0 cross-sector connections in today's briefing".into());
        }
        if grounded_count == 0 {
            degradations.push("0 stories grounded in article text (all snippet-only)".into());
        }
        if feeds_failed > 0 {
            degradations.push(format!("{} of {} sources failed or timed out",
                feeds_failed, crate::sources::SOURCE_COUNT));
        }
        if crate::claude::client::groq_is_blocked() {
            // The briefing LANDED — that is the whole point of the fallback — but on the
            // paid provider, so this must be visible rather than silently more expensive.
            degradations.push(
                "Groq was IP-blocked; the whole briefing ran on the Anthropic fallback".into(),
            );
        }
        // Embedding coverage: the number the app has been recording faithfully and never
        // acting on. It fell 100% (Mar 2026) -> 49.7% (Aug 2026) — ~15.8k stories absent
        // from vector search — entirely unnoticed, because pipeline_health had no reader.
        // Two separate checks: an absolute floor, and a drop vs the last recorded run
        // (which catches a sudden collapse long before it drags the average down).
        if emb_pct < EMBEDDING_COVERAGE_FLOOR_PCT {
            degradations.push(format!(
                "embedding coverage {:.1}% is below the {:.0}% floor — {} stories are invisible to search",
                emb_pct,
                EMBEDDING_COVERAGE_FLOOR_PCT,
                total_stories - total_embeddings
            ));
        }
        if let Ok(prev) = conn.query_row::<f64, _, _>(
            "SELECT embedding_coverage_pct FROM pipeline_health
             WHERE run_date < ?1 ORDER BY run_date DESC LIMIT 1",
            [&today],
            |r| r.get(0),
        ) {
            if prev - emb_pct > EMBEDDING_COVERAGE_DROP_PCT {
                degradations.push(format!(
                    "embedding coverage dropped {:.1}pp since the last run ({:.1}% -> {:.1}%)",
                    prev - emb_pct, prev, emb_pct
                ));
            }
        }
        if !analysis_degraded {
            // analyze succeeded — but on which provider? No anthropic row today
            // means the Haiku path silently fell back to Groq 70B.
            let anthropic_analyze_today: i64 = conn.query_row(
                "SELECT COUNT(*) FROM api_usage WHERE provider='anthropic' AND endpoint='analyze'
                 AND DATE(created_at) = DATE('now')",
                [], |r| r.get(0),
            ).unwrap_or(0);
            if anthropic_analyze_today == 0 {
                degradations.push("analyze ran on Groq fallback, not Claude Haiku".into());
            }
        }
        if !degradations.is_empty() {
            let msg = format!("Briefing OK but degraded: {}", degradations.join("; "));
            tracing::warn!("{}", msg);
            notify_degraded(&msg);
        }
    }

    Ok(())
}


// ----------------------------------------------------------------------------
// Hybrid resolver types + helpers (Push 2, Task 2.7)
// ----------------------------------------------------------------------------











async fn generate_freedoms_summary(curated: &[(&str, &crate::claude::SummarizedStory)], db_path: &Path) -> anyhow::Result<String> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = crate::claude::client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;

    let mut input = String::new();
    for (freedom, story) in curated {
        input.push_str(&format!("[{}] {} — {}\n", freedom, story.headline, story.summary.chars().take(100).collect::<String>()));
    }

    let system = r#"You write the executive summary for a daily Four Freedoms briefing covering Time, Wealth, Location, and Health, plus a dedicated Whoop section when there is news worth reporting on the Whoop wearable.

=== HARD FORMAT RULE — read this first ===

Output is ONE single paragraph of 4-5 sentences. Flowing prose. No section labels. No headers. No bullets. No markdown of any kind — no asterisks, no bold, no italics, no colons used as labels. Do not write the words "Time Freedom", "Wealth Freedom", "Location Freedom", "Health Freedom", or "Whoop" as a section header anywhere in the output. Do not segment by category. Move between topics naturally inside one paragraph.

=== CONTENT ===

Describe what is happening across the categories today. Name specific companies, people, dollar amounts, tickers, and concrete numbers. Cover at least three of the five categories (Time, Wealth, Location, Health, Whoop). Mention Whoop only when there is genuine Whoop news in the input — otherwise skip it entirely rather than padding. Connect stories when they are genuinely related. Stay third-person and descriptive — this is a news briefing, not coaching.

=== VOICE: DESCRIPTIVE, NOT ADVISORY ===

Report on events. Do NOT address the reader. Never use "you" or "your". Never use imperatives directed at the reader. Never use phrasings like "consider...", "explore...", "leverage...", "keep an eye on...", "watch for...", "build a presence...".

=== OPENING ===

Start the FIRST sentence with a concrete subject — a company name, person, or specific trend. Do NOT start with "Here's...", "Today's...", "This summary...", "The following...", or any meta-framing.

=== EXAMPLE — exactly this format ===

GOOD (one paragraph, no labels, flowing):
Microsoft's AI integration push is reshaping productivity tools while SpaceX's Starlink revenue hit $4.4B, shifting the economics of working remotely. A $292M Kelp DAO exploit underscored DeFi fragility even as Strategy overtook BlackRock in Bitcoin holdings, and Q1 2026 venture capital deployment hit a record $148B. New US Citizenship and Immigration Services guidance expanded STEM OPT eligibility to twelve additional fields, while Whoop launched a continuous glucose monitor priced at $299. A Trump executive order accelerated FDA review of psychedelic therapies, potentially opening a major mental-health treatment pipeline within the year.

BAD — section-label format (NEVER produce this):
**Time Freedom**: Google's AI-powered smart glasses are revolutionizing wearables. **Wealth Freedom**: Nasdaq's CEO predicts a fundamental shift in markets. **Location Freedom**: SpaceX is gearing up for its IPO.

BAD — advisory tone (NEVER produce this):
To optimize for time freedom, consider leveraging AI tools. Explore the creator economy by building a presence on YouTube.

BAD — meta preamble (NEVER produce this):
Here's a summary of today's four freedoms: ..."#;

    let raw = client.call_text(&crate::claude::client::strong_model(), "freedoms_executive_summary", system, &input, 600).await?;
    Ok(clean_theme_output(&raw))
}

/// Strip common preamble/formatting noise from LLM executive-summary outputs.
fn clean_theme_output(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    // Remove surrounding quotes (straight, smart, single)
    let quote_chars: &[char] = &['"', '\u{201C}', '\u{201D}', '\'', '\u{2018}', '\u{2019}'];
    if let (Some(first), Some(last)) = (s.chars().next(), s.chars().last()) {
        if quote_chars.contains(&first) && quote_chars.contains(&last) && s.len() > 1 {
            s = s.trim_matches(quote_chars).to_string();
        }
    }

    // Strip markdown bold/italic
    s = s.replace("**", "").replace("__", "");

    // Strip freedom section labels if the model regressed to label format.
    // The "X Freedom:" forms are unambiguous and safe to strip anywhere.
    // The bare "X:" forms can match mid-sentence ("In Health: a new wearable...")
    // so we only strip them at line starts where they signal a label-list regression.
    const UNAMBIGUOUS_LABELS: &[&str] = &[
        "Time Freedom:", "Wealth Freedom:", "Location Freedom:", "Health Freedom:",
        "TIME FREEDOM:", "WEALTH FREEDOM:", "LOCATION FREEDOM:", "HEALTH FREEDOM:",
        "Whoop Freedom:", "WHOOP FREEDOM:",
    ];
    for label in UNAMBIGUOUS_LABELS {
        s = s.replace(label, "");
    }
    const BARE_LINE_LABELS: &[&str] = &[
        "Time:", "Wealth:", "Location:", "Health:", "Whoop:", "WHOOP:",
    ];
    s = s
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            for label in BARE_LINE_LABELS {
                if let Some(rest) = trimmed.strip_prefix(label) {
                    return rest.trim_start().to_string();
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Collapse runs of whitespace introduced by the strip
    while s.contains("  ") { s = s.replace("  ", " "); }
    s = s.replace(" .", ".").replace(" ,", ",");

    // Smart preamble strip: if the text opens with a meta-framing clause
    // followed by a colon (e.g. "Here's a 2-4 sentence summary of today's
    // briefing:", "Following is a summary:", "Below are the highlights:"),
    // strip everything up to and including that first colon.
    //
    // Only triggers if the pre-colon chunk contains a meta signal word AND
    // is short enough to be a preamble (≤200 chars) — prevents chopping
    // real prose that happens to have an early colon.
    const META_SIGNALS: &[&str] = &[
        "here's", "here is", "heres",
        "summary", "briefing", "overview", "highlights",
        "following", "below",
        "most important story",
    ];
    if let Some(colon_pos) = s.find(':') {
        if colon_pos <= 200 {
            let pre = &s[..colon_pos].to_lowercase();
            if META_SIGNALS.iter().any(|sig| pre.contains(sig)) {
                s = s[colon_pos + 1..].trim_start_matches([' ', '-', '—', '\n']).to_string();
            }
        }
    }

    // Fallback: static prefix match for preambles that don't end in a colon
    let preambles = [
        "today's thread",
        "today's theme",
        "the most important story today is",
        "the most important story is",
        "today's most important story is",
    ];
    let lower = s.to_lowercase();
    for p in &preambles {
        if lower.starts_with(p) {
            s = s[p.len()..].trim_start_matches([':', ' ', '-', '—']).trim().to_string();
            break;
        }
    }

    s.trim().to_string()
}

// ============================================================================
// Prediction generator (Task 2.4 + 2.4b) — Sonnet daily, Opus Sunday
// ============================================================================






async fn generate_executive_summary(analysis: &crate::claude::AnalysisResult, db_path: &Path) -> anyhow::Result<String> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = crate::claude::client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;

    // Build compact input: top 5 stories by importance + connections
    let mut sorted = analysis.curated_stories.clone();
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));
    sorted.truncate(5);

    let mut input = String::new();
    for s in &sorted {
        input.push_str(&format!("[{}] {} — {}\n", s.article.sector, s.headline, s.summary.chars().take(100).collect::<String>()));
    }
    if !analysis.connections.is_empty() {
        input.push_str("\nCross-sector connections:\n");
        for c in &analysis.connections {
            input.push_str(&format!("- {} → {}\n", c.connection, c.insight));
        }
    }

    let system = "You write executive summaries for a daily intelligence briefing. The reader is a tech founder in Miami who builds AI apps, Shopify tools, and iOS apps. Italian heritage, follows Serie A.\n\nWrite exactly 3-5 sentences synthesizing today's most important developments. Name specific companies, numbers, and developments. Be direct and insightful — no preamble, no bullet points, no greeting. Just flowing prose that answers 'what happened today?'";

    client.call_text(crate::claude::client::fast_model(), "executive_summary", system, &input, 300).await
}

/// Generate deep summaries for stories with relevance_score >= 8.
/// Uses Anthropic Claude Sonnet for higher quality analysis.
/// Capped at 5 stories to control API costs.
// Takes no `analysis`: it selects its own candidates straight from the DB
// (relevance_score >= 8 for today), so the parameter was always ignored.
async fn generate_deep_summaries(db_path: &std::path::Path) -> anyhow::Result<usize> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;

    // Find stories with relevance_score >= 8 from today's briefing
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.what_to_watch, s.sector
         FROM stories s
         JOIN briefings b ON b.id = s.briefing_id
         WHERE b.date = ?1 AND s.relevance_score >= 8
         ORDER BY s.relevance_score DESC, s.importance_score DESC
         LIMIT 5"
    )?;

    let candidates: Vec<(i64, String, String, String, String, String, String)> = stmt
        .query_map([&today], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if candidates.is_empty() {
        tracing::info!("No stories scored >= 8 for deep summaries");
        return Ok(0);
    }

    tracing::info!("{} stories qualify for deep analysis", candidates.len());

    // 60s per-request timeout — without this, a hung Anthropic connection
    // (e.g. school-network DNS stall) freezes the whole pipeline indefinitely.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut count = 0;

    for (story_id, headline, summary, key_facts, why_it_matters, what_to_watch, sector) in &candidates {
        let system = "You are an intelligence analyst writing a deep briefing for a tech founder. Write a thorough analysis (400-600 words) structured as:\n\n**Background**: Context and history behind this story\n**Key Players**: Who's involved and their motivations\n**Multiple Angles**: Different perspectives on this development\n**Implications**: What this means for tech, business, and the reader's world\n**What Happens Next**: Prediction of likely outcomes\n\nBe specific — name companies, cite numbers, draw connections. No preamble.";

        let user_msg = format!(
            "Sector: {}\nHeadline: {}\nSummary: {}\nKey Facts: {}\nWhy It Matters: {}\nWhat to Watch: {}",
            sector, headline, summary, key_facts, why_it_matters, what_to_watch
        );

        let body = serde_json::json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 1500,
            "system": system,
            "messages": [{"role": "user", "content": user_msg}]
        });

        match client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(parsed) = resp.json::<serde_json::Value>().await {
                    if let Some(text) = parsed["content"][0]["text"].as_str() {
                        conn.execute(
                            "UPDATE stories SET summary_depth = 'deep', deep_summary = ?1 WHERE id = ?2",
                            rusqlite::params![text, story_id],
                        )?;
                        count += 1;
                        tracing::info!("Deep summary for story {} ({} chars)", story_id, text.len());
                    }
                }
            }
            Ok(resp) => {
                let status = resp.status();
                tracing::warn!("Deep summary API returned {} for story {}", status, story_id);
            }
            Err(e) => {
                tracing::warn!("Deep summary request failed for story {}: {}", story_id, e);
            }
        }

        // Brief delay between requests
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(count)
}

use crate::sources;


/// Backfill embeddings for stories that are missing them (from previous failed runs).
/// Processes up to `max_stories` at a time to avoid long-running API calls.
async fn backfill_missing_embeddings(db_path: &Path, max_stories: usize) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;

    let stories: Vec<(i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.headline, s.summary, s.key_facts
             FROM stories s
             LEFT JOIN story_embeddings se ON se.story_id = s.id
             WHERE se.story_id IS NULL
             ORDER BY s.id DESC
             LIMIT ?1"
        )?;
        stmt.query_map([max_stories as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?.collect::<Result<Vec<_>, _>>()?
    };

    if stories.is_empty() {
        return Ok(0);
    }

    // Batch by tokens, not a fixed 10 — the free tier caps TPM as well as RPM, and a
    // 10-story request spends a quarter of the per-request token budget while still paying
    // the full rate-limit pause. See embeddings::VOYAGE_REQUEST_TOKEN_BUDGET.
    let all_texts: Vec<String> = stories.iter().map(|(_, headline, summary, key_facts)| {
        format!("{}. {}. {}", headline, summary, key_facts)
    }).collect();
    let batches = crate::embeddings::batch_by_tokens(
        &all_texts,
        crate::embeddings::VOYAGE_REQUEST_TOKEN_BUDGET,
    );

    let mut filled = 0;
    for (bstart, bend) in batches {
        let chunk = &stories[bstart..bend];
        if filled > 0 {
            // Was a hardcoded 21s that silently ignored PULSE_VOYAGE_RPM, so raising the
            // rate limit sped up every embedding path EXCEPT this one.
            let pause = crate::embeddings::rate_limit_pause_secs();
            tokio::time::sleep(std::time::Duration::from_secs(pause)).await;
        }

        let texts: Vec<String> = all_texts[bstart..bend].to_vec();
        let ids: Vec<i64> = chunk.iter().map(|(id, _, _, _)| *id).collect();

        match crate::embeddings::generate_from_texts(&texts).await {
            Ok(embeddings) => {
                for (i, emb) in embeddings.iter().enumerate() {
                    let blob: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
                    conn.execute(
                        "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                        rusqlite::params![ids[i], blob],
                    )?;
                    filled += 1;
                }
            }
            Err(e) => {
                if e.to_string().contains("429") {
                    tracing::info!("Auto-backfill: rate limited, waiting 60s before retry...");
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    // Retry once
                    match crate::embeddings::generate_from_texts(&texts).await {
                        Ok(embeddings) => {
                            for (i, emb) in embeddings.iter().enumerate() {
                                let blob: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
                                conn.execute(
                                    "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                                    rusqlite::params![ids[i], blob],
                                )?;
                                filled += 1;
                            }
                        }
                        Err(_) => {
                            tracing::warn!("Auto-backfill: retry also failed, stopping (filled {} so far)", filled);
                            break;
                        }
                    }
                } else {
                    tracing::warn!("Auto-backfill batch failed: {}", e);
                    break;
                }
            }
        }
    }

    if filled > 0 {
        log_usage(db_path, "voyage", "voyage-3-lite", "backfill_embeddings", (filled as i64) * 200, 0);
    }
    Ok(filled)
}

fn write_to_db(db_path: &Path, analysis: &crate::claude::AnalysisResult, embeddings: Option<&[crate::embeddings::StoryEmbedding]>, prefixes: Option<&[Option<String>]>, executive_summary: Option<&str>) -> anyhow::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;

    // Run migrations (transaction-wrapped, with ALTER TABLE guards)
    crate::db::run_migrations(&conn)?;

    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let time_label = now.format("%-I:%M %p").to_string(); // e.g. "8:00 AM", "9:00 PM"

    // Count stories per sector
    let ai_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "ai").count();
    let miami_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "miami").count();
    let italy_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "italy").count();
    let tech_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "tech").count();
    let total = analysis.curated_stories.len();

    let tx = conn.unchecked_transaction()?;

    // Insert new briefing (no delete — multiple briefings per day are allowed)
    tx.execute(
        "INSERT INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status, time_label)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'complete', ?7)",
        rusqlite::params![today, total, ai_count, miami_count, italy_count, tech_count, time_label],
    )?;
    let briefing_id = tx.last_insert_rowid();

    // 1.5 Store executive summary if available
    if let Some(summary) = executive_summary {
        tx.execute(
            "UPDATE briefings SET executive_summary = ?1 WHERE id = ?2",
            rusqlite::params![summary, briefing_id],
        )?;
    }

    // 2. Insert stories, tracking IDs for connection mapping
    let mut story_db_ids: Vec<i64> = Vec::with_capacity(total);
    let mut first_ai = true;

    for (i, story) in analysis.curated_stories.iter().enumerate() {
        let is_hero = if story.article.sector == "ai" && first_ai {
            first_ai = false;
            1
        } else {
            0
        };

        let key_facts_json = serde_json::to_string(&story.key_facts)?;

        let context_prefix = prefixes
            .and_then(|p| p.get(i))
            .and_then(|p| p.as_ref().map(|s| s.as_str()));

        tx.execute(
            "INSERT INTO stories (
                briefing_id, sector, original_title, original_url, original_language,
                content_snippet, source_name, published_at, headline, summary,
                key_facts, why_it_matters, what_to_watch, importance_score,
                is_hero, display_order, url_hash, title_hash, context_prefix,
                sentiment, novelty, event_type, source_type, financial_metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            rusqlite::params![
                briefing_id,
                story.article.sector,
                story.article.title,
                story.article.url,
                story.article.language,
                story.article.content_snippet,
                story.article.source_name,
                story.article.published_at,
                story.headline,
                story.summary,
                key_facts_json,
                story.why_it_matters,
                story.what_to_watch,
                story.importance_score,
                is_hero,
                i as i32,
                crate::dedup::url_hash(&story.article.url),
                crate::dedup::title_hash(&story.article.title),
                context_prefix,
                story.sentiment,
                story.novelty,
                story.event_type,
                story.article.source_type,
                story.article.financial_metadata,
            ],
        )?;
        story_db_ids.push(tx.last_insert_rowid());

        // Insert primary source
        let story_id = *story_db_ids.last().unwrap();
        tx.execute(
            "INSERT INTO story_sources (story_id, source_name, source_url, article_url, is_primary)
             VALUES (?1, ?2, ?3, ?4, 1)",
            rusqlite::params![
                story_id,
                story.article.source_name,
                story.article.source_url,
                story.article.url,
            ],
        )?;
    }

    // 3. Apply relevance scores
    //
    // An out-of-range story_idx means the model invented a story number. That was
    // dropped silently by a bare `if let Some`, so a run where the LLM hallucinated
    // half its indices looked identical to a clean one — the stories simply kept
    // their default relevance and nothing said why.
    let mut hallucinated = 0usize;
    for score in &analysis.relevance_scores {
        if let Some(&db_id) = story_db_ids.get(score.story_idx) {
            tx.execute(
                "UPDATE stories SET relevance_score = ?1, relevance_reason = ?2 WHERE id = ?3",
                rusqlite::params![score.relevance, score.reason, db_id],
            )?;
        } else {
            hallucinated += 1;
            tracing::warn!(
                "LLM hallucinated story_idx {} (only {} stories) — relevance score dropped",
                score.story_idx,
                story_db_ids.len()
            );
        }
    }
    if hallucinated > 0 {
        tracing::warn!(
            "{} of {} relevance scores referenced a story that does not exist",
            hallucinated,
            analysis.relevance_scores.len()
        );
    }

    // 4. Insert cross-connections
    for conn_link in &analysis.connections {
        let id_a = story_db_ids.get(conn_link.story_idx_a);
        let id_b = story_db_ids.get(conn_link.story_idx_b);
        if let (Some(&a), Some(&b)) = (id_a, id_b) {
            tx.execute(
                "INSERT INTO cross_connections (briefing_id, story_id_a, story_id_b, connection_text, insight_text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![briefing_id, a, b, conn_link.connection, conn_link.insight],
            )?;
        }
    }

    // 5. Update hero_story_id on briefing
    if let Some(&hero_id) = story_db_ids.first() {
        tx.execute(
            "UPDATE briefings SET hero_story_id = ?1 WHERE id = ?2",
            rusqlite::params![hero_id, briefing_id],
        )?;
    }

    // 6. Store embeddings (if available)
    if let Some(embs) = embeddings {
        let mut stored = 0;
        for emb in embs {
            if let Some(&db_id) = story_db_ids.get(emb.story_index) {
                let blob: Vec<u8> = emb.embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
                tx.execute(
                    "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![db_id, blob],
                )?;
                stored += 1;
            }
        }
        tracing::info!("Stored {} embeddings", stored);
    }

    tx.commit()?;
    tracing::info!("Wrote {} stories to briefing {}", total, briefing_id);
    Ok(())
}

async fn extract_entities_from_stories(db_path: &Path, analysis: &crate::claude::AnalysisResult, progress: &ProgressWriter) -> anyhow::Result<usize> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let conn = rusqlite::Connection::open(db_path)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Build a lookup from story index to its DB ID and sector
    let story_ids: Vec<(i64, String)> = {
        let mut result = Vec::new();
        for story in &analysis.curated_stories {
            let id: Option<i64> = conn.query_row(
                "SELECT id FROM stories WHERE headline = ?1 AND published_at = ?2",
                rusqlite::params![story.headline, today],
                |row| row.get(0),
            ).ok().or_else(|| {
                conn.query_row(
                    "SELECT id FROM stories WHERE headline = ?1 ORDER BY id DESC LIMIT 1",
                    rusqlite::params![story.headline],
                    |row| row.get(0),
                ).ok()
            });
            result.push((id.unwrap_or(0), story.article.sector.clone()));
        }
        result
    };

    let valid_types = [
        "company", "person", "topic", "product", "regulation",
        "insider_trade", "contract_award", "patent_cluster",
        "lobbying_disclosure", "institutional_holding",
        "private_placement", "material_event", "regulatory_action",
    ];
    let mut total_stored = 0;

    // Process stories in batches of 15
    let entity_batches = (analysis.curated_stories.len() + 29) / 30;
    for (batch_start, chunk) in analysis.curated_stories.chunks(30).enumerate().map(|(i, c)| (i * 30, c)) {
        // Heartbeat: keep the progress file fresh so a long entity pass isn't misread as
        // interrupted, and so the bar visibly advances within stage 9.
        let batch_no = batch_start / 30 + 1;
        progress.update_detail(
            &format!("Extracting entities (batch {}/{})", batch_no, entity_batches.max(1)),
            (batch_no as f64 / entity_batches.max(1) as f64) * 100.0,
        );
        let mut stories_text = String::new();
        for (i, story) in chunk.iter().enumerate() {
            let global_idx = batch_start + i;
            let story_id = story_ids.get(global_idx).map(|(id, _)| *id).unwrap_or(0);
            stories_text.push_str(&format!(
                "\n[Story {}] [{}] {}\n{}\n{}\n",
                story_id, story.article.sector, story.headline, story.summary, story.why_it_matters
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
Focus on MOST important entities (max 5 per story). Prioritize companies, key people, and products over generic topics."#,
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
            tracing::warn!("Entity extraction batch failed: {}", resp.status());
            continue;
        }

        let response: serde_json::Value = resp.json().await?;
        let text = response["content"][0]["text"].as_str().unwrap_or("{}");
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') { &text[start..=end] } else { text }
        } else { text };

        #[derive(serde::Deserialize)]
        struct Ent { name: String, entity_type: String, sentiment: f64, context: Option<String>, story_id: Option<i64> }
        #[derive(serde::Deserialize)]
        struct Res { entities: Vec<Ent> }

        if let Ok(result) = serde_json::from_str::<Res>(json_str) {
            for ent in &result.entities {
                let et = ent.entity_type.to_lowercase();
                let et = et.trim();
                if !valid_types.contains(&et) { continue; }
                let nn = ent.name.to_lowercase().trim().to_string();
                if nn.is_empty() { continue; }

                // Find the sector for this story
                let sector = ent.story_id
                    .and_then(|sid| story_ids.iter().find(|(id, _)| *id == sid))
                    .map(|(_, s)| s.as_str())
                    .unwrap_or("general");

                if let Err(e) = conn.execute(
                    "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6)
                     ON CONFLICT(name_normalized, entity_type) DO UPDATE SET
                       last_seen = MAX(last_seen, ?5),
                       sentiment_avg = (sentiment_avg * mention_count + ?6) / (mention_count + 1),
                       mention_count = mention_count + 1",
                    rusqlite::params![ent.name, nn, et, sector, today, ent.sentiment],
                ) {
                    tracing::warn!("entity insert failed for '{}': {}", ent.name, e);
                    continue;
                }

                // Insert entity_mention linking entity to story
                if let Some(story_id) = ent.story_id {
                    if story_id > 0 {
                        let entity_id: Option<i64> = conn.query_row(
                            "SELECT id FROM entities WHERE name_normalized = ?1 AND entity_type = ?2",
                            rusqlite::params![nn, et],
                            |row| row.get(0),
                        ).ok();

                        if let Some(eid) = entity_id {
                            if let Err(e) = conn.execute(
                                "INSERT INTO entity_mentions (entity_id, story_id, sentiment, context, mentioned_at)
                                 VALUES (?1, ?2, ?3, ?4, ?5)",
                                rusqlite::params![eid, story_id, ent.sentiment, ent.context, today],
                            ) {
                                tracing::warn!("entity_mention insert failed for '{}': {}", ent.name, e);
                            }
                        }
                    }
                }

                total_stored += 1;
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // Recompute signals
    let mut stmt = conn.prepare(
        "SELECT e.name, e.sector,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-7 days') THEN 1 ELSE 0 END),
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-30 days') THEN 1 ELSE 0 END),
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-90 days') THEN 1 ELSE 0 END),
            COUNT(DISTINCT em.mentioned_at)
         FROM entities e JOIN entity_mentions em ON em.entity_id = e.id
         GROUP BY e.name, e.sector"
    )?;
    let rows: Vec<(String, Option<String>, i64, i64, i64, i64)> = stmt.query_map(
        [&today], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
    )?.collect::<Result<Vec<_>, _>>()?;

    for (topic, sector, w7, w30, w90, days_active) in &rows {
        let acc = if *w30 == 0 { if *w7 > 0 { 10.0 } else { 0.0 } }
            else { (*w7 as f64 / 7.0) / (*w30 as f64 / 30.0).max(0.001) };
        let total = (*w30).max(*w7);
        let traj = if *w7 == 0 && *w30 == 0 { "dormant" }
            else if total >= 14 && *days_active >= 10 { "dominant" }
            else if total >= 7 && *days_active >= 5 { "hot" }
            else if acc < 0.8 && total >= 3 { "fading" }
            else if total >= 3 || *days_active >= 2 { "rising" }
            else if *w7 > 0 { "rising" }
            else { "dormant" };
        if let Err(e) = conn.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(topic, sector) DO UPDATE SET
               window_7d=?3, window_30d=?4, window_90d=?5, acceleration=?6, trajectory=?7, updated_at=datetime('now')",
            rusqlite::params![topic, sector, w7, w30, w90, acc, traj],
        ) {
            tracing::warn!("signal upsert failed for '{}': {}", topic, e);
        }
    }

    Ok(total_stored)
}

async fn extract_entities_from_freedoms(
    db_path: &Path,
    curated: &[(&str, &crate::claude::SummarizedStory)],
) -> anyhow::Result<usize> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let conn = rusqlite::Connection::open(db_path)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let valid_types = [
        "company", "person", "topic", "product", "regulation",
        "insider_trade", "contract_award", "patent_cluster",
        "lobbying_disclosure", "institutional_holding",
        "private_placement", "material_event", "regulatory_action",
    ];
    let mut total_stored = 0;

    // Build stories text
    let mut stories_text = String::new();
    for (i, (freedom, story)) in curated.iter().enumerate() {
        stories_text.push_str(&format!(
            "\n[Story {}] [{}] {}\n{}\n{}\n",
            i, freedom, story.headline, story.summary, story.why_it_matters
        ));
    }

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 2000,
        "system": r#"Extract named entities from these news stories. Return valid JSON only.
For each entity: name, entity_type (company/person/topic/product/regulation), sentiment (-1.0 to 1.0), context (brief).
Return: {"entities": [{"name": "...", "entity_type": "...", "sentiment": 0.5, "context": "..."}]}
Focus on MOST important entities (max 5 per story)."#,
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
        tracing::warn!("Freedom entity extraction failed: {}", resp.status());
        return Ok(0);
    }

    let response: serde_json::Value = resp.json().await?;
    let text = response["content"][0]["text"].as_str().unwrap_or("{}");
    let json_str = if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') { &text[start..=end] } else { text }
    } else { text };

    #[derive(serde::Deserialize)]
    // No `context` field: this path inserts into `entities` only, which has no context
    // column. (entity_mentions.context is populated by the main extraction path.)
    struct Ent { name: String, entity_type: String, sentiment: f64 }
    #[derive(serde::Deserialize)]
    struct Res { entities: Vec<Ent> }

    if let Ok(result) = serde_json::from_str::<Res>(json_str) {
        for ent in &result.entities {
            let et = ent.entity_type.to_lowercase();
            let et = et.trim();
            if !valid_types.contains(&et) { continue; }
            let nn = ent.name.to_lowercase().trim().to_string();
            if nn.is_empty() { continue; }

            // Use the specific freedom type as sector (e.g., "freedom_time", "freedom_wealth")
            // Determine which freedom this entity likely belongs to from the curated list
            let freedom_sector = curated.iter()
                .find(|(_, s)| s.headline.to_lowercase().contains(&ent.name.to_lowercase())
                    || s.summary.to_lowercase().contains(&ent.name.to_lowercase()))
                .map(|(f, _)| format!("freedom_{}", f))
                .unwrap_or_else(|| "freedom".to_string());

            if let Err(e) = conn.execute(
                "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6)
                 ON CONFLICT(name_normalized, entity_type) DO UPDATE SET
                   last_seen = MAX(last_seen, ?5),
                   sentiment_avg = (sentiment_avg * mention_count + ?6) / (mention_count + 1),
                   mention_count = mention_count + 1",
                rusqlite::params![ent.name, nn, et, freedom_sector, today, ent.sentiment],
            ) {
                tracing::warn!("freedom entity insert failed for '{}': {}", ent.name, e);
                continue;
            }
            total_stored += 1;
        }
    }

    Ok(total_stored)
}

/// DIAGNOSTIC (Task #7): log the per-category count of freedom articles at a
/// pipeline stage, so we can localize where health/whoop coverage drops to zero.
fn log_freedom_histogram<'a>(stage: &str, sectors: impl Iterator<Item = &'a str>) {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for s in sectors {
        *counts.entry(s).or_insert(0) += 1;
    }
    let summary: Vec<String> = ["freedom_time", "freedom_wealth", "freedom_location", "freedom_health", "freedom_whoop"]
        .iter()
        .map(|cat| format!("{}={}", cat.trim_start_matches("freedom_"), counts.get(cat).copied().unwrap_or(0)))
        .collect();
    tracing::info!("Freedoms[{}]: {}", stage, summary.join(" "));
}

/// Assign each curated story to exactly one freedom, first list winning.
///
/// The curator returns five independent index lists and nothing stops it filing
/// one story under two freedoms — the prompt does not forbid it and the parser
/// cannot see across lists. Measured on the live database before this guard
/// existed: **53 briefing-days had the same headline on two different freedom
/// cards**, which is the most visible way a curated feature can look sloppy.
///
/// Two mechanisms produce that, and both are covered here: the same index
/// appearing in two lists, and two *different* indices whose stories share a
/// title (the known dedup residual — identical headline, different URL). The
/// caller supplies the title hash so this stays pure and so "near-identical"
/// means the same thing here as everywhere else in the pipeline.
///
/// Order of `lists` is the priority order; a story lands under the first freedom
/// that claims it. `max_per_freedom` counts stories actually kept, so a freedom
/// whose top picks were claimed earlier still fills up from its remaining ones.
fn assign_freedom_indices<'a>(
    lists: &[(&'a str, &Vec<usize>)],
    title_hash_of: impl Fn(usize) -> Option<String>,
    max_per_freedom: usize,
) -> Vec<(&'a str, usize)> {
    let mut claimed_idx: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut claimed_title: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<(&'a str, usize)> = Vec::new();

    for (label, indices) in lists {
        let mut kept = 0;
        for &idx in indices.iter() {
            if kept >= max_per_freedom {
                break;
            }
            // An index the curator invented for a story that does not exist is
            // dropped, exactly as the previous `sorted.get(idx)` guard did.
            let Some(th) = title_hash_of(idx) else { continue };
            if !claimed_idx.insert(idx) || !claimed_title.insert(th) {
                continue;
            }
            out.push((label, idx));
            kept += 1;
        }
    }
    out
}

pub async fn run_freedoms(db_path: &Path) -> anyhow::Result<()> {
    let start = std::time::Instant::now();

    // Cost guardrail: bail before any LLM call if today's spend already crossed the cap.
    check_daily_cost_cap(db_path)?;

    // Phase 1: Collect from the news sources, filter to freedom_* only.
    // The filter stays even though collect_freedoms only fetches sources that
    // can produce a freedom sector — those sources also produce daily-sector
    // articles, and it is the guard that makes a stale source list survivable.
    tracing::info!("Freedoms: Collecting articles...");
    sources::API_CALLS.reset();
    let all_articles = sources::collect_freedoms().await?;
    // Log per-call API usage for this fetch run
    {
        let snapshot = sources::API_CALLS.snapshot();
        for (provider, calls) in snapshot {
            for _ in 0..calls {
                log_usage(db_path, provider, "fetch", "collect", 0, 0);
            }
        }
    }
    let freedom_articles: Vec<_> = all_articles
        .into_iter()
        .filter(|a| a.sector.starts_with("freedom_"))
        .collect();
    tracing::info!("Freedoms: {} raw articles", freedom_articles.len());
    log_freedom_histogram("raw", freedom_articles.iter().map(|a| a.sector.as_str()));

    // Phase 1.5: Freshness filter — drop articles older than 72h.
    // Exempt academic sources (ArXiv, bioRxiv) which publish on weekly cycles
    // and would be gutted by a news-oriented cutoff.
    // Keep articles with missing published_at (Google News often lacks it;
    // Google News URLs themselves already filter with when:1d).
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(72);
    let pre_fresh = freedom_articles.len();
    let freedom_articles: Vec<_> = freedom_articles
        .into_iter()
        .filter(|a| {
            // Academic sources use a 14-day window instead of 72h
            let is_academic = a.source_name.starts_with("ArXiv:") || a.source_name.starts_with("bioRxiv:");
            let effective_cutoff = if is_academic {
                chrono::Utc::now() - chrono::Duration::days(14)
            } else {
                cutoff
            };
            match a.published_at.as_deref() {
                None => true,
                Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                    Ok(dt) => dt.with_timezone(&chrono::Utc) >= effective_cutoff,
                    Err(_) => true, // unparseable date → keep, don't over-prune
                },
            }
        })
        .collect();
    let dropped = pre_fresh.saturating_sub(freedom_articles.len());
    tracing::info!("Freedoms: freshness filter dropped {} stale articles (news >72h, academic >14d); {} remain", dropped, freedom_articles.len());
    log_freedom_histogram("after-freshness", freedom_articles.iter().map(|a| a.sector.as_str()));

    if freedom_articles.is_empty() {
        tracing::warn!("Freedoms: No freedom articles found, skipping");
        return Ok(());
    }

    // Phase 2: Deduplicate (including cross-day dedup against last 7 days)
    tracing::info!("Freedoms: Deduplicating...");
    let (historical_hashes, historical_titles) = if db_path.exists() {
        match rusqlite::Connection::open(db_path) {
            Ok(conn) => crate::dedup::load_recent_hashes(&conn, 7),
            Err(e) => {
                tracing::warn!("Could not open DB for historical dedup: {}", e);
                (std::collections::HashSet::new(), Vec::new())
            }
        }
    } else {
        (std::collections::HashSet::new(), Vec::new())
    };
    let unique = crate::dedup::deduplicate_with_history(freedom_articles, historical_hashes, historical_titles);
    tracing::info!("Freedoms: {} after dedup", unique.len());
    log_freedom_histogram("after-dedup (pre_curate INPUT)", unique.iter().map(|a| a.sector.as_str()));

    // Phase 2.5: Pre-curate if many articles
    let to_summarize = if unique.len() > 40 {
        tracing::info!("Freedoms: Pre-curating from {} articles...", unique.len());
        let api_key = std::env::var("GROQ_API_KEY")
            .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
        let client = crate::claude::client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;
        match client.pre_curate_freedoms(&unique).await {
            Ok(indices) => {
                let curated: Vec<_> = indices.into_iter()
                    .filter_map(|i| unique.get(i).cloned())
                    .collect();
                tracing::info!("Freedoms: Pre-curated to {} articles", curated.len());
                log_freedom_histogram("after-pre_curate (LLM selection)", curated.iter().map(|a| a.sector.as_str()));
                curated
            }
            Err(e) => {
                tracing::warn!("Freedoms pre-curation failed (non-fatal): {}", e);
                let mut fallback = unique;
                if fallback.len() > 150 {
                    tracing::info!("Freedoms: Capping from {} to 150 articles (pre-curation fallback)", fallback.len());
                    fallback.truncate(150);
                }
                fallback
            }
        }
    } else {
        unique
    };

    // Phase 3: Summarize (grounded in fetched article bodies, best-effort)
    tracing::info!("Freedoms: Fetching article bodies for {} stories...", to_summarize.len());
    let to_summarize = crate::article_text::enrich(to_summarize).await;
    let grounded = to_summarize.iter()
        .filter(|a| a.content_snippet.contains("\n\nArticle text: "))
        .count();
    tracing::info!("Freedoms: Summarizing {} stories ({} grounded in article text)...",
        to_summarize.len(), grounded);
    let outcome = crate::claude::summarize_stories(&to_summarize, None, db_path).await?;
    if let Some(ref why) = outcome.failure {
        tracing::warn!("Freedoms: some summaries failed — {}", why);
    }
    let summaries = outcome.stories;
    tracing::info!("Freedoms: {} summaries", summaries.len());

    // Phase 4: Curate with freedoms prompt
    tracing::info!("Freedoms: Curating...");
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = crate::claude::client::GroqClient::new(&api_key, Some(db_path.to_path_buf()))?;

    // Stratified truncate: take the top 40 from EACH freedom_* sector, then
    // re-sort the union by importance. Whoop and Health articles tend to score
    // lower on raw importance than mainstream news; without stratification they
    // get bumped off the curator's input window and the LLM returns whoop=0 /
    // health=0 even when the source pool has plenty of candidates.
    const PER_SECTOR_CAP: usize = 40;
    let mut by_sector: std::collections::HashMap<String, Vec<crate::claude::SummarizedStory>> =
        std::collections::HashMap::new();
    for s in &summaries {
        by_sector.entry(s.article.sector.clone()).or_default().push(s.clone());
    }
    let mut sorted: Vec<crate::claude::SummarizedStory> = Vec::new();
    for bucket in by_sector.values_mut() {
        bucket.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));
        bucket.truncate(PER_SECTOR_CAP);
        sorted.extend(bucket.drain(..));
    }
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));
    tracing::info!("Freedoms: curator input = {} stories (stratified, max {}/sector)", sorted.len(), PER_SECTOR_CAP);

    let mut user_msg = String::new();
    for (i, s) in sorted.iter().enumerate() {
        user_msg.push_str(&format!(
            "\n[{}] [{}] {}\nSummary: {}\nImportance: {}\n",
            i, s.article.sector, s.headline, s.summary, s.importance_score
        ));
    }
    user_msg.push_str("\nReturn valid JSON with key: curation.");

    let curation_text = client
        .call(
            &crate::claude::client::strong_model(),
            "freedoms_analyze",
            crate::claude::prompts::FREEDOMS_ANALYSIS_SYSTEM,
            &user_msg,
            2000,
        )
        .await?;

    // Parse curation result
    let json_str = extract_json_str(&curation_text);

    #[derive(serde::Deserialize)]
    struct FreedomsCuration {
        time: Vec<usize>,
        wealth: Vec<usize>,
        location: Vec<usize>,
        health: Vec<usize>,
        #[serde(default)]
        whoop: Vec<usize>,
    }
    #[derive(serde::Deserialize)]
    struct FreedomsResponse {
        curation: FreedomsCuration,
    }

    let parsed: FreedomsResponse = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse freedoms curation: {} — raw: {}", e, json_str))?;

    // Build curated list with freedom labels, cap at 10 per freedom
    let max_per_freedom = 10;
    let mut curated: Vec<(&str, &crate::claude::SummarizedStory)> = Vec::new();
    let freedom_lists = [
        ("time", &parsed.curation.time),
        ("wealth", &parsed.curation.wealth),
        ("location", &parsed.curation.location),
        ("health", &parsed.curation.health),
        ("whoop", &parsed.curation.whoop),
    ];
    let assigned = assign_freedom_indices(
        &freedom_lists,
        |idx| sorted.get(idx).map(|s| crate::dedup::title_hash(&s.headline)),
        max_per_freedom,
    );
    for (label, idx) in &assigned {
        curated.push((label, &sorted[*idx]));
    }
    for (label, indices) in &freedom_lists {
        let kept = assigned.iter().filter(|(l, _)| l == label).count();
        tracing::info!(
            "Freedoms: {} = {} stories (LLM returned {}, {} dropped as duplicates)",
            label, kept, indices.len(), indices.len().saturating_sub(kept)
        );
    }

    tracing::info!("Freedoms: {} curated stories total", curated.len());

    // Phase 5: Write to database
    tracing::info!("Freedoms: Writing to database...");
    write_freedoms_to_db(db_path, &curated)?;

    // Phase 5.5: Generate executive summary for freedoms (non-fatal)
    tracing::info!("Freedoms: Generating executive summary...");
    match generate_freedoms_summary(&curated, db_path).await {
        Ok(summary) => {
            if let Ok(conn) = rusqlite::Connection::open(db_path) {
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                conn.execute(
                    "UPDATE briefings SET executive_summary = ?1 WHERE date = ?2 AND briefing_type = 'freedoms'",
                    rusqlite::params![summary, today],
                ).ok();
                tracing::info!("Freedoms executive summary: {} chars", summary.len());
            }
        }
        Err(e) => tracing::warn!("Freedoms summary generation failed (non-fatal): {}", e),
    }

    // Phase 6: Generate contextual prefixes for freedom stories (non-fatal)
    tracing::info!("Freedoms: Generating contextual prefixes...");
    let freedom_context: String = curated.iter()
        .map(|(f, s)| format!("[{}] {}", f, s.headline))
        .collect::<Vec<_>>()
        .join("\n");
    let prefix_stories: Vec<crate::claude::SummarizedStory> = curated.iter()
        .map(|(_, s)| (*s).clone())
        .collect();
    match crate::contextual::generate_prefixes(&prefix_stories, &freedom_context).await {
        Ok(prefixes) => {
            if let Ok(conn) = rusqlite::Connection::open(db_path) {
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                // Get the briefing_id for today's freedoms briefing
                let briefing_id: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM briefings WHERE date = ?1 AND briefing_type = 'freedoms'",
                        [&today],
                        |row| row.get(0),
                    )
                    .ok();
                if let Some(bid) = briefing_id {
                    let mut updated = 0;
                    for (i, prefix) in prefixes.iter().enumerate() {
                        if let Some(p) = prefix {
                            conn.execute(
                                "UPDATE freedom_stories SET context_prefix = ?1
                                 WHERE briefing_id = ?2 AND display_order = ?3",
                                rusqlite::params![p, bid, i as i32],
                            ).ok();
                            updated += 1;
                        }
                    }
                    tracing::info!("Updated {} freedom stories with contextual prefixes", updated);
                }
            }
        }
        Err(e) => tracing::warn!("Freedom prefix generation failed (non-fatal): {}", e),
    }

    // Phase 7: Extract entities from freedom stories (non-fatal)
    tracing::info!("Freedoms: Extracting entities...");
    match extract_entities_from_freedoms(db_path, &curated).await {
        Ok(count) => tracing::info!("Extracted {} entity mentions from freedoms", count),
        Err(e) => tracing::warn!("Freedom entity extraction failed (non-fatal): {}", e),
    }

    // Phase 8: Generate embeddings for freedom stories (non-fatal)
    // Stored in story_embeddings with encoded ID: -(freedom_id + 100_000) to
    // distinguish from daily stories. This lets semantic search surface them.
    tracing::info!("Freedoms: Generating embeddings...");
    let freedom_summaries: Vec<crate::claude::SummarizedStory> = curated.iter().map(|(_, s)| (*s).clone()).collect();
    match crate::embeddings::generate(&freedom_summaries, None, |_, _| {}).await {
        Ok(embs) => {
            if let Ok(conn) = rusqlite::Connection::open(db_path) {
                let mut stored = 0;
                for se in &embs {
                    if let Some((_, story)) = curated.get(se.story_index) {
                        let freedom_id: Option<i64> = conn.query_row(
                            "SELECT id FROM freedom_stories WHERE headline = ?1 ORDER BY id DESC LIMIT 1",
                            rusqlite::params![story.headline],
                            |row| row.get(0),
                        ).ok();
                        if let Some(fid) = freedom_id {
                            let encoded_id = -(fid + 100_000);
                            let blob: Vec<u8> = se.embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
                            conn.execute(
                                "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                                rusqlite::params![encoded_id, blob],
                            ).ok();
                            stored += 1;
                        }
                    }
                }
                tracing::info!("Stored {} freedom story embeddings (encoded IDs)", stored);
            }
            log_usage(db_path, "voyage", "voyage-3-lite", "freedom_embeddings", (embs.len() as i64) * 200, 0);
            tracing::info!("Generated {} freedom story embeddings", embs.len());
        }
        Err(e) => tracing::warn!("Freedom embedding generation failed (non-fatal): {}", e),
    }

    let duration = start.elapsed();
    tracing::info!("Freedoms pipeline complete in {:.1}s", duration.as_secs_f64());

    Ok(())
}

fn extract_json_str(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

fn write_freedoms_to_db(
    db_path: &Path,
    curated: &[(&str, &crate::claude::SummarizedStory)],
) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;

    // Run migrations (transaction-wrapped, with ALTER TABLE guards)
    crate::db::run_migrations(&conn)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let tx = conn.unchecked_transaction()?;

    // Check if freedoms briefing exists for today, replace if so
    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM briefings WHERE date = ?1 AND briefing_type = 'freedoms'",
            [&today],
            |row| row.get(0),
        )
        .ok();
    if let Some(old_id) = existing_id {
        tracing::info!("Replacing existing freedoms briefing {} for {}", old_id, today);
        tx.execute("DELETE FROM freedom_stories WHERE briefing_id = ?1", [old_id])?;
        tx.execute("DELETE FROM stories WHERE briefing_id = ?1", [old_id])?;
        tx.execute("DELETE FROM briefings WHERE id = ?1", [old_id])?;
    }

    // Count per freedom. The briefings table has exactly four sector-count
    // columns, named for the daily briefing's sectors, and a freedoms row
    // borrows them positionally: ai=time, miami=wealth, italy=location,
    // tech=health. There is no fifth column, so `whoop_count` reaches the log
    // line at the end of this function but never the database.
    //
    // That is fine and deliberate: nothing reads these columns for a freedoms
    // row — every reader (Sidebar, BriefingSummary, Archive) goes through the
    // daily briefing — and the page counts each bucket from the stories it
    // loaded. `story_count` is the honest total and does include Whoop. Adding
    // a column for a number no one reads would cost a migration and buy
    // nothing; this comment exists so the asymmetry doesn't read as an
    // oversight next time.
    let time_count = curated.iter().filter(|(f, _)| *f == "time").count();
    let wealth_count = curated.iter().filter(|(f, _)| *f == "wealth").count();
    let location_count = curated.iter().filter(|(f, _)| *f == "location").count();
    let health_count = curated.iter().filter(|(f, _)| *f == "health").count();
    let whoop_count = curated.iter().filter(|(f, _)| *f == "whoop").count();
    let total = curated.len();

    // Insert briefing with freedoms type
    tx.execute(
        "INSERT INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status, briefing_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'complete', 'freedoms')",
        rusqlite::params![today, total, time_count, wealth_count, location_count, health_count],
    )?;
    let briefing_id = tx.last_insert_rowid();

    // Insert freedom stories.
    //
    // `is_hero` marks the story each freedom card leads with — one per freedom,
    // which is what the page renders (it takes stories[0] of each bucket). It
    // used to be `i == 0`, flagging only the single first row of the whole
    // briefing, so four of the five cards had no hero at all. `curated` arrives
    // grouped by freedom in priority order, so the first row of each group is
    // that freedom's lead.
    //
    // Deliberately different from `briefings.hero_story_id` set below, which is
    // one story for the whole briefing. Both are correct; don't collapse them.
    let mut freedom_seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (i, (freedom, story)) in curated.iter().enumerate() {
        let key_facts_json = serde_json::to_string(&story.key_facts)?;
        let is_hero = if freedom_seen.insert(freedom) { 1 } else { 0 };

        tx.execute(
            "INSERT INTO freedom_stories (
                briefing_id, freedom, headline, summary, key_facts,
                why_it_matters, what_to_watch, importance_score,
                is_hero, display_order, original_title, original_url,
                original_language, content_snippet, source_name, source_url,
                published_at, url_hash, title_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            rusqlite::params![
                briefing_id,
                freedom,
                story.headline,
                story.summary,
                key_facts_json,
                story.why_it_matters,
                story.what_to_watch,
                story.importance_score,
                is_hero,
                i as i32,
                story.article.title,
                story.article.url,
                story.article.language,
                story.article.content_snippet,
                story.article.source_name,
                story.article.source_url,
                story.article.published_at,
                crate::dedup::url_hash(&story.article.url),
                crate::dedup::title_hash(&story.article.title),
            ],
        )?;
    }

    // Update hero_story_id on briefing (use first freedom_story id)
    let first_story_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM freedom_stories WHERE briefing_id = ?1 ORDER BY display_order LIMIT 1",
            [briefing_id],
            |row| row.get(0),
        )
        .ok();
    if let Some(hero_id) = first_story_id {
        tx.execute(
            "UPDATE briefings SET hero_story_id = ?1 WHERE id = ?2",
            rusqlite::params![hero_id, briefing_id],
        )?;
    }

    // Also insert into main stories table for RAG (search, embeddings, Ask Pulse)
    // Use sector = "freedom_{type}" so they're searchable but distinguishable from daily stories
    for (i, (freedom, story)) in curated.iter().enumerate() {
        let key_facts_json = serde_json::to_string(&story.key_facts)?;
        let sector = format!("freedom_{}", freedom);
        let url_hash = crate::dedup::url_hash(&story.article.url);
        let title_hash = crate::dedup::title_hash(&story.article.title);

        // Skip if already exists (dedup by url_hash)
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stories WHERE url_hash = ?1)",
            [&url_hash], |row| row.get(0),
        ).unwrap_or(false);

        if !exists {
            tx.execute(
                "INSERT INTO stories (briefing_id, sector, original_title, original_url, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, is_hero, display_order,
                    source_name, url_hash, title_hash, published_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    briefing_id, sector, story.article.title, story.article.url,
                    story.headline, story.summary, key_facts_json,
                    story.why_it_matters, story.what_to_watch, story.importance_score,
                    i as i32, story.article.source_name, url_hash, title_hash,
                    story.article.published_at,
                ],
            ).ok(); // Non-fatal — if it fails, the freedom_stories entry still works
        }
    }

    tx.commit()?;
    tracing::info!(
        "Wrote {} freedom stories to briefing {} (time={}, wealth={}, location={}, health={}, whoop={})",
        total, briefing_id, time_count, wealth_count, location_count, health_count, whoop_count
    );

    Ok(())
}

/// Drop articles that duplicate an EARLIER ARTICLE IN THE SAME BATCH.
///
/// The `financial_dedup` table only knows about previous runs — `record_financial_dedup`
/// writes to it *after* storage — so when a source returned the same record twice in one
/// fetch, both copies passed the table check because neither was in the table yet.
///
/// Measured 2026-08-15 before this existed: 7,563 duplicate story rows, **92% of every
/// within-briefing duplicate in the database**. Worst case was 92 copies of one FEC
/// contribution written in a single second (briefing 227, display_order 50..141,
/// one distinct url_hash). Content-keyed dedup would have addressed only the other 8%.
///
/// Keyed on (feed_id, url) to match `financial_dedup`'s primary key exactly, so passing
/// this filter and missing the table mean the same thing.
fn dedup_within_batch(articles: Vec<sources::RawArticle>) -> Vec<sources::RawArticle> {
    let mut seen: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::with_capacity(articles.len());
    articles
        .into_iter()
        .filter(|a| seen.insert((a.feed_id.clone(), a.url.clone())))
        .collect()
}

/// Dedup financial articles: first against the rest of this batch, then against the
/// financial_dedup table (which covers previous runs only — see `dedup_within_batch`).
/// Uses feed_id as source_type and url as source_id for dedup.
/// Returns only articles not previously seen.
fn dedup_financial_articles(
    db_path: &Path,
    articles: Vec<sources::RawArticle>,
) -> Vec<sources::RawArticle> {
    // Batch-level dedup runs FIRST and unconditionally: the early returns below hand
    // back the articles unfiltered, and a self-duplicating batch must not survive them.
    let articles = dedup_within_batch(articles);

    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Could not open DB for financial dedup: {}", e);
            return articles;
        }
    };

    // Check if financial_dedup table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='financial_dedup')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !table_exists {
        return articles;
    }

    articles
        .into_iter()
        .filter(|article| {
            // Use feed_id as source_type and url as source_id for dedup
            let source_type = &article.feed_id;
            let source_id = &article.url;

            let already_seen: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM financial_dedup WHERE source_type = ?1 AND source_id = ?2)",
                    rusqlite::params![source_type, source_id],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            !already_seen
        })
        .collect()
}

/// Write financial stories directly to the database without embeddings.
/// These are structured data (SEC filings, contracts, etc.) that don't need LLM summarization.
fn write_financial_stories(
    db_path: &Path,
    stories: &[crate::claude::SummarizedStory],
) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;
    crate::db::run_migrations(&conn)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Find today's briefing (must exist — write_to_db creates it first)
    let briefing_id: i64 = conn
        .query_row(
            "SELECT id FROM briefings WHERE date = ?1 AND briefing_type = 'daily' ORDER BY created_at DESC LIMIT 1",
            [&today],
            |row| row.get(0),
        )
        .map_err(|_| anyhow::anyhow!("No daily briefing found for {} — write_to_db must run first", today))?;

    // Continue the briefing's existing display_order instead of restarting at 0.
    // write_to_db has already numbered the news stories from 0, and a second run can
    // write financial rows into the same briefing, so restarting collided both ways.
    // Measured 2026-08-15: 453 (briefing_id, display_order) collision groups among
    // financial rows alone.
    let next_order: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(display_order), -1) + 1 FROM stories WHERE briefing_id = ?1",
            [briefing_id],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let mut stored: i64 = 0;
    for story in stories {
        if story.article.source_type != "financial" {
            continue;
        }

        let key_facts_json = serde_json::to_string(&story.key_facts).unwrap_or_else(|_| "[]".to_string());

        match conn.execute(
            "INSERT INTO stories (
                briefing_id, sector, original_title, original_url, original_language,
                content_snippet, source_name, published_at, headline, summary,
                key_facts, why_it_matters, what_to_watch, importance_score,
                is_hero, display_order, url_hash, title_hash,
                source_type, financial_metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0, ?15, ?16, ?17, ?18, ?19)",
            rusqlite::params![
                briefing_id,
                story.article.sector,
                story.article.title,
                story.article.url,
                story.article.language,
                story.article.content_snippet,
                story.article.source_name,
                story.article.published_at,
                story.headline,
                story.summary,
                key_facts_json,
                story.why_it_matters,
                story.what_to_watch,
                story.importance_score,
                (next_order + stored) as i32,
                crate::dedup::url_hash(&story.article.url),
                crate::dedup::title_hash(&story.article.title),
                story.article.source_type,
                story.article.financial_metadata,
            ],
        ) {
            // Non-fatal per story — a UNIQUE index may reject it. `stored` must count
            // rows that ACTUALLY landed: the old code discarded the result with .ok()
            // and then incremented unconditionally, so both the returned count and the
            // next display_order advanced on failed inserts.
            Ok(n) if n > 0 => stored += 1,
            Ok(_) => {}
            Err(e) => tracing::debug!("Financial story insert skipped: {}", e),
        }
    }

    // Update briefing story count — NEWS only. Every surface that renders this column
    // labels it "stories" (the header's "N stories across 4 sectors", the sidebar's All
    // row, the archive timeline), but a raw COUNT(*) folded in the day's regulatory
    // filings, so 2026-08-14 advertised 582 stories when it held 120 and 462 filings.
    // Migration 032 backfills the same definition over the existing history.
    conn.execute(
        "UPDATE briefings SET story_count = (
             SELECT COUNT(*) FROM stories
             WHERE briefing_id = ?1 AND COALESCE(source_type, 'news') != 'financial'
         ) WHERE id = ?1",
        [briefing_id],
    ).ok();

    Ok(stored as usize)
}

/// Record financial articles in the dedup table after they're stored.
fn record_financial_dedup(db_path: &Path, articles: &[crate::claude::SummarizedStory]) {
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for story in articles {
        if story.article.source_type != "financial" {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO financial_dedup (source_type, source_id) VALUES (?1, ?2)",
            rusqlite::params![story.article.feed_id, story.article.url],
        )
        .ok();
    }
}
















/// Generate key_facts, why_it_matters, what_to_watch from financial_metadata.
/// No LLM needed — structured data → structured FTS fields.
fn generate_financial_fts_fields(article: &crate::sources::RawArticle) -> (Vec<String>, String, String) {
    let meta: serde_json::Value = article.financial_metadata
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    if meta.is_null() {
        return (Vec::new(), String::new(), String::new());
    }

    let source = article.source_name.as_str();
    let mut facts = Vec::new();
    let mut why = String::new();
    let mut watch = String::new();

    match source {
        s if s.contains("EDGAR") => {
            let filing = meta.get("filing_type").and_then(|v| v.as_str()).unwrap_or("filing");
            let entity = meta.get("entity_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
            facts.push(format!("SEC {} filing by {}", filing, entity));
            if let Some(cik) = meta.get("cik").and_then(|v| v.as_str()) {
                facts.push(format!("CIK: {}", cik));
            }
            why = match filing {
                "4" => format!("{} insider trading activity detected — insiders buying or selling their own stock", entity),
                "8-K" => format!("{} filed a material event disclosure — potential market-moving information", entity),
                "D" => format!("{} filed a private placement — raising capital outside public markets", entity),
                _ => format!("{} SEC filing may indicate significant corporate activity", entity),
            };
            watch = format!("Monitor {} stock price and subsequent filings for pattern confirmation", entity);
        }
        s if s.contains("USASpending") => {
            let recipient = meta.get("recipient").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let agency = meta.get("agency").and_then(|v| v.as_str()).unwrap_or("Unknown agency");
            let amount = meta.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            facts.push(format!("Government contract awarded to {}", recipient));
            facts.push(format!("Awarding agency: {}", agency));
            if amount > 0.0 { facts.push(format!("Value: ${:.0}", amount)); }
            why = format!("{} received a government contract — signals revenue pipeline and government trust", recipient);
            watch = format!("Track {} for additional contract awards and revenue impact", recipient);
        }
        "FEC" => {
            let contributor = meta.get("contributor").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let recipient = meta.get("recipient").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let amount = meta.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            facts.push(format!("Political contribution: {} → {}", contributor, recipient));
            if amount > 0.0 { facts.push(format!("Amount: ${:.0}", amount)); }
            if let Some(party) = meta.get("party").and_then(|v| v.as_str()) {
                facts.push(format!("Party: {}", party));
            }
            why = format!("Large political donations can signal industry lobbying priorities and regulatory expectations");
            watch = format!("Monitor related regulatory actions and policy changes affecting {}'s industry", contributor);
        }
        s if s.contains("LDA") || s.contains("Senate") => {
            let client = meta.get("client").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let registrant = meta.get("registrant").and_then(|v| v.as_str()).unwrap_or("Unknown");
            facts.push(format!("Lobbying: {} via {}", client, registrant));
            if let Some(issues) = meta.get("issues").and_then(|v| v.as_str()) {
                if !issues.is_empty() { facts.push(format!("Issues: {}", issues)); }
            }
            why = format!("{} is lobbying — indicates they're trying to influence policy that affects their business", client);
            watch = format!("Track related legislation and regulatory actions in lobbied areas");
        }
        "FRED" => {
            let series = meta.get("series_name").and_then(|v| v.as_str()).unwrap_or("indicator");
            let value = meta.get("value").and_then(|v| v.as_f64());
            let change = meta.get("change_pct").and_then(|v| v.as_f64());
            facts.push(format!("Economic indicator: {}", series));
            if let Some(v) = value { facts.push(format!("Current value: {:.2}", v)); }
            if let Some(c) = change { facts.push(format!("Change: {:.1}%", c)); }
            why = format!("{} is a key macro indicator — changes affect market sentiment and Fed policy expectations", series);
            watch = format!("Monitor for trend continuation and divergence from market expectations");
        }
        "EIA" => {
            let value = meta.get("value").and_then(|v| v.as_f64());
            let unit = meta.get("unit").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(v) = value { facts.push(format!("Price: {:.2} {}", v, unit)); }
            why = "Energy prices directly impact transportation, manufacturing, and consumer costs".to_string();
            watch = "Track supply disruptions, OPEC decisions, and seasonal demand patterns".to_string();
        }
        "USPTO" => {
            let assignee = meta.get("assignee").and_then(|v| v.as_str()).unwrap_or("Unknown");
            facts.push(format!("Patent filed by {}", assignee));
            why = format!("{} patent activity may indicate R&D direction and competitive positioning", assignee);
            watch = format!("Monitor {} patent cluster for technology trend signals", assignee);
        }
        s if s.contains("Federal Register") => {
            let rule_type = meta.get("rule_type").and_then(|v| v.as_str()).unwrap_or("rule");
            let agencies = meta.get("agencies").and_then(|v| v.as_str()).unwrap_or("Unknown");
            facts.push(format!("{} by {}", rule_type, agencies));
            why = format!("Regulatory action by {} — may create compliance requirements or market opportunities", agencies);
            watch = "Track comment periods, effective dates, and industry response".to_string();
        }
        _ => {}
    }

    (facts, why, watch)
}

/// Extract entities from financial_metadata JSON without LLM.
/// Parses entity_name/recipient/client/assignee/contributor from structured metadata
/// and creates entity + entity_mention records.
fn extract_entities_from_financial_metadata(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Get financial stories that don't yet have entity_mentions
    let mut stmt = conn.prepare(
        "SELECT s.id, s.source_name, s.financial_metadata, s.sector
         FROM stories s
         WHERE s.source_type = 'financial'
           AND s.financial_metadata IS NOT NULL
           AND json_valid(s.financial_metadata)
           AND s.id NOT IN (SELECT DISTINCT story_id FROM entity_mentions)
         ORDER BY s.id DESC
         LIMIT 500"
    )?;

    let rows: Vec<(i64, String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        return Ok(0);
    }

    let mut total = 0usize;

    for (story_id, source_name, metadata_str, sector) in &rows {
        let meta: serde_json::Value = match serde_json::from_str(metadata_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Determine entity type and extract names based on source
        let entities: Vec<(&str, &str, f64)> = match source_name.as_str() {
            s if s.contains("EDGAR") => {
                let mut ents = Vec::new();
                if let Some(name) = meta.get("entity_name").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        let etype = match meta.get("filing_type").and_then(|v| v.as_str()).unwrap_or("") {
                            "4" => "insider_trade",
                            "8-K" => "material_event",
                            "D" => "private_placement",
                            _ => "company",
                        };
                        ents.push((name, etype, 0.0));
                    }
                }
                // Also extract from display_names array
                if let Some(names) = meta.get("display_names").and_then(|v| v.as_array()) {
                    for n in names.iter().take(3) {
                        if let Some(name) = n.as_str() {
                            if !name.is_empty() && ents.iter().all(|(e, _, _)| *e != name) {
                                ents.push((name, "company", 0.0));
                            }
                        }
                    }
                }
                ents
            }
            s if s.contains("USASpending") => {
                let mut ents = Vec::new();
                if let Some(name) = meta.get("recipient").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "contract_award", 0.5));
                    }
                }
                if let Some(name) = meta.get("agency").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        // Was typed "company" — a federal agency (e.g. "Department of
                        // Transportation") is never ticker-eligible or tradeable, but that type
                        // let it INTO populate_tickers_limited's eligible set and let it
                        // accumulate a compound_score across every contract it appears in
                        // (an agency shows up in hundreds of unrelated awards), producing fake
                        // convergence unrelated to any single company. "regulatory_action"
                        // (already used for Federal Register agencies) is excluded from
                        // ticker-mapping and semantically correct. (calibration-backtest-universe
                        // Task 5, 2026-07-24 — found via the 290-NULL-ticker candidate audit.)
                        ents.push((name, "regulatory_action", 0.0));
                    }
                }
                ents
            }
            "FEC" => {
                let mut ents = Vec::new();
                if let Some(name) = meta.get("recipient").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "person", 0.0));
                    }
                }
                if let Some(name) = meta.get("contributor").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "person", 0.0));
                    }
                }
                if let Some(name) = meta.get("employer").and_then(|v| v.as_str()) {
                    if !name.is_empty() && name != "SELF-EMPLOYED" && name != "NOT EMPLOYED" && name != "RETIRED" {
                        ents.push((name, "company", 0.0));
                    }
                }
                ents
            }
            s if s.contains("LDA") || s.contains("Senate") => {
                let mut ents = Vec::new();
                // Types were swapped: `client` (the real company paying for lobbying, e.g.
                // "MOHAWK INDUSTRIES, INC.") was typed "lobbying_disclosure" — which
                // populate_tickers_limited's eligible-type filter EXCLUDES, so a genuine
                // public company could never get a ticker attempt. `registrant` (the law
                // firm/lobbying shop actually filing, e.g. "ALSTON & BIRD LLP") was typed
                // "company" — ticker-eligible despite never being a tradeable entity, and it
                // accumulates fake convergence by aggregating across every unrelated client
                // it files for (one firm, dozens of clients' filings). Swapping the labels
                // makes the real company ticker-eligible and keeps the intermediary out of
                // the ticker-mapping and now-more-obviously-not-a-company bucket. (See
                // USASpending block above for the sibling bug/fix.
                // calibration-backtest-universe Task 5, 2026-07-24.)
                if let Some(name) = meta.get("client").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "company", 0.0));
                    }
                }
                if let Some(name) = meta.get("registrant").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "lobbying_disclosure", 0.0));
                    }
                }
                ents
            }
            "USPTO" | "Google Patents" => {
                let mut ents = Vec::new();
                if let Some(name) = meta.get("assignee").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "patent_cluster", 0.3));
                    }
                }
                ents
            }
            "Wikipedia Pageviews" => {
                let mut ents = Vec::new();
                if let Some(name) = meta.get("entity_name").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "company", 0.0)); // Use 'company' — matches CHECK constraint
                    }
                }
                ents
            }
            "EIA" => {
                let mut ents = Vec::new();
                if let Some(product) = meta.get("product").and_then(|v| v.as_str()) {
                    if !product.is_empty() {
                        ents.push((product, "topic", 0.0)); // Use 'topic' — matches CHECK constraint
                    }
                }
                ents
            }
            s if s.contains("Federal Register") => {
                let mut ents = Vec::new();
                if let Some(agencies) = meta.get("agencies").and_then(|v| v.as_str()) {
                    for agency in agencies.split(',').map(|s| s.trim()) {
                        if !agency.is_empty() {
                            ents.push((agency, "regulatory_action", 0.0));
                        }
                    }
                }
                ents
            }
            _ => Vec::new(),
        };

        for (name, entity_type, sentiment) in &entities {
            let nn = name.to_lowercase().trim().to_string();
            if nn.is_empty() || nn.len() < 2 { continue; }

            // Upsert entity
            conn.execute(
                "INSERT INTO entities (name, name_normalized, entity_type, sector, first_seen, last_seen, mention_count, sentiment_avg)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6)
                 ON CONFLICT(name_normalized, entity_type) DO UPDATE SET
                   last_seen = MAX(last_seen, ?5),
                   sentiment_avg = (sentiment_avg * mention_count + ?6) / (mention_count + 1),
                   mention_count = mention_count + 1",
                rusqlite::params![name, nn, entity_type, sector, today, sentiment],
            ).ok();

            // Get entity_id and insert mention
            let entity_id: Option<i64> = conn.query_row(
                "SELECT id FROM entities WHERE name_normalized = ?1 AND entity_type = ?2",
                rusqlite::params![nn, entity_type],
                |row| row.get(0),
            ).ok();

            if let Some(eid) = entity_id {
                conn.execute(
                    "INSERT INTO entity_mentions (entity_id, story_id, sentiment, context, mentioned_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![eid, story_id, sentiment, format!("From {} financial data", source_name), today],
                ).ok();
                total += 1;
            }
        }
    }

    // Update source_diversity in signals table for entities with financial mentions
    let mut div_stmt = conn.prepare(
        "SELECT e.name, COUNT(DISTINCT s.source_name) as src_count
         FROM entity_mentions em
         JOIN entities e ON e.id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE em.mentioned_at >= date(?1, '-30 days')
         GROUP BY e.name
         HAVING src_count >= 2"
    )?;

    let diversity_rows: Vec<(String, i64)> = div_stmt
        .query_map([&today], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (topic, src_count) in &diversity_rows {
        conn.execute(
            "UPDATE signals SET source_diversity = ?1 WHERE topic = ?2",
            rusqlite::params![src_count, topic],
        ).ok();
    }

    if !diversity_rows.is_empty() {
        tracing::info!("Updated source_diversity for {} entities", diversity_rows.len());
    }

    Ok(total)
}

#[cfg(test)]
mod financial_dedup_tests {
    use super::*;

    fn article(feed_id: &str, url: &str, title: &str) -> sources::RawArticle {
        sources::RawArticle {
            title: title.to_string(),
            url: url.to_string(),
            source_name: "FEC".to_string(),
            source_url: "https://api.open.fec.gov".to_string(),
            published_at: None,
            content_snippet: String::new(),
            sector: "wealth".to_string(),
            feed_id: feed_id.to_string(),
            language: "en".to_string(),
            source_type: "financial".to_string(),
            financial_metadata: None,
        }
    }

    /// Steady state: a clean batch must survive untouched. This is the case the
    /// old code already handled, kept so a too-aggressive key shows up here.
    #[test]
    fn keeps_every_distinct_article() {
        let batch = vec![
            article("fec", "https://fec.gov/a", "A"),
            article("fec", "https://fec.gov/b", "B"),
            article("fec", "https://fec.gov/c", "C"),
        ];
        assert_eq!(dedup_within_batch(batch).len(), 3);
    }
    /// The mutation this function exists to survive, in the exact shape production
    /// produced: one FEC contribution repeated 92 times inside a single fetch.
    /// Before the fix all 92 were written (briefing 227, 2026-06-10 01:14:09).
    #[test]
    fn collapses_the_92_copy_batch() {
        let batch: Vec<_> = (0..92)
            .map(|_| article("fec", "https://fec.gov/dup", "ACTBLUE contributes $13K"))
            .collect();
        let out = dedup_within_batch(batch);
        assert_eq!(out.len(), 1, "92 identical records must collapse to one");
    }

    /// Duplicates interleaved with real articles — an append-only test would pass
    /// while a positional key silently mangled the middle of the batch.
    #[test]
    fn collapses_duplicates_interleaved_and_keeps_first_occurrence_order() {
        let batch = vec![
            article("fec", "https://fec.gov/a", "first A"),
            article("fec", "https://fec.gov/b", "B"),
            article("fec", "https://fec.gov/a", "second A"),
            article("fec", "https://fec.gov/c", "C"),
            article("fec", "https://fec.gov/b", "second B"),
        ];
        let out = dedup_within_batch(batch);
        let titles: Vec<_> = out.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, vec!["first A", "B", "C"]);
    }

    /// The key must match financial_dedup's PRIMARY KEY (source_type, source_id)
    /// exactly. Same URL under a different feed is a genuinely different record;
    /// collapsing it here would silently drop data the table would have kept.
    #[test]
    fn same_url_under_different_feeds_is_not_a_duplicate() {
        let batch = vec![
            article("fec", "https://example.gov/x", "via FEC"),
            article("edgar", "https://example.gov/x", "via EDGAR"),
        ];
        assert_eq!(dedup_within_batch(batch).len(), 2);
    }

    #[test]
    fn empty_batch_is_handled() {
        assert!(dedup_within_batch(Vec::new()).is_empty());
    }
}

/// Guards the defect measured on the live database: 53 briefing-days carried
/// the same headline under two different freedom cards.
#[cfg(test)]
mod freedom_assignment_tests {
    use super::*;

    /// Titles keyed by index. Distinct titles unless a test says otherwise.
    fn titles(n: usize) -> impl Fn(usize) -> Option<String> {
        move |i| (i < n).then(|| format!("title-{i}"))
    }

    fn lists<'a>(pairs: &'a [(&'a str, Vec<usize>)]) -> Vec<(&'a str, &'a Vec<usize>)> {
        pairs.iter().map(|(l, v)| (*l, v)).collect()
    }

    #[test]
    fn a_story_claimed_by_two_freedoms_lands_in_the_first_only() {
        let p = [("time", vec![0, 1]), ("wealth", vec![1, 2])];
        let got = assign_freedom_indices(&lists(&p), titles(3), 10);
        assert_eq!(got, vec![("time", 0), ("time", 1), ("wealth", 2)]);
    }

    #[test]
    fn two_different_indices_sharing_a_title_count_as_one_story() {
        // The dedup residual: same headline, different URL, so different indices.
        let p = [("time", vec![0]), ("health", vec![7])];
        let same = |_: usize| Some("identical headline".to_string());
        let got = assign_freedom_indices(&lists(&p), same, 10);
        assert_eq!(got, vec![("time", 0)]);
    }

    #[test]
    fn a_repeated_index_within_one_list_is_taken_once() {
        let p = [("time", vec![0, 0, 1])];
        let got = assign_freedom_indices(&lists(&p), titles(2), 10);
        assert_eq!(got, vec![("time", 0), ("time", 1)]);
    }

    #[test]
    fn the_cap_counts_kept_stories_so_a_freedom_still_fills_up() {
        // wealth's first two picks were claimed by time; with a cap of 2 it must
        // still end up with two stories, not zero.
        let p = [("time", vec![0, 1]), ("wealth", vec![0, 1, 2, 3, 4])];
        let got = assign_freedom_indices(&lists(&p), titles(5), 2);
        assert_eq!(got, vec![("time", 0), ("time", 1), ("wealth", 2), ("wealth", 3)]);
    }

    #[test]
    fn an_index_with_no_story_behind_it_is_dropped_not_counted() {
        // The curator can hallucinate an index past the end of the list.
        let p = [("time", vec![99, 0])];
        let got = assign_freedom_indices(&lists(&p), titles(1), 10);
        assert_eq!(got, vec![("time", 0)]);
    }

    #[test]
    fn distinct_stories_across_freedoms_are_all_kept() {
        // The guard must not be so aggressive that it eats legitimate coverage.
        let p = [
            ("time", vec![0, 1]),
            ("wealth", vec![2]),
            ("location", vec![3]),
            ("health", vec![4]),
            ("whoop", vec![5]),
        ];
        let got = assign_freedom_indices(&lists(&p), titles(6), 10);
        assert_eq!(got.len(), 6);
    }
}

#[cfg(test)]
mod financial_write_tests {
    use super::*;

    fn story(url: &str, title: &str) -> crate::claude::SummarizedStory {
        crate::claude::SummarizedStory {
            article: sources::RawArticle {
                title: title.to_string(),
                url: url.to_string(),
                source_name: "FEC".to_string(),
                source_url: "https://api.open.fec.gov".to_string(),
                published_at: None,
                content_snippet: "snippet".to_string(),
                // `finance`, not `wealth` — stories.sector has a CHECK constraint that
                // predates the freedoms rename, and all 22,551 real financial rows use it.
                sector: "finance".to_string(),
                feed_id: "fec".to_string(),
                language: "en".to_string(),
                source_type: "financial".to_string(),
                financial_metadata: None,
            },
            headline: title.to_string(),
            summary: "summary".to_string(),
            key_facts: vec!["fact".to_string()],
            why_it_matters: "matters".to_string(),
            what_to_watch: "watch".to_string(),
            importance_score: 5,
            sentiment: None,
            novelty: None,
            event_type: Some("financial_data".to_string()),
        }
    }

    /// Builds a DB with today's daily briefing already present, since
    /// write_financial_stories requires write_to_db to have run first.
    fn db_with_briefing() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        crate::db::run_migrations(&conn).unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT INTO briefings (date, briefing_type, status) VALUES (?1, 'daily', 'complete')",
            [&today],
        )
        .unwrap();
        (dir, path)
    }

    /// The state transition the fix exists for: a SECOND write into the same briefing
    /// must not restart display_order at 0. Before the fix both runs numbered from 0,
    /// producing 453 (briefing_id, display_order) collision groups in production.
    #[test]
    fn second_write_continues_display_order_instead_of_restarting() {
        let (_dir, path) = db_with_briefing();

        let first = vec![story("https://fec.gov/1", "one"), story("https://fec.gov/2", "two")];
        assert_eq!(write_financial_stories(&path, &first).unwrap(), 2);

        let second = vec![story("https://fec.gov/3", "three")];
        assert_eq!(write_financial_stories(&path, &second).unwrap(), 1);

        let conn = rusqlite::Connection::open(&path).unwrap();
        let distinct: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT display_order) FROM stories WHERE source_type = 'financial'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM stories WHERE source_type = 'financial'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3);
        assert_eq!(distinct, 3, "every financial row needs its own display_order");
    }

    /// Financial rows must not reuse the display_order the news rows already hold.
    #[test]
    fn financial_does_not_collide_with_existing_news_ordering() {
        let (_dir, path) = db_with_briefing();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let bid: i64 = conn
            .query_row("SELECT id FROM briefings LIMIT 1", [], |r| r.get(0))
            .unwrap();
        for i in 0..3 {
            conn.execute(
                "INSERT INTO stories (briefing_id, sector, original_title, original_url,
                                      source_name, headline, summary, key_facts,
                                      why_it_matters, what_to_watch, url_hash, title_hash,
                                      importance_score, is_hero, display_order, source_type)
                 VALUES (?1, 'ai', 'news', 'https://example.com/news', 'Example',
                         'news', 's', '[]', 'w', 'w', ?3, ?4, 8, 0, ?2, 'news')",
                rusqlite::params![bid, i, format!("url-hash-{i}"), format!("title-hash-{i}")],
            )
            .unwrap();
        }

        write_financial_stories(&path, &[story("https://fec.gov/9", "nine")]).unwrap();

        let min_fin: i64 = conn
            .query_row(
                "SELECT MIN(display_order) FROM stories WHERE source_type = 'financial'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(min_fin, 3, "financial must start after the news rows, not at 0");
    }

    /// The returned count must reflect rows that actually landed. The old code
    /// discarded the insert result with .ok() and incremented unconditionally, so a
    /// rejected row still advanced the count — a false measurement of its own work.
    #[test]
    fn count_excludes_rows_that_failed_to_insert() {
        let (_dir, path) = db_with_briefing();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "CREATE UNIQUE INDEX idx_test_unique_url ON stories(briefing_id, url_hash)",
            [],
        )
        .unwrap();

        let dupes = vec![story("https://fec.gov/same", "a"), story("https://fec.gov/same", "b")];
        let stored = write_financial_stories(&path, &dupes).unwrap();
        assert_eq!(stored, 1, "the rejected duplicate must not be counted as stored");
    }

    /// Non-financial stories are skipped, and skipping must not consume an order slot.
    #[test]
    fn ignores_non_financial_stories() {
        let (_dir, path) = db_with_briefing();
        let mut news = story("https://example.com/n", "news item");
        news.article.source_type = "news".to_string();
        assert_eq!(write_financial_stories(&path, &[news]).unwrap(), 0);
    }
}

/// `is_hero` marks the story each freedom card leads with. It used to be
/// `i == 0` over the whole briefing, so four of the five cards had none.
#[cfg(test)]
mod freedom_hero_tests {
    use super::*;

    fn story(url: &str, title: &str) -> crate::claude::SummarizedStory {
        crate::claude::SummarizedStory {
            article: sources::RawArticle {
                title: title.to_string(),
                url: url.to_string(),
                source_name: "RSS".to_string(),
                source_url: "https://example.com".to_string(),
                published_at: None,
                content_snippet: "snippet".to_string(),
                sector: "freedom_time".to_string(),
                feed_id: "rss".to_string(),
                language: "en".to_string(),
                source_type: "news".to_string(),
                financial_metadata: None,
            },
            headline: title.to_string(),
            summary: "summary".to_string(),
            key_facts: vec!["fact".to_string()],
            why_it_matters: "matters".to_string(),
            what_to_watch: "watch".to_string(),
            importance_score: 5,
            sentiment: None,
            novelty: None,
            event_type: None,
        }
    }

    /// Reads back (freedom, headline) for every row written with is_hero = 1.
    fn heroes_of(curated: &[(&str, &crate::claude::SummarizedStory)]) -> Vec<(String, String)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        write_freedoms_to_db(&path, curated).unwrap();

        let conn = rusqlite::Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT freedom, headline FROM freedom_stories
                 WHERE is_hero = 1 ORDER BY display_order",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn every_freedom_leads_with_its_own_first_story() {
        let a = story("https://a.test/1", "Time first");
        let b = story("https://b.test/2", "Time second");
        let c = story("https://c.test/3", "Wealth first");
        let d = story("https://d.test/4", "Wealth second");
        let e = story("https://e.test/5", "Whoop only");
        let curated = vec![
            ("time", &a),
            ("time", &b),
            ("wealth", &c),
            ("wealth", &d),
            ("whoop", &e),
        ];

        assert_eq!(
            heroes_of(&curated),
            vec![
                ("time".to_string(), "Time first".to_string()),
                ("wealth".to_string(), "Wealth first".to_string()),
                ("whoop".to_string(), "Whoop only".to_string()),
            ],
            "each freedom that has stories should contribute exactly one hero"
        );
    }

    #[test]
    fn a_freedom_with_no_stories_contributes_no_hero() {
        let a = story("https://a.test/1", "Only story");
        let curated = vec![("health", &a)];
        assert_eq!(
            heroes_of(&curated),
            vec![("health".to_string(), "Only story".to_string())]
        );
    }
}
