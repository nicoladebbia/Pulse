use std::path::Path;

// --- Progress reporting ---

/// Stage weights (approximate % of total pipeline time)
const STAGE_WEIGHTS: &[(u8, &str, &str)] = &[
    (5,  "collecting",         "Collecting sources"),
    (2,  "deduplicating",      "Deduplicating articles"),
    (40, "summarizing",        "Summarizing stories"),
    (10, "analyzing",          "Cross-sector analysis"),
    (3,  "executive_summary",  "Executive summary"),
    (7,  "contextual",         "Contextual prefixes"),
    (5,  "embeddings",         "Generating embeddings"),
    (2,  "writing_db",         "Writing to database"),
    (18, "entities",           "Extracting entities"),
    (8,  "deep_summaries",     "Deep analysis (top stories)"),
];

pub struct ProgressWriter {
    path: std::path::PathBuf,
    started_at: String,
    current_stage: usize,
}

impl ProgressWriter {
    pub fn new(db_path: &Path) -> Self {
        let dir = db_path.parent().unwrap_or(Path::new("."));
        Self {
            path: dir.join("fetch-progress.json"),
            started_at: chrono::Utc::now().to_rfc3339(),
            current_stage: 0,
        }
    }

    pub fn start_stage(&mut self, stage_num: usize) {
        self.current_stage = stage_num;
        self.write_progress(None, 0.0);
    }

    pub fn update_detail(&self, detail: &str, sub_pct: f64) {
        self.write_progress(Some(detail), sub_pct);
    }

    pub fn finish(&self) {
        let json = serde_json::json!({
            "stage": "complete",
            "stage_label": "Complete",
            "stage_num": STAGE_WEIGHTS.len(),
            "total_stages": STAGE_WEIGHTS.len(),
            "percent": 100,
            "detail": null,
            "started_at": self.started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = self.atomic_write(&json);
    }

    fn write_progress(&self, detail: Option<&str>, sub_pct: f64) {
        let idx = self.current_stage.saturating_sub(1).min(STAGE_WEIGHTS.len() - 1);
        let (weight, stage_id, stage_label) = STAGE_WEIGHTS[idx];

        // percent = sum of completed stage weights + current stage partial
        let completed_weight: u8 = STAGE_WEIGHTS.iter().take(idx).map(|(w, _, _)| w).sum();
        let percent = (completed_weight as f64 + (weight as f64 * sub_pct / 100.0)).min(99.0) as u8;

        let json = serde_json::json!({
            "stage": stage_id,
            "stage_label": stage_label,
            "stage_num": self.current_stage,
            "total_stages": STAGE_WEIGHTS.len(),
            "percent": percent,
            "detail": detail,
            "started_at": self.started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = self.atomic_write(&json);
    }

    fn atomic_write(&self, json: &serde_json::Value) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string(json).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Log API usage to the database (opens its own connection for real-time visibility).
fn log_usage(db_path: &Path, provider: &str, model: &str, endpoint: &str, input_tokens: i64, output_tokens: i64) {
    if let Ok(conn) = rusqlite::Connection::open(db_path) {
        crate::db::log_api_usage(&conn, provider, model, endpoint, input_tokens, output_tokens);
    }
}

/// Default daily cost cap in USD. Override via PULSE_DAILY_COST_CAP env var.
/// Set to a low value because Pulse should normally run for ~$0.30/day; anything
/// above $0.50 means something is looping or a manual rerun got out of hand.
const DEFAULT_DAILY_COST_CAP_USD: f64 = 0.50;

/// Read today's accumulated API spend and abort if it's over the cap.
/// Called at the top of `run()` / `run_freedoms()` so a stuck loop cannot keep
/// burning money across multiple manual reruns in the same day.
fn check_daily_cost_cap(db_path: &Path) -> anyhow::Result<()> {
    let cap = std::env::var("PULSE_DAILY_COST_CAP")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_DAILY_COST_CAP_USD);

    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        // If we can't open the DB at all, let the rest of the pipeline surface the real error.
        Err(_) => return Ok(()),
    };

    let spent: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(estimated_cost_usd), 0.0)
             FROM api_usage
             WHERE date(created_at) = date('now')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0.0);

    if spent >= cap {
        anyhow::bail!(
            "Daily cost cap hit: ${:.4} spent today (cap: ${:.2}). \
             Aborting before more API calls are made. \
             Override with PULSE_DAILY_COST_CAP=<usd> env var.",
            spent, cap
        );
    }

    if spent > cap * 0.5 {
        tracing::warn!(
            "Daily spend so far: ${:.4} (cap ${:.2}, {:.0}% used)",
            spent, cap, spent / cap * 100.0
        );
    }
    Ok(())
}

pub async fn run(db_path: &Path) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    let mut progress = ProgressWriter::new(db_path);

