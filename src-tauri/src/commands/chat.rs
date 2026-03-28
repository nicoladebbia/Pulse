use serde::{Deserialize, Serialize};
use tauri::State;
use crate::db::DbState;

const SONNET_MODEL: &str = "claude-sonnet-4-6";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub answer: String,
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

#[tauri::command]
pub async fn ask_pulse(db: State<'_, DbState>, question: String) -> Result<ChatResponse, String> {
    // 1. Search the archive using FTS5
    let retrieved = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        search_stories(&conn, &question)?
    };

    if retrieved.is_empty() {
        return Ok(ChatResponse {
            answer: "I don't have any stories in the archive that match your question. Try asking about topics from your daily briefings.".to_string(),
            source_stories: vec![],
        });
    }

    // 2. Build the RAG prompt
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
        r#"You are Pulse, an AI news analyst assistant. You have access to a curated archive of daily news briefings. Answer questions by synthesizing information from the retrieved stories below.

Rules:
- Only use information from the provided stories. If the archive doesn't contain relevant information, say so.
- When citing, reference story headlines naturally: "According to [headline]..."
- Connect dots across sectors when relevant.
- Be concise but thorough. The reader is a tech founder — no hand-holding.
- If asked about trends, reference the timeline of stories on that topic.

Retrieved stories (ranked by relevance):
{}"#,
        context
    );

    // 3. Call Claude Sonnet
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY not set. Add it to your .env file.".to_string())?;

    let client = reqwest::Client::new();
    let request = MessageRequest {
        model: SONNET_MODEL.to_string(),
        max_tokens: 1500,
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
    let answer = response
        .content
        .first()
        .and_then(|c| c.text.clone())
        .unwrap_or_else(|| "No response generated.".to_string());

    Ok(ChatResponse {
        answer,
        source_stories,
    })
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

fn search_stories(conn: &rusqlite::Connection, query: &str) -> Result<Vec<RetrievedStory>, String> {
    // Clean the query for FTS5 — escape special chars and add wildcards
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
        // Fallback: return most recent stories
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.sector,
                        b.date
                 FROM stories s
                 JOIN briefings b ON b.id = s.briefing_id
                 ORDER BY s.created_at DESC
                 LIMIT 10",
            )
            .map_err(|e| e.to_string())?;

        return stmt
            .query_map([], |row| {
                Ok(RetrievedStory {
                    id: row.get(0)?,
                    headline: row.get(1)?,
                    summary: row.get(2)?,
                    key_facts: row.get(3)?,
                    why_it_matters: row.get(4)?,
                    sector: row.get(5)?,
                    date: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
    }

    // Try FTS5 search first
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.sector,
                    b.date
             FROM stories_fts fts
             JOIN stories s ON s.id = fts.rowid
             JOIN briefings b ON b.id = s.briefing_id
             WHERE stories_fts MATCH ?1
             ORDER BY rank
             LIMIT 10",
        )
        .map_err(|e| e.to_string())?;

    let results: Vec<RetrievedStory> = stmt
        .query_map([&fts_query], |row| {
            Ok(RetrievedStory {
                id: row.get(0)?,
                headline: row.get(1)?,
                summary: row.get(2)?,
                key_facts: row.get(3)?,
                why_it_matters: row.get(4)?,
                sector: row.get(5)?,
                date: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    // If FTS returns nothing, fall back to LIKE search
    if results.is_empty() {
        let like_pattern = format!("%{}%", query);
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.headline, s.summary, s.key_facts, s.why_it_matters, s.sector,
                        b.date
                 FROM stories s
                 JOIN briefings b ON b.id = s.briefing_id
                 WHERE s.headline LIKE ?1 OR s.summary LIKE ?1
                 ORDER BY s.relevance_score DESC
                 LIMIT 10",
            )
            .map_err(|e| e.to_string())?;

        return stmt
            .query_map([&like_pattern], |row| {
                Ok(RetrievedStory {
                    id: row.get(0)?,
                    headline: row.get(1)?,
                    summary: row.get(2)?,
                    key_facts: row.get(3)?,
                    why_it_matters: row.get(4)?,
                    sector: row.get(5)?,
                    date: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
    }

    Ok(results)
}
