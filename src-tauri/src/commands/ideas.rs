use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::{ipc::Channel, State};

use crate::db::DbState;
use crate::services::{brave_search, conversation, conversation::ConversationLLM};

// ============ Types ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIdea {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub why_now: String,
    pub competitors: Vec<Competitor>,
    pub differentiation: String,
    pub tech_stack: String,
    pub difficulty: String,
    pub relevance_score: f64,
    pub source_story_ids: Vec<i64>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Competitor {
    pub name: String,
    pub url: String,
    pub weakness: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum IdeaStreamEvent {
    Progress { stage: String, detail: String },
    Complete { ideas: Vec<ProjectIdea> },
    Error { message: String },
}

#[derive(Deserialize)]
struct IdeaResponse {
    ideas: Vec<RawIdea>,
}

#[derive(Deserialize)]
struct RawIdea {
    title: String,
    description: String,
    why_now: String,
    differentiation: String,
    tech_stack: String,
    difficulty: String,
    relevance_score: f64,
    source_story_indices: Vec<usize>,
    competitor_queries: Vec<String>,
}

// ============ Commands ============

#[tauri::command]
pub async fn generate_ideas(
    db: State<'_, DbState>,
    on_event: Channel<IdeaStreamEvent>,
) -> Result<(), String> {
    // 1. Load recent stories (7 days)
    on_event
        .send(IdeaStreamEvent::Progress {
            stage: "loading".to_string(),
            detail: "Loading recent stories...".to_string(),
        })
        .ok();

    let (stories_text, story_ids, headlines) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.headline, s.summary, s.sector, s.why_it_matters
                 FROM stories s
                 JOIN briefings b ON b.id = s.briefing_id
                 WHERE b.date >= date('now', '-7 days')
                 ORDER BY s.relevance_score DESC NULLS LAST, s.importance_score DESC
                 LIMIT 40",
            )
            .map_err(|e| e.to_string())?;

        let rows: Vec<(i64, String, String, String, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        if rows.is_empty() {
            on_event
                .send(IdeaStreamEvent::Error {
                    message: "No recent stories found. Run a fetch first.".to_string(),
                })
                .ok();
            return Ok(());
        }

        let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
        let headlines: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
        let text: String = rows
            .iter()
            .enumerate()
            .map(|(i, (_, headline, summary, sector, why))| {
                format!("[{}] [{}] {}\n{}\nWhy it matters: {}", i, sector, headline, summary, why)
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        (text, ids, headlines)
    };

    // 2. Generate ideas with Claude
    on_event
        .send(IdeaStreamEvent::Progress {
            stage: "generating".to_string(),
            detail: "Analyzing trends and generating project ideas...".to_string(),
        })
        .ok();

    let llm =
        conversation::ClaudeConversation::from_env().map_err(|e| e.to_string())?;

    let system_prompt = r#"You are a startup idea analyst for a tech founder who builds AI/ML apps, Shopify tools, iOS apps, and web products using Claude Code. He ships fast (weekend-to-week projects) and values practical, monetizable ideas.

Analyze the recent news stories below and generate 3-5 actionable project ideas. Each idea should:
1. Be inspired by trends or gaps you spot in the news
2. Be buildable by a solo developer in a weekend to a month
3. Have a clear differentiation angle
4. Include 1-2 search queries to find existing competitors

Respond ONLY with valid JSON in this exact format:
{
  "ideas": [
    {
      "title": "Short catchy name",
      "description": "What to build — 2-3 sentences",
      "why_now": "Why this is timely based on the news — reference specific stories by index",
      "differentiation": "What makes this unique vs existing solutions",
      "tech_stack": "Suggested tech stack",
      "difficulty": "weekend|week|month",
      "relevance_score": 0.0-1.0,
      "source_story_indices": [0, 3, 7],
      "competitor_queries": ["query to search for competitors", "another query"]
    }
  ]
}"#;

    let messages = vec![("user".to_string(), format!("Recent news stories:\n\n{}", stories_text))];

    let response: String = llm
        .send_message(system_prompt, &messages)
        .await
        .map_err(|e| {
            on_event
                .send(IdeaStreamEvent::Error {
                    message: format!("Claude error: {}", e),
                })
                .ok();
            e.to_string()
        })?;

    // Parse response
    let json_str = extract_json(&response);
    let parsed: IdeaResponse = serde_json::from_str(&json_str).map_err(|e| {
        on_event
            .send(IdeaStreamEvent::Error {
                message: format!("Failed to parse ideas: {}", e),
            })
            .ok();
        e.to_string()
    })?;

    // 3. Research competitors for each idea
    on_event
        .send(IdeaStreamEvent::Progress {
            stage: "researching".to_string(),
            detail: "Researching competitors...".to_string(),
        })
        .ok();

    let mut ideas = Vec::new();
    for raw in &parsed.ideas {
        let mut competitors = Vec::new();

        for query in raw.competitor_queries.iter().take(2) {
            // Check quota first
            let quota_ok = {
                let conn = db.0.lock().map_err(|e| e.to_string())?;
                brave_search::check_quota(&conn).is_ok()
            };
            if !quota_ok {
                tracing::info!("Skipping competitor search: Tavily limit reached");
                break;
            }

            match brave_search::search(query, 3).await {
                Ok(results) => {
                    // Log successful call
                    let conn = db.0.lock().map_err(|e| e.to_string())?;
                    brave_search::log_call(&conn);
                    drop(conn);

                    for r in results {
                        competitors.push(Competitor {
                            name: r.title,
                            url: r.url,
                            weakness: r.description,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("Tavily search failed for '{}': {}", query, e);
                }
            }
        }

        let source_ids: Vec<i64> = raw
            .source_story_indices
            .iter()
            .filter_map(|&i| story_ids.get(i).copied())
            .collect();

        // Resolve story references in why_now (e.g. "Story 38" → actual headline)
        let mut why_now = raw.why_now.clone();
        for &idx in &raw.source_story_indices {
            if let Some(headline) = headlines.get(idx) {
                // Replace patterns like "Story 38", "story 38", "[38]"
                let patterns = [
                    format!("Story {}", idx),
                    format!("story {}", idx),
                    format!("[{}]", idx),
                ];
                for pat in &patterns {
                    why_now = why_now.replace(pat, &format!("\"{}\"", headline));
                }
            }
        }

        ideas.push(ProjectIdea {
            id: 0, // will be set on DB insert
            title: raw.title.clone(),
            description: raw.description.clone(),
            why_now,
            competitors,
            differentiation: raw.differentiation.clone(),
            tech_stack: raw.tech_stack.clone(),
            difficulty: raw.difficulty.clone(),
            relevance_score: raw.relevance_score,
            source_story_ids: source_ids,
            status: "new".to_string(),
            created_at: String::new(),
        });
    }

    // 4. Store in DB
    on_event
        .send(IdeaStreamEvent::Progress {
            stage: "saving".to_string(),
            detail: "Saving ideas...".to_string(),
        })
        .ok();

    let mut saved_ideas = Vec::new();
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        for mut idea in ideas {
            let competitors_json = serde_json::to_string(&idea.competitors)
                .map_err(|e| format!("failed to serialize competitors: {e}"))?;
            let source_ids_json = serde_json::to_string(&idea.source_story_ids)
                .map_err(|e| format!("failed to serialize source_story_ids: {e}"))?;

            conn.execute(
                "INSERT INTO project_ideas (title, description, why_now, competitors, differentiation, tech_stack, difficulty, relevance_score, source_story_ids, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'new')",
                params![
                    idea.title,
                    idea.description,
                    idea.why_now,
                    competitors_json,
                    idea.differentiation,
                    idea.tech_stack,
                    idea.difficulty,
                    idea.relevance_score,
                    source_ids_json,
                ],
            )
            .map_err(|e| e.to_string())?;

            idea.id = conn.last_insert_rowid();
            idea.created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            saved_ideas.push(idea);
        }
    }

    on_event
        .send(IdeaStreamEvent::Complete { ideas: saved_ideas })
        .ok();

    Ok(())
}

#[tauri::command]
pub fn get_ideas(db: State<DbState>, status_filter: Option<String>) -> Result<Vec<ProjectIdea>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;

    let (sql, filter) = match &status_filter {
        Some(s) => (
            "SELECT id, title, description, why_now, competitors, differentiation, tech_stack, difficulty, relevance_score, source_story_ids, status, created_at
             FROM project_ideas WHERE status = ?1 ORDER BY created_at DESC",
            Some(s.clone()),
        ),
        None => (
            "SELECT id, title, description, why_now, competitors, differentiation, tech_stack, difficulty, relevance_score, source_story_ids, status, created_at
             FROM project_ideas WHERE status != 'dismissed' ORDER BY created_at DESC",
            None,
        ),
    };

    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;

    let ideas = if let Some(f) = &filter {
        stmt.query_map(params![f], read_idea)
    } else {
        stmt.query_map([], read_idea)
    }
    .map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())?;

    Ok(ideas)
}

#[tauri::command]
pub fn update_idea_status(db: State<DbState>, id: i64, status: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE project_ideas SET status = ?1 WHERE id = ?2",
        params![status, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ============ Helpers ============

fn read_idea(row: &rusqlite::Row) -> rusqlite::Result<ProjectIdea> {
    let competitors_json: Option<String> = row.get(4)?;
    let source_ids_json: Option<String> = row.get(9)?;

    Ok(ProjectIdea {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        why_now: row.get(3)?,
        competitors: competitors_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        differentiation: row.get(5)?,
        tech_stack: row.get(6)?,
        difficulty: row.get(7)?,
        relevance_score: row.get(8)?,
        source_story_ids: source_ids_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        status: row.get(10)?,
        created_at: row.get(11)?,
    })
}

fn extract_json(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{')
        && let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    trimmed.to_string()
}
