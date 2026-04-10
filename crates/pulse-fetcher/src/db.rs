use rusqlite::Connection;

const MIGRATION_001: &str = include_str!("../../../migrations/001_initial_schema.sql");
const MIGRATION_002: &str = include_str!("../../../migrations/002_fts_indexes.sql");
const MIGRATION_003: &str = include_str!("../../../migrations/003_intelligence.sql");
const MIGRATION_004: &str = include_str!("../../../migrations/004_freedoms.sql");
const MIGRATION_005: &str = include_str!("../../../migrations/005_contextual_prefix.sql");
const MIGRATION_006: &str = include_str!("../../../migrations/006_freedoms_search.sql");
const MIGRATION_007: &str = include_str!("../../../migrations/007_executive_summary.sql");
const MIGRATION_008: &str = include_str!("../../../migrations/008_intelligence_upgrade.sql");
const MIGRATION_009: &str = include_str!("../../../migrations/009_multiple_daily_briefings.sql");
const MIGRATION_010: &str = include_str!("../../../migrations/010_trajectory_labels.sql");
const MIGRATION_011: &str = include_str!("../../../migrations/011_rename_financial_to_wealth.sql");

/// Check if a column exists on a table via PRAGMA table_info.
fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .any(|r| r.as_deref() == Ok(column))
}

/// Run all pending migrations, each wrapped in a transaction for atomicity.
pub fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    let applied: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?
    };

    // Migrations 1-4 run as pure SQL batches
    let batch_migrations: &[(i64, &str)] = &[
        (1, MIGRATION_001),
        (2, MIGRATION_002),
        (3, MIGRATION_003),
        (4, MIGRATION_004),
    ];

    for &(version, sql) in batch_migrations {
        if !applied.contains(&version) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                [version],
            )?;
            tx.commit()?;
        }
    }

    // Migration 5: ALTER TABLE stories ADD COLUMN context_prefix + FTS rebuild
    if !applied.contains(&5) {
        if !column_exists(conn, "stories", "context_prefix") {
            conn.execute_batch("ALTER TABLE stories ADD COLUMN context_prefix TEXT;")?;
        }
        let sql_005_rest = MIGRATION_005
            .lines()
            .filter(|line| !line.trim().starts_with("ALTER TABLE stories ADD COLUMN"))
            .collect::<Vec<_>>()
            .join("\n");
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(&sql_005_rest)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (5)", [])?;
        tx.commit()?;
    }

    // Migration 6: ALTER TABLE freedom_stories ADD COLUMN context_prefix + FTS
    if !applied.contains(&6) {
        if !column_exists(conn, "freedom_stories", "context_prefix") {
            conn.execute_batch("ALTER TABLE freedom_stories ADD COLUMN context_prefix TEXT;")?;
        }
        let sql_006_rest = MIGRATION_006
            .lines()
            .filter(|line| !line.trim().starts_with("ALTER TABLE freedom_stories ADD COLUMN"))
            .collect::<Vec<_>>()
            .join("\n");
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(&sql_006_rest)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (6)", [])?;
        tx.commit()?;
    }

    // Migration 7: ALTER TABLE briefings ADD COLUMN executive_summary
    if !applied.contains(&7) {
        if !column_exists(conn, "briefings", "executive_summary") {
            conn.execute_batch("ALTER TABLE briefings ADD COLUMN executive_summary TEXT;")?;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (7)", [])?;
        tx.commit()?;
    }

    // Migration 8: api_usage, project_ideas tables + adaptive summary columns
    if !applied.contains(&8) {
        let sql_008_tables = MIGRATION_008
            .lines()
            .filter(|line| !line.trim().starts_with("ALTER TABLE"))
            .collect::<Vec<_>>()
            .join("\n");
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(&sql_008_tables)?;
        tx.commit()?;

        if !column_exists(conn, "stories", "summary_depth") {
            conn.execute_batch("ALTER TABLE stories ADD COLUMN summary_depth TEXT DEFAULT 'standard';")?;
        }
        if !column_exists(conn, "stories", "deep_summary") {
            conn.execute_batch("ALTER TABLE stories ADD COLUMN deep_summary TEXT;")?;
        }

        let tx = conn.unchecked_transaction()?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (8)", [])?;
        tx.commit()?;
    }

    // Migration 9: Multiple daily briefings
    if !applied.contains(&9) {
        conn.execute_batch("DROP INDEX IF EXISTS idx_briefings_date_type;")?;
        if !column_exists(conn, "briefings", "time_label") {
            conn.execute_batch("ALTER TABLE briefings ADD COLUMN time_label TEXT;")?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_briefings_date_type_time ON briefings(date, briefing_type, created_at DESC);"
        )?;
        let tx = conn.unchecked_transaction()?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (9)", [])?;
        tx.commit()?;
    }

    // Migration 10: Update trajectory labels
    if !applied.contains(&10) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_010)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (10)", [])?;
        tx.commit()?;
    }

    // Migration 11: Rename financial → wealth
    if !applied.contains(&11) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_011)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (11)", [])?;
        tx.commit()?;
    }

    // Ensure composite indexes exist (idempotent)
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_freedom_stories_bf ON freedom_stories(briefing_id, freedom, display_order);"
    )?;

    Ok(())
}

/// Log an API call to the usage tracking table.
pub fn log_api_usage(
    conn: &rusqlite::Connection,
    provider: &str,
    model: &str,
    endpoint: &str,
    input_tokens: i64,
    output_tokens: i64,
) {
    let cost = match (provider, model) {
        ("groq", m) if m.contains("8b") => (input_tokens as f64 * 0.05 + output_tokens as f64 * 0.08) / 1_000_000.0,
        ("groq", _) => (input_tokens as f64 * 0.59 + output_tokens as f64 * 0.79) / 1_000_000.0,
        ("anthropic", m) if m.contains("haiku") => (input_tokens as f64 * 0.25 + output_tokens as f64 * 1.25) / 1_000_000.0,
        ("anthropic", m) if m.contains("sonnet") => (input_tokens as f64 * 3.0 + output_tokens as f64 * 15.0) / 1_000_000.0,
        ("voyage", _) => (input_tokens as f64 * 0.02) / 1_000_000.0,
        _ => 0.0,
    };

    conn.execute(
        "INSERT INTO api_usage (provider, model, endpoint, input_tokens, output_tokens, estimated_cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![provider, model, endpoint, input_tokens, output_tokens, cost],
    ).ok();
}
