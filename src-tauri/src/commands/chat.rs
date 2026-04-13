use chrono::Timelike;
use serde::{Deserialize, Serialize};
use tauri::{ipc::Channel, State};
use crate::db::DbState;
use crate::services::{api_usage, brave_search, conversation, search, reranking, profile, embeddings, embeddings::EmbeddingProvider, entities, signals, causality, contrarian, patterns, predictions, feedback};

// ============ Streaming types ============

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum ChatStreamEvent {
    Searching { stage: String, detail: String },
    Delta { text: String },
    Complete {
        message: String,
        source_story_ids: Vec<i64>,
        suggested_followups: Vec<String>,
        thread_topic: String,
        thread_title: Option<String>,
        proactive_connections: Vec<conversation::ProactiveInsight>,
        search_source: String, // "archive", "web", or "archive+web"
        estimated_cost: f64,
        model_used: String,
    },
    Error { message: String },
}

// ============ Legacy Ask Pulse (backward compat) ============

const SONNET_MODEL: &str = "claude-sonnet-4-6";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub title: String,
    pub summary: String,
    pub key_points: Vec<String>,
    pub implications: String,
    pub watch_next: String,
    pub source_stories: Vec<SourceStory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStory {
    pub id: i64,
    pub headline: String,
    pub sector: String,
    pub date: String,
}

#[derive(Serialize)]
struct MessageRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

#[derive(Deserialize)]
struct StructuredAnswer {
    title: String,
    summary: String,
    key_points: Vec<String>,
    implications: String,
    watch_next: String,
}

#[tauri::command]
pub async fn ask_pulse(db: State<'_, DbState>, question: String) -> Result<ChatResponse, String> {
    let retrieved = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        search_stories_legacy(&conn, &question)?
    };

    if retrieved.is_empty() {
        return Ok(ChatResponse {
            title: "No Results".to_string(),
            summary: "No stories in the archive match your question. Try asking about topics from your daily briefings.".to_string(),
            key_points: vec![],
            implications: String::new(),
            watch_next: String::new(),
            source_stories: vec![],
        });
    }

    let mut context = String::new();
    let mut source_stories = Vec::new();

    for story in &retrieved {
        context.push_str(&format!(
            "\n---\n[Story ID: {}]\nDate: {}\nSector: {}\nHeadline: {}\nSummary: {}\nKey Facts: {}\nWhy It Matters: {}\n",
            story.id, story.date, story.sector, story.headline, story.summary, story.key_facts, story.why_it_matters
        ));
        source_stories.push(SourceStory {
            id: story.id,
            headline: story.headline.clone(),
            sector: story.sector.clone(),
            date: story.date.clone(),
        });
    }

    let system = format!(
        r#"You are Pulse, an AI news analyst. Answer questions using ONLY the retrieved stories below.

IMPORTANT: Return ONLY valid JSON with these exact keys:
{{
  "title": "Short analysis title (max 60 chars)",
  "summary": "2-3 sentence overview answering the question directly",
  "key_points": ["4-6 bullet points with specific facts, names, numbers from the stories"],
  "implications": "1-2 sentences on what this means for a tech founder building AI apps",
  "watch_next": "1 sentence on what to monitor going forward"
}}

Rules:
- Be specific. Use company names, numbers, dates from the stories.
- No markdown, no hashtags, no dashes. Just clean JSON.
- The reader is a tech founder in Miami Beach who builds AI/ML products.

Retrieved stories:
{}"#,
        context
    );

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY not set.".to_string())?;

    let client = reqwest::Client::new();
    let request = MessageRequest {
        model: SONNET_MODEL.to_string(),
        max_tokens: 1200,
        system,
        messages: vec![Message {
            role: "user".to_string(),
            content: question,
        }],
    };

    let resp = client
        .post(API_URL)
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Claude API error {}: {}", status, body));
    }

    let response: MessageResponse = resp.json().await.map_err(|e| format!("Parse error: {}", e))?;
    let text = response
        .content
        .first()
        .and_then(|c| c.text.clone())
        .unwrap_or_default();

    let json_str = extract_json(&text);

    let parsed: StructuredAnswer = serde_json::from_str(&json_str)
        .map_err(|e| format!("Failed to parse structured response: {}. Raw: {}", e, &text[..200.min(text.len())]))?;

    Ok(ChatResponse {
        title: parsed.title,
        summary: parsed.summary,
        key_points: parsed.key_points,
        implications: parsed.implications,
        watch_next: parsed.watch_next,
        source_stories,
    })
}

fn extract_json(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    }
    trimmed.to_string()
}

struct RetrievedStory {
    id: i64,
    headline: String,
    summary: String,
    key_facts: String,
    why_it_matters: String,
    sector: String,
    date: String,
}

fn search_stories_legacy(conn: &rusqlite::Connection, query: &str) -> Result<Vec<RetrievedStory>, String> {
    let fts_query = query
        .split_whitespace()
        .map(|word| {
            let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
            if clean.is_empty() { String::new() } else { format!("{}*", clean) }
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() {
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.sector, b.date
                 FROM stories s JOIN briefings b ON b.id = s.briefing_id
                 ORDER BY s.created_at DESC LIMIT 10",
            )
            .map_err(|e| e.to_string())?;

        return stmt
            .query_map([], |row| Ok(RetrievedStory {
                id: row.get(0)?, headline: row.get(1)?, summary: row.get(2)?,
                key_facts: row.get(3)?, why_it_matters: row.get(4)?,
                sector: row.get(5)?, date: row.get(6)?,
            }))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
    }

    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.sector, b.date
             FROM stories_fts fts JOIN stories s ON s.id = fts.rowid
             JOIN briefings b ON b.id = s.briefing_id
             WHERE stories_fts MATCH ?1 ORDER BY rank LIMIT 10",
        )
        .map_err(|e| e.to_string())?;

    let results: Vec<RetrievedStory> = stmt
        .query_map([&fts_query], |row| Ok(RetrievedStory {
            id: row.get(0)?, headline: row.get(1)?, summary: row.get(2)?,
            key_facts: row.get(3)?, why_it_matters: row.get(4)?,
            sector: row.get(5)?, date: row.get(6)?,
        }))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    if results.is_empty() {
        let like_pattern = format!("%{}%", query);
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.sector, b.date
                 FROM stories s JOIN briefings b ON b.id = s.briefing_id
                 WHERE s.headline LIKE ?1 OR s.summary LIKE ?1
                 ORDER BY s.relevance_score DESC LIMIT 10",
            )
            .map_err(|e| e.to_string())?;

        return stmt
            .query_map([&like_pattern], |row| Ok(RetrievedStory {
                id: row.get(0)?, headline: row.get(1)?, summary: row.get(2)?,
                key_facts: row.get(3)?, why_it_matters: row.get(4)?,
                sector: row.get(5)?, date: row.get(6)?,
            }))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
    }

    Ok(results)
}

