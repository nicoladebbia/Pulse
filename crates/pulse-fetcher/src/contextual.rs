use crate::claude::SummarizedStory;
use serde::Deserialize;

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Generate contextual prefixes for stories using Haiku.
/// Each prefix situates the story within its sector, names key entities,
/// and connects it to ongoing narratives — improving both FTS and semantic search.
/// Returns one Option<String> per story (None if generation failed for that story).
pub async fn generate_prefixes(
    stories: &[SummarizedStory],
    day_context: &str,
) -> anyhow::Result<Vec<Option<String>>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut all_prefixes: Vec<Option<String>> = vec![None; stories.len()];

    // Process in batches of 10
    for (batch_start, chunk) in stories.chunks(10).enumerate().map(|(i, c)| (i * 10, c)) {
        let mut stories_text = String::new();
        for (i, story) in chunk.iter().enumerate() {
            stories_text.push_str(&format!(
                "\n[{}] [{}] {}\n{}\nWhy it matters: {}\n",
                i, story.article.sector, story.headline, story.summary, story.why_it_matters
            ));
        }

        let body = serde_json::json!({
            "model": HAIKU_MODEL,
            "max_tokens": 1500,
            "system": format!(
                r#"You generate contextual prefixes for news stories to improve search retrieval.

Today's full briefing (for cross-referencing):
{}

For each story below, write a 1-2 sentence prefix that:
1. Names the sector and key entities (companies, people, technologies)
2. Connects the story to related ongoing narratives or trends
3. Notes temporal context if relevant (what preceded this, what might follow)

Return ONLY valid JSON: {{"prefixes": ["prefix for story 0", "prefix for story 1", ...]}}
Keep each prefix under 80 words. Be specific — use names, not descriptions."#,
                day_context
            ),
            "messages": [{"role": "user", "content": stories_text}]
        });

        let resp = match client
            .post(API_URL)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Contextual prefix batch failed (network): {}", e);
                continue;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!("Contextual prefix batch failed: {}", resp.status());
            continue;
        }

        let response: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Contextual prefix batch parse failed: {}", e);
                continue;
            }
        };

        let text = response["content"][0]["text"].as_str().unwrap_or("{}");
        let json_str = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else {
                text
            }
        } else {
            text
        };

        #[derive(Deserialize)]
        struct PrefixResponse {
            prefixes: Vec<String>,
        }

        if let Ok(parsed) = serde_json::from_str::<PrefixResponse>(json_str) {
            for (i, prefix) in parsed.prefixes.into_iter().enumerate() {
                if !prefix.is_empty() && batch_start + i < all_prefixes.len() {
                    all_prefixes[batch_start + i] = Some(prefix);
                }
            }
        } else {
            tracing::warn!("Failed to parse contextual prefix JSON for batch starting at {}", batch_start);
        }

        // Rate limit between batches
        if chunk.len() == 10 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    let count = all_prefixes.iter().filter(|p| p.is_some()).count();
    tracing::info!("Generated {}/{} contextual prefixes", count, stories.len());
    Ok(all_prefixes)
}