    // Cost guardrail: bail before any LLM call if today's spend already crossed the cap.
    check_daily_cost_cap(db_path)?;

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
                // Sector-balanced cap: ensure each sector is represented
                let mut fallback = if news_articles.len() > 150 {
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
    tracing::info!("Phase 3: Summarizing {} stories...", articles_to_summarize.len());
    let summaries = crate::claude::summarize_stories(&articles_to_summarize, Some(&progress), db_path).await?;
    let sum_count = summaries.len() as i64;
    let sum_failed = articles_to_summarize.len() as i64 - sum_count;
    // No hardcoded log_usage here — GroqClient now logs each summarize_story call
    // with REAL token counts from the API response.

    if sum_failed > 0 {
        tracing::warn!("Summarized {}/{} stories ({} failed)", sum_count, articles_to_summarize.len(), sum_failed);
    } else {
        tracing::info!("Summarized all {} stories successfully", sum_count);
    }

    if summaries.is_empty() && financial_stories.is_empty() {
        anyhow::bail!("No stories could be summarized — all API calls failed. Aborting to avoid storing empty briefing.");
    }

    // Phase 4: Cross-sector analysis (news only)
    progress.start_stage(4);
    tracing::info!("Phase 4: Cross-sector analysis...");
    let mut analysis = crate::claude::analyze_cross_sector(&summaries, db_path).await?;

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
    let embeddings = match crate::embeddings::generate(&analysis.curated_stories, prefixes.as_deref()).await {
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
    progress.start_stage(9);
    tracing::info!("Phase 9: Extracting entities...");
    match extract_entities_from_stories(db_path, &analysis).await {
        Ok(count) => {
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
    match generate_deep_summaries(db_path, &analysis).await {
        Ok(count) => tracing::info!("Generated {} deep summaries", count),
        Err(e) => tracing::warn!("Deep summary generation failed (non-fatal): {}", e),
    }

    // Phase 11: Resolve predictions (hybrid router: market/LLM/manual) (non-fatal)
    progress.update_detail("Resolving predictions", 100.0);
    tracing::info!("Phase 11: Resolving predictions...");
    match validate_and_expire_predictions(db_path).await {
        Ok((resolved, expired)) => {
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
    {
        let conn = rusqlite::Connection::open(db_path)?;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        match recompute_signals_pipeline(&conn, &today) {
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
    match compute_cross_signals(db_path) {
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
                tracing::info!("Calibration: signal weights auto-adjusted");
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
    send_notification(analysis.curated_stories.len())?;

    let duration = start.elapsed();

    // Pipeline health summary — write to DB and log
    {
        let conn = rusqlite::Connection::open(db_path)?;
        crate::db::run_migrations(&conn)?;

        let total_stories: i64 = conn.query_row("SELECT COUNT(*) FROM stories", [], |r| r.get(0)).unwrap_or(0);
        let total_embeddings: i64 = conn.query_row("SELECT COUNT(*) FROM story_embeddings WHERE story_id > 0", [], |r| r.get(0)).unwrap_or(0);
        let emb_pct = if total_stories > 0 { (total_embeddings as f64 / total_stories as f64) * 100.0 } else { 0.0 };

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT INTO pipeline_health (run_date, stories_fetched, stories_summarized, stories_embedded, embedding_coverage_pct, summary_failures, duration_secs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![today, raw_count, analysis.curated_stories.len(), total_embeddings, emb_pct, sum_failed, duration.as_secs_f64()],
        ).ok();

        tracing::info!("╔══════════════════════════════════════════╗");
        tracing::info!("║         PIPELINE HEALTH SUMMARY          ║");
        tracing::info!("╠══════════════════════════════════════════╣");
        tracing::info!("║ Articles fetched:    {:>6}              ║", raw_count);
        tracing::info!("║ Stories summarized:  {:>6}              ║", analysis.curated_stories.len());
        tracing::info!("║ Summary failures:    {:>6}              ║", sum_failed);
        tracing::info!("║ Embedding coverage:  {:>5.1}%              ║", emb_pct);
        tracing::info!("║ Total stories in DB: {:>6}              ║", total_stories);
        tracing::info!("║ Duration:            {:>5.1}s              ║", duration.as_secs_f64());
        tracing::info!("╚══════════════════════════════════════════╝");
    }

    Ok(())
}

/// Validate active predictions via hybrid router (Push 2 rebuild).
///
/// Routing:
///   - target_metric has ticker → market-based resolution from entity_prices
///   - else → Sonnet LLM outcome check (capped at 15/day)
///   - LLM "unclear" → increments resolution_attempts; after 3 → needs_review
///
/// Returns (resolved, expired).
async fn validate_and_expire_predictions(db_path: &std::path::Path) -> anyhow::Result<(usize, usize)> {
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

    tracing::info!("Predictions v2: {} resolved, {} expired, {} LLM checks used",
        resolved, expired, llm_checks_used);

    Ok((resolved, expired))
}

// ----------------------------------------------------------------------------
// Hybrid resolver types + helpers (Push 2, Task 2.7)
// ----------------------------------------------------------------------------

#[derive(Debug)]
struct PredToResolve {
    id: i64,
    title: String,
    content: String,
    confidence: f64,
    target_metric: Option<String>,
    target_date: Option<String>,
    resolution_attempts: i64,
}

#[derive(Debug)]
enum ResolutionOutcome {
    Validated { summary: String, method: String },
    Invalidated { summary: String, method: String },
    Partial { summary: String, method: String },
    Unclear,
}

/// Resolve a ticker-grounded prediction using entity_prices.
/// target_metric is JSON {ticker, operator, value, unit, baseline_date?}
fn resolve_market_prediction(
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
async fn resolve_llm_prediction(
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

    let mut input = format!("PREDICTION (made earlier): {}\n\nTARGET DATE: {}\n\nRECENT STORIES:\n",
        p.title, p.target_date.as_deref().unwrap_or("unknown"));
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
async fn compute_calibration_stats(db_path: &std::path::Path) -> anyhow::Result<()> {
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
fn apply_resolution(
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

/// Keyword-based fallback when Voyage API is unavailable.
fn validate_predictions_keyword_fallback(
    conn: &rusqlite::Connection,
    predictions: &[(i64, String, String, f64)],
    story_embeddings: &[(i64, Vec<f32>)],
    today: &str,
) -> anyhow::Result<(usize, usize)> {
    let mut validated = 0usize;

    for (pred_id, title, content, confidence) in predictions {
        let pred_text = format!("{} {}", title, content).to_lowercase();
        let pred_terms: std::collections::HashSet<String> = pred_text.split_whitespace()
            .filter(|w| w.len() >= 4)
            .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
            .filter(|w| !w.is_empty() && !["this", "that", "will", "with", "from", "have", "been",
                "more", "than", "about", "would", "could", "should", "their", "these", "those",
                "when", "what", "which", "there", "based", "likely", "expect"].contains(&w.as_str()))
            .collect();

        if pred_terms.len() < 3 { continue; }

        let mut best_match: Option<(i64, f32)> = None;
        for &(sid, _) in story_embeddings {
            let story_text: Option<String> = conn.query_row(
                "SELECT LOWER(headline || ' ' || summary || ' ' || key_facts) FROM stories WHERE id = ?1",
                [sid], |row| row.get(0),
            ).ok();

            if let Some(text) = story_text {
                let story_terms: std::collections::HashSet<&str> = text.split_whitespace()
                    .filter(|w| w.len() >= 4).collect();
                let overlap = pred_terms.iter()
                    .filter(|t| story_terms.contains(t.as_str())).count();
                let overlap_ratio = overlap as f32 / pred_terms.len() as f32;

                if overlap_ratio > 0.4 {
                    if best_match.is_none() || overlap_ratio > best_match.unwrap().1 {
                        best_match = Some((sid, overlap_ratio));
                    }
                }
            }
        }

        if let Some((sid, overlap)) = best_match {
            let nudge = if overlap > 0.8 { 0.10 } else if overlap > 0.6 { 0.06 } else { 0.03 };
            let new_prob = (confidence + nudge).min(0.95);

            let history: String = conn.query_row(
                "SELECT COALESCE(probability_history, '[]') FROM insights WHERE id = ?1",
                [pred_id], |row| row.get(0),
            ).unwrap_or_else(|_| "[]".to_string());

            let mut entries: Vec<serde_json::Value> = serde_json::from_str(&history).unwrap_or_default();
            entries.push(serde_json::json!({
                "date": today,
                "probability": new_prob,
                "overlap": overlap,
                "story_id": sid,
                "reason": format!("Keyword fallback ({:.0}% term overlap)", overlap * 100.0)
            }));

            conn.execute(
                "UPDATE insights SET probability_history = ?1, confidence = ?2 WHERE id = ?3",
                rusqlite::params![serde_json::to_string(&entries).unwrap_or_default(), new_prob, pred_id],
            ).ok();
            validated += 1;
        }
    }

    let expired: usize = conn.execute(
        "UPDATE insights SET status = 'expired' WHERE insight_type = 'prediction' AND status = 'active' AND predicted_date IS NOT NULL AND predicted_date < date('now')",
        [],
    ).unwrap_or(0);

    Ok((validated, expired))
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max.min(s.len())]) }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 { 0.0 } else { (dot / denom) as f32 }
}

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

    let raw = client.call_text("llama-3.3-70b-versatile", "freedoms_executive_summary", system, &input, 600).await?;
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

#[derive(serde::Deserialize)]
struct GeneratedPrediction {
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
struct PredictionGenResponse {
    predictions: Vec<GeneratedPrediction>,
}

/// Generate fresh predictions from today's top stories + cross-signals.
/// Runs once per daily pipeline. Uses Sonnet by default; on Sunday, uses Opus
/// with a bigger input for a "weekly deep-dive" run (see plan Q2).
async fn generate_predictions(
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

    // Build input block
    let mut input = String::from("Today's top stories:\n");
    for (i, (id, headline, summary, sector)) in top_stories.iter().take(max_stories).enumerate() {
        input.push_str(&format!(
            "[{}] story_id={} sector={} | {} — {}\n",
            i, id, sector, headline,
            summary.chars().take(150).collect::<String>()
        ));
    }
    input.push_str("\nTop cross-signals today:\n");
    for (i, (eid, name, ticker, score)) in top_signals.iter().take(max_signals).enumerate() {
        input.push_str(&format!(
            "[{}] signal_id={} entity=\"{}\" ticker={} score={:.2}\n",
            i, eid, name, ticker.as_deref().unwrap_or("-"), score
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
    for p in &parsed.predictions {
        // Confidence validation
        let confidence = p.confidence.clamp(0.5, 0.95);

        // Build "evidence" JSON in the legacy shape for back-compat
        let evidence = serde_json::json!(
            p.source_story_ids.iter().map(|sid| serde_json::json!({
                "story_id": sid,
                "reasoning": "Evidence type: source"
            })).collect::<Vec<_>>()
        );

        // Seed confidence_history with the initial confidence
        let confidence_history = serde_json::json!([confidence]);

        let target_metric_str = p.target_metric.as_ref().map(|v| v.to_string());
        let source_story_ids_str = serde_json::to_string(&p.source_story_ids).unwrap_or_else(|_| "[]".to_string());
        let source_signal_ids_str = serde_json::to_string(&p.source_signal_ids).unwrap_or_else(|_| "[]".to_string());

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
            Ok(_) => inserted += 1,
            Err(e) => tracing::warn!("Prediction insert failed: {}", e),
        }
    }

    tracing::info!("Predictions: inserted {} of {} generated predictions", inserted, parsed.predictions.len());
    Ok(inserted)
}

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

    client.call_text("llama-3.1-8b-instant", "executive_summary", system, &input, 300).await
}

/// Generate deep summaries for stories with relevance_score >= 8.
/// Uses Anthropic Claude Sonnet for higher quality analysis.
/// Capped at 5 stories to control API costs.
async fn generate_deep_summaries(db_path: &std::path::Path, analysis: &crate::claude::AnalysisResult) -> anyhow::Result<usize> {
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
use crate::sources::RawArticle;

/// Pre-filter articles to ~150, balanced across sectors.
/// Prioritizes RSS/direct feeds over Google News duplicates.
fn prefilter_articles(mut articles: Vec<RawArticle>) -> Vec<RawArticle> {
    let per_sector = 50; // 50 per sector → ~200 total max

    // Prioritize: RSS/HN feeds first (higher quality), then Google News
    articles.sort_by(|a, b| {
        let a_priority = if a.feed_id.starts_with("google_news") { 1 } else { 0 };
        let b_priority = if b.feed_id.starts_with("google_news") { 1 } else { 0 };
        a_priority.cmp(&b_priority)
    });

    let mut result = Vec::new();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    for article in &articles {
        let count = counts.entry(&article.sector).or_insert(0);
        if *count < per_sector {
            result.push(article.clone());
            *count += 1;
        }
    }

    tracing::info!(
        "Pre-filter: ai={}, miami={}, italy={}, tech={}",
        counts.get("ai").unwrap_or(&0),
        counts.get("miami").unwrap_or(&0),
        counts.get("italy").unwrap_or(&0),
        counts.get("tech").unwrap_or(&0),
    );

    result
}

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

    let mut filled = 0;
    for chunk in stories.chunks(10) {
        if filled > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(21)).await;
        }

        let texts: Vec<String> = chunk.iter().map(|(_, headline, summary, key_facts)| {
            format!("{}. {}. {}", headline, summary, key_facts)
        }).collect();
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
    for score in &analysis.relevance_scores {
        if let Some(&db_id) = story_db_ids.get(score.story_idx) {
            tx.execute(
                "UPDATE stories SET relevance_score = ?1, relevance_reason = ?2 WHERE id = ?3",
                rusqlite::params![score.relevance, score.reason, db_id],
            )?;
        }
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

async fn extract_entities_from_stories(db_path: &Path, analysis: &crate::claude::AnalysisResult) -> anyhow::Result<usize> {
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
    for (batch_start, chunk) in analysis.curated_stories.chunks(30).enumerate().map(|(i, c)| (i * 30, c)) {
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
    struct Ent { name: String, entity_type: String, sentiment: f64, context: Option<String> }
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

pub async fn run_freedoms(db_path: &Path) -> anyhow::Result<()> {
    let start = std::time::Instant::now();

    // Cost guardrail: bail before any LLM call if today's spend already crossed the cap.
    check_daily_cost_cap(db_path)?;

    // Phase 1: Collect from all sources, filter to freedom_* only
    tracing::info!("Freedoms: Collecting articles...");
    sources::API_CALLS.reset();
    let all_articles = sources::collect_all().await?;
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

    // Phase 3: Summarize
    tracing::info!("Freedoms: Summarizing {} stories...", to_summarize.len());
    let summaries = crate::claude::summarize_stories(&to_summarize, None, db_path).await?;
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
            "llama-3.3-70b-versatile",
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
    for (label, indices) in &freedom_lists {
        let mut count = 0;
        for &idx in *indices {
            if count >= max_per_freedom { break; }
            if let Some(s) = sorted.get(idx) {
                curated.push((label, s));
                count += 1;
            }
        }
        tracing::info!("Freedoms: {} = {} stories (LLM returned {})", label, count, indices.len());
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
    match crate::embeddings::generate(&freedom_summaries, None).await {
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

    // Count per freedom
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

    // Insert freedom stories
    for (i, (freedom, story)) in curated.iter().enumerate() {
        let key_facts_json = serde_json::to_string(&story.key_facts)?;
        let is_hero = if i == 0 { 1 } else { 0 };

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

/// Dedup financial articles against the financial_dedup table.
/// Uses feed_id as source_type and url as source_id for dedup.
/// Returns only articles not previously seen.
fn dedup_financial_articles(
    db_path: &Path,
    articles: Vec<sources::RawArticle>,
) -> Vec<sources::RawArticle> {
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

    let mut stored = 0;
    for story in stories {
        if story.article.source_type != "financial" {
            continue;
        }

        let key_facts_json = serde_json::to_string(&story.key_facts).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
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
                stored as i32,
                crate::dedup::url_hash(&story.article.url),
                crate::dedup::title_hash(&story.article.title),
                story.article.source_type,
                story.article.financial_metadata,
            ],
        ).ok(); // Non-fatal per story — dedup might reject
        stored += 1;
    }

    // Update briefing story count
    conn.execute(
        "UPDATE briefings SET story_count = (SELECT COUNT(*) FROM stories WHERE briefing_id = ?1) WHERE id = ?1",
        [briefing_id],
    ).ok();

    Ok(stored)
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

/// Compute cross-signal scores for entities after signal recomputation.
/// This is called from the pipeline after entity extraction.
/// Auto-populate entity_tickers from SEC company_tickers.json.
fn populate_tickers(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;

    // Check if entity_tickers table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entity_tickers')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(0);
    }

    // Get unmapped entities that could be companies (multiple entity types)
    let unmapped: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.name FROM entities e
             WHERE e.entity_type IN ('company', 'insider_trade', 'contract_award',
                   'patent_cluster', 'material_event', 'private_placement')
             AND e.id NOT IN (SELECT entity_id FROM entity_tickers)
             LIMIT 200"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if unmapped.is_empty() {
        return Ok(0);
    }

    // Step 1: Map from financial_metadata (CIK/ticker from SEC filings = highest confidence)
    let mut mapped = 0;
    let mut still_unmapped = Vec::new();

    for (entity_id, name) in &unmapped {
        let ticker_from_metadata: Option<(String, String)> = conn.query_row(
            "SELECT
                json_extract(s.financial_metadata, '$.ticker'),
                json_extract(s.financial_metadata, '$.cik')
             FROM entity_mentions em
             JOIN stories s ON em.story_id = s.id
             WHERE em.entity_id = ?1
               AND s.financial_metadata IS NOT NULL
               AND json_valid(s.financial_metadata)
               AND json_extract(s.financial_metadata, '$.ticker') IS NOT NULL
             LIMIT 1",
            [entity_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1).unwrap_or_default())),
        ).ok();

        if let Some((ticker, cik)) = ticker_from_metadata {
            conn.execute(
                "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                 VALUES (?1, ?2, ?3, 1.0)",
                rusqlite::params![entity_id, ticker, cik],
            )?;
            mapped += 1;
        } else {
            still_unmapped.push((*entity_id, name.clone()));
        }
    }

    // Step 2: Download SEC company_tickers.json and match remaining (by name + CIK)
    if !still_unmapped.is_empty() {
        match download_sec_tickers() {
            Ok(sec_map) => {
                // Build reverse CIK→ticker map for CIK-bearing entity names
                let cik_map: std::collections::HashMap<String, (String, String)> = sec_map.values()
                    .filter(|(_, cik)| !cik.is_empty())
                    .map(|(ticker, cik)| (cik.clone(), (ticker.clone(), cik.clone())))
                    .collect();

                for (entity_id, name) in &still_unmapped {
                    // Try CIK extraction from entity name first (e.g. "UL Solutions Inc.  (CIK 0001901440)")
                    let mut found = false;
                    if let Some(cik_start) = name.to_lowercase().find("(cik") {
                        let cik_part = &name[cik_start..];
                        let cik_digits: String = cik_part.chars().filter(|c| c.is_ascii_digit()).collect();
                        if !cik_digits.is_empty() {
                            if let Some((ticker, cik)) = cik_map.get(&cik_digits) {
                                conn.execute(
                                    "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                                     VALUES (?1, ?2, ?3, 0.95)",
                                    rusqlite::params![entity_id, ticker, cik],
                                )?;
                                mapped += 1;
                                found = true;
                            }
                        }
                    }
                    if found { continue; }

                    if let Some((ticker, cik, confidence)) = resolve_ticker_sec(name, &sec_map) {
                        conn.execute(
                            "INSERT OR IGNORE INTO entity_tickers (entity_id, ticker, cik, confidence)
                             VALUES (?1, ?2, ?3, ?4)",
                            rusqlite::params![entity_id, ticker, cik, confidence],
                        )?;
                        mapped += 1;
                    }
                }
            }
            Err(e) => tracing::warn!("SEC ticker download failed (non-fatal): {}", e),
        }
    }

    Ok(mapped)
}

/// Download SEC company_tickers.json with 7-day file cache.
fn download_sec_tickers() -> anyhow::Result<std::collections::HashMap<String, (String, String)>> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(download_sec_tickers_async())
    })
}

async fn download_sec_tickers_async() -> anyhow::Result<std::collections::HashMap<String, (String, String)>> {
    // Check file cache first (7-day TTL)
    let cache_dir = dirs::home_dir().unwrap_or_default().join(".pulse");
    let cache_path = cache_dir.join("sec_tickers.json");

    if cache_path.exists() {
        if let Ok(metadata) = std::fs::metadata(&cache_path) {
            if let Ok(modified) = metadata.modified() {
                let age = std::time::SystemTime::now().duration_since(modified).unwrap_or_default();
                if age < std::time::Duration::from_secs(7 * 86400) {
                    // Cache hit
                    if let Ok(data) = std::fs::read_to_string(&cache_path) {
                        if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, (String, String)>>(&data) {
                            tracing::info!("SEC tickers: loaded {} from cache (age: {}h)", map.len(), age.as_secs() / 3600);
                            return Ok(map);
                        }
                    }
                }
            }
        }
    }

    let url = "https://www.sec.gov/files/company_tickers.json";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let resp = client
        .get(url)
        .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
        .send()
        .await?;

    if !resp.status().is_success() {
        // Try cache even if expired
        if cache_path.exists() {
            if let Ok(data) = std::fs::read_to_string(&cache_path) {
                if let Ok(map) = serde_json::from_str(&data) {
                    tracing::warn!("SEC tickers: API returned {}, using stale cache", resp.status());
                    return Ok(map);
                }
            }
        }
        anyhow::bail!("SEC company_tickers.json returned {}", resp.status());
    }

    #[derive(serde::Deserialize)]
    struct SecEntry { cik_str: String, ticker: String, title: String }

    let raw: std::collections::HashMap<String, SecEntry> = resp.json().await?;
    let mut map = std::collections::HashMap::with_capacity(raw.len());
    for entry in raw.values() {
        map.insert(entry.title.to_lowercase(), (entry.ticker.clone(), entry.cik_str.clone()));
    }

    // Write cache
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = std::fs::write(&cache_path, serde_json::to_string(&map).unwrap_or_default());

    tracing::info!("SEC tickers: downloaded {} mappings (cached)", map.len());
    Ok(map)
}

/// Resolve entity name to ticker using SEC data.
/// Tries: exact match → suffix-stripped match → contains match.
fn resolve_ticker_sec(
    name: &str,
    sec_map: &std::collections::HashMap<String, (String, String)>,
) -> Option<(String, String, f64)> {
    // Strip CIK patterns like "(CIK 0001901440)" and clean up
    let cleaned = name
        .trim()
        .to_lowercase();
    let name_lower = if let Some(idx) = cleaned.find("(cik") {
        cleaned[..idx].trim().to_string()
    } else {
        cleaned
    };

    if name_lower.is_empty() || name_lower.len() < 2 {
        return None;
    }

    // 1. Exact match
    if let Some((ticker, cik)) = sec_map.get(&name_lower) {
        return Some((ticker.clone(), cik.clone(), 1.0));
    }

    // 2. Strip common suffixes
    let suffixes = [
        " inc", " inc.", " corp", " corp.", " ltd", " ltd.", " llc",
        " co", " co.", " plc", " sa", " ag", " se", " nv",
        " holdings", " group", " technologies", " technology",
        " international", " solutions", " systems", " enterprises",
    ];
    let stripped = suffixes.iter().fold(name_lower.as_str(), |s, sfx| s.trim_end_matches(sfx)).trim();
    if stripped != name_lower && stripped.len() >= 3 {
        for (key, (ticker, cik)) in sec_map.iter() {
            let key_stripped = suffixes.iter().fold(key.as_str(), |s, sfx| s.trim_end_matches(sfx)).trim();
            if key_stripped == stripped {
                return Some((ticker.clone(), cik.clone(), 0.8));
            }
        }
    }

    // 3. Contains match (min 5 chars to avoid false positives)
    if name_lower.len() >= 5 {
        for (key, (ticker, cik)) in sec_map.iter() {
            if key.contains(&name_lower) || name_lower.contains(key.as_str()) {
                return Some((ticker.clone(), cik.clone(), 0.6));
            }
        }
    }

    None
}

/// Recompute all entity signals from entity_mentions data.
/// Pipeline version — matches src-tauri/src/services/signals.rs::recompute_signals
/// plus new dimensions (lobbying, regulatory, patent).
///
/// Performance: rewritten 2026-05-10 from per-topic subqueries (O(topics × stories))
/// to per-metric GROUP BY (O(stories) per metric). Was hanging at >25min on 6566
/// topics × 9 subqueries × full canonical scan. Now ~9 single-pass queries.
fn recompute_signals_pipeline(conn: &rusqlite::Connection, today: &str) -> anyhow::Result<usize> {
    use std::collections::HashMap;

    // Check if entity_canonical table exists for canonical grouping
    let has_canonical: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entity_canonical')",
        [], |row| row.get(0),
    ).unwrap_or(false);

    // Build a temp table mapping entity_id -> (topic, sector). This makes every
    // downstream metric a simple JOIN on entity_id (indexed) instead of a
    // COALESCE(...) = ?  predicate that forces a full entities scan per topic.
    conn.execute_batch("DROP TABLE IF EXISTS temp_topic_map;")?;
    if has_canonical {
        conn.execute_batch(
            "CREATE TEMP TABLE temp_topic_map AS
             SELECT e.id AS entity_id,
                    COALESCE(ec.canonical_name, e.name) AS topic,
                    COALESCE(ec.sector, e.sector) AS sector
             FROM entities e
             LEFT JOIN entity_canonical ec ON ec.id = e.canonical_id;
             CREATE INDEX temp_topic_map_entity ON temp_topic_map(entity_id);
             CREATE INDEX temp_topic_map_topic ON temp_topic_map(topic, sector);"
        )?;
    } else {
        conn.execute_batch(
            "CREATE TEMP TABLE temp_topic_map AS
             SELECT e.id AS entity_id, e.name AS topic, e.sector AS sector
             FROM entities e;
             CREATE INDEX temp_topic_map_entity ON temp_topic_map(entity_id);
             CREATE INDEX temp_topic_map_topic ON temp_topic_map(topic, sector);"
        )?;
    }

    // Stage 1 — window counts (7d/30d/90d) + days_active per (topic, sector).
    // Single pass over entity_mentions ⋈ temp_topic_map.
    let mut stmt = conn.prepare(
        "SELECT tm.topic, tm.sector,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-7 days') THEN 1 ELSE 0 END) AS w7,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-30 days') THEN 1 ELSE 0 END) AS w30,
            SUM(CASE WHEN em.mentioned_at >= date(?1, '-90 days') THEN 1 ELSE 0 END) AS w90,
            COUNT(DISTINCT em.mentioned_at) AS days_active
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         WHERE em.mentioned_at >= date(?1, '-90 days')
         GROUP BY tm.topic, tm.sector"
    )?;

    let window_rows: Vec<(String, Option<String>, i64, i64, i64, i64)> = stmt
        .query_map([today], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?
        .filter_map(|r| r.ok())
        .collect();

    // Map (topic, sector) -> aggregated metrics. Sector key is normalized to
    // empty string so it round-trips through HashMap (we restore Option<String> on write).
    type Key = (String, String);
    let key = |t: &str, s: &Option<String>| -> Key {
        (t.to_string(), s.clone().unwrap_or_default())
    };

    // Helper: load a single-metric GROUP BY result into a HashMap<Key, f64>.
    let load_metric = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> rusqlite::Result<HashMap<Key, f64>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params, |row| {
                let topic: String = row.get(0)?;
                let sector: Option<String> = row.get(1)?;
                let val: f64 = row.get(2)?;
                Ok(((topic, sector.unwrap_or_default()), val))
            })?
            .filter_map(|r| r.ok());
        Ok(rows.collect())
    };

    // Stage 2 — source diversity (last 7 days). Distinct source_names per topic.
    let diversity_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, CAST(COUNT(DISTINCT s.source_name) AS REAL)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE em.mentioned_at >= date(?1, '-7 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 3 — insider buy volume (Form 4, last 30d), weighted by signal_weight.
    let insider_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND json_extract(s.financial_metadata, '$.filing_type') IN ('4', 'form4')
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.total_value') AS REAL), 0)
                 * COALESCE(CAST(json_extract(s.financial_metadata, '$.signal_weight') AS REAL), 0.0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, '-30 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 4 — government contract value (USASpending, last 90d).
    let contract_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND s.source_name LIKE '%USASpending%'
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.amount') AS REAL), 0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, '-90 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 5 — lobbying spend (LDA / Senate, last 90d).
    let lobby_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND (s.source_name LIKE '%LDA%' OR s.source_name LIKE '%Senate%')
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.amount') AS REAL), 0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, '-90 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 6 — Federal Register mentions (last 30d).
    let reg_count_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, CAST(COUNT(*) AS REAL)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_name LIKE '%Federal Register%'
           AND em.mentioned_at >= date(?1, '-30 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 7 — patent filings (USPTO / Google Patents, last 30d).
    let patent_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, CAST(COUNT(*) AS REAL)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE (s.source_name = 'USPTO' OR s.source_name = 'Google Patents')
           AND em.mentioned_at >= date(?1, '-30 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 8 — 8-K event severity (max severity, last 14d).
    let event_severity_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(MAX(
            CASE WHEN json_valid(s.financial_metadata)
                 AND json_extract(s.financial_metadata, '$.event_severity') IS NOT NULL
            THEN CAST(json_extract(s.financial_metadata, '$.event_severity') AS REAL)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_name LIKE '%EDGAR 8-K%'
           AND em.mentioned_at >= date(?1, '-14 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 9 — Wikipedia search-trend delta (last 7d).
    let search_map: HashMap<Key, f64> = load_metric(
        "SELECT tm.topic, tm.sector, COALESCE(SUM(
            CASE WHEN json_valid(s.financial_metadata)
                 AND s.source_name = 'Wikipedia Pageviews'
            THEN COALESCE(CAST(json_extract(s.financial_metadata, '$.views_delta_pct') AS REAL), 0)
            ELSE 0 END
         ), 0)
         FROM entity_mentions em
         JOIN temp_topic_map tm ON tm.entity_id = em.entity_id
         JOIN stories s ON s.id = em.story_id
         WHERE s.source_type = 'financial'
           AND em.mentioned_at >= date(?1, '-7 days')
         GROUP BY tm.topic, tm.sector",
        &[&today],
    ).unwrap_or_default();

    // Stage 10 — institutional flow (13F filings). Parse all 13F holdings JSON
    // ONCE upfront, build issuer-name index, then look up per topic.
    let inst_flow_map: HashMap<Key, f64> = {
        let mut stmt = conn.prepare(
            "SELECT s.financial_metadata FROM stories s
             WHERE s.source_name LIKE '%13F%'
               AND s.financial_metadata IS NOT NULL
               AND json_valid(s.financial_metadata)
               AND json_extract(s.financial_metadata, '$.holdings') IS NOT NULL
               AND s.published_at >= date(?1, '-90 days')"
        )?;

        let metadata_rows: Vec<String> = stmt.query_map([today], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        // Pre-extract normalized issuer names per filing.
        let normalize = |s: &str| -> String {
            s.to_uppercase()
                .replace(" INC", "").replace(" CORP", "").replace(" CO", "")
                .replace(" LTD", "").replace(" LLC", "").replace(" PLC", "")
                .replace(",", "").replace(".", "").trim().to_string()
        };

        // Per filing: set of normalized issuer names it holds.
        let filings_issuers: Vec<std::collections::HashSet<String>> = metadata_rows.iter()
            .filter_map(|md_str| serde_json::from_str::<serde_json::Value>(md_str).ok())
            .map(|md| {
                md.get("holdings")
                    .and_then(|h| h.as_array())
                    .map(|arr| arr.iter()
                        .filter_map(|h| h.get("issuer").and_then(|i| i.as_str()).map(normalize))
                        .collect::<std::collections::HashSet<_>>())
                    .unwrap_or_default()
            })
            .collect();

        // For each (topic, sector), count filings whose normalized issuer set
        // matches (substring either way). This is still O(topics × filings) but
        // the inner work is set lookup not SQL — typically a few hundred filings.
        let topic_keys: Vec<(String, Option<String>)> = window_rows.iter()
            .map(|(t, s, _, _, _, _)| (t.clone(), s.clone()))
            .collect();

        let mut map: HashMap<Key, f64> = HashMap::new();
        for (topic, sector) in &topic_keys {
            let topic_norm = normalize(topic);
            if topic_norm.is_empty() { continue; }
            let mut count = 0i64;
            for issuer_set in &filings_issuers {
                let hit = issuer_set.iter().any(|issuer_norm| {
                    issuer_norm == &topic_norm
                        || issuer_norm.contains(&topic_norm)
                        || topic_norm.contains(issuer_norm)
                });
                if hit { count += 1; }
            }
            if count > 0 {
                map.insert(key(topic, sector), count as f64);
            }
        }
        map
    };

    // Stage 11 — sector-level FRED indicator (single value, applied to all rows).
    let import_delta: f64 = conn.query_row(
        "SELECT COALESCE(AVG(ABS(
            CAST(json_extract(s.financial_metadata, '$.change_pct') AS REAL)
         )), 0)
         FROM stories s
         WHERE s.source_name = 'FRED'
           AND json_valid(s.financial_metadata)
           AND json_extract(s.financial_metadata, '$.series_id') IN ('INDPRO', 'PPIACO', 'DCOILWTICO')
           AND s.created_at >= datetime('now', '-7 days')",
        [],
        |row| row.get(0),
    ).unwrap_or(0.0);

    // Final write — single transaction, two SQL statements per topic
    // (upsert windows + update metrics). All metric lookups are O(1).
    let tx = conn.unchecked_transaction()?;
    let mut count = 0usize;

    for (topic, sector, w7, w30, w90, days_active) in &window_rows {
        let rate_7d = *w7 as f64 / 7.0;
        let rate_30d = *w30 as f64 / 30.0;
        let acc = if *w30 == 0 { if *w7 > 0 { 10.0 } else { 0.0 } }
            else if rate_30d < 0.001 { if *w7 > 0 { 10.0 } else { 0.0 } }
            else { rate_7d / rate_30d };

        let total = (*w30).max(*w7);
        let traj = if *w7 == 0 && *w30 == 0 { "dormant" }
            else if total >= 14 && *days_active >= 10 { "dominant" }
            else if total >= 7 && *days_active >= 5 { "hot" }
            else if acc < 0.8 && total >= 3 { "fading" }
            else if total >= 3 || *days_active >= 2 { "rising" }
            else if *w7 > 0 { "rising" }
            else { "dormant" };

        tx.execute(
            "INSERT INTO signals (topic, sector, window_7d, window_30d, window_90d, acceleration, trajectory, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(topic, sector) DO UPDATE SET
                 window_7d = excluded.window_7d, window_30d = excluded.window_30d,
                 window_90d = excluded.window_90d, acceleration = excluded.acceleration,
                 trajectory = excluded.trajectory, updated_at = datetime('now')",
            rusqlite::params![topic, sector, w7, w30, w90, acc, traj],
        ).ok();

        let k = key(topic, sector);
        let diversity = diversity_map.get(&k).copied().unwrap_or(0.0) as i64;
        let insider_vol = insider_map.get(&k).copied().unwrap_or(0.0);
        let contract_val = contract_map.get(&k).copied().unwrap_or(0.0);
        let lobby_spend = lobby_map.get(&k).copied().unwrap_or(0.0);
        let reg_count = reg_count_map.get(&k).copied().unwrap_or(0.0);
        let patent_count = patent_map.get(&k).copied().unwrap_or(0.0);
        let event_boost = event_severity_map.get(&k).copied().unwrap_or(0.0);
        let inst_flow = inst_flow_map.get(&k).copied().unwrap_or(0.0);
        let search_delta = search_map.get(&k).copied().unwrap_or(0.0);

        // Composite regulatory_sentiment: Fed Reg count + 8-K event boost
        let reg_composite = reg_count + (event_boost * 3.0);

        tx.execute(
            "UPDATE signals SET source_diversity = ?1, insider_buy_volume = ?2, contract_value = ?3,
                 lobbying_spend_delta = ?4, regulatory_sentiment = ?5, patent_filing_rate = ?6,
                 institutional_flow = ?7, search_trend_delta = ?8, import_volume_delta = ?9
             WHERE topic = ?10 AND (sector = ?11 OR (?11 IS NULL AND sector IS NULL))",
            rusqlite::params![diversity, insider_vol, contract_val, lobby_spend, reg_composite, patent_count,
                              inst_flow, search_delta, import_delta, topic, sector],
        ).ok();

        count += 1;
    }

    tx.commit()?;
    conn.execute_batch("DROP TABLE IF EXISTS temp_topic_map;").ok();

    Ok(count)
}

/// Sigmoid normalization: maps raw value to [0, 1]. Matches cross_signals.rs.
fn normalize_signal(value: f64, scale: f64) -> f64 {
    if value <= 0.0 { return 0.0; }
    1.0 - (-value / scale).exp()
}

/// Compute cross-signal scores for all entities.
/// Unified with src-tauri/src/services/cross_signals.rs — same 8 dimensions,
/// same weights, same convergence threshold (3+ signals > 0.3, diversity >= 3).
fn compute_cross_signals(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Load calibrated weights if available, else use defaults
    let weights = load_calibrated_weights(&conn);

    // Get all non-dormant signals with ALL 8 dimensions
    let mut stmt = conn.prepare(
        "SELECT s.topic, s.sector, s.window_7d, s.window_30d, s.acceleration,
                COALESCE(s.source_diversity, 0),
                COALESCE(s.insider_buy_volume, 0),
                COALESCE(s.institutional_flow, 0),
                COALESCE(s.contract_value, 0),
                COALESCE(s.patent_filing_rate, 0),
                COALESCE(s.search_trend_delta, 0),
                COALESCE(s.import_volume_delta, 0),
                COALESCE(s.regulatory_sentiment, 0),
                COALESCE(s.lobbying_spend_delta, 0)
         FROM signals s
         WHERE s.trajectory != 'dormant'
         ORDER BY s.window_30d DESC"
    )?;

    let rows: Vec<(String, Option<String>, i64, i64, f64, i64, f64, f64, f64, f64, f64, f64, f64, f64)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                row.get(12)?, row.get(13)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for (topic, _sector, w7, _w30, acceleration, src_diversity,
         insider_vol, inst_flow, contract_val, patent_rate,
         search_delta, import_delta, reg_sentiment, lobby_delta) in &rows
    {
        // Normalize each dimension — same scales as cross_signals.rs
        let insider_norm = normalize_signal(*insider_vol, 1_000_000.0);
        let inst_norm = normalize_signal(*inst_flow, 15.0); // Count of distinct 13F filers holding this stock
        // Pure acceleration ratio, gated by minimum sample. Mirrors cross_signals.rs.
        let news_norm = if *w7 >= 3 { normalize_signal(*acceleration, 2.0) } else { 0.0 };
        // Government signal: contract value + regulatory/8-K severity composite
        let contract_norm = normalize_signal(*contract_val, 10_000_000.0);
        let reg_norm = normalize_signal(*reg_sentiment, 3.0);
        let gov_norm = (contract_norm * 0.6 + reg_norm * 0.4).min(1.0);
        let search_norm = normalize_signal(*search_delta, 50.0);
        let patent_norm = normalize_signal(*patent_rate, 5.0);
        let supply_norm = normalize_signal(*import_delta, 30.0);
        let political_norm = normalize_signal(*lobby_delta, 100_000.0);

        // Weighted compound score
        let compound = (insider_norm * weights[0]
            + inst_norm * weights[1]
            + news_norm * weights[2]
            + gov_norm * weights[3]
            + search_norm * weights[4]
            + patent_norm * weights[5]
            + supply_norm * weights[6]
            + political_norm * weights[7])
            .max(0.0).min(1.0);

        // Convergence: 3+ signals > 0.3 AND source_diversity >= 3
        let positive = [insider_norm, inst_norm, news_norm, gov_norm,
            search_norm, patent_norm, supply_norm, political_norm]
            .iter().filter(|&&v| v > 0.3).count();
        // Convergence: 2+ signals > 0.3 AND diversity >= 2, OR very high compound score
        let convergence = (positive >= 2 && *src_diversity >= 2) || compound >= 0.40;

        if compound < 0.01 && !convergence {
            continue;
        }

        // Look up entity_id and ticker — prefer canonical entity for lookup
        let (eid, ticker) = {
            // Try canonical first
            let canonical: Option<(i64, Option<String>)> = conn.query_row(
                "SELECT ec.id, ec.ticker FROM entity_canonical ec WHERE ec.canonical_name = ?1",
                [topic], |row| Ok((row.get(0)?, row.get(1)?)),
            ).ok();

            if let Some((cid, ct)) = canonical {
                // Get first linked entity_id for the cross_signals table
                let eid: i64 = conn.query_row(
                    "SELECT id FROM entities WHERE canonical_id = ?1 LIMIT 1",
                    [cid], |row| row.get(0),
                ).unwrap_or(0);
                // Prefer canonical ticker, fall back to entity_tickers
                let ticker = ct.or_else(|| {
                    conn.query_row(
                        "SELECT et.ticker FROM entity_tickers et JOIN entities e ON e.id = et.entity_id WHERE e.canonical_id = ?1 LIMIT 1",
                        [cid], |row| row.get(0),
                    ).ok()
                });
                (eid, ticker)
            } else {
                // Fall back to direct name match
                let eid: i64 = conn.query_row(
                    "SELECT id FROM entities WHERE LOWER(name) = LOWER(?1) LIMIT 1",
                    [topic], |row| row.get(0),
                ).unwrap_or(0);
                let ticker: Option<String> = if eid > 0 {
                    conn.query_row("SELECT ticker FROM entity_tickers WHERE entity_id = ?1", [eid], |row| row.get(0)).ok()
                } else { None };
                (eid, ticker)
            }
        };

        conn.execute(
            "INSERT OR REPLACE INTO cross_signals (entity_id, ticker, compound_score,
                insider_signal, institutional_flow, news_momentum, government_signal,
                search_trend, patent_signal, supply_chain, political_signal,
                source_diversity, convergence_detected, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                eid, ticker, compound,
                insider_norm, inst_norm, news_norm, gov_norm,
                search_norm, patent_norm, supply_norm, political_norm,
                src_diversity, convergence as i32, today,
            ],
        ).ok();

        count += 1;
    }

    Ok(count)
}

/// Load calibrated weights from DB, or return defaults.
/// Matches the weight order: [insider, institutional, news, government, search, patent, supply_chain, political]
fn load_calibrated_weights(conn: &rusqlite::Connection) -> [f64; 8] {
    // [insider, institutional, news, government, search, patent, supply, political]
    // All 7 active dimensions (patent stays low — USPTO API in migration).
    // supply_chain uses FRED macro indicators as market-wide signal.
    let defaults = [0.22, 0.05, 0.22, 0.17, 0.05, 0.04, 0.03, 0.22];

    let json: Option<String> = conn.query_row(
        "SELECT value FROM user_profile WHERE key = 'calibrated_weights'",
        [], |row| row.get(0),
    ).ok();

    if let Some(json_str) = json {
        if let Ok(pairs) = serde_json::from_str::<Vec<(String, f64)>>(&json_str) {
            let mut w = defaults;
            for (key, val) in &pairs {
                match key.as_str() {
                    "insider_signal" => w[0] = *val,
                    "institutional_flow" => w[1] = *val,
                    "news_momentum" => w[2] = *val,
                    "government_signal" => w[3] = *val,
                    "search_trend" => w[4] = *val,
                    "patent_signal" => w[5] = *val,
                    "supply_chain" => w[6] = *val,
                    "political_signal" => w[7] = *val,
                    _ => {}
                }
            }
            return w;
        }
    }

    defaults
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
                        ents.push((name, "company", 0.0));
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
                if let Some(name) = meta.get("client").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "lobbying_disclosure", 0.0));
                    }
                }
                if let Some(name) = meta.get("registrant").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        ents.push((name, "company", 0.0));
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

/// Auto-execute paper trades when convergence signals are detected.
/// Only trades entities with tickers, not already held, with convergence_detected = true.
///
/// SAFETY GATE: this is hard-disabled by default. Both the entry path and the
/// scale-in path within this function only run when AUTO_TRADE_ENABLED=true is
/// set in the environment. Pulse is a news/intelligence app first — the trading
/// layer is dormant scaffolding that should not place real orders until a
/// 6-month auto-backtest history demonstrates a durable edge.
async fn auto_trade_on_convergence(db_path: &Path) -> anyhow::Result<usize> {
    // Hard kill switch. Default = OFF. Re-enable via `AUTO_TRADE_ENABLED=true`
    // in `.env` once the auto-backtest has shown a positive expectancy across
    // a meaningful window of resolved trades.
    let trading_enabled = std::env::var("AUTO_TRADE_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    if !trading_enabled {
        tracing::info!("Auto-trade: DISABLED (set AUTO_TRADE_ENABLED=true to re-enable)");
        return Ok(0);
    }

    let alpaca_key = match std::env::var("ALPACA_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("Auto-trade: skipping (ALPACA_API_KEY not set)");
            return Ok(0);
        }
    };
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY not set"))?;

    let conn = rusqlite::Connection::open(db_path)?;

    // Find convergence signals with tickers, not already in open trades.
    // cross_signals stores 1 row per (entity, day), so the same ticker can have
    // multiple historical rows — we want the freshest row per ticker only,
    // otherwise LIMIT N gets eaten by 5+ stale rows of the same name and the
    // system places one trade per day on whichever ticker has the most history.
    let mut stmt = conn.prepare(
        "WITH latest AS (
             SELECT cs.entity_id, cs.ticker, cs.compound_score,
                    cs.insider_signal, cs.institutional_flow, cs.news_momentum,
                    cs.government_signal, cs.search_trend, cs.patent_signal,
                    cs.supply_chain, cs.political_signal,
                    cs.computed_at,
                    ROW_NUMBER() OVER (PARTITION BY cs.ticker ORDER BY cs.computed_at DESC, cs.compound_score DESC) AS rn
             FROM cross_signals cs
             WHERE cs.convergence_detected = 1
               AND cs.ticker IS NOT NULL
               AND cs.compound_score > 0.3
               AND cs.computed_at >= date('now', '-1 day')
         )
         SELECT l.entity_id, l.ticker, l.compound_score, e.name,
                l.insider_signal, l.institutional_flow, l.news_momentum,
                l.government_signal, l.search_trend, l.patent_signal,
                l.supply_chain, l.political_signal,
                COALESCE((SELECT s.insider_buy_volume FROM signals s
                          WHERE s.topic = LOWER(e.name)
                          ORDER BY s.updated_at DESC LIMIT 1), 0) AS insider_raw
         FROM latest l
         JOIN entities e ON e.id = l.entity_id
         WHERE l.rn = 1
           AND l.ticker NOT IN (
               SELECT ticker FROM paper_trades WHERE status = 'open'
           )
         ORDER BY l.compound_score DESC
         LIMIT 5"
    )?;

    #[allow(clippy::type_complexity)]
    let candidates: Vec<(i64, String, f64, String, f64, f64, f64, f64, f64, f64, f64, f64, f64)> = stmt
        .query_map([], |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
            row.get::<_, f64>(4).unwrap_or(0.0), row.get::<_, f64>(5).unwrap_or(0.0),
            row.get::<_, f64>(6).unwrap_or(0.0), row.get::<_, f64>(7).unwrap_or(0.0),
            row.get::<_, f64>(8).unwrap_or(0.0), row.get::<_, f64>(9).unwrap_or(0.0),
            row.get::<_, f64>(10).unwrap_or(0.0), row.get::<_, f64>(11).unwrap_or(0.0),
            row.get::<_, f64>(12).unwrap_or(0.0),
        )))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() {
        return Ok(0);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // Get account buying power
    let account: serde_json::Value = client
        .get("https://paper-api.alpaca.markets/v2/account")
        .header("APCA-API-KEY-ID", &alpaca_key)
        .header("APCA-API-SECRET-KEY", &alpaca_secret)
        .send()
        .await?
        .json()
        .await?;

    let buying_power: f64 = account.get("buying_power")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    // portfolio_value is the equity-based denominator for concentration limits.
    // Buying power can swing with leverage and pending orders; portfolio_value
    // is the stable "size of the pie" we want to cap each name against.
    let portfolio_value: f64 = account.get("portfolio_value")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if buying_power < 100.0 {
        tracing::info!("Auto-trade: insufficient buying power (${:.2})", buying_power);
        return Ok(0);
    }

    // Hard cap on per-ticker exposure. Without this, a bug or repeated signals
    // can stack the same name into 20%+ of the portfolio (META did exactly this
    // — 5x duplicate fills put 23% of equity into a single position before any
    // human review). 5% is restrictive enough to *bite* against the existing
    // $10k ENTRY_CAP at $100k portfolio sizes.
    const MAX_PER_TICKER_PCT: f64 = 0.05;
    let max_per_ticker_dollars = if portfolio_value > 0.0 {
        portfolio_value * MAX_PER_TICKER_PCT
    } else {
        f64::INFINITY // No portfolio value reported — fall back to ENTRY_CAP only.
    };

    let now = chrono::Local::now();
    let entry_datetime = now.format("%Y-%m-%dT%H:%M:%S").to_string();
    let mut traded = 0;

    // Veto threshold: heavy net insider selling. The $1M scale matches the
    // positive-side normalize_signal scale, so -$1M is roughly the mirror image
    // of what would have shown up as a meaningful BUY signal.
    const INSIDER_VETO_THRESHOLD: f64 = -1_000_000.0;

    for (entity_id, ticker, score, name, insider, inst, news, gov, search, patent, supply, political, insider_raw) in &candidates {
        if *insider_raw < INSIDER_VETO_THRESHOLD {
            tracing::info!(
                "Auto-trade: vetoing {} ({}) — heavy insider selling (net ${:.0})",
                name, ticker, insider_raw
            );
            continue;
        }

        let notional = match crate::position_sizing::entry_notional(buying_power, *score) {
            Some(n) => n,
            None => {
                tracing::info!("Auto-trade: skipping {} — buying power below entry floor", ticker);
                continue;
            }
        };

        // Concentration check: ask Alpaca for the current market value of any
        // existing position in this ticker. If proposed notional + existing
        // exposure would exceed MAX_PER_TICKER_PCT of portfolio_value, skip.
        let existing_exposure: f64 = match client
            .get(format!("https://paper-api.alpaca.markets/v2/positions/{}", ticker))
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                r.json::<serde_json::Value>().await.ok()
                    .and_then(|v| v.get("market_value")
                        .and_then(|m| m.as_str())
                        .and_then(|s| s.parse::<f64>().ok()))
                    .unwrap_or(0.0)
            }
            // 404 = no existing position, treat as 0 exposure. Other errors:
            // be conservative and treat as 0 (the trade still gets capped by
            // notional itself, just won't account for hidden exposure).
            _ => 0.0,
        };

        if existing_exposure + notional > max_per_ticker_dollars {
            tracing::warn!(
                "Auto-trade: blocking {} ({}) — would push exposure to ${:.0} (existing ${:.0} + ${:.0}), cap is ${:.0} ({:.0}% of ${:.0} portfolio)",
                name, ticker, existing_exposure + notional, existing_exposure, notional,
                max_per_ticker_dollars, MAX_PER_TICKER_PCT * 100.0, portfolio_value
            );
            continue;
        }

        tracing::info!("Auto-trade: {} ({}) — score {:.2}, notional ${:.2}", name, ticker, score, notional);

        let order = serde_json::json!({
            "symbol": ticker,
            "notional": format!("{:.2}", notional),
            "side": "buy",
            "type": "market",
            "time_in_force": "day"
        });

        let resp = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await?;

        if resp.status().is_success() {
            let order_resp: serde_json::Value = resp.json().await?;
            let order_id = order_resp.get("id").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

            // Poll for fill — market orders fill within seconds
            let mut filled_price = 0.0;
            let mut filled_qty = 0.0;
            let mut fill_time = entry_datetime.clone();
            for attempt in 0..5 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if let Ok(check_resp) = client
                    .get(format!("https://paper-api.alpaca.markets/v2/orders/{}", order_id))
                    .header("APCA-API-KEY-ID", &alpaca_key)
                    .header("APCA-API-SECRET-KEY", &alpaca_secret)
                    .send().await
                {
                    if let Ok(check) = check_resp.json::<serde_json::Value>().await {
                        let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
                        if status == "filled" {
                            filled_price = check.get("filled_avg_price")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0.0);
                            filled_qty = check.get("filled_qty")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0.0);
                            if let Some(ft) = check.get("filled_at").and_then(|v| v.as_str()) {
                                // Parse ISO 8601 to local datetime
                                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ft) {
                                    fill_time = dt.with_timezone(&chrono::Local).format("%Y-%m-%dT%H:%M:%S").to_string();
                                }
                            }
                            break;
                        }
                        if attempt == 4 {
                            tracing::warn!("Auto-trade: order {} not filled after 5s (status: {})", order_id, status);
                        }
                    }
                }
            }

            if filled_price <= 0.0 {
                // Order submitted but fill price unknown — use latest known price as estimate
                // to avoid ghost positions (Alpaca has it, but we don't track it)
                filled_price = conn.query_row(
                    "SELECT close FROM entity_prices WHERE ticker = ?1 ORDER BY date DESC LIMIT 1",
                    [ticker.as_str()], |row| row.get(0),
                ).unwrap_or(0.0);
                tracing::warn!("Auto-trade: fill price unknown for {} ({}), using estimate ${:.2}", ticker, order_id, filled_price);
                if filled_price <= 0.0 {
                    tracing::warn!("Auto-trade: no price data for {}, cancelling order {}", ticker, order_id);
                    client.delete(format!("https://paper-api.alpaca.markets/v2/orders/{}", order_id))
                        .header("APCA-API-KEY-ID", &alpaca_key)
                        .header("APCA-API-SECRET-KEY", &alpaca_secret)
                        .send().await.ok();
                    continue;
                }
            }

            // Capture the recent stories that drove this signal so the trade
            // journal can show "we bought because of these specific headlines".
            // Limit to the 8 most recent / most-cited stories per entity.
            let story_refs: Vec<(i64, String, String)> = {
                let mut s = match conn.prepare(
                    "SELECT s.id, s.headline, s.source_name
                     FROM entity_mentions em
                     JOIN stories s ON s.id = em.story_id
                     WHERE em.entity_id = ?1
                       AND em.mentioned_at >= date('now', '-14 days')
                     ORDER BY em.mentioned_at DESC, s.importance_score DESC
                     LIMIT 8"
                ) {
                    Ok(stmt) => stmt,
                    Err(_) => continue,
                };
                s.query_map([entity_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default()
            };

            // Build signal_profile JSON matching calibration keys.
            // Now includes `stories[]` so the trade journal can reference the
            // specific headlines that drove the entry decision.
            let signal_profile = serde_json::json!({
                "insider": insider,
                "institutional": inst,
                "news": news,
                "government": gov,
                "search": search,
                "patent": patent,
                "supply_chain": supply,
                "political": political,
                "stories": story_refs.iter().map(|(id, head, src)| {
                    serde_json::json!({"id": id, "headline": head, "source": src})
                }).collect::<Vec<_>>(),
            });

            // Re-check: the candidate query filtered open positions at fetch time,
            // but Alpaca may have filled this order while another iteration was
            // processing the same ticker. Refuse the second insert.
            let already_open: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM paper_trades WHERE ticker = ?1 AND status = 'open')",
                    [ticker.as_str()],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if already_open {
                tracing::warn!("Auto-trade: skipping duplicate insert for {} — already open", ticker);
                continue;
            }

            // Record in paper_trades with position management columns
            if let Err(e) = conn.execute(
                "INSERT INTO paper_trades (entity_id, ticker, direction, entry_price, entry_date, position_size, confidence, signal_profile, alpaca_order_id, status, high_water_mark, original_compound_score)
                 VALUES (?1, ?2, 'long', ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?3, ?6)",
                rusqlite::params![entity_id, ticker, filled_price, fill_time, notional, score, signal_profile.to_string(), order_id],
            ) {
                tracing::warn!("Auto-trade: failed to record trade for {}: {}", ticker, e);
            }

            tracing::info!("Auto-trade: placed order {} for {} (${:.2} @ ${:.2}, qty {:.4})", order_id, ticker, notional, filled_price, filled_qty);
            traded += 1;
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!("Auto-trade: order failed for {} — {} {}", ticker, status, body);
        }

        // Rate limit
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // Scale-in: check if existing positions have strengthening signals
    let scale_in_candidates: Vec<(i64, String, f64, f64)> = conn.prepare(
        "SELECT pt.id, pt.ticker, pt.original_compound_score, cs.compound_score
         FROM paper_trades pt
         JOIN cross_signals cs ON cs.ticker = pt.ticker
         WHERE pt.status = 'open'
           AND pt.pnl_pct > 0.0
           AND COALESCE(pt.scale_in_count, 0) < 1
           AND cs.compound_score > COALESCE(pt.original_compound_score, pt.confidence) * 1.2
           AND cs.convergence_detected = 1
         ORDER BY cs.compound_score DESC
         LIMIT 3"
    ).ok()
    .map(|mut stmt| {
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default()
    })
    .unwrap_or_default();

    for (trade_id, ticker, _old_score, new_score) in &scale_in_candidates {
        let scale_notional = match crate::position_sizing::scale_in_notional(buying_power) {
            Some(n) => n,
            None => {
                tracing::info!("Scale-in: skipping {} — buying power below floor", ticker);
                continue;
            }
        };

        tracing::info!("Scale-in: {} — score increased to {:.2}, adding ${:.2}", ticker, new_score, scale_notional);

        let order = serde_json::json!({
            "symbol": ticker,
            "notional": format!("{:.2}", scale_notional),
            "side": "buy",
            "type": "market",
            "time_in_force": "day"
        });

        let resp = client
            .post("https://paper-api.alpaca.markets/v2/orders")
            .header("APCA-API-KEY-ID", &alpaca_key)
            .header("APCA-API-SECRET-KEY", &alpaca_secret)
            .json(&order)
            .send()
            .await?;

        if resp.status().is_success() {
            conn.execute(
                "UPDATE paper_trades SET scale_in_count = COALESCE(scale_in_count, 0) + 1,
                 position_size = position_size + ?1 WHERE id = ?2",
                rusqlite::params![scale_notional, trade_id],
            ).ok();
            tracing::info!("Scale-in: added ${:.2} to {} position", scale_notional, ticker);
            traded += 1;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(traded)
}

/// Snapshot the live portfolio state into the `portfolio_snapshots` table.
///
/// Runs once per day (UNIQUE constraint on `date` column makes it idempotent
/// via INSERT OR REPLACE). Pulls equity from Alpaca rather than aggregating
/// `paper_trades.pnl` because the DB can drift from Alpaca and we want the
/// snapshot to reflect ground truth, not our internal accounting.
///
/// MUST run AFTER Phase 14 (calibration) so any closes from this morning's
/// signal-decay or trailing-stop checks are reflected in the recorded equity.
async fn snapshot_portfolio(db_path: &Path) -> anyhow::Result<bool> {
    let alpaca_key = match std::env::var("ALPACA_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            tracing::info!("Snapshot: skipping (ALPACA_API_KEY not set)");
            return Ok(false);
        }
    };
    let alpaca_secret = std::env::var("ALPACA_SECRET_KEY")
        .map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY not set"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let account: serde_json::Value = client
        .get("https://paper-api.alpaca.markets/v2/account")
        .header("APCA-API-KEY-ID", &alpaca_key)
        .header("APCA-API-SECRET-KEY", &alpaca_secret)
        .send()
        .await?
        .json()
        .await?;

    let portfolio_value: f64 = account.get("portfolio_value")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if portfolio_value <= 0.0 {
        tracing::warn!("Snapshot: Alpaca returned zero portfolio_value, skipping");
        return Ok(false);
    }

    // Initial equity is whatever Alpaca says the account started with.
    // Alpaca paper accounts default to $100k; if the user has a different
    // baseline, the equity column would tell us — fall back to 100k.
    const INITIAL_EQUITY: f64 = 100_000.0;
    let total_pnl = portfolio_value - INITIAL_EQUITY;
    let total_pnl_pct = (total_pnl / INITIAL_EQUITY) * 100.0;

    let conn = rusqlite::Connection::open(db_path)?;

    let open_positions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM paper_trades WHERE status = 'open'",
        [],
        |row| row.get(0),
    ).unwrap_or(0);

    // High-water mark = max(previous HWM, today's portfolio_value).
    let prev_hwm: f64 = conn.query_row(
        "SELECT MAX(high_water_mark) FROM portfolio_snapshots",
        [],
        |row| row.get::<_, Option<f64>>(0),
    ).unwrap_or(None).unwrap_or(INITIAL_EQUITY);
    let high_water_mark = prev_hwm.max(portfolio_value);
    let drawdown_pct = if high_water_mark > 0.0 {
        ((high_water_mark - portfolio_value) / high_water_mark) * 100.0
    } else {
        0.0
    };

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    conn.execute(
        "INSERT INTO portfolio_snapshots
            (date, total_value, total_pnl, total_pnl_pct, open_positions, high_water_mark, drawdown_pct)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(date) DO UPDATE SET
             total_value = excluded.total_value,
             total_pnl = excluded.total_pnl,
             total_pnl_pct = excluded.total_pnl_pct,
             open_positions = excluded.open_positions,
             high_water_mark = excluded.high_water_mark,
             drawdown_pct = excluded.drawdown_pct",
        rusqlite::params![today, portfolio_value, total_pnl, total_pnl_pct, open_positions, high_water_mark, drawdown_pct],
    )?;

    tracing::info!(
        "Snapshot: ${:.0} equity, ${:+.0} PnL ({:+.2}%), {} open, HWM ${:.0}, DD {:.2}%",
        portfolio_value, total_pnl, total_pnl_pct, open_positions, high_water_mark, drawdown_pct
    );

    Ok(true)
}

/// Enrich stored Form 4 stories that are missing transaction data.
/// Downloads the actual XML from EDGAR and parses buy/sell/shares/price.
async fn enrich_form4_stories(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    // Find Form 4 stories missing transaction_code (not yet enriched)
    let mut stmt = conn.prepare(
        "SELECT id, financial_metadata FROM stories
         WHERE source_type = 'financial'
           AND source_name LIKE '%EDGAR 4%'
           AND json_valid(financial_metadata)
           AND json_extract(financial_metadata, '$.transaction_code') IS NULL
           AND created_at >= datetime('now', '-7 days')
         LIMIT 30"
    )?;

    let candidates: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() { return Ok(0); }

    let mut enriched = 0;

    for (story_id, metadata_str) in &candidates {
        let meta: serde_json::Value = serde_json::from_str(metadata_str)?;
        let cik = meta.get("cik").and_then(|v| v.as_str()).unwrap_or("");
        let accession = meta.get("accession_number").and_then(|v| v.as_str()).unwrap_or("");

        if cik.is_empty() || accession.is_empty() { continue; }

        let cik_clean = cik.trim_start_matches('0');
        let accession_nd = accession.replace('-', "");

        // Try to find the Form 4 XML via index page
        let index_url = format!("https://www.sec.gov/Archives/edgar/data/{}/{}/", cik_clean, accession_nd);

        let index_resp = client
            .get(&index_url)
            .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
            .send()
            .await;

        let xml = if let Ok(resp) = index_resp {
            let status = resp.status();
            if status.is_success() {
                let html = resp.text().await.unwrap_or_default();
                let mut found_xml = None;
                for part in html.split("href=\"") {
                    let href = part.split('"').next().unwrap_or("");
                    if href.ends_with(".xml") && !href.contains("R1") && !href.contains("R2") && !href.contains("index") {
                        let full_url = if href.starts_with("/") {
                            format!("https://www.sec.gov{}", href)
                        } else {
                            format!("{}{}", index_url, href)
                        };
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        if let Ok(xml_resp) = client.get(&full_url)
                            .header("User-Agent", "Pulse/1.0 (pulse-app@example.com)")
                            .send().await
                        {
                            if xml_resp.status().is_success() {
                                let text = xml_resp.text().await.unwrap_or_default();
                                if text.contains("<ownershipDocument") {
                                    found_xml = Some(text);
                                    break;
                                }
                            }
                        }
                    }
                }
                found_xml
            } else {
                if status.as_u16() == 429 {
                    tracing::warn!("Form 4 enrichment: SEC rate limit (429), stopping early after {} enriched", enriched);
                    break;
                }
                None
            }
        } else { None };

        let Some(xml) = xml else {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            continue;
        };

        // Parse using the same logic as edgar.rs
        let txn_code = extract_xml_value_simple(&xml, "transactionCode").unwrap_or_else(|| "?".to_string());
        let shares = extract_nested_val(&xml, "transactionShares").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        let price = extract_nested_val(&xml, "transactionPricePerShare").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        let total_value = shares * price;
        let owner_name = extract_xml_value_simple(&xml, "rptOwnerName").unwrap_or_default();
        let is_officer = xml.contains("<isOfficer>1</isOfficer>") || xml.contains("<isOfficer>true</isOfficer>");
        let is_director = xml.contains("<isDirector>1</isDirector>") || xml.contains("<isDirector>true</isDirector>");
        let officer_title = extract_xml_value_simple(&xml, "officerTitle").unwrap_or_default();
        let post_shares = extract_nested_val(&xml, "sharesOwnedFollowingTransaction").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);

        // Classify
        let (classification, signal_weight) = classify_form4_trade(&txn_code, is_officer, is_director, total_value, shares, post_shares);

        // Update metadata
        let mut updated: serde_json::Value = serde_json::from_str(metadata_str)?;
        updated["transaction_code"] = serde_json::json!(txn_code);
        updated["shares"] = serde_json::json!(shares);
        updated["price_per_share"] = serde_json::json!(price);
        updated["total_value"] = serde_json::json!(total_value);
        updated["owner_name"] = serde_json::json!(owner_name);
        updated["is_officer"] = serde_json::json!(is_officer);
        updated["is_director"] = serde_json::json!(is_director);
        updated["officer_title"] = serde_json::json!(officer_title);
        updated["post_transaction_shares"] = serde_json::json!(post_shares);
        updated["trade_classification"] = serde_json::json!(classification);
        updated["signal_weight"] = serde_json::json!(signal_weight);

        conn.execute(
            "UPDATE stories SET financial_metadata = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&updated)?, story_id],
        ).ok();

        enriched += 1;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }

