//! `--mode reembed`: move a database's search vectors to the current embedding
//! model (local EmbeddingGemma with `PULSE_LLM=local`, Voyage otherwise).
//!
//! Vectors from two models can't be compared, so the switch is all or nothing,
//! and it must never leave the database without vectors. New vectors are built
//! in a staging table while search keeps using the old ones; only when every
//! story has a new vector are they swapped in, in one transaction. A run that
//! stops partway keeps its progress: run `reembed` again to resume.

use crate::embeddings;
use anyhow::Context;
use rusqlite::Connection;
use std::path::Path;

const STAGING: &str = "story_embeddings_next";

/// Freedom stories share `story_embeddings` under this encoded id
/// (see the freedoms pipeline's Phase 8).
const FREEDOM_ID_OFFSET: i64 = 100_000;

pub async fn run(db_path: &Path, force: bool) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    crate::db::run_migrations(&conn)?;

    let from = pulse_llm::space::stored_model(&conn)?;
    let to = pulse_llm::space::current_model();
    if from.as_deref() == Some(to.as_str()) && !force {
        tracing::info!("Search vectors are already from {to}; nothing to do (--force rebuilds anyway).");
        return Ok(());
    }

    // Prove the embedder works before doing anything else.
    let probe = embeddings::embed_texts(&["Pulse embedding check".to_string()])
        .await
        .with_context(|| format!("can't reach the embedding model {to}; nothing was changed"))?;
    if probe.first().map(Vec::len) != Some(pulse_llm::EMBEDDING_DIMS) {
        anyhow::bail!("{to} returned the wrong number of dimensions; nothing was changed");
    }

    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {STAGING} (
             story_id  INTEGER PRIMARY KEY,
             embedding BLOB NOT NULL,
             model     TEXT NOT NULL
         )"
    ))?;
    // Progress toward a different target is useless.
    conn.execute(&format!("DELETE FROM {STAGING} WHERE model != ?1"), [&to])?;

    let todo = pending(&conn)?;
    tracing::info!(
        "Re-embedding {} stories with {to} (was {}); already staged: {}",
        todo.len(),
        from.as_deref().unwrap_or("none"),
        staged(&conn)?
    );

    let texts: Vec<String> = todo.iter().map(|(_, t)| t.clone()).collect();
    let batches = embeddings::batch_by_tokens(&texts, embeddings::VOYAGE_REQUEST_TOKEN_BUDGET);
    let pause = embeddings::rate_limit_pause_secs();
    let mut failed = 0usize;
    for (n, &(a, b)) in batches.iter().enumerate() {
        if n > 0 && pause > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(pause)).await;
        }
        let ids: Vec<i64> = todo[a..b].iter().map(|(id, _)| *id).collect();
        match embeddings::embed_texts(&texts[a..b]).await {
            Ok(embs) if embs.len() == ids.len()
                && embs.iter().all(|e| e.len() == pulse_llm::EMBEDDING_DIMS) =>
            {
                let tx = conn.unchecked_transaction()?;
                for (id, emb) in ids.iter().zip(&embs) {
                    let blob: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
                    tx.execute(
                        &format!("INSERT OR REPLACE INTO {STAGING} (story_id, embedding, model) VALUES (?1, ?2, ?3)"),
                        rusqlite::params![id, blob, to],
                    )?;
                }
                tx.commit()?;
            }
            Ok(_) => {
                failed += ids.len();
                tracing::warn!("Batch {}: wrong count or size of vectors, skipped", n + 1);
            }
            Err(e) => {
                failed += ids.len();
                tracing::warn!("Batch {} failed: {e}", n + 1);
            }
        }
        if (n + 1) % 20 == 0 || n + 1 == batches.len() {
            tracing::info!("Re-embed: batch {}/{} done", n + 1, batches.len());
        }
    }

    if failed > 0 {
        anyhow::bail!(
            "{failed} stories failed to embed. Search still uses the old vectors; run \
             `--mode reembed` again to retry them (finished ones are kept)."
        );
    }

    // Swap only while no fetch is writing vectors.
    let Some(_lock) = crate::acquire_single_instance_lock(db_path) else {
        anyhow::bail!("A fetch is running. Progress is kept; run `--mode reembed` again to finish the switch.");
    };
    let swapped = swap_in(&conn, &to)?;
    tracing::info!("Switched search to {to}: {swapped} vectors.");
    Ok(())
}

