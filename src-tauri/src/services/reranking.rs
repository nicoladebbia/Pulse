use super::search::ScoredStory;
use anyhow::Context;

const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
const HAIKU_API_URL: &str = "https://api.anthropic.com/v1/messages";
const RERANK_TIMEOUT_SECS: u64 = 8;

/// Rerank stories by relevance to the user question using Haiku.
/// Returns the stories reordered by LLM-judged relevance, keeping at most `top_k`.
/// On any failure, returns the original stories truncated to `top_k` (non-fatal).
pub async fn llm_rerank(
    api_key: &str,
    question: &str,
    stories: Vec<ScoredStory>,
    top_k: usize,
) -> Vec<ScoredStory> {
    if api_key.is_empty() || stories.len() <= top_k {
        return stories.into_iter().take(top_k).collect();
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(RERANK_TIMEOUT_SECS),
        rerank_inner(api_key, question, &stories),
    )
    .await;

    match result {
        Ok(Ok(indices)) => reorder_by_indices(stories, &indices, top_k),
        Ok(Err(e)) => {
            tracing::warn!("LLM reranking failed (non-fatal): {}", e);
            stories.into_iter().take(top_k).collect()
        }
        Err(_) => {
            tracing::warn!("LLM reranking timed out after {}s", RERANK_TIMEOUT_SECS);
            stories.into_iter().take(top_k).collect()
        }
    }
}

async fn rerank_inner(
    api_key: &str,
    question: &str,
    stories: &[ScoredStory],
) -> anyhow::Result<Vec<usize>> {
    // Build a compact story list (headline + summary only to minimize tokens)
    let mut story_list = String::new();
    for (i, s) in stories.iter().enumerate() {
        story_list.push_str(&format!(
            "[{}] [{}] {} — {}\n",
            i, s.sector, s.headline, s.summary
        ));
    }

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": HAIKU_MODEL,
        "max_tokens": 200,
        "system": "You are a relevance judge for a news intelligence system. Given a user question and a list of news stories, rank the stories by relevance to the question.\nReturn ONLY a JSON array of story indices in order of relevance, most relevant first.\nExample: [3, 7, 1, 0, 5]\nInclude ALL indices. Return nothing but the JSON array.",
        "messages": [{
            "role": "user",
            "content": format!("Question: {}\n\nStories:\n{}", question, story_list)
        }]
    });

    let resp = client
        .post(HAIKU_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Haiku rerank request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("Haiku rerank returned {}", resp.status());
    }

    let response: serde_json::Value = resp.json().await?;
    let text = response["content"][0]["text"]
        .as_str()
        .unwrap_or("[]");

    // Extract JSON array from response
    let json_str = if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            &text[start..=end]
        } else {
            text
        }
    } else {
        text
    };

    let indices: Vec<usize> = serde_json::from_str(json_str)
        .context("failed to parse reranking indices")?;

    Ok(indices)
}

/// Reorder stories according to LLM-provided indices, skipping invalid ones.
fn reorder_by_indices(
    stories: Vec<ScoredStory>,
    indices: &[usize],
    top_k: usize,
) -> Vec<ScoredStory> {
    let mut result = Vec::with_capacity(top_k);
    let mut used = vec![false; stories.len()];

    // Add stories in the order specified by indices
    for &idx in indices {
        if idx < stories.len() && !used[idx] && result.len() < top_k {
            used[idx] = true;
            result.push(stories[idx].clone());
        }
    }

    // If LLM missed some indices, append remaining stories in original order
    if result.len() < top_k {
        for (i, story) in stories.into_iter().enumerate() {
            if !used[i] && result.len() < top_k {
                result.push(story);
            }
        }
    }

    result
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::search::{MatchType, StorySource};

    fn test_story(id: i64, headline: &str) -> ScoredStory {
        ScoredStory {
            story_id: id,
            headline: headline.to_string(),
            summary: format!("Summary for {}", headline),
            key_facts: "[]".to_string(),
            why_it_matters: String::new(),
            sector: "ai".to_string(),
            date: "2026-04-01".to_string(),
            source_name: "Test".to_string(),
            score: 0.5,
            match_type: MatchType::Fts,
            source: StorySource::Daily,
        }
    }

    #[test]
    fn test_reorder_by_indices_valid() {
        let stories = vec![
            test_story(1, "First"),
            test_story(2, "Second"),
            test_story(3, "Third"),
        ];

        let result = reorder_by_indices(stories, &[2, 0, 1], 3);
        assert_eq!(result[0].story_id, 3);
        assert_eq!(result[1].story_id, 1);
        assert_eq!(result[2].story_id, 2);
    }

    #[test]
    fn test_reorder_by_indices_with_invalid() {
        let stories = vec![
            test_story(1, "First"),
            test_story(2, "Second"),
            test_story(3, "Third"),
        ];

        // Index 5 is out of bounds — should be skipped
        let result = reorder_by_indices(stories, &[2, 5, 0], 3);
        assert_eq!(result[0].story_id, 3);
        assert_eq!(result[1].story_id, 1);
        // Story 2 wasn't in valid indices, gets appended
        assert_eq!(result[2].story_id, 2);
    }

    #[test]
    fn test_reorder_by_indices_respects_top_k() {
        let stories = vec![
            test_story(1, "First"),
            test_story(2, "Second"),
            test_story(3, "Third"),
            test_story(4, "Fourth"),
        ];

        let result = reorder_by_indices(stories, &[3, 1, 0, 2], 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].story_id, 4);
        assert_eq!(result[1].story_id, 2);
    }

    #[test]
    fn test_reorder_by_indices_empty() {
        let stories = vec![
            test_story(1, "First"),
            test_story(2, "Second"),
        ];

        // Empty indices — falls back to original order
        let result = reorder_by_indices(stories, &[], 2);
        assert_eq!(result[0].story_id, 1);
        assert_eq!(result[1].story_id, 2);
    }

    #[test]
    fn test_reorder_by_indices_no_duplicates() {
        let stories = vec![
            test_story(1, "First"),
            test_story(2, "Second"),
        ];

        // Duplicate index — should only include once
        let result = reorder_by_indices(stories, &[0, 0, 1], 3);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].story_id, 1);
        assert_eq!(result[1].story_id, 2);
    }
}
