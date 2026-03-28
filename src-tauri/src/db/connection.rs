use rusqlite::{Connection, Result};
use std::path::Path;

const MIGRATION_001: &str = include_str!("../../../migrations/001_initial_schema.sql");
const MIGRATION_002: &str = include_str!("../../../migrations/002_fts_indexes.sql");

pub fn initialize(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;

    run_migrations(&conn)?;

    Ok(conn)
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let applied: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>>>()?
    };

    if !applied.contains(&1) {
        conn.execute_batch(MIGRATION_001)?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (1)", [])?;
    }

    if !applied.contains(&2) {
        conn.execute_batch(MIGRATION_002)?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (2)", [])?;
    }

    Ok(())
}