/// Stories (and freedom stories) without a staged vector, newest first, with
/// the same text the backfill embeds.
fn pending(conn: &Connection) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, text FROM (
             SELECT s.id AS id, s.headline || '. ' || s.summary || '. ' || s.key_facts AS text
             FROM stories s
             UNION ALL
             SELECT -(f.id + {FREEDOM_ID_OFFSET}), f.headline || '. ' || f.summary || '. ' || f.key_facts
             FROM freedom_stories f
         )
         WHERE id NOT IN (SELECT story_id FROM {STAGING})
         ORDER BY abs(id) DESC"
    ))?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
}

fn staged(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT count(*) FROM {STAGING}"), [], |r| r.get(0))
}

/// Replace the live vectors with the staged ones and record the new model, all
/// in one transaction. Stories added during the run have no staged vector; the
/// next backfill embeds them with the new model.
fn swap_in(conn: &Connection, model: &str) -> anyhow::Result<usize> {
    // Freedom vectors live under negative ids that aren't in `stories`, so they
    // can't satisfy story_embeddings' foreign key; the freedoms pipeline writes
    // them on a connection without FK enforcement, and so must this. (The pragma
    // is a no-op inside a transaction, hence before it.)
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM story_embeddings", [])?;
    let n = tx.execute(
        &format!(
            "INSERT INTO story_embeddings (story_id, embedding)
             SELECT story_id, embedding FROM {STAGING}
             WHERE story_id < 0 OR story_id IN (SELECT id FROM stories)"
        ),
        [],
    )?;
    pulse_llm::space::record_model(&tx, model)?;
    tx.execute(&format!("DROP TABLE {STAGING}"), [])?;
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_replaces_vectors_and_records_the_model() {
        let conn = Connection::open_in_memory().unwrap();
        // Same FK as the real schema, enforced, as run_migrations leaves it.
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE stories (id INTEGER PRIMARY KEY, headline TEXT, summary TEXT, key_facts TEXT);
             CREATE TABLE freedom_stories (id INTEGER PRIMARY KEY, headline TEXT, summary TEXT, key_facts TEXT);
             CREATE TABLE story_embeddings (story_id INTEGER PRIMARY KEY REFERENCES stories(id) ON DELETE CASCADE, embedding BLOB NOT NULL);
             PRAGMA foreign_keys=OFF;
             CREATE TABLE {STAGING} (story_id INTEGER PRIMARY KEY, embedding BLOB NOT NULL, model TEXT NOT NULL);
             INSERT INTO stories VALUES (1, 'a', 'b', '[]'), (2, 'c', 'd', '[]');
             INSERT INTO freedom_stories VALUES (7, 'f', 'g', '[]');
             INSERT INTO story_embeddings VALUES (1, x'01'), (2, x'01'), (-100007, x'01');
             INSERT INTO {STAGING} VALUES (1, x'02', 'm'), (-100007, x'02', 'm'), (99, x'02', 'm');
             PRAGMA foreign_keys=ON;"
        ))
        .unwrap();

        // Story 2 isn't staged yet, so it shows up as pending; 1 and the freedom story don't.
        let todo: Vec<i64> = pending(&conn).unwrap().into_iter().map(|(id, _)| id).collect();
        assert_eq!(todo, vec![2]);

        // 99 was deleted from stories during the run: not carried over.
        assert_eq!(swap_in(&conn, "m").unwrap(), 2);
        let rows: Vec<(i64, Vec<u8>)> = conn
            .prepare("SELECT story_id, embedding FROM story_embeddings ORDER BY story_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![(-100007, vec![2]), (1, vec![2])]);
        assert_eq!(pulse_llm::space::stored_model(&conn).unwrap().as_deref(), Some("m"));
        let staging_left: bool = conn
            .query_row(&format!("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = '{STAGING}')"), [], |r| r.get(0))
            .unwrap();
        assert!(!staging_left);
    }
}
