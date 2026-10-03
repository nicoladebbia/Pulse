//! Which embedding model the vectors in `story_embeddings` came from.
//!
//! Cosine similarity is only meaningful between vectors from the SAME model:
//! a Voyage vector and an EmbeddingGemma vector are both 512 floats, but
//! comparing them gives noise that looks like a score. So a database belongs
//! to one model at a time, recorded here, and switching means re-embedding
//! everything (`pulse-fetcher --mode reembed`).

use rusqlite::{Connection, OptionalExtension};

/// The model every vector came from before this table existed.
pub const LEGACY_MODEL: &str = "voyage-3-lite";

/// The embedding space this process would write right now. Locally the
/// dimension count is part of it, since it is ours to choose.
pub fn current_model() -> String {
    if crate::is_local() {
        format!("{}@{}d", crate::local_embed_model(), crate::EMBEDDING_DIMS)
    } else {
        LEGACY_MODEL.to_string()
    }
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

/// The model the stored vectors belong to, or `None` for an empty database.
/// Read-only: safe on the app's shared connection.
pub fn stored_model(conn: &Connection) -> rusqlite::Result<Option<String>> {
    if table_exists(conn, "embedding_space")? {
        let recorded: Option<String> = conn
            .query_row("SELECT model FROM embedding_space WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        if recorded.is_some() {
            return Ok(recorded);
        }
    }
    if !table_exists(conn, "story_embeddings")? {
        return Ok(None);
    }
    let has_vectors: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM story_embeddings)", [], |r| r.get(0))?;
    Ok(has_vectors.then(|| LEGACY_MODEL.to_string()))
}

/// Vectors made now can be compared with the stored ones.
pub fn matches(conn: &Connection) -> bool {
    matches_model(conn, &current_model())
}

fn matches_model(conn: &Connection, current: &str) -> bool {
    match stored_model(conn) {
        Ok(m) => m.is_none_or(|m| m == current),
        Err(e) => {
            // Don't silently drop vector search on a read hiccup in the common case.
            eprintln!("embedding_space check failed ({e}); assuming vectors match");
            true
        }
    }
}

/// Call before writing vectors. Claims an empty database for the current
/// model; refuses (Err with the reason) when the stored vectors are another model's.
pub fn ensure_writable(conn: &Connection) -> Result<(), String> {
    ensure_writable_for(conn, &current_model())
}

fn ensure_writable_for(conn: &Connection, current: &str) -> Result<(), String> {
    match stored_model(conn).map_err(|e| e.to_string())? {
        Some(stored) if stored == current => Ok(()),
        Some(stored) => Err(format!(
            "this database's search vectors were made with {stored}, but the current \
             embedding model is {current}. Mixing them would make search meaningless; run \
             `pulse-fetcher --mode reembed` to switch the whole database to {current}."
        )),
        None => record_model(conn, current).map_err(|e| e.to_string()),
    }
}

/// Record `model` as the owner of the stored vectors.
pub fn record_model(conn: &Connection, model: &str) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embedding_space (
             id     INTEGER PRIMARY KEY CHECK (id = 1),
             model  TEXT NOT NULL,
             set_at TEXT NOT NULL DEFAULT (datetime('now'))
         )",
    )?;
    conn.execute(
        "INSERT INTO embedding_space (id, model) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET model = excluded.model, set_at = datetime('now')",
        [model],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Explicit models throughout: other tests in this crate flip PULSE_LLM.
    const GEMMA: &str = "embeddinggemma@512d";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE story_embeddings (story_id INTEGER PRIMARY KEY, embedding BLOB NOT NULL)",
        )
        .unwrap();
        conn
    }

    #[test]
    fn empty_db_is_claimed_by_the_current_model() {
        let conn = db();
        assert_eq!(stored_model(&conn).unwrap(), None);
        assert!(matches_model(&conn, GEMMA));
        ensure_writable_for(&conn, GEMMA).unwrap();
        assert_eq!(stored_model(&conn).unwrap().as_deref(), Some(GEMMA));
    }

    #[test]
    fn reading_never_writes() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(stored_model(&conn).unwrap(), None); // no tables at all
        assert!(!table_exists(&conn, "embedding_space").unwrap());
    }

    #[test]
    fn vectors_without_a_record_are_legacy_voyage() {
        let conn = db();
        conn.execute("INSERT INTO story_embeddings VALUES (1, x'00')", []).unwrap();
        assert_eq!(stored_model(&conn).unwrap().as_deref(), Some(LEGACY_MODEL));
        ensure_writable_for(&conn, LEGACY_MODEL).unwrap();
        let err = ensure_writable_for(&conn, GEMMA).unwrap_err();
        assert!(err.contains("--mode reembed"), "{err}");
        assert!(!matches_model(&conn, GEMMA));
    }

    #[test]
    fn recorded_model_wins() {
        let conn = db();
        conn.execute("INSERT INTO story_embeddings VALUES (1, x'00')", []).unwrap();
        record_model(&conn, GEMMA).unwrap();
        assert!(matches_model(&conn, GEMMA));
        assert!(ensure_writable_for(&conn, LEGACY_MODEL).is_err());
    }
}
