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

pub async fn run(db_path: &Path) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    let mut progress = ProgressWriter::new(db_path);

    // Phase 1: Collect from all sources
    progress.start_stage(1);
    tracing::info!("Phase 1: Collecting from sources...");
    let all_articles = sources::collect_all().await?;
    let raw_articles: Vec<_> = all_articles.into_iter()
        .filter(|a| !a.sector.starts_with("freedom_"))
        .collect();
    tracing::info!("Collected {} raw articles (excluding freedom sources)", raw_articles.len());

    // Phase 2: Deduplicate
    progress.start_stage(2);
    tracing::info!("Phase 2: Deduplicating...");
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
    let unique_articles = crate::dedup::deduplicate_with_history(raw_articles, historical_hashes, historical_titles);
    tracing::info!("{} articles after dedup", unique_articles.len());

    // Phase 2.5: Pre-curate — pick the best ~90 articles BEFORE expensive summarization
    let articles_to_summarize = if unique_articles.len() > 100 {
        tracing::info!("Pre-curating: selecting best articles from {} candidates...", unique_articles.len());
        let api_key = std::env::var("GROQ_API_KEY")
            .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
        let client = crate::claude::client::GroqClient::new(&api_key);
        match client.pre_curate(&unique_articles).await {
            Ok(indices) => {
                let curated: Vec<_> = indices.into_iter()
                    .filter_map(|i| unique_articles.get(i).cloned())
                    .collect();
                log_usage(db_path, "groq", "llama-3.3-70b-versatile", "pre_curate",
                    (unique_articles.len() * 30) as i64, 500);
                tracing::info!("Pre-curated to {} articles (saved {} summarization calls)",
                    curated.len(), unique_articles.len() - curated.len());
                curated
            }
            Err(e) => {
                tracing::warn!("Pre-curation failed (non-fatal), summarizing all: {}", e);
                unique_articles
            }
        }
    } else {
        tracing::info!("Skipping pre-curation ({} articles, threshold 100)", unique_articles.len());
        unique_articles
    };

    // Phase 3: Summarize pre-curated articles (not ALL articles)
    progress.start_stage(3);
    tracing::info!("Phase 3: Summarizing {} stories...", articles_to_summarize.len());
    let summaries = crate::claude::summarize_stories(&articles_to_summarize, Some(&progress)).await?;
    let sum_count = summaries.len() as i64;
    log_usage(db_path, "groq", "llama-3.1-8b-instant", "summarize", sum_count * 500, sum_count * 300);

    if summaries.is_empty() {
        anyhow::bail!("No stories could be summarized — all API calls failed. Aborting to avoid storing empty briefing.");
    }

    // Phase 4: Cross-sector analysis
    progress.start_stage(4);
    tracing::info!("Phase 4: Cross-sector analysis...");
    let analysis = crate::claude::analyze_cross_sector(&summaries).await?;
    log_usage(db_path, "groq", "llama-3.3-70b-versatile", "analyze", 8000, 2000);

    // Phase 5: Executive summary (non-fatal)
    progress.start_stage(5);
    tracing::info!("Phase 5: Generating executive summary...");
    let executive_summary = match generate_executive_summary(&analysis).await {
        Ok(s) => {
            tracing::info!("Executive summary: {} chars", s.len());
            log_usage(db_path, "groq", "llama-3.1-8b-instant", "executive_summary", 2000, 300);
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

    // Phase 7: Embeddings (non-fatal)
    progress.start_stage(7);
    tracing::info!("Phase 7: Generating embeddings...");
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

    // Phase 8: Write to database
    progress.start_stage(8);
    tracing::info!("Phase 8: Writing to database...");
    write_to_db(db_path, &analysis, embeddings.as_deref(), prefixes.as_deref(), executive_summary.as_deref())?;

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

    // Phase 10: Deep summaries for top stories (non-fatal)
    progress.start_stage(10);
    tracing::info!("Phase 10: Generating deep summaries for top stories...");
    match generate_deep_summaries(db_path, &analysis).await {
        Ok(count) => tracing::info!("Generated {} deep summaries", count),
        Err(e) => tracing::warn!("Deep summary generation failed (non-fatal): {}", e),
    }

    // Done
    progress.finish();
    send_notification(analysis.curated_stories.len())?;

    let duration = start.elapsed();
    tracing::info!("Pipeline complete in {:.1}s", duration.as_secs_f64());

    Ok(())
}

async fn generate_freedoms_summary(curated: &[(&str, &crate::claude::SummarizedStory)]) -> anyhow::Result<String> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = crate::claude::client::GroqClient::new(&api_key);

    let mut input = String::new();
    for (freedom, story) in curated {
        input.push_str(&format!("[{}] {} — {}\n", freedom, story.headline, story.summary.chars().take(80).collect::<String>()));
    }

    let system = "You write a brief executive summary of a Four Freedoms daily briefing covering Time, Financial, Location, and Health freedom stories. The reader is a tech founder optimizing for personal freedom.\n\nWrite exactly 2-4 sentences covering the most actionable highlights across all four freedoms. Be specific — name tools, companies, trends. No preamble.";

    client.call_text("llama-3.1-8b-instant", system, &input, 250).await
}

async fn generate_executive_summary(analysis: &crate::claude::AnalysisResult) -> anyhow::Result<String> {
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = crate::claude::client::GroqClient::new(&api_key);

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

    client.call_text("llama-3.1-8b-instant", system, &input, 300).await
}

