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

fn write_to_db(_db_path: &Path, _analysis: &crate::claude::AnalysisResult) -> anyhow::Result<()> {
    // TODO: Implement database writes
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
