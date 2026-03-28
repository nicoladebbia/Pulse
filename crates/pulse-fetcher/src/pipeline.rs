use std::path::Path;

pub async fn run(db_path: &Path) -> anyhow::Result<()> {
    let start = std::time::Instant::now();

    // Phase 1: Collect from all sources
    tracing::info!("Phase 1: Collecting from sources...");
    let raw_articles = sources::collect_all().await?;
    tracing::info!("Collected {} raw articles", raw_articles.len());

    // Phase 2: Deduplicate
    tracing::info!("Phase 2: Deduplicating...");
    let unique_articles = crate::dedup::deduplicate(raw_articles);
    tracing::info!("{} articles after dedup", unique_articles.len());

    // Phase 3: Translate Italian articles
    tracing::info!("Phase 3: Translating Italian articles...");
    let translated = crate::claude::translate_italian(&unique_articles).await?;

    // Phase 4: Summarize with Haiku
    tracing::info!("Phase 4: Summarizing with Claude Haiku...");
    let summaries = crate::claude::summarize_stories(&translated).await?;

    // Phase 5: Cross-sector analysis with Sonnet
    tracing::info!("Phase 5: Cross-sector analysis with Claude Sonnet...");
    let analysis = crate::claude::analyze_cross_sector(&summaries).await?;

    // Phase 6: Generate embeddings
    tracing::info!("Phase 6: Generating embeddings...");
    let _embeddings = crate::embeddings::generate(&analysis.curated_stories).await?;

    // Phase 7: Write to database
    tracing::info!("Phase 7: Writing to database...");
    write_to_db(db_path, &analysis)?;

    // Phase 8: Send notification
    tracing::info!("Phase 8: Sending notification...");
    send_notification(analysis.curated_stories.len())?;

    let duration = start.elapsed();
    tracing::info!("Pipeline complete in {:.1}s", duration.as_secs_f64());

    Ok(())
}

use crate::sources;

fn write_to_db(db_path: &Path, analysis: &crate::claude::AnalysisResult) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Count stories per sector
    let ai_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "ai").count();
    let miami_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "miami").count();
    let italy_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "italy").count();
    let tech_count = analysis.curated_stories.iter().filter(|s| s.article.sector == "tech").count();
    let total = analysis.curated_stories.len();

    let tx = conn.unchecked_transaction()?;

    // 1. Insert briefing
    tx.execute(
        "INSERT OR REPLACE INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'complete')",
        rusqlite::params![today, total, ai_count, miami_count, italy_count, tech_count],
    )?;
    let briefing_id = tx.last_insert_rowid();

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

        tx.execute(
            "INSERT INTO stories (
                briefing_id, sector, original_title, original_url, original_language,
                content_snippet, source_name, published_at, headline, summary,
                key_facts, why_it_matters, what_to_watch, importance_score,
                is_hero, display_order, url_hash, title_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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

    tx.commit()?;
    tracing::info!("Wrote {} stories to briefing {}", total, briefing_id);
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