/// Generate deep summaries for stories with relevance_score >= 8.
/// Uses Anthropic Claude Sonnet for higher quality analysis.
/// Capped at 5 stories to control API costs.
async fn generate_deep_summaries(db_path: &std::path::Path, analysis: &crate::claude::AnalysisResult) -> anyhow::Result<usize> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;

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

    let client = reqwest::Client::new();
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

fn write_to_db(db_path: &Path, analysis: &crate::claude::AnalysisResult, embeddings: Option<&[crate::embeddings::StoryEmbedding]>, prefixes: Option<&[Option<String>]>, executive_summary: Option<&str>) -> anyhow::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

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
                is_hero, display_order, url_hash, title_hash, context_prefix
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
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

    let valid_types = ["company", "person", "topic", "product", "regulation"];
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
    let valid_types = ["company", "person", "topic", "product", "regulation"];
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

    // Phase 1: Collect from all sources, filter to freedom_* only
    tracing::info!("Freedoms: Collecting articles...");
    let all_articles = sources::collect_all().await?;
    let freedom_articles: Vec<_> = all_articles
        .into_iter()
        .filter(|a| a.sector.starts_with("freedom_"))
        .collect();
    tracing::info!("Freedoms: {} raw articles", freedom_articles.len());

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
        let client = crate::claude::client::GroqClient::new(&api_key);
        match client.pre_curate(&unique).await {
            Ok(indices) => {
                let curated: Vec<_> = indices.into_iter()
                    .filter_map(|i| unique.get(i).cloned())
                    .collect();
                tracing::info!("Freedoms: Pre-curated to {} articles", curated.len());
                curated
            }
            Err(e) => {
                tracing::warn!("Freedoms pre-curation failed, summarizing all: {}", e);
                unique
            }
        }
    } else {
        unique
    };

    // Phase 3: Summarize
    tracing::info!("Freedoms: Summarizing {} stories...", to_summarize.len());
    let summaries = crate::claude::summarize_stories(&to_summarize, None).await?;
    tracing::info!("Freedoms: {} summaries", summaries.len());

    // Phase 4: Curate with freedoms prompt
    tracing::info!("Freedoms: Curating...");
    let api_key = std::env::var("GROQ_API_KEY")
        .map_err(|_| anyhow::anyhow!("GROQ_API_KEY not set"))?;
    let client = crate::claude::client::GroqClient::new(&api_key);

    // Sort by importance and take top stories for curation
    let mut sorted = summaries.clone();
    sorted.sort_by(|a, b| b.importance_score.cmp(&a.importance_score));
    sorted.truncate(120);

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
    }
    #[derive(serde::Deserialize)]
    struct FreedomsResponse {
        curation: FreedomsCuration,
    }

    let parsed: FreedomsResponse = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse freedoms curation: {} — raw: {}", e, json_str))?;

    // Build curated list with freedom labels
    let mut curated: Vec<(&str, &crate::claude::SummarizedStory)> = Vec::new();
    for &idx in &parsed.curation.time {
        if let Some(s) = sorted.get(idx) {
            curated.push(("time", s));
        }
    }
    for &idx in &parsed.curation.wealth {
        if let Some(s) = sorted.get(idx) {
            curated.push(("wealth", s));
        }
    }
    for &idx in &parsed.curation.location {
        if let Some(s) = sorted.get(idx) {
            curated.push(("location", s));
        }
    }
    for &idx in &parsed.curation.health {
        if let Some(s) = sorted.get(idx) {
            curated.push(("health", s));
        }
    }

    tracing::info!("Freedoms: {} curated stories", curated.len());

    // Phase 5: Write to database
    tracing::info!("Freedoms: Writing to database...");
    write_freedoms_to_db(db_path, &curated)?;

    // Phase 5.5: Generate executive summary for freedoms (non-fatal)
    tracing::info!("Freedoms: Generating executive summary...");
    match generate_freedoms_summary(&curated).await {
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

    // Phase 8: Generate embeddings for freedom stories in main stories table (non-fatal)
    tracing::info!("Freedoms: Generating embeddings...");
    let freedom_summaries: Vec<crate::claude::SummarizedStory> = curated.iter().map(|(_, s)| (*s).clone()).collect();
    match crate::embeddings::generate(&freedom_summaries, None).await {
        Ok(embs) => {
            if let Ok(conn) = rusqlite::Connection::open(db_path) {
                for se in &embs {
                    if let Some((_, story)) = curated.get(se.story_index) {
                        let story_id: Option<i64> = conn.query_row(
                            "SELECT id FROM stories WHERE headline = ?1 ORDER BY id DESC LIMIT 1",
                            rusqlite::params![story.headline],
                            |row| row.get(0),
                        ).ok();
                        if let Some(sid) = story_id {
                            let blob: Vec<u8> = se.embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
                            conn.execute(
                                "INSERT OR REPLACE INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
                                rusqlite::params![sid, blob],
                            ).ok();
                        }
                    }
                }
            }
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
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

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
        tx.execute("DELETE FROM briefings WHERE id = ?1", [old_id])?;
    }

    // Count per freedom
    let time_count = curated.iter().filter(|(f, _)| *f == "time").count();
    let wealth_count = curated.iter().filter(|(f, _)| *f == "wealth").count();
    let location_count = curated.iter().filter(|(f, _)| *f == "location").count();
    let health_count = curated.iter().filter(|(f, _)| *f == "health").count();
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
        "Wrote {} freedom stories to briefing {} (time={}, wealth={}, location={}, health={})",
        total, briefing_id, time_count, wealth_count, location_count, health_count
    );

    Ok(())
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