// ============ New Intelligence Chat Commands ============

#[tauri::command]
pub async fn chat_send(
    db: State<'_, DbState>,
    thread_id: Option<String>,
    message: String,
) -> Result<conversation::ConversationResponse, String> {
    if message.len() > 20_000 {
        return Err("Message too long".to_string());
    }

    // 1. Classify topic and resolve thread
    let topic = conversation::classify_topic(&message);

    let (thread, is_new_thread) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        if let Some(tid) = &thread_id {
            // Use existing thread
            let threads = conversation::list_threads(&conn).map_err(|e| e.to_string())?;
            let thread = threads.into_iter().find(|t| t.id == *tid)
                .ok_or_else(|| "Thread not found".to_string())?;
            (thread, false)
        } else {
            // Try to find a recent thread for the same topic
            let recent = conversation::find_recent_thread(&conn, topic).map_err(|e| e.to_string())?;
            if let Some(t) = recent {
                (t, false)
            } else {
                let t = conversation::create_thread(&conn, topic).map_err(|e| e.to_string())?;
                (t, true)
            }
        }
    };

    // 2. Store user message
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::store_message(&conn, &thread.id, "user", &message, None, None)
            .map_err(|e| e.to_string())?;

        // Track user interests
        profile::track_interest(&conn, &format!("interest:{}", topic)).ok();
    }

    // 2.5. Load conversation context for context-aware query rewriting
    let conversation_context: Option<String> = if !is_new_thread {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let history = conversation::get_thread_messages(&conn, &thread.id)
            .map_err(|e| e.to_string())?;
        let recent: Vec<_> = history.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        if recent.is_empty() {
            None
        } else {
            Some(recent.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n"))
        }
    } else {
        None
    };

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY not set. Add it to .env".to_string())?;

    let query_type = search::classify_query_type(&message);
    tracing::info!("Query type: {:?}", query_type);

    // 3. Parallel: rewrite query + embed raw query concurrently
    let rewrite_fut = search::rewrite_query(&api_key, &message, conversation_context.as_deref());
    let embed_fut = async {
        match embeddings::VoyageProvider::from_env() {
            Ok(provider) => {
                match provider.embed(&[message.clone()], "query").await {
                    Ok(mut embs) if !embs.is_empty() => Some(embs.swap_remove(0)),
                    _ => None,
                }
            }
            Err(_) => None,
        }
    };
    let (expanded, query_embedding) = tokio::join!(rewrite_fut, embed_fut);

    // HyDE recovery + entity expansion
    let hyde_embedding: Option<Vec<f32>> = if expanded.semantic_text != expanded.original && expanded.semantic_text.len() > 20 {
        match embeddings::VoyageProvider::from_env() {
            Ok(provider) => provider.embed(&[expanded.semantic_text.clone()], "query").await.ok().and_then(|mut e| if e.is_empty() { None } else { Some(e.swap_remove(0)) }),
            Err(_) => None,
        }
    } else { None };

    let expanded_fts = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        search::expand_query_with_entities(&conn, &expanded.fts_keywords)
    };

    let stories = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        search::hybrid_search_with_hyde(&conn, &expanded_fts, query_embedding.as_deref(), hyde_embedding.as_deref(), 25, query_type, None)
            .map_err(|e| e.to_string())?
    };

    // 3.5. Voyage rerank (falls back to Haiku, then to original order)
    let stories = if stories.len() > 6 {
        let reranked = search::voyage_rerank(&expanded_fts, stories.clone(), 10).await;
        if reranked.len() == stories.len() && reranked.first().map(|s| s.story_id) == stories.first().map(|s| s.story_id) {
            reranking::llm_rerank(&api_key, &message, stories, 10).await
        } else {
            reranked
        }
    } else {
        stories
    };

    // 4. Gather intelligence context (filtered by query relevance)
    let keywords = extract_keywords(&message);

    let (entity_context, signal_context, causal_context, contrarian_context, pattern_context, predictions_context, profile_str) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;

        // Entity context: find entities mentioned in the query
        let entity_ctx = match entities::search_entities(&conn, &message) {
            Ok(ents) => {
                let mut pairs = Vec::new();
                for ent in ents.into_iter().take(5) {
                    // Limit to last 5 mentions per entity to avoid context bloat
                    let mut mentions = entities::get_entity_mentions(&conn, ent.id).unwrap_or_default();
                    if mentions.len() > 5 {
                        mentions = mentions.into_iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
                    }
                    pairs.push((ent, mentions));
                }
                conversation::format_entity_context(&pairs)
            }
            Err(_) => String::new(),
        };

        // Signal context: only signals relevant to the query (not global top)
        let signal_ctx = match signals::get_top_accelerating(&conn, 15) {
            Ok(sigs) => {
                let filtered: Vec<_> = if keywords.is_empty() {
                    sigs.into_iter().take(5).collect()
                } else {
                    sigs.into_iter()
                        .filter(|s| keywords_match_text(&s.topic, &keywords))
                        .take(5)
                        .collect()
                };
                conversation::format_signal_context(&filtered)
            }
            Err(_) => String::new(),
        };

        // Causal chains for the topic (already filtered by trigger)
        let causal_ctx = match causality::find_causal_chains(&conn, &message, 30, 2) {
            Ok(chains) if !chains.is_empty() => {
                chains.iter().map(|c| {
                    format!("When '{}' occurs, '{}' typically follows ~{:.0} days later ({}x observed, confidence: {:.0}%)",
                        c.trigger_event, c.consequence, c.avg_delay_days, c.occurrences, c.confidence * 100.0)
                }).collect::<Vec<_>>().join("\n")
            }
            _ => String::new(),
        };

        // Contrarian signals (already filtered by entity)
        let contrarian_ctx = match contrarian::detect_contrarian(&conn, &message, 30, 0.4) {
            Ok(Some(signal)) => {
                format!("Consensus sentiment: {:.1} ({} stories). But {} dissenting stories suggest otherwise.",
                    signal.consensus_sentiment, signal.total_stories, signal.dissent_count)
            }
            _ => String::new(),
        };

        // Cross-sector patterns: only those relevant to query
        let pattern_ctx = match patterns::detect_cross_sector_patterns(&conn, 5, 2, 30) {
            Ok(pats) if !pats.is_empty() => {
                let filtered: Vec<_> = if keywords.is_empty() {
                    pats
                } else {
                    pats.into_iter()
                        .filter(|p| {
                            keywords_match_text(&p.source_pattern, &keywords)
                            || keywords_match_text(&p.predicted_pattern, &keywords)
                            || keywords_match_text(&p.source_sector, &keywords)
                            || keywords_match_text(&p.target_sector, &keywords)
                        })
                        .collect()
                };
                if filtered.is_empty() {
                    String::new()
                } else {
                    filtered.iter().map(|p| {
                        format!("Pattern from {}: '{}' → Now emerging in {}: '{}' (confidence: {:.0}%)",
                            p.source_sector, p.source_pattern, p.target_sector, p.predicted_pattern, p.confidence * 100.0)
                    }).collect::<Vec<_>>().join("\n")
                }
            }
            _ => String::new(),
        };

        // Active predictions: try full query + individual keywords for broader recall
        let pred_ctx = {
            let mut all_preds = Vec::new();
            let mut seen_titles = std::collections::HashSet::new();
            // Full query match
            if let Ok(preds) = predictions::get_predictions_for_topic(&conn, &message) {
                for p in preds {
                    if seen_titles.insert(p.title.clone()) {
                        all_preds.push(p);
                    }
                }
            }
            // Individual keyword matches for broader recall
            for kw in &keywords {
                if let Ok(preds) = predictions::get_predictions_for_topic(&conn, kw) {
                    for p in preds {
                        if seen_titles.insert(p.title.clone()) {
                            all_preds.push(p);
                        }
                    }
                }
            }
            all_preds.truncate(5);
            if all_preds.is_empty() {
                String::new()
            } else {
                all_preds.iter().map(|p| {
                    format!("[{}] {} (confidence: {:.0}%, timeframe: {}, status: {})",
                        p.sector.as_deref().unwrap_or("general"), p.prediction, p.confidence * 100.0, p.predicted_timeframe, p.status)
                }).collect::<Vec<_>>().join("\n")
            }
        };

        // User profile
        let prof = match profile::get_profile(&conn) {
            Ok(p) => profile::profile_summary(&p),
            Err(_) => String::new(),
        };

        (entity_ctx, signal_ctx, causal_ctx, contrarian_ctx, pattern_ctx, pred_ctx, prof)
    };

    // 5. Build system prompt with prediction calibration
    let stories_context = conversation::format_stories_context(&stories, query_type);
    let prediction_calibration = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        match predictions::get_prediction_stats(&conn) {
            Ok(stats) if stats.total > 0 => {
                {
                    let mut cal = format!("Predictions made: {} total. Validated: {} ({:.0}% accuracy). Invalidated: {}. Active: {}.",
                        stats.total, stats.validated + stats.partially_validated,
                        stats.accuracy_rate * 100.0, stats.invalidated, stats.active);
                    if let Some(brier) = stats.avg_brier_score {
                        cal.push_str(&format!(" Brier score: {:.3} (0=perfect, 0.25=random).", brier));
                    }
                    cal
                }
            }
            _ => String::new(),
        }
    };
    let system_prompt = conversation::build_system_prompt(
        &profile_str,
        &stories_context,
        &entity_context,
        &signal_context,
        &causal_context,
        &contrarian_context,
        &pattern_context,
        &predictions_context,
        &prediction_calibration,
    );

    // 6. Load conversation history
    let history = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::get_thread_messages(&conn, &thread.id)
            .map_err(|e| e.to_string())?
    };

    let messages_for_llm: Vec<(String, String)> = history.iter()
        .rev().take(10).collect::<Vec<_>>().into_iter().rev()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    // 7. Call Claude (smart model selection)
    let llm = conversation::ClaudeConversation::from_env()
        .map_err(|e| e.to_string())?;

    let (chat_model, chat_max_tokens) = match query_type {
        search::QueryType::Analytical | search::QueryType::Comparative => {
            (conversation::CONVERSATION_MODEL_DEEP, 2000u32)
        }
        _ => (conversation::CONVERSATION_MODEL_FAST, 1200u32),
    };

    let raw_response = llm.send_message_with_model(&system_prompt, &messages_for_llm, chat_model, chat_max_tokens)
        .await
        .map_err(|e| format!("Claude error: {}", e))?;

    // 8. Parse response
    let source_ids = conversation::extract_story_references(&raw_response);
    let followups = conversation::extract_followups(&raw_response);
    let extracted_predictions = conversation::extract_predictions(&raw_response);
    let clean_message = conversation::clean_response(&raw_response);

    // 9. Find proactive connections (older similar stories)
    let proactive = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;

        // Use query embedding to find older related stories (deduplicated)
        let mut connections = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        if let (Some(first_story), Some(qe)) = (stories.first(), &query_embedding) {
            if let Ok(older) = embeddings::find_similar_older_than(&conn, qe, 7, 3, 0.5) {
                for (sid, _score) in older {
                    if !seen_ids.insert(sid) { continue; } // Skip duplicates
                    if let Ok(mut s) = conn.prepare(
                        "SELECT s.headline, b.date FROM stories s JOIN briefings b ON b.id = s.briefing_id WHERE s.id = ?1"
                    ) {
                        if let Ok(row) = s.query_row([sid], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        }) {
                            connections.push(conversation::ProactiveInsight {
                                story_id: sid,
                                headline: row.0,
                                date: row.1,
                                connection: format!("Related to '{}'", first_story.headline),
                            });
                        }
                    }
                }
            }
        }
        connections
    };

    // 10. Generate thread title if new
    let thread_title = if is_new_thread {
        // Use first few words of the message as a simple title
        let title = message.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        let title = if title.chars().count() > 50 {
            format!("{}...", title.chars().take(47).collect::<String>())
        } else {
            title
        };
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::update_thread_title(&conn, &thread.id, &title).ok();
        Some(title)
    } else {
        thread.title.clone()
    };

    // 11. Store assistant response
    let all_source_ids: Vec<i64> = source_ids.iter().copied()
        .chain(stories.iter().map(|s| s.story_id))
        .filter(|id| *id > 0) // Filter out encoded freedom story IDs (negative)
        .collect::<std::collections::HashSet<_>>()
        .into_iter().collect();
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::store_message(
            &conn,
            &thread.id,
            "assistant",
            &clean_message,
            Some(&all_source_ids),
            None,
        ).map_err(|e| e.to_string())?;
    }

    // 12. Store any predictions made
    if !extracted_predictions.is_empty() {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        for (pred_text, confidence, timeframe) in &extracted_predictions {
            let pred = predictions::Prediction {
                id: None,
                title: pred_text.chars().take(100).collect(),
                prediction: pred_text.clone(),
                confidence: *confidence,
                reasoning: format!("Generated in response to: {}", message),
                evidence_types: vec!["conversation".to_string()],
                evidence_story_ids: all_source_ids.clone(),
                predicted_timeframe: timeframe.clone(),
                sector: Some(topic.to_string()),
                status: "active".to_string(),
            };
            predictions::store_prediction(&conn, &pred).ok();
        }
    }

    Ok(conversation::ConversationResponse {
        message: clean_message,
        source_story_ids: all_source_ids,
        suggested_followups: followups,
        thread_topic: topic.to_string(),
        thread_title,
        proactive_connections: proactive,
    })
}

