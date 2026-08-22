use crate::claude::SummarizedStory;
use serde::Deserialize;

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Generate contextual prefixes for stories using Haiku.
/// Each prefix situates the story within its sector, names key entities,
/// and connects it to ongoing narratives — improving both FTS and semantic search.
/// Returns one Option<String> per story (None if generation failed for that story).
/// Pull the model's text out of an Anthropic response.
///
/// On failure the Err carries a truncated dump of what the response actually
/// held, so the log names the real cause. This replaced `unwrap_or("{}")`,
/// which converted "no text field" into "empty JSON object" and made every
/// such failure look like a parse error one line later.
///
/// Truncation is by `chars()`, never bytes — a byte truncate panics mid-UTF-8,
/// which took Pulse down for two days in August.
fn prefix_text(response: &serde_json::Value) -> Result<&str, String> {
    response["content"][0]["text"].as_str().ok_or_else(|| {
        let dump = response.to_string();
        let mut out: String = dump.chars().take(200).collect();
        if dump.chars().count() > 200 {
            out.push('…');
        }
        out
    })
}

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

        let text = match prefix_text(&response) {
            Ok(t) => t,
            Err(shape) => {
                // NOT a parse failure. `unwrap_or("{}")` used to turn a missing
                // text field into an empty object, which then failed to parse and
                // logged "Failed to parse contextual prefix JSON" — a diagnosis
                // naming the wrong layer while the real cause (usually an API
                // error body) went unquoted.
                tracing::warn!(
                    "Contextual prefix batch at {} returned no content[0].text; response was: {}",
                    batch_start, shape
                );
                continue;
            }
        };
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
            if parsed.prefixes.len() != chunk.len() {
                tracing::warn!(
                    "contextual prefixes short: got {} expected {} for batch starting at {}",
                    parsed.prefixes.len(), chunk.len(), batch_start
                );
            }
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

#[cfg(test)]
mod prefix_text_tests {
    use super::prefix_text;
    use serde_json::json;

    #[test]
    fn a_well_formed_response_yields_the_text() {
        let v = json!({"content": [{"text": "{\"prefixes\":[]}"}]});
        assert_eq!(prefix_text(&v).unwrap(), "{\"prefixes\":[]}");
    }

    /// The bug: an API error body has no content[0].text, and `unwrap_or("{}")`
    /// turned it into an empty object so the next line blamed JSON parsing. The
    /// error must carry the real body so the log names the real cause.
    #[test]
    fn an_api_error_body_is_reported_with_its_own_content() {
        let v = json!({"error": {"type": "rate_limit_error", "message": "slow down"}});
        let err = prefix_text(&v).unwrap_err();
        assert!(err.contains("rate_limit_error"), "got: {err}");
        assert!(err.contains("slow down"), "got: {err}");
    }

    #[test]
    fn a_huge_body_is_truncated_on_a_char_boundary_not_a_byte_one() {
        let v = json!({"error": "è".repeat(500)});
        let err = prefix_text(&v).unwrap_err();
        assert!(err.chars().count() <= 201, "len {}", err.chars().count());
        assert!(err.ends_with('…'));
    }
}