    Ok(enriched)
}

fn extract_xml_value_simple(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

fn extract_nested_val(xml: &str, parent_tag: &str) -> Option<String> {
    let open = format!("<{}>", parent_tag);
    let close = format!("</{}>", parent_tag);
    let start = xml.find(&open)?;
    let end = xml.find(&close).unwrap_or(start + 200).min(start + 200);
    let section = &xml[start..end];
    let val_start = section.find("<value>")? + 7;
    let val_end = section[val_start..].find("</value>")? + val_start;
    Some(section[val_start..val_end].trim().to_string())
}

fn classify_form4_trade(code: &str, is_officer: bool, is_director: bool, total_value: f64, shares: f64, post_shares: f64) -> (&'static str, f64) {
    match code {
        "P" => {
            if is_officer && total_value >= 100_000.0 { ("strong_buy", 1.0) }
            else if is_officer && total_value >= 25_000.0 { ("moderate_buy", 0.7) }
            else if is_director && total_value >= 50_000.0 { ("moderate_buy", 0.6) }
            else if total_value >= 10_000.0 { ("small_buy", 0.3) }
            else { ("minimal_buy", 0.1) }
        }
        "S" => {
            if is_officer && post_shares > 0.0 && shares > 0.0 {
                let pct = shares / (post_shares + shares);
                if pct > 0.20 { return ("informative_sale", -0.3); }
            }
            ("routine_sale", 0.0)
        }
        "A" => ("award", 0.0),
        "M" => ("option_exercise", 0.0),
        "G" => ("gift", 0.0),
        "F" => ("tax_withholding", 0.0),
        _ => ("unknown", 0.0),
    }
}

/// Classify ambiguous 8-K filings (Item 8.01 "other_event") using Claude Haiku.
/// Only processes recent unclassified 8-Ks. ~$0.0005 per call.
async fn classify_ambiguous_8ks(db_path: &Path) -> anyhow::Result<usize> {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return Ok(0),
    };

    let conn = rusqlite::Connection::open(db_path)?;

    // Find 8-K stories with "other_event" classification and content_preview
    let mut stmt = conn.prepare(
        "SELECT id, financial_metadata FROM stories
         WHERE source_type = 'financial'
           AND source_name LIKE '%EDGAR 8-K%'
           AND json_valid(financial_metadata)
           AND json_extract(financial_metadata, '$.event_classification') = 'other_event'
           AND json_extract(financial_metadata, '$.content_preview') IS NOT NULL
           AND json_extract(financial_metadata, '$.llm_classification') IS NULL
           AND created_at >= datetime('now', '-3 days')
         LIMIT 15"
    )?;

    let candidates: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() { return Ok(0); }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut classified = 0;

    for (story_id, metadata_str) in &candidates {
        let meta: serde_json::Value = serde_json::from_str(metadata_str)?;
        let preview = meta.get("content_preview").and_then(|v| v.as_str()).unwrap_or("");
        let entity = meta.get("entity_name").and_then(|v| v.as_str()).unwrap_or("Unknown");

        if preview.len() < 20 { continue; }

        let body = serde_json::json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 150,
            "system": "Classify this SEC 8-K filing into exactly one category. Return only valid JSON.\n\nCategories:\n- earnings_surprise: unexpected financial results\n- acquisition: M&A activity, merger, purchase\n- partnership: strategic alliance, joint venture\n- restructuring: layoffs, cost cuts, exit activities\n- product_launch: new product, service, or technology\n- regulatory: FDA approval, compliance action, government\n- executive_change: C-suite departure or hire\n- bankruptcy_risk: going concern, debt default\n- shareholder_action: buyback, dividend, activist\n- capital_raise: debt offering, equity raise, IPO\n- litigation: lawsuit, settlement, legal action\n- other: none of the above\n\nReturn: {\"category\": \"...\", \"severity\": 0.0-1.0, \"summary\": \"one sentence\"}",
            "messages": [{"role": "user", "content": format!("Company: {}\n\n8-K content:\n{}", entity, &preview[..preview.len().min(1500)])}]
        });

        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() { continue; }

        let response: serde_json::Value = resp.json().await?;
        let text = response["content"][0]["text"].as_str().unwrap_or("{}");

        // Parse the JSON response
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') { &text[start..=end] } else { text }
        } else { text };

        if let Ok(result) = serde_json::from_str::<serde_json::Value>(json_str) {
            let category = result.get("category").and_then(|v| v.as_str()).unwrap_or("other");
            let severity = result.get("severity").and_then(|v| v.as_f64()).unwrap_or(0.3);
            let summary = result.get("summary").and_then(|v| v.as_str()).unwrap_or("");

            // Map to actual severity sign (some categories are negative)
            let signed_severity = match category {
                "restructuring" | "bankruptcy_risk" | "litigation" => -severity,
                _ => severity,
            };

            // Update the financial_metadata JSON with LLM classification
            let mut updated_meta: serde_json::Value = serde_json::from_str(metadata_str)?;
            updated_meta["llm_classification"] = serde_json::json!(category);
            updated_meta["event_classification"] = serde_json::json!(category);
            updated_meta["event_severity"] = serde_json::json!(signed_severity);
            updated_meta["llm_summary"] = serde_json::json!(summary);

            conn.execute(
                "UPDATE stories SET financial_metadata = ?1 WHERE id = ?2",
                rusqlite::params![serde_json::to_string(&updated_meta)?, story_id],
            ).ok();

            classified += 1;
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    if classified > 0 {
        log_usage(db_path, "anthropic", "claude-haiku", "8k_classification",
            (classified * 500) as i64, (classified * 50) as i64);
    }

    Ok(classified)
}

