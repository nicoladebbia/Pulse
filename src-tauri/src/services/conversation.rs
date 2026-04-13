use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatThread {
    pub id: String,
    pub topic: String,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub content: String,
    pub sources: Option<Vec<i64>>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveInsight {
    pub story_id: i64,
    pub headline: String,
    pub date: String,
    pub connection: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationResponse {
    pub message: String,
    pub source_story_ids: Vec<i64>,
    pub suggested_followups: Vec<String>,
    pub thread_topic: String,
    pub thread_title: Option<String>,
    pub proactive_connections: Vec<ProactiveInsight>,
}

// ---------------------------------------------------------------------------
// Thread management
// ---------------------------------------------------------------------------

/// Create a new chat thread.
pub fn create_thread(conn: &Connection, topic: &str) -> Result<ChatThread> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO chat_threads (id, topic, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, topic, now],
    )?;
    Ok(ChatThread {
        id,
        topic: topic.to_string(),
        title: None,
        created_at: now.clone(),
        updated_at: now,
    })
}

/// List all threads, most recent first.
pub fn list_threads(conn: &Connection) -> Result<Vec<ChatThread>> {
    let mut stmt = conn.prepare(
        "SELECT id, topic, title, created_at, updated_at
         FROM chat_threads
         ORDER BY updated_at DESC",
    )?;

    let threads = stmt
        .query_map([], |row| {
            Ok(ChatThread {
                id: row.get(0)?,
                topic: row.get(1)?,
                title: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(threads)
}

/// Get all messages in a thread, ordered chronologically.
pub fn get_thread_messages(conn: &Connection, thread_id: &str) -> Result<Vec<ChatMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, thread_id, role, content, sources, metadata, created_at
         FROM chat_messages
         WHERE thread_id = ?1
         ORDER BY created_at ASC",
    )?;

    let messages = stmt
        .query_map(params![thread_id], |row| {
            let sources_json: Option<String> = row.get(4)?;
            let metadata_json: Option<String> = row.get(5)?;

            Ok(ChatMessage {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                sources: sources_json
                    .and_then(|s| serde_json::from_str(&s).ok()),
                metadata: metadata_json
                    .and_then(|s| serde_json::from_str(&s).ok()),
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(messages)
}

/// Delete a thread and its messages (cascade via FK).
pub fn delete_thread(conn: &Connection, thread_id: &str) -> Result<()> {
    // Foreign key cascade should handle messages, but be explicit for safety.
    conn.execute(
        "DELETE FROM chat_messages WHERE thread_id = ?1",
        params![thread_id],
    )?;
    conn.execute(
        "DELETE FROM chat_threads WHERE id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

/// Update a thread's title.
pub fn update_thread_title(conn: &Connection, thread_id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE chat_threads SET title = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![title, thread_id],
    )?;
    Ok(())
}

/// Store a message in a thread. Returns the new message ID.
pub fn store_message(
    conn: &Connection,
    thread_id: &str,
    role: &str,
    content: &str,
    sources: Option<&[i64]>,
    metadata: Option<&serde_json::Value>,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let sources_json = sources.map(|s| serde_json::to_string(s).unwrap_or_default());
    let metadata_json = metadata.map(|m| serde_json::to_string(m).unwrap_or_default());

    conn.execute(
        "INSERT INTO chat_messages (id, thread_id, role, content, sources, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        params![id, thread_id, role, content, sources_json, metadata_json],
    )
    .context("failed to insert chat message")?;

    // Bump thread updated_at
    conn.execute(
        "UPDATE chat_threads SET updated_at = datetime('now') WHERE id = ?1",
        params![thread_id],
    )?;

    Ok(id)
}

/// Find an existing thread for the same topic updated within the last 24 hours.
pub fn find_recent_thread(conn: &Connection, topic: &str) -> Result<Option<ChatThread>> {
    let mut stmt = conn.prepare(
        "SELECT id, topic, title, created_at, updated_at
         FROM chat_threads
         WHERE topic = ?1
           AND updated_at >= datetime('now', '-24 hours')
         ORDER BY updated_at DESC
         LIMIT 1",
    )?;

    let mut rows = stmt.query_map(params![topic], |row| {
        Ok(ChatThread {
            id: row.get(0)?,
            topic: row.get(1)?,
            title: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    })?;

    match rows.next() {
        Some(Ok(thread)) => Ok(Some(thread)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Auto-threading: topic classification
// ---------------------------------------------------------------------------

/// Classify a message into a topic using keyword heuristics.
pub fn classify_topic(message: &str) -> &'static str {
    let lower = message.to_lowercase();

    let ai_keywords: &[&str] = &[
        "ai", "llm", "gpt", "claude", "openai", "anthropic", "gemini",
        "model", "transformer", "neural", "machine learning", "deep learning",
        "artificial intelligence", "chatbot", "copilot",
    ];
    let miami_keywords: &[&str] = &[
        "miami", "beach", "florida", "south florida", "dade", "brickell",
        "wynwood", "coral gables", "fort lauderdale", "broward",
    ];
    let italy_keywords: &[&str] = &[
        "italy", "italian", "roma", "rome", "meloni", "politica", "governo",
        "parlamento", "serie a", "calcio", "vatican", "pope", "europa",
    ];
    let tech_keywords: &[&str] = &[
        "startup", "apple", "google", "meta", "chip", "quantum", "robot",
        "hardware", "iphone", "samsung", "nvidia", "gpu", "ev", "tesla",
    ];

    let score = |keywords: &[&str]| -> usize {
        keywords.iter().filter(|kw| lower.contains(*kw)).count()
    };

    let scores = [
        ("ai", score(ai_keywords)),
        ("miami", score(miami_keywords)),
        ("italy", score(italy_keywords)),
        ("tech", score(tech_keywords)),
    ];

    let (best_topic, best_score) = scores.iter().max_by_key(|(_, s)| s).unwrap();
    if *best_score == 0 {
        "general"
    } else {
        best_topic
    }
}

// ---------------------------------------------------------------------------
// Context assembly
// ---------------------------------------------------------------------------

/// Build the system prompt with all intelligence context.
pub fn build_system_prompt(
    profile_summary: &str,
    stories_context: &str,
    entity_context: &str,
    signal_context: &str,
    causal_context: &str,
    contrarian_context: &str,
    pattern_context: &str,
    predictions_context: &str,
    prediction_calibration: &str,
) -> String {
    let mut prompt = String::from(
        r#"You are Pulse, an intelligence analyst and forecaster. You track AI/LLMs, Miami Beach, Italian politics, and tech/innovation daily. You have deep temporal awareness of how stories evolve over time and make specific, actionable predictions.

The reader is a tech founder in Miami Beach who builds AI/ML apps, Shopify tools, and iOS apps. Italian heritage, follows Serie A. He wants specific, bold analysis — not hedging.

RESPONSE STRUCTURE:
Start with a direct opening statement — the bottom line, the direct answer, no preamble. Then use --- or ## headers to organize the analysis. End with what to watch going forward.

When expressing uncertainty, use HIGH, MODERATE, or LOW inline.

PREDICTION RULES:
- When asked about stocks, investments, or future outcomes, give your BEST assessment based on the evidence in your archive. Name specific companies, give direction (bullish/bearish), state confidence, and cite the stories that support your view.
- You are NOT a financial advisor. But you ARE an intelligence analyst who can say "Based on 15 stories showing X trend, entity Y has momentum, and catalyst Z is coming on [date], I assess [prediction] with [confidence]."
- Use signal trajectory data (rising/declining/accelerating) to ground temporal predictions.
- Reference specific upcoming events (earnings dates, policy deadlines, product launches) when they appear in stories.
- When past predictions are provided, use your accuracy track record to calibrate confidence. If you've been 80% right on AI predictions, lean into those. If 30% on geopolitics, be more cautious.

RULES:
- Lead with the conclusion, not the setup
- Be direct and opinionated — you're an analyst, not a summarizer
- Use concrete details: names, dates, numbers, not vague references
- If the archive doesn't cover a topic, say so plainly
- When you spot a pattern across time, call it out
- Keep total response under 300 words for simple factual questions, under 500 words for analysis. Be dense, not verbose.
- Never list more than 5 bullet points in any section.
- Never repeat section headers — one Bottom Line, one Analysis, one What to Watch. No duplicate ## headers.

"#,
    );

    // Only include non-empty context sections to avoid wasting tokens
    prompt.push_str("RETRIEVED STORIES (ranked by relevance):\n");
    prompt.push_str(stories_context);

    if !entity_context.is_empty() {
        prompt.push_str("\n\nENTITY INTELLIGENCE:\n");
        prompt.push_str(entity_context);
    }
    if !signal_context.is_empty() {
        prompt.push_str("\n\nSIGNAL STRENGTH:\n");
        prompt.push_str(signal_context);
    }
    if !causal_context.is_empty() {
        prompt.push_str("\n\nCAUSAL PATTERNS:\n");
        prompt.push_str(causal_context);
    }
    if !contrarian_context.is_empty() {
        prompt.push_str("\n\nCONTRARIAN SIGNALS:\n");
        prompt.push_str(contrarian_context);
    }
    if !pattern_context.is_empty() {
        prompt.push_str("\n\nCROSS-SECTOR PATTERNS:\n");
        prompt.push_str(pattern_context);
    }
    if !predictions_context.is_empty() {
        prompt.push_str("\n\nACTIVE PREDICTIONS:\n");
        prompt.push_str(predictions_context);
    }
    if !prediction_calibration.is_empty() {
        prompt.push_str("\n\nYOUR PREDICTION TRACK RECORD:\n");
        prompt.push_str(prediction_calibration);
    }
    if !profile_summary.is_empty() {
        prompt.push_str("\n\nUSER PROFILE:\n");
        prompt.push_str(profile_summary);
    }

    prompt.push_str(r#"

METADATA MARKERS (the UI extracts these — include them naturally):
- Reference stories: [story:123]
- Suggest follow-ups: [followup: "question text" | type: "deeper"], [followup: "question text" | type: "compare"], [followup: "question text" | type: "predict"], or [followup: "question text" | type: "timeline"]
- Make predictions: [prediction: "prediction text" | confidence: 0.X | timeframe: "..."]

Suggest 2-4 follow-up questions. Use the types: "deeper" (drill into sub-topic), "compare" (contrast), "predict" (what happens next), "timeline" (how it evolved)."#);

    prompt
}

/// Format scored stories into a context string for the prompt.
/// Format stories for the prompt. Dynamic context size based on query type:
/// - Breaking/General: 8 stories (focused)
/// - Analytical/Comparative: 12 stories (more breadth for prediction/analysis)
pub fn format_stories_context(stories: &[super::search::ScoredStory], query_type: super::search::QueryType) -> String {
    let max_stories = match query_type {
        super::search::QueryType::Analytical | super::search::QueryType::Comparative => 12,
        _ => 8,
    };
    let selected: Vec<_> = stories.iter().take(max_stories).collect();
    if selected.is_empty() {
        return "No stories found in archive.".to_string();
    }

    let reordered = reorder_for_attention(&selected);

    reordered
        .iter()
        .map(|s| {
            format!(
                "[ID:{}] [{}] {} ({}, {})\n  {}\n  Why it matters: {}",
                s.story_id, s.sector, s.headline, s.date, s.source_name,
                s.summary, s.why_it_matters
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Reorder stories so the most relevant appear at start and end of the list.
/// This mitigates the "lost in the middle" effect in long-context LLMs.
/// Input must be pre-sorted by score descending.
fn reorder_for_attention<'a>(stories: &[&'a super::search::ScoredStory]) -> Vec<&'a super::search::ScoredStory> {
    if stories.len() <= 6 {
        return stories.to_vec();
    }
    // Even-indexed (rank 1, 3, 5, ...) go to the front,
    // Odd-indexed (rank 2, 4, 6, ...) go to the back in reverse.
    // Result: #1 at start, #2 at end, #3 at position 2, #4 at second-to-last, etc.
    let mut front: Vec<_> = stories.iter().step_by(2).copied().collect();
    let mut back: Vec<_> = stories.iter().skip(1).step_by(2).copied().collect();
    back.reverse();
    front.extend(back);
    front
}

/// Format entity context for the prompt.
pub fn format_entity_context(
    entities: &[(super::entities::Entity, Vec<super::entities::EntityMention>)],
) -> String {
    if entities.is_empty() {
        return "No entity data available.".to_string();
    }

    entities
        .iter()
        .map(|(entity, mentions)| {
            let recent: String = mentions
                .iter()
                .rev()
                .take(3)
                .map(|m| {
                    format!(
                        "  - [{}] sentiment {:.1}: {}",
                        m.mentioned_at,
                        m.sentiment,
                        m.context.as_deref().unwrap_or("(no context)")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");

            let first = mentions.first().map(|m| m.mentioned_at.as_str()).unwrap_or("?");
            let last = mentions.last().map(|m| m.mentioned_at.as_str()).unwrap_or("?");

            format!(
                "Entity: {} ({}) — {} mentions, sentiment: {:.2}, first seen: {}, last seen: {}\n  Recent mentions:\n{}",
                entity.name,
                entity.entity_type,
                entity.mention_count,
                entity.sentiment_avg,
                first,
                last,
                recent
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Format signal context for the prompt.
pub fn format_signal_context(signals: &[super::signals::Signal]) -> String {
    if signals.is_empty() {
        return "No signal data available.".to_string();
    }

    signals
        .iter()
        .map(|s| {
            format!(
                "Topic: {} — {} (acceleration: {:.1}x, 7d: {}, 30d: {}, 90d: {})",
                s.topic, s.trajectory, s.acceleration, s.window_7d, s.window_30d, s.window_90d
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Extract story IDs from assistant response (finds `[story:123]` patterns).
pub fn extract_story_references(response: &str) -> Vec<i64> {
    let mut ids = Vec::new();
    let mut search_from = 0;

    while let Some(start) = response[search_from..].find("[story:") {
        let abs_start = search_from + start + 7; // skip "[story:"
        if abs_start >= response.len() {
            break;
        }
        if let Some(end) = response[abs_start..].find(']') {
            let num_str = &response[abs_start..abs_start + end];
            if let Ok(id) = num_str.trim().parse::<i64>() {
                ids.push(id);
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    ids
}

/// Extract follow-up suggestions from response.
pub fn extract_followups(response: &str) -> Vec<String> {
    let mut followups = Vec::new();
    let mut search_from = 0;

    while let Some(start) = response[search_from..].find("[followup:") {
        let abs_start = search_from + start + 10; // skip "[followup:"
        if abs_start >= response.len() {
            break;
        }
        // Find the closing ]
        if let Some(end) = response[abs_start..].find(']') {
            let inner = response[abs_start..abs_start + end].trim();
            // Parse: "question text" | type: "deeper"
            let parts: Vec<&str> = inner.split('|').collect();
            let question = parts[0]
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim()
                .to_string();

            let ftype = if parts.len() >= 2 {
                parts[1]
                    .trim()
                    .strip_prefix("type:")
                    .map(|s| s.trim().trim_matches('"').trim().to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            if !question.is_empty() {
                // Encode type into the string as "type::question" for frontend parsing
                if ftype.is_empty() {
                    followups.push(question);
                } else {
                    followups.push(format!("{}::{}", ftype, question));
                }
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    followups
}

/// Extract predictions from response.
/// Returns `(prediction_text, confidence, timeframe)`.
pub fn extract_predictions(response: &str) -> Vec<(String, f32, String)> {
    let mut predictions = Vec::new();
    let mut search_from = 0;

    while let Some(start) = response[search_from..].find("[prediction:") {
        let abs_start = search_from + start + 12; // skip "[prediction:"
        if abs_start >= response.len() {
            break;
        }
        if let Some(end) = response[abs_start..].find(']') {
            let inner = &response[abs_start..abs_start + end];
            // Parse: "text" | confidence: 0.X | timeframe: "..."
            let parts: Vec<&str> = inner.split('|').collect();
            if parts.len() >= 3 {
                let text = parts[0]
                    .trim()
                    .trim_start_matches('"')
                    .trim_end_matches('"')
                    .trim()
                    .to_string();

                let confidence = parts[1]
                    .trim()
                    .strip_prefix("confidence:")
                    .and_then(|s| s.trim().parse::<f32>().ok())
                    .unwrap_or(0.5);

                let timeframe = parts[2]
                    .trim()
                    .strip_prefix("timeframe:")
                    .map(|s| {
                        s.trim()
                            .trim_start_matches('"')
                            .trim_end_matches('"')
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_default();

                if !text.is_empty() {
                    predictions.push((text, confidence, timeframe));
                }
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    predictions
}

/// Clean response by removing inline markers, preserving natural text.
pub fn clean_response(response: &str) -> String {
    let mut result = response.to_string();

    // Remove [story:N] markers
    while let Some(start) = result.find("[story:") {
        if let Some(end) = result[start..].find(']') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }

    // Remove [followup: "..."] markers
    while let Some(start) = result.find("[followup:") {
        if let Some(end) = result[start..].find(']') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }

    // Remove [prediction: ... ] markers
    while let Some(start) = result.find("[prediction:") {
        if let Some(end) = result[start..].find(']') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }

    // Remove [entity: ...] markers
    while let Some(start) = result.find("[entity:") {
        if let Some(end) = result[start..].find(']') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }

    // Clean up double spaces and trim
    while result.contains("  ") {
        result = result.replace("  ", " ");
    }

    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Claude API trait + implementation
// ---------------------------------------------------------------------------

/// Trait for the conversation LLM (testable via mock).
#[async_trait]
pub trait ConversationLLM: Send + Sync {
    async fn send_message(
        &self,
        system: &str,
        messages: &[(String, String)], // (role, content) pairs
    ) -> Result<String>;
}

/// Claude Sonnet implementation for production use.
pub struct ClaudeConversation {
    api_key: String,
    http: reqwest::Client,
}

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const CONVERSATION_MODEL_FAST: &str = "claude-haiku-4-5-20251001";
pub const CONVERSATION_MODEL_DEEP: &str = "claude-sonnet-4-6";

impl ClaudeConversation {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
        Ok(Self::new(&key))
    }
}

#[derive(Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContentBlock>,
}

#[derive(Deserialize)]
struct ClaudeContentBlock {
    text: Option<String>,
}

#[async_trait]
impl ConversationLLM for ClaudeConversation {
    async fn send_message(
        &self,
        system: &str,
        messages: &[(String, String)],
    ) -> Result<String> {
        self.send_message_with_model(system, messages, CONVERSATION_MODEL_FAST, 1200).await
    }
}

impl ClaudeConversation {
    pub async fn send_message_with_model(
        &self,
        system: &str,
        messages: &[(String, String)],
        model: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|(role, content)| {
                serde_json::json!({
                    "role": role,
                    "content": content,
                })
            })
            .collect();

        let body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "system": system,
            "messages": api_messages,
        });

        let resp = self
            .http
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Claude API request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Claude API returned {status}: {text}");
        }

        let parsed: ClaudeResponse = resp
            .json()
            .await
            .context("failed to parse Claude API response")?;

        let text = parsed
            .content
            .into_iter()
            .find_map(|block| block.text)
            .unwrap_or_default();

        Ok(text)
    }
}

impl ClaudeConversation {
    /// Stream a Claude response, calling `on_chunk` for each text delta.
    /// Returns the fully accumulated response text.
    pub async fn send_message_stream<F>(
        &self,
        system: &str,
        messages: &[(String, String)],
        model: &str,
        max_tokens: u32,
        on_chunk: F,
    ) -> Result<String>
    where
        F: Fn(&str) + Send,
    {
        let api_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|(role, content)| {
                serde_json::json!({ "role": role, "content": content })
            })
            .collect();

        let body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "stream": true,
            "system": system,
            "messages": api_messages,
        });

        let resp = self
            .http
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Claude streaming request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Claude API returned {status}: {text}");
        }

        let mut stream = resp.bytes_stream();
        let mut accumulated = String::new();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("stream read error")?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Parse SSE events from buffer (events are separated by double newline)
            while let Some(event_end) = buffer.find("\n\n") {
                let event_text = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();

                for line in event_text.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            continue;
                        }
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                            if parsed["type"] == "content_block_delta" {
                                if let Some(text) = parsed["delta"]["text"].as_str() {
                                    accumulated.push_str(text);
                                    on_chunk(text);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(accumulated)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::*;

    // -----------------------------------------------------------------------
    // Topic classification
    // -----------------------------------------------------------------------

    #[test]
    fn test_classify_topic_ai() {
        assert_eq!(classify_topic("What's happening with OpenAI?"), "ai");
    }

    #[test]
    fn test_classify_topic_miami() {
        assert_eq!(classify_topic("Miami Beach real estate"), "miami");
    }

    #[test]
    fn test_classify_topic_italy() {
        assert_eq!(classify_topic("Meloni's new policy"), "italy");
    }

    #[test]
    fn test_classify_topic_tech() {
        assert_eq!(classify_topic("New NVIDIA chip release"), "tech");
    }

    #[test]
    fn test_classify_topic_general() {
        assert_eq!(classify_topic("Hello, how are you?"), "general");
    }

    // -----------------------------------------------------------------------
    // Thread management (DB tests)
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_thread() {
        let conn = test_db();
        let thread = create_thread(&conn, "ai").unwrap();

        assert_eq!(thread.topic, "ai");
        assert!(thread.title.is_none());
        assert!(!thread.id.is_empty());

        // Verify it's actually in the DB
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chat_threads WHERE id = ?1",
                params![thread.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_list_threads_ordered() {
        let conn = test_db();

        // Create 3 threads — they get created in sequence so updated_at differs
        let t1 = create_thread(&conn, "ai").unwrap();
        let t2 = create_thread(&conn, "miami").unwrap();
        let t3 = create_thread(&conn, "tech").unwrap();

        // Manually set updated_at to control ordering
        conn.execute(
            "UPDATE chat_threads SET updated_at = '2026-03-31 08:00:00' WHERE id = ?1",
            params![t1.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE chat_threads SET updated_at = '2026-03-31 10:00:00' WHERE id = ?1",
            params![t2.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE chat_threads SET updated_at = '2026-03-31 12:00:00' WHERE id = ?1",
            params![t3.id],
        )
        .unwrap();

        let threads = list_threads(&conn).unwrap();
        assert_eq!(threads.len(), 3);
        assert_eq!(threads[0].id, t3.id, "most recent first");
        assert_eq!(threads[1].id, t2.id);
        assert_eq!(threads[2].id, t1.id);
    }

    #[test]
    fn test_store_and_get_messages() {
        let conn = test_db();
        let thread = create_thread(&conn, "ai").unwrap();

        let m1 = store_message(&conn, &thread.id, "user", "What about OpenAI?", None, None).unwrap();
        let m2 = store_message(
            &conn,
            &thread.id,
            "assistant",
            "Here is what I know [story:42]",
            Some(&[42]),
            None,
        )
        .unwrap();
        let m3 = store_message(&conn, &thread.id, "user", "Tell me more", None, None).unwrap();

        let messages = get_thread_messages(&conn, &thread.id).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].id, m1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].id, m2);
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].sources, Some(vec![42]));
        assert_eq!(messages[2].id, m3);
    }

    #[test]
    fn test_delete_thread_cascades() {
        let conn = test_db();
        let thread = create_thread(&conn, "ai").unwrap();
        store_message(&conn, &thread.id, "user", "Hello", None, None).unwrap();
        store_message(&conn, &thread.id, "assistant", "Hi there", None, None).unwrap();

        delete_thread(&conn, &thread.id).unwrap();

        let threads = list_threads(&conn).unwrap();
        assert!(threads.is_empty());

        let messages = get_thread_messages(&conn, &thread.id).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_find_recent_thread() {
        let conn = test_db();
        let thread = create_thread(&conn, "ai").unwrap();

        // Set updated_at to 2 hours ago (within 24h window)
        conn.execute(
            "UPDATE chat_threads SET updated_at = datetime('now', '-2 hours') WHERE id = ?1",
            params![thread.id],
        )
        .unwrap();

        let found = find_recent_thread(&conn, "ai").unwrap();
        assert!(found.is_some(), "should find thread updated 2h ago");
        assert_eq!(found.unwrap().id, thread.id);
    }

    #[test]
    fn test_find_recent_thread_stale() {
        let conn = test_db();
        let thread = create_thread(&conn, "ai").unwrap();

        // Set updated_at to 48 hours ago (outside 24h window)
        conn.execute(
            "UPDATE chat_threads SET updated_at = datetime('now', '-48 hours') WHERE id = ?1",
            params![thread.id],
        )
        .unwrap();

        let found = find_recent_thread(&conn, "ai").unwrap();
        assert!(found.is_none(), "should NOT find thread updated 48h ago");
    }

    #[test]
    fn test_update_thread_title() {
        let conn = test_db();
        let thread = create_thread(&conn, "ai").unwrap();
        assert!(thread.title.is_none());

        update_thread_title(&conn, &thread.id, "OpenAI Discussion").unwrap();

        let threads = list_threads(&conn).unwrap();
        assert_eq!(threads[0].title.as_deref(), Some("OpenAI Discussion"));
    }

    // -----------------------------------------------------------------------
    // Response parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_story_references() {
        let response = "This [story:42] relates to [story:99] and also [story:7].";
        let ids = extract_story_references(response);
        assert_eq!(ids, vec![42, 99, 7]);
    }

    #[test]
    fn test_extract_story_references_empty() {
        let response = "No references here.";
        let ids = extract_story_references(response);
        assert!(ids.is_empty());
    }

    #[test]
    fn test_extract_followups() {
        let response = r#"Some text [followup: "What about X?"] more text [followup: "How does Y work?"]"#;
        let followups = extract_followups(response);
        assert_eq!(followups.len(), 2);
        assert_eq!(followups[0], "What about X?");
        assert_eq!(followups[1], "How does Y work?");
    }

    #[test]
    fn test_extract_predictions() {
        let response =
            r#"I think [prediction: "X will happen" | confidence: 0.8 | timeframe: "2 weeks"] soon."#;
        let preds = extract_predictions(response);
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].0, "X will happen");
        assert!((preds[0].1 - 0.8).abs() < 0.01);
        assert_eq!(preds[0].2, "2 weeks");
    }

    #[test]
    fn test_extract_predictions_multiple() {
        let response = r#"First [prediction: "A happens" | confidence: 0.9 | timeframe: "1 month"] and [prediction: "B follows" | confidence: 0.6 | timeframe: "3 months"]"#;
        let preds = extract_predictions(response);
        assert_eq!(preds.len(), 2);
        assert_eq!(preds[0].0, "A happens");
        assert_eq!(preds[1].0, "B follows");
    }

    #[test]
    fn test_clean_response() {
        let response = "This [story:42] relates to AI. [followup: \"What next?\"] Some prediction [prediction: \"X\" | confidence: 0.8 | timeframe: \"soon\"] here.";
        let cleaned = clean_response(response);
        assert!(!cleaned.contains("[story:"));
        assert!(!cleaned.contains("[followup:"));
        assert!(!cleaned.contains("[prediction:"));
        assert!(cleaned.contains("relates to AI"));
        assert!(cleaned.contains("here."));
    }

    // -----------------------------------------------------------------------
    // Context formatting
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_stories_context() {
        use super::super::search::{MatchType, ScoredStory};

        let stories = vec![
            ScoredStory {
                story_id: 1,
                headline: "AI Breakthrough".to_string(),
                summary: "Summary one".to_string(),
                key_facts: "[]".to_string(),
                why_it_matters: "It matters a lot".to_string(),
                sector: "ai".to_string(),
                date: "2026-03-31".to_string(),
                source_name: "TechCrunch".to_string(),
                score: 0.9,
                match_type: MatchType::Both,
                source: super::super::search::StorySource::Daily,
            },
            ScoredStory {
                story_id: 2,
                headline: "New Chip".to_string(),
                summary: "Summary two".to_string(),
                key_facts: "[]".to_string(),
                why_it_matters: "Speed gains".to_string(),
                sector: "tech".to_string(),
                date: "2026-03-30".to_string(),
                source_name: "Ars Technica".to_string(),
                score: 0.8,
                match_type: MatchType::Fts,
                source: super::super::search::StorySource::Daily,
            },
            ScoredStory {
                story_id: 3,
                headline: "Beach News".to_string(),
                summary: "Summary three".to_string(),
                key_facts: "[]".to_string(),
                why_it_matters: "Local impact".to_string(),
                sector: "miami".to_string(),
                date: "2026-03-29".to_string(),
                source_name: "Miami Herald".to_string(),
                score: 0.7,
                match_type: MatchType::Semantic,
                source: super::super::search::StorySource::Daily,
            },
        ];
        let formatted = format_stories_context(&stories, crate::services::search::QueryType::General);

        assert!(formatted.contains("[ID:1]"));
        assert!(formatted.contains("[ai]"));
        assert!(formatted.contains("AI Breakthrough"));
        assert!(formatted.contains("[ID:2]"));
        assert!(formatted.contains("[ID:3]"));
        assert!(formatted.contains("Why it matters: Speed gains"));
    }

    #[test]
    fn test_reorder_for_attention() {
        use super::super::search::{MatchType, ScoredStory};

        // Create 8 stories ranked by score descending
        let stories: Vec<ScoredStory> = (0..8)
            .map(|i| ScoredStory {
                story_id: i as i64,
                headline: format!("Story {i}"),
                summary: String::new(),
                key_facts: String::new(),
                why_it_matters: String::new(),
                sector: "ai".to_string(),
                date: "2026-04-01".to_string(),
                source_name: "Test".to_string(),
                score: 1.0 - (i as f32 * 0.1),
                match_type: MatchType::Fts,
                source: super::super::search::StorySource::Daily,
            })
            .collect();

        let refs: Vec<&ScoredStory> = stories.iter().collect();
        let reordered = reorder_for_attention(&refs);

        // Story 0 (rank 1, best) should be first
        assert_eq!(reordered[0].story_id, 0, "best story should be first");
        // Story 1 (rank 2) should be last
        assert_eq!(reordered[7].story_id, 1, "second best should be last");
        // All stories should be present
        assert_eq!(reordered.len(), 8);
    }

    #[test]
    fn test_reorder_small_list_unchanged() {
        use super::super::search::{MatchType, ScoredStory};

        let stories: Vec<ScoredStory> = (0..4)
            .map(|i| ScoredStory {
                story_id: i as i64,
                headline: format!("Story {i}"),
                summary: String::new(),
                key_facts: String::new(),
                why_it_matters: String::new(),
                sector: "ai".to_string(),
                date: "2026-04-01".to_string(),
                source_name: "Test".to_string(),
                score: 1.0 - (i as f32 * 0.1),
                match_type: MatchType::Fts,
                source: super::super::search::StorySource::Daily,
            })
            .collect();

        let refs: Vec<&ScoredStory> = stories.iter().collect();
        let reordered = reorder_for_attention(&refs);

        // Small lists should not be reordered
        for (i, s) in reordered.iter().enumerate() {
            assert_eq!(s.story_id, i as i64, "small list should stay in original order");
        }
    }

    #[test]
    fn test_format_stories_context_empty() {
        let stories: Vec<super::super::search::ScoredStory> = vec![];
        let formatted = format_stories_context(&stories, crate::services::search::QueryType::General);
        assert_eq!(formatted, "No stories found in archive.");
    }

    #[test]
    fn test_format_signal_context() {
        let signals = vec![
            super::super::signals::Signal {
                id: 1,
                topic: "OpenAI".to_string(),
                sector: Some("ai".to_string()),
                window_7d: 10,
                window_30d: 20,
                window_90d: 40,
                acceleration: 2.14,
                trajectory: "emerging".to_string(),
                updated_at: "2026-03-31".to_string(),
            },
        ];
        let formatted = format_signal_context(&signals);
        assert!(formatted.contains("OpenAI"));
        assert!(formatted.contains("emerging"));
        assert!(formatted.contains("2.1"));
        assert!(formatted.contains("7d: 10"));
    }

    #[test]
    fn test_format_entity_context() {
        let entities = vec![(
            super::super::entities::Entity {
                id: 1,
                name: "OpenAI".to_string(),
                entity_type: "company".to_string(),
                sector: Some("ai".to_string()),
                first_seen: "2026-01-01".to_string(),
                last_seen: "2026-03-31".to_string(),
                mention_count: 15,
                sentiment_avg: 0.72,
            },
            vec![
                super::super::entities::EntityMention {
                    id: 1,
                    entity_id: 1,
                    story_id: 10,
                    sentiment: 0.8,
                    context: Some("OpenAI released a new model".to_string()),
                    mentioned_at: "2026-03-30".to_string(),
                },
                super::super::entities::EntityMention {
                    id: 2,
                    entity_id: 1,
                    story_id: 11,
                    sentiment: 0.6,
                    context: Some("OpenAI partnership announced".to_string()),
                    mentioned_at: "2026-03-31".to_string(),
                },
            ],
        )];

        let formatted = format_entity_context(&entities);
        assert!(formatted.contains("OpenAI (company)"));
        assert!(formatted.contains("15 mentions"));
        assert!(formatted.contains("0.72"));
        assert!(formatted.contains("released a new model"));
    }

    #[test]
    fn test_format_entity_context_empty() {
        let formatted = format_entity_context(&[]);
        assert_eq!(formatted, "No entity data available.");
    }

    #[test]
    fn test_format_signal_context_empty() {
        let formatted = format_signal_context(&[]);
        assert_eq!(formatted, "No signal data available.");
    }

    // -----------------------------------------------------------------------
    // System prompt
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_system_prompt_contains_sections() {
        let prompt = build_system_prompt(
            "Tech founder profile",
            "Story context here",
            "Entity context here",
            "Signal context here",
            "Causal context here",
            "Contrarian context here",
            "Pattern context here",
            "Predictions context here",
            "Predictions made: 10 total. Validated: 7 (70% accuracy).",
        );

        assert!(prompt.contains("You are Pulse"));
        assert!(prompt.contains("Tech founder profile"));
        assert!(prompt.contains("Story context here"));
        assert!(prompt.contains("Entity context here"));
        assert!(prompt.contains("Signal context here"));
        assert!(prompt.contains("[story:123]"));
        assert!(prompt.contains("[followup:"));
        assert!(prompt.contains("[prediction:"));
    }
}