/// Streaming version of chat_send. Sends text deltas as they arrive from Claude,
/// then sends a Complete event with metadata (sources, followups, etc).
/// Steps 1-6 are identical to chat_send. Step 7 streams instead of blocking.
#[tauri::command]
pub async fn chat_send_stream(
    db: State<'_, DbState>,
    thread_id: Option<String>,
    message: String,
    on_event: Channel<ChatStreamEvent>,
) -> Result<(), String> {
    if message.len() > 20_000 {
        return Err("Message too long".to_string());
    }

    // --- Steps 1-6 identical to chat_send ---

    // 1. Classify topic and resolve thread
    let topic = conversation::classify_topic(&message);

    let (thread, is_new_thread) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        if let Some(tid) = &thread_id {
            let threads = conversation::list_threads(&conn).map_err(|e| e.to_string())?;
            let thread = threads.into_iter().find(|t| t.id == *tid)
                .ok_or_else(|| "Thread not found".to_string())?;
            (thread, false)
        } else {
            let recent = conversation::find_recent_thread(&conn, topic).map_err(|e| e.to_string())?;
            if let Some(t) = recent {
                (t, false)
            } else {
                let t = conversation::create_thread(&conn, topic).map_err(|e| e.to_string())?;
                (t, true)
            }
        }
    };

    // 2. Store user message
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::store_message(&conn, &thread.id, "user", &message, None, None)
            .map_err(|e| e.to_string())?;
        profile::track_interest(&conn, &format!("interest:{}", topic)).ok();
    }

    // --- Emit search progress events ---
    on_event.send(ChatStreamEvent::Searching {
        stage: "searching".into(),
        detail: "Searching your archive...".into(),
    }).ok();

    // 2.5. Load recent conversation history for context-aware query rewriting
    let conversation_context: Option<String> = if !is_new_thread {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let history = conversation::get_thread_messages(&conn, &thread.id)
            .map_err(|e| e.to_string())?;
        let recent: Vec<_> = history.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        if recent.is_empty() {
            None
        } else {
            Some(recent.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n"))
        }
    } else {
        None
    };

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY not set. Add it to .env".to_string())?;

    let query_type = search::classify_query_type(&message);
    tracing::info!("Query type: {:?}", query_type);

    // 3. Parallel: rewrite query + embed raw query concurrently (saves 2-3s)
    let rewrite_fut = search::rewrite_query(&api_key, &message, conversation_context.as_deref());
    let embed_fut = async {
        match embeddings::VoyageProvider::from_env() {
            Ok(provider) => {
                match provider.embed(&[message.clone()], "query").await {
                    Ok(mut embs) if !embs.is_empty() => Some(embs.swap_remove(0)),
                    _ => None,
                }
            }
            Err(_) => None,
        }
    };

    let (expanded, query_embedding) = tokio::join!(rewrite_fut, embed_fut);

    // 3.1 HyDE recovery: if rewrite produced a meaningfully different semantic text,
    // do a second embedding and pass both to hybrid_search for merged results
    let hyde_embedding: Option<Vec<f32>> = if expanded.semantic_text != expanded.original && expanded.semantic_text.len() > 20 {
        match embeddings::VoyageProvider::from_env() {
            Ok(provider) => {
                match provider.embed(&[expanded.semantic_text.clone()], "query").await {
                    Ok(mut embs) if !embs.is_empty() => Some(embs.swap_remove(0)),
                    _ => None,
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };

    // 3.2 Entity alias expansion
    let expanded_fts = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        search::expand_query_with_entities(&conn, &expanded.fts_keywords)
    };

    let stories = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        search::hybrid_search_with_hyde(
            &conn, &expanded_fts, query_embedding.as_deref(), hyde_embedding.as_deref(),
            25, query_type, None,
        ).map_err(|e| e.to_string())?
    };

    on_event.send(ChatStreamEvent::Searching {
        stage: "found".into(),
        detail: format!("Found {} stories", stories.len()),
    }).ok();

    // 3.5. Voyage rerank (falls back to Haiku, then to original order)
    let stories = if stories.len() > 6 {
        on_event.send(ChatStreamEvent::Searching {
            stage: "reranking".into(),
            detail: "Ranking by relevance...".into(),
        }).ok();
        let reranked = search::voyage_rerank(&expanded_fts, stories.clone(), 10).await;
        // If Voyage returned same order (fallback), try Haiku as secondary fallback
        if reranked.len() == stories.len() && reranked.first().map(|s| s.story_id) == stories.first().map(|s| s.story_id) {
            reranking::llm_rerank(&api_key, &message, stories, 10).await
        } else {
            reranked
        }
    } else {
        stories
    };

    // 3.6. Smart web search trigger (4 rules)
    let should_web_search = should_trigger_web_search(&message, &stories, &db)?;

    if should_web_search {
        on_event.send(ChatStreamEvent::Searching {
            stage: "web".into(),
            detail: "Searching the web...".into(),
        }).ok();
    }

    let (web_context, search_source) = if should_web_search {
        let quota_ok = {
            let conn = db.0.lock().map_err(|e| e.to_string())?;
            brave_search::check_quota(&conn).is_ok()
        };

        if !quota_ok {
            tracing::info!("Web search skipped: Tavily monthly limit reached");
            (None, "archive".to_string())
        } else {
            match brave_search::search(&message, 5).await {
                Ok(results) if !results.is_empty() => {
                    let conn = db.0.lock().map_err(|e| e.to_string())?;
                    brave_search::log_call(&conn);
                    drop(conn);

                    let ctx = brave_search::format_web_context(&results);
                    let source = if stories.is_empty() {
                        "web".to_string()
                    } else {
                        "archive+web".to_string()
                    };
                    (Some(ctx), source)
                }
                Err(e) => {
                    tracing::info!("Web search failed: {}", e);
                    (None, "archive".to_string())
                }
                _ => (None, "archive".to_string()),
            }
        }
    } else {
        (None, "archive".to_string())
    };

    // 4. Gather intelligence context (filtered)
    on_event.send(ChatStreamEvent::Searching {
        stage: "analyzing".into(),
        detail: "Analyzing patterns & signals...".into(),
    }).ok();

    let keywords = extract_keywords(&message);
    let (entity_context, signal_context, causal_context, contrarian_context, pattern_context, predictions_context, profile_str) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;

        let entity_ctx = match entities::search_entities(&conn, &message) {
            Ok(ents) => {
                let mut pairs = Vec::new();
                for ent in ents.into_iter().take(5) {
                    let mut mentions = entities::get_entity_mentions(&conn, ent.id).unwrap_or_default();
                    if mentions.len() > 5 {
                        mentions = mentions.into_iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
                    }
                    pairs.push((ent, mentions));
                }
                conversation::format_entity_context(&pairs)
            }
            Err(_) => String::new(),
        };

        let signal_ctx = match signals::get_top_accelerating(&conn, 15) {
            Ok(sigs) => {
                let filtered: Vec<_> = if keywords.is_empty() {
                    sigs.into_iter().take(5).collect()
                } else {
                    sigs.into_iter().filter(|s| keywords_match_text(&s.topic, &keywords)).take(5).collect()
                };
                conversation::format_signal_context(&filtered)
            }
            Err(_) => String::new(),
        };

        let causal_ctx = match causality::find_causal_chains(&conn, &message, 30, 2) {
            Ok(chains) if !chains.is_empty() => {
                chains.iter().map(|c| {
                    format!("When '{}' occurs, '{}' typically follows ~{:.0} days later ({}x observed, confidence: {:.0}%)",
                        c.trigger_event, c.consequence, c.avg_delay_days, c.occurrences, c.confidence * 100.0)
                }).collect::<Vec<_>>().join("\n")
            }
            _ => String::new(),
        };

        let contrarian_ctx = match contrarian::detect_contrarian(&conn, &message, 30, 0.4) {
            Ok(Some(signal)) => {
                format!("Consensus sentiment: {:.1} ({} stories). But {} dissenting stories suggest otherwise.",
                    signal.consensus_sentiment, signal.total_stories, signal.dissent_count)
            }
            _ => String::new(),
        };

        let pattern_ctx = match patterns::detect_cross_sector_patterns(&conn, 5, 2, 30) {
            Ok(pats) if !pats.is_empty() => {
                let filtered: Vec<_> = if keywords.is_empty() { pats } else {
                    pats.into_iter().filter(|p| {
                        keywords_match_text(&p.source_pattern, &keywords)
                        || keywords_match_text(&p.predicted_pattern, &keywords)
                        || keywords_match_text(&p.source_sector, &keywords)
                        || keywords_match_text(&p.target_sector, &keywords)
                    }).collect()
                };
                if filtered.is_empty() { String::new() } else {
                    filtered.iter().map(|p| {
                        format!("Pattern from {}: '{}' → Now emerging in {}: '{}' (confidence: {:.0}%)",
                            p.source_sector, p.source_pattern, p.target_sector, p.predicted_pattern, p.confidence * 100.0)
                    }).collect::<Vec<_>>().join("\n")
                }
            }
            _ => String::new(),
        };

        let pred_ctx = {
            let mut all_preds = Vec::new();
            let mut seen_titles = std::collections::HashSet::new();
            if let Ok(preds) = predictions::get_predictions_for_topic(&conn, &message) {
                for p in preds { if seen_titles.insert(p.title.clone()) { all_preds.push(p); } }
            }
            for kw in &keywords {
                if let Ok(preds) = predictions::get_predictions_for_topic(&conn, kw) {
                    for p in preds { if seen_titles.insert(p.title.clone()) { all_preds.push(p); } }
                }
            }
            all_preds.truncate(5);
            if all_preds.is_empty() { String::new() } else {
                all_preds.iter().map(|p| {
                    format!("[{}] {} (confidence: {:.0}%, timeframe: {}, status: {})",
                        p.sector.as_deref().unwrap_or("general"), p.prediction, p.confidence * 100.0, p.predicted_timeframe, p.status)
                }).collect::<Vec<_>>().join("\n")
            }
        };

        let prof = match profile::get_profile(&conn) { Ok(p) => profile::profile_summary(&p), Err(_) => String::new() };
        (entity_ctx, signal_ctx, causal_ctx, contrarian_ctx, pattern_ctx, pred_ctx, prof)
    };

    // 5. Build system prompt with prediction calibration
    let stories_context = conversation::format_stories_context(&stories, query_type);
    let prediction_calibration = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        match predictions::get_prediction_stats(&conn) {
            Ok(stats) if stats.total > 0 => {
                {
                    let mut cal = format!("Predictions made: {} total. Validated: {} ({:.0}% accuracy). Invalidated: {}. Active: {}.",
                        stats.total, stats.validated + stats.partially_validated,
                        stats.accuracy_rate * 100.0, stats.invalidated, stats.active);
                    if let Some(brier) = stats.avg_brier_score {
                        cal.push_str(&format!(" Brier score: {:.3} (0=perfect, 0.25=random).", brier));
                    }
                    cal
                }
            }
            _ => String::new(),
        }
    };
    let mut system_prompt = conversation::build_system_prompt(
        &profile_str, &stories_context, &entity_context, &signal_context,
        &causal_context, &contrarian_context, &pattern_context, &predictions_context,
        &prediction_calibration,
    );

    // Append web search results if available
    if let Some(ref wc) = web_context {
        system_prompt.push_str(&format!(
            "\n\nWEB SEARCH RESULTS (live web — use when archive doesn't cover the topic):\n{}",
            wc
        ));
    }

    // 6. Load conversation history
    let history = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::get_thread_messages(&conn, &thread.id).map_err(|e| e.to_string())?
    };
    let messages_for_llm: Vec<(String, String)> = history.iter()
        .rev().take(10).collect::<Vec<_>>().into_iter().rev()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    // --- Step 7: Stream Claude response ---
    on_event.send(ChatStreamEvent::Searching {
        stage: "thinking".into(),
        detail: "Generating analysis...".into(),
    }).ok();

    let llm = conversation::ClaudeConversation::from_env().map_err(|e| e.to_string())?;

    // Smart model selection: Haiku for simple queries, Sonnet for complex analysis
    let (chat_model, chat_max_tokens) = match query_type {
        search::QueryType::Analytical | search::QueryType::Comparative => {
            tracing::info!("Using Sonnet (complex query)");
            (conversation::CONVERSATION_MODEL_DEEP, 2000u32)
        }
        _ => {
            tracing::info!("Using Haiku (simple query)");
            (conversation::CONVERSATION_MODEL_FAST, 1200u32)
        }
    };

    let on_event_clone = on_event.clone();
    let raw_response = llm.send_message_stream(
        &system_prompt,
        &messages_for_llm,
        chat_model,
        chat_max_tokens,
        move |chunk| {
            if let Err(e) = on_event_clone.send(ChatStreamEvent::Delta { text: chunk.to_string() }) {
                tracing::warn!("failed to send Delta event: {}", e);
            }
        },
    )
    .await
    .map_err(|e| {
        on_event.send(ChatStreamEvent::Error { message: e.to_string() }).ok();
        format!("Claude error: {}", e)
    })?;

    // --- Steps 8-12: Parse, store, send Complete ---
    let source_ids = conversation::extract_story_references(&raw_response);
    let followups = conversation::extract_followups(&raw_response);
    let extracted_predictions = conversation::extract_predictions(&raw_response);
    let clean_message = conversation::clean_response(&raw_response);

    // Proactive connections
    let proactive = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let mut connections = Vec::new();
        if let (Some(first_story), Some(qe)) = (stories.first(), &query_embedding) {
            if let Ok(older) = embeddings::find_similar_older_than(&conn, qe, 7, 3, 0.5) {
                for (sid, _score) in older {
                    if let Ok(mut s) = conn.prepare("SELECT s.headline, b.date FROM stories s JOIN briefings b ON b.id = s.briefing_id WHERE s.id = ?1") {
                        if let Ok(row) = s.query_row([sid], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                            connections.push(conversation::ProactiveInsight {
                                story_id: sid, headline: row.0, date: row.1,
                                connection: format!("Related to '{}'", first_story.headline),
                            });
                        }
                    }
                }
            }
        }
        connections
    };

    // Thread title — use Haiku for a concise title, fall back to first words
    let thread_title = if is_new_thread {
        let title = generate_thread_title(&api_key, &message, &clean_message).await;
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::update_thread_title(&conn, &thread.id, &title).ok();
        Some(title)
    } else {
        thread.title.clone()
    };

    // Store assistant response
    let all_source_ids: Vec<i64> = source_ids.iter().copied()
        .chain(stories.iter().map(|s| s.story_id))
        .filter(|id| *id > 0) // Filter out encoded freedom story IDs (negative)
        .collect::<std::collections::HashSet<_>>()
        .into_iter().collect();
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        conversation::store_message(&conn, &thread.id, "assistant", &clean_message, Some(&all_source_ids), None)
            .map_err(|e| e.to_string())?;
    }

    // Store predictions
    if !extracted_predictions.is_empty() {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        for (pred_text, confidence, timeframe) in &extracted_predictions {
            let pred = predictions::Prediction {
                id: None,
                title: pred_text.chars().take(100).collect(),
                prediction: pred_text.clone(),
                confidence: *confidence,
                reasoning: format!("Generated in response to: {}", message),
                evidence_types: vec!["conversation".to_string()],
                evidence_story_ids: all_source_ids.clone(),
                predicted_timeframe: timeframe.clone(),
                sector: Some(topic.to_string()),
                status: "active".to_string(),
            };
            predictions::store_prediction(&conn, &pred).ok();
        }
    }

    // Log token usage (best-effort estimate) and compute cost
    let estimated_cost = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let input_est = (system_prompt.len() + message.len()) as i64 / 4;
        let output_est = clean_message.len() as i64 / 4;
        let model_label = if chat_model.contains("haiku") { "claude-haiku" } else { "claude-sonnet" };
        api_usage::log_usage(&conn, "anthropic", model_label, "chat", input_est, output_est).ok();
        // Estimate cost using pricing table
        let cost = api_usage::estimate_cost("anthropic", model_label, input_est, output_est);
        // Add ~$0.002 for rewrite + embedding + rerank overhead
        cost + 0.002
    };

    let model_label_display = if chat_model.contains("haiku") { "Haiku" } else { "Sonnet" }.to_string();

    // Send Complete event with cleaned message text
    if let Err(e) = on_event.send(ChatStreamEvent::Complete {
        message: clean_message,
        source_story_ids: all_source_ids,
        suggested_followups: followups,
        thread_topic: topic.to_string(),
        thread_title,
        proactive_connections: proactive,
        search_source,
        estimated_cost,
        model_used: model_label_display,
    }) {
        tracing::warn!("failed to send Complete event: {}", e);
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSuggestion {
    pub text: String,
    pub category: String, // "signal", "story", "prediction", "general"
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatContext {
    pub greeting: String,
    pub suggestions: Vec<ChatSuggestion>,
    pub story_count: i64,
    pub briefing_days: i64,
    pub entity_count: i64,
}

#[tauri::command]
pub fn get_chat_context(db: State<'_, DbState>) -> Result<ChatContext, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    // Counts for greeting
    let story_count: i64 = conn.query_row("SELECT COUNT(*) FROM stories", [], |r| r.get(0)).unwrap_or(0);
    let briefing_days: i64 = conn.query_row("SELECT COUNT(DISTINCT date) FROM briefings WHERE briefing_type = 'daily'", [], |r| r.get(0)).unwrap_or(0);
    let entity_count: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0)).unwrap_or(0);

    let mut suggestions = Vec::new();

    // Top accelerating signals → "What's driving the surge in [topic]?"
    if let Ok(sigs) = signals::get_top_accelerating(&conn, 3) {
        for sig in sigs {
            if sig.acceleration > 1.5 {
                suggestions.push(ChatSuggestion {
                    text: format!("What's driving the surge in {}?", sig.topic),
                    category: "signal".into(),
                });
            }
        }
    }

    // Today's hero story → "Tell me more about [headline]"
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let hero: Option<String> = conn.query_row(
        "SELECT s.headline FROM stories s JOIN briefings b ON b.id = s.briefing_id
         WHERE b.date = ?1 AND s.is_hero = 1 AND b.briefing_type = 'daily'
         ORDER BY s.importance_score DESC LIMIT 1",
        [&today],
        |row| row.get(0),
    ).ok();
    if let Some(headline) = hero {
        let short = if headline.len() > 60 {
            format!("{}...", &headline[..57])
        } else {
            headline
        };
        suggestions.push(ChatSuggestion {
            text: format!("Tell me more about {}", short),
            category: "story".into(),
        });
    }

    // Active predictions → "Update on your prediction about [topic]"
    if let Ok(preds) = predictions::get_active_predictions(&conn) {
        let preds: Vec<_> = preds.into_iter().take(2).collect();
        for pred in preds {
            let short = if pred.title.len() > 40 {
                format!("{}...", &pred.title[..37])
            } else {
                pred.title.clone()
            };
            suggestions.push(ChatSuggestion {
                text: format!("Update on the prediction: {}", short),
                category: "prediction".into(),
            });
        }
    }

    // Always include a general fallback
    if suggestions.len() < 3 {
        suggestions.push(ChatSuggestion {
            text: "What patterns are emerging across all sectors?".into(),
            category: "general".into(),
        });
    }

    suggestions.truncate(4);

    // Time-aware greeting
    let hour = chrono::Local::now().hour();
    let time_greeting = if hour < 12 { "Good morning" } else if hour < 17 { "Good afternoon" } else { "Good evening" };

    let greeting = if story_count > 0 {
        format!("{}. Your archive spans {} stories across {} days.", time_greeting, story_count, briefing_days)
    } else {
        format!("{}. Ask me anything about the news.", time_greeting)
    };

    Ok(ChatContext {
        greeting,
        suggestions,
        story_count,
        briefing_days,
        entity_count,
    })
}