/// Resolve entities to canonical records.
/// Merges duplicates like "NVIDIA Corp", "Nvidia", "NVIDIA CORPORATION" into one canonical entity.
/// Strategy: CIK match → ticker match → normalized name match → suffix-stripped match.
fn resolve_entities(db_path: &Path) -> anyhow::Result<usize> {
    let conn = rusqlite::Connection::open(db_path)?;
    crate::db::run_migrations(&conn)?;

    // Check if entity_canonical table exists
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='entity_canonical')",
        [], |row| row.get(0),
    ).unwrap_or(false);
    if !table_exists { return Ok(0); }

    // Check if canonical_id column exists on entities
    let col_exists: bool = {
        let mut stmt = conn.prepare("PRAGMA table_info(entities)")?;
        stmt.query_map([], |row| row.get::<_, String>(1))?
            .any(|r| r.as_deref() == Ok("canonical_id"))
    };
    if !col_exists { return Ok(0); }

    // Get all entities without a canonical_id
    let unresolved: Vec<(i64, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, e.name, e.entity_type, et.ticker
             FROM entities e
             LEFT JOIN entity_tickers et ON et.entity_id = e.id
             WHERE e.canonical_id IS NULL
             ORDER BY e.mention_count DESC
             LIMIT 500"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    if unresolved.is_empty() { return Ok(0); }

    // Load existing canonical entities for matching
    let existing_canonicals: Vec<(i64, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, canonical_name, ticker, cik FROM entity_canonical"
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    // Common suffixes to strip for matching
    let suffixes = [
        " inc", " inc.", " corp", " corp.", " corporation", " ltd", " ltd.",
        " llc", " co", " co.", " plc", " sa", " ag", " se", " nv",
        " holdings", " group", " technologies", " technology",
        " international", " solutions", " systems", " enterprises",
        " company", " partners", " capital", " industries",
    ];

    let normalize = |name: &str| -> String {
        let mut n = name.to_lowercase().trim().to_string();
        // Strip CIK suffix like "(CIK 0001234567)"
        if let Some(idx) = n.find("(cik") {
            n = n[..idx].trim().to_string();
        }
        // Strip common suffixes
        for suffix in &suffixes {
            n = n.trim_end_matches(suffix).to_string();
        }
        n.trim().to_string()
    };

    let mut resolved = 0usize;

    for (entity_id, name, entity_type, ticker) in &unresolved {
        let name_norm = normalize(name);
        if name_norm.len() < 2 { continue; }

        // Skip non-company-like entities (topics, generic terms)
        if matches!(entity_type.as_str(), "topic" | "regulation") { continue; }

        let mut matched_canonical_id: Option<i64> = None;

        // 1. Try CIK match (highest confidence)
        let entity_cik: Option<String> = conn.query_row(
            "SELECT cik FROM entity_tickers WHERE entity_id = ?1 AND cik IS NOT NULL AND cik != ''",
            [entity_id], |row| row.get(0),
        ).ok();

        if let Some(ref cik) = entity_cik {
            for (cid, _, _, c_cik) in &existing_canonicals {
                if c_cik.as_deref() == Some(cik) {
                    matched_canonical_id = Some(*cid);
                    break;
                }
            }
        }

        // 2. Try ticker match
        if matched_canonical_id.is_none() {
            if let Some(t) = ticker {
                for (cid, _, c_ticker, _) in &existing_canonicals {
                    if c_ticker.as_deref() == Some(t.as_str()) {
                        matched_canonical_id = Some(*cid);
                        break;
                    }
                }
            }
        }

        // 3. Try normalized name match against existing canonicals
        if matched_canonical_id.is_none() {
            for (cid, c_name, _, _) in &existing_canonicals {
                let c_norm = normalize(c_name);
                if c_norm == name_norm {
                    matched_canonical_id = Some(*cid);
                    break;
                }
                // Also try contains for short names (>= 5 chars)
                if name_norm.len() >= 5 && (c_norm.contains(&name_norm) || name_norm.contains(&c_norm)) {
                    matched_canonical_id = Some(*cid);
                    break;
                }
            }
        }

        // 4. No match — create a new canonical entity
        if matched_canonical_id.is_none() {
            // Use the best name we have
            let canon_name = name.trim().to_string();
            conn.execute(
                "INSERT OR IGNORE INTO entity_canonical (canonical_name, ticker, cik, sector, entity_type)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    canon_name,
                    ticker,
                    entity_cik,
                    conn.query_row("SELECT sector FROM entities WHERE id = ?1", [entity_id], |row| row.get::<_, Option<String>>(0)).ok().flatten(),
                    if matches!(entity_type.as_str(), "company" | "insider_trade" | "contract_award" | "patent_cluster" | "material_event" | "private_placement") { "company" } else { entity_type.as_str() },
                ],
            ).ok();

            matched_canonical_id = conn.query_row(
                "SELECT id FROM entity_canonical WHERE canonical_name = ?1",
                [&canon_name], |row| row.get(0),
            ).ok();
        }

        // Link entity to canonical
        if let Some(cid) = matched_canonical_id {
            conn.execute(
                "UPDATE entities SET canonical_id = ?1 WHERE id = ?2",
                rusqlite::params![cid, entity_id],
            ).ok();

            // Also populate entity_aliases
            let alias_norm = name.to_lowercase().trim().to_string();
            conn.execute(
                "INSERT OR IGNORE INTO entity_aliases (entity_id, alias, alias_type)
                 VALUES (?1, ?2, 'alternate_name')",
                rusqlite::params![entity_id, alias_norm],
            ).ok();

            // If we have a ticker, add it as an alias too
            if let Some(t) = ticker {
                conn.execute(
                    "INSERT OR IGNORE INTO entity_aliases (entity_id, alias, alias_type)
                     VALUES (?1, ?2, 'ticker')",
                    rusqlite::params![entity_id, t],
                ).ok();
            }

            resolved += 1;
        }
    }

    // Update canonical entities with best available ticker/CIK from linked entities
    conn.execute_batch(
        "UPDATE entity_canonical SET ticker = (
            SELECT et.ticker FROM entity_tickers et
            JOIN entities e ON e.id = et.entity_id
            WHERE e.canonical_id = entity_canonical.id
            LIMIT 1
        ) WHERE ticker IS NULL AND id IN (
            SELECT DISTINCT e.canonical_id FROM entities e
            JOIN entity_tickers et ON et.entity_id = e.id
            WHERE e.canonical_id IS NOT NULL
        )"
    ).ok();

    Ok(resolved)
}

fn send_notification(story_count: usize) -> anyhow::Result<()> {
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display notification "Your daily briefing is ready. {} stories across 4 sectors." with title "Pulse" sound name "Glass""#,
            story_count
        ))
        .spawn()?;
    Ok(())
}
