use rusqlite::Connection;

/// Create an in-memory SQLite DB with all migrations applied
pub fn test_db() -> Connection {
    crate::db::connection::initialize_in_memory().expect("failed to create test DB")
}

/// A test story for seeding
pub struct TestStory {
    pub sector: &'static str,
    pub headline: String,
    pub summary: String,
    pub key_facts: Vec<String>,
    pub why_it_matters: String,
    pub what_to_watch: String,
    pub importance_score: i32,
    pub source_name: String,
    pub original_url: String,
}

impl TestStory {
    /// Quick builder with sensible defaults
    pub fn new(sector: &'static str, headline: &str) -> Self {
        Self {
            sector,
            headline: headline.to_string(),
            summary: format!("Summary of: {}", headline),
            key_facts: vec!["Fact 1".to_string(), "Fact 2".to_string()],
            why_it_matters: "This matters because...".to_string(),
            what_to_watch: "Watch for...".to_string(),
            importance_score: 7,
            source_name: "Test Source".to_string(),
            original_url: format!("https://example.com/{}", headline.to_lowercase().replace(' ', "-")),
        }
    }
}

/// Insert a test briefing + stories, return (briefing_id, story_ids)
pub fn seed_briefing(conn: &Connection, date: &str, stories: &[TestStory]) -> (i64, Vec<i64>) {
    let ai_count = stories.iter().filter(|s| s.sector == "ai").count();
    let miami_count = stories.iter().filter(|s| s.sector == "miami").count();
    let italy_count = stories.iter().filter(|s| s.sector == "italy").count();
    let tech_count = stories.iter().filter(|s| s.sector == "tech").count();

    conn.execute(
        "INSERT INTO briefings (date, story_count, ai_count, miami_count, italy_count, tech_count, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'complete')",
        rusqlite::params![date, stories.len(), ai_count, miami_count, italy_count, tech_count],
    ).expect("insert briefing");

    let briefing_id = conn.last_insert_rowid();
    let mut story_ids = Vec::new();

    for (i, story) in stories.iter().enumerate() {
        let key_facts_json = serde_json::to_string(&story.key_facts).unwrap();
        let url_hash = format!("hash_{}", i);
        let title_hash = format!("thash_{}", i);

        conn.execute(
            "INSERT INTO stories (
                briefing_id, sector, original_title, headline, summary, key_facts,
                why_it_matters, what_to_watch, importance_score,
                is_hero, display_order, original_url, source_name,
                url_hash, title_hash
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                briefing_id, story.sector, story.headline, story.headline, story.summary, key_facts_json,
                story.why_it_matters, story.what_to_watch, story.importance_score,
                if i == 0 { 1 } else { 0 }, i as i32, story.original_url, story.source_name,
                url_hash, title_hash,
            ],
        ).expect("insert story");

        story_ids.push(conn.last_insert_rowid());
    }

    (briefing_id, story_ids)
}

/// Insert a story embedding
pub fn seed_embedding(conn: &Connection, story_id: i64, embedding: &[f32]) {
    let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    conn.execute(
        "INSERT INTO story_embeddings (story_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![story_id, blob],
    ).expect("insert embedding");
}

/// Generate a deterministic 512-dim normalized embedding from a seed
pub fn fake_embedding(seed: u64) -> Vec<f32> {
    let mut v = Vec::with_capacity(512);
    let mut x = seed;
    for _ in 0..512 {
        // Simple LCG PRNG
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.push(((x >> 33) as f32) / (u32::MAX as f32) - 0.5);
    }
    // Normalize to unit vector
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut v {
            *val /= norm;
        }
    }
    v
}

/// Generate an embedding that is close to another (by mixing with noise)
pub fn similar_embedding(base: &[f32], similarity: f32, seed: u64) -> Vec<f32> {
    let noise = fake_embedding(seed);
    let mut v: Vec<f32> = base.iter().zip(noise.iter())
        .map(|(b, n)| b * similarity + n * (1.0 - similarity))
        .collect();
    // Normalize
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut v {
            *val /= norm;
        }
    }
    v
}