#[tauri::command]
pub fn chat_list_threads(db: State<'_, DbState>) -> Result<Vec<conversation::ChatThread>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conversation::list_threads(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn chat_get_thread(db: State<'_, DbState>, thread_id: String) -> Result<Vec<conversation::ChatMessage>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conversation::get_thread_messages(&conn, &thread_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn chat_delete_thread(db: State<'_, DbState>, thread_id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conversation::delete_thread(&conn, &thread_id).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Smart web search trigger
// ---------------------------------------------------------------------------

/// Decide whether to trigger a web search based on 4 rules:
/// 1. Comparison questions ("X vs Y", "best tools", "alternatives to")
/// 2. Time-sensitive words ("latest", "today", "just released", "new")
/// 3. Low archive confidence (<5 results)
/// 4. Unknown entities (query keywords not found in archive entities)
fn should_trigger_web_search(
    message: &str,
    stories: &[search::ScoredStory],
    db: &State<'_, crate::db::DbState>,
) -> Result<bool, String> {
    let lower = message.to_lowercase();

    // Rule 1: Comparison questions
    const COMPARISON_PATTERNS: &[&str] = &[
        " vs ", " versus ", "compare", "comparison", "better than",
        "best tool", "best app", "alternatives to", "alternative to",
        "instead of", "competitor", "which is better", "difference between",
    ];
    if COMPARISON_PATTERNS.iter().any(|p| lower.contains(p)) {
        tracing::info!("Web search triggered: comparison question");
        return Ok(true);
    }

    // Rule 2: Time-sensitive words
    const TIME_PATTERNS: &[&str] = &[
        "latest", "newest", "just released", "just launched", "just came out",
        "this week", "this month", "today", "yesterday", "right now",
        "current state", "as of", "new version", "new release", "new update",
        "recently", "breaking", "announce",
    ];
    if TIME_PATTERNS.iter().any(|p| lower.contains(p)) {
        tracing::info!("Web search triggered: time-sensitive query");
        return Ok(true);
    }

    // Rule 3: Low archive confidence
    if stories.len() < 5 {
        tracing::info!("Web search triggered: low archive results ({})", stories.len());
        return Ok(true);
    }

    // Rule 4: Unknown entities — check if main keywords exist in entities table
    let keywords = extract_keywords(message);
    if !keywords.is_empty() {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let known_count: i64 = keywords.iter().filter(|kw| {
            conn.query_row(
                "SELECT COUNT(*) FROM entities WHERE name_normalized LIKE '%' || ?1 || '%'",
                [kw.as_str()],
                |row| row.get::<_, i64>(0),
            ).unwrap_or(0) > 0
        }).count() as i64;

        let total = keywords.len() as i64;
        // If less than half of keywords are known entities, search the web
        if total > 0 && known_count < (total + 1) / 2 {
            tracing::info!("Web search triggered: unknown entities ({}/{} known)", known_count, total);
            return Ok(true);
        }
    }

    Ok(false)
}

// ---------------------------------------------------------------------------
// Keyword helpers for intelligence layer filtering
// ---------------------------------------------------------------------------

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can",
    "has", "her", "was", "one", "our", "out", "what", "when", "who",
    "how", "why", "this", "that", "with", "from", "about", "been",
    "have", "will", "more", "some", "than", "them", "into", "most",
    "could", "would", "should", "there", "their", "which", "being",
    "does", "doing", "going", "happening", "tell", "know",
];

/// Extract meaningful lowercased keywords from text (3+ chars, no stopwords).
fn extract_keywords(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.to_lowercase())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Generate a concise thread title using Haiku. Falls back to first words on failure.
async fn generate_thread_title(api_key: &str, question: &str, answer: &str) -> String {
    let fallback = || {
        let title: String = question.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        if title.chars().count() > 50 {
            format!("{}...", title.chars().take(47).collect::<String>())
        } else {
            title
        }
    };

    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build() {
        Ok(c) => c,
        Err(_) => return fallback(),
    };

    let body = serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 30,
        "system": "Generate a 3-5 word title for this conversation. No quotes, no punctuation, just the topic. Examples: 'OpenAI GPT-5 Analysis', 'Miami Tech Funding', 'EU AI Regulation Impact'",
        "messages": [{"role": "user", "content": format!("Q: {}\nA: {}", &question[..question.len().min(200)], &answer[..answer.len().min(300)])}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                let title = json["content"][0]["text"].as_str().unwrap_or("").trim().to_string();
                if !title.is_empty() && title.len() < 60 {
                    return title;
                }
            }
            fallback()
        }
        _ => fallback(),
    }
}

/// Check if any keyword appears as a substring in the target text.
fn keywords_match_text(text: &str, keywords: &[String]) -> bool {
    let lower = text.to_lowercase();
    keywords.iter().any(|kw| lower.contains(kw.as_str()))
}

// ============ Chat Feedback ============

#[tauri::command]
pub async fn submit_chat_feedback(
    db: State<'_, DbState>,
    message_id: String,
    thread_id: String,
    rating: String,
    reason: Option<String>,
) -> Result<(), String> {
    if rating != "up" && rating != "down" {
        return Err("rating must be 'up' or 'down'".to_string());
    }

    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO chat_feedback (message_id, thread_id, rating, reason) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![message_id, thread_id, rating, reason],
    )
    .map_err(|e| e.to_string())?;

    // Propagate feedback to source/sector reputation scores (non-fatal)
    if let Err(e) = feedback::propagate_feedback(&conn, &message_id) {
        tracing::warn!("Feedback propagation failed (non-fatal): {}", e);
    }

    Ok(())
}

#[tauri::command]
pub fn get_feedback_stats(db: State<'_, DbState>) -> Result<serde_json::Value, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let total_up: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chat_feedback WHERE rating = 'up'", [], |r| r.get(0),
    ).unwrap_or(0);
    let total_down: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chat_feedback WHERE rating = 'down'", [], |r| r.get(0),
    ).unwrap_or(0);

    // Top and bottom sources by reputation
    let mut top_sources = Vec::new();
    let mut bottom_sources = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT key, reputation, boost, upvotes, downvotes FROM feedback_reputation
         WHERE kind = 'source' ORDER BY reputation DESC"
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "name": row.get::<_, String>(0)?.replace("source:", ""),
                "reputation": row.get::<_, f64>(1)?,
                "boost": row.get::<_, f64>(2)?,
                "upvotes": row.get::<_, i64>(3)?,
                "downvotes": row.get::<_, i64>(4)?,
            }))
        }) {
            let all: Vec<_> = rows.flatten().collect();
            top_sources = all.iter().take(5).cloned().collect();
            bottom_sources = all.iter().rev().take(5).cloned().collect();
        }
    }

    // Sector reputations
    let mut sectors = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT key, reputation, boost, upvotes, downvotes FROM feedback_reputation
         WHERE kind = 'sector' ORDER BY reputation DESC"
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "name": row.get::<_, String>(0)?.replace("sector:", ""),
                "reputation": row.get::<_, f64>(1)?,
                "boost": row.get::<_, f64>(2)?,
                "upvotes": row.get::<_, i64>(3)?,
                "downvotes": row.get::<_, i64>(4)?,
            }))
        }) {
            sectors = rows.flatten().collect();
        }
    }

    Ok(serde_json::json!({
        "total_up": total_up,
        "total_down": total_down,
        "top_sources": top_sources,
        "bottom_sources": bottom_sources,
        "sectors": sectors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords() {
        let kws = extract_keywords("What's happening with OpenAI regulation?");
        assert!(kws.contains(&"openai".to_string()));
        assert!(kws.contains(&"regulation".to_string()));
        assert!(!kws.contains(&"what".to_string())); // stopword
        assert!(!kws.contains(&"with".to_string())); // stopword
    }

    #[test]
    fn test_extract_keywords_short_words_filtered() {
        let kws = extract_keywords("Is AI ok?");
        assert!(!kws.contains(&"is".to_string())); // 2 chars
        assert!(!kws.contains(&"ai".to_string())); // 2 chars
        assert!(!kws.contains(&"ok".to_string())); // 2 chars
    }

    #[test]
    fn test_keywords_match_text() {
        let keywords = vec!["openai".to_string(), "regulation".to_string()];
        assert!(keywords_match_text("OpenAI launches new model", &keywords));
        assert!(keywords_match_text("EU regulation framework", &keywords));
        assert!(!keywords_match_text("Google releases Gemini", &keywords));
    }

    #[test]
    fn test_keywords_match_text_empty() {
        let empty: Vec<String> = vec![];
        assert!(!keywords_match_text("anything", &empty));
    }
}
