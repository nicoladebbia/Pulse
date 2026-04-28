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
const MIGRATION_015: &str = include_str!("../../../migrations/015_entity_aliases.sql");
const MIGRATION_016: &str = include_str!("../../../migrations/016_feed_health.sql");
const MIGRATION_017: &str = include_str!("../../../migrations/017_financial_data.sql");
const MIGRATION_018: &str = include_str!("../../../migrations/018_entity_resolution.sql");
const MIGRATION_019: &str = include_str!("../../../migrations/019_position_management.sql");
const MIGRATION_021: &str = include_str!("../../../migrations/021_trade_journal.sql");
const MIGRATION_023: &str = include_str!("../../../migrations/023_freedom_whoop.sql");

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

    // Migration 15: Entity aliases table
    if !applied.contains(&15) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_015)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (15)", [])?;
        tx.commit()?;
    }

    // Migration 16: Feed health + pipeline health tables
    if !applied.contains(&16) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_016)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (16)", [])?;
        tx.commit()?;
    }

    // Migration 17: Financial data support — table recreation + new tables + signal columns
    if !applied.contains(&17) {
        // Disable foreign keys during table recreation
        conn.execute_batch("PRAGMA foreign_keys = OFF;")?;

        // Step 1: Recreate stories table with finance sector + source_type + financial_metadata
        if !column_exists(conn, "stories", "source_type") {
            conn.execute_batch(
                "CREATE TABLE stories_new (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    briefing_id     INTEGER NOT NULL REFERENCES briefings(id),
                    sector          TEXT NOT NULL CHECK(sector IN ('ai', 'miami', 'italy', 'tech', 'finance')),
                    original_title  TEXT NOT NULL,
                    original_url    TEXT NOT NULL,
                    original_language TEXT NOT NULL DEFAULT 'en',
                    content_snippet TEXT,
                    source_name     TEXT NOT NULL,
                    published_at    TEXT,
                    headline        TEXT NOT NULL,
                    summary         TEXT NOT NULL,
                    key_facts       TEXT NOT NULL,
                    why_it_matters  TEXT NOT NULL,
                    what_to_watch   TEXT NOT NULL,
                    importance_score INTEGER NOT NULL DEFAULT 5,
                    relevance_score  INTEGER,
                    relevance_reason TEXT,
                    is_hero         INTEGER NOT NULL DEFAULT 0,
                    display_order   INTEGER NOT NULL DEFAULT 0,
                    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
                    url_hash        TEXT NOT NULL,
                    title_hash      TEXT NOT NULL,
                    context_prefix  TEXT,
                    summary_depth   TEXT DEFAULT 'standard',
                    deep_summary    TEXT,
                    sentiment       REAL,
                    novelty         REAL,
                    event_type      TEXT,
                    source_type     TEXT NOT NULL DEFAULT 'news' CHECK(source_type IN ('news', 'financial')),
                    financial_metadata TEXT
                );

                INSERT INTO stories_new (
                    id, briefing_id, sector, original_title, original_url, original_language,
                    content_snippet, source_name, published_at, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, relevance_score, relevance_reason,
                    is_hero, display_order, created_at, url_hash, title_hash,
                    context_prefix, summary_depth, deep_summary, sentiment, novelty, event_type,
                    source_type, financial_metadata
                )
                SELECT
                    id, briefing_id, sector, original_title, original_url, original_language,
                    content_snippet, source_name, published_at, headline, summary, key_facts,
                    why_it_matters, what_to_watch, importance_score, relevance_score, relevance_reason,
                    is_hero, display_order, created_at, url_hash, title_hash,
                    context_prefix, summary_depth, deep_summary, sentiment, novelty, event_type,
                    'news', NULL
                FROM stories;

                DROP TABLE stories;
                ALTER TABLE stories_new RENAME TO stories;

                CREATE INDEX IF NOT EXISTS idx_stories_briefing ON stories(briefing_id);
                CREATE INDEX IF NOT EXISTS idx_stories_sector ON stories(sector);
                CREATE INDEX IF NOT EXISTS idx_stories_date ON stories(created_at);
                CREATE INDEX IF NOT EXISTS idx_stories_url_hash ON stories(url_hash);
                CREATE INDEX IF NOT EXISTS idx_stories_title_hash ON stories(title_hash);
                CREATE INDEX IF NOT EXISTS idx_stories_source_type ON stories(source_type);
                CREATE INDEX IF NOT EXISTS idx_stories_published_at ON stories(published_at);"
            )?;
        }

        // Step 2: Recreate FTS table and triggers
        conn.execute_batch(
            "DROP TABLE IF EXISTS stories_fts;
            CREATE VIRTUAL TABLE stories_fts USING fts5(
                headline, summary, key_facts, why_it_matters, context_prefix,
                content='stories',
                content_rowid='id'
            );
            INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
                SELECT id, headline, summary, key_facts, why_it_matters, context_prefix FROM stories;

            DROP TRIGGER IF EXISTS stories_fts_insert;
            DROP TRIGGER IF EXISTS stories_fts_delete;
            DROP TRIGGER IF EXISTS stories_fts_update;

            CREATE TRIGGER stories_fts_insert AFTER INSERT ON stories BEGIN
                INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
                VALUES (NEW.id, NEW.headline, NEW.summary, NEW.key_facts, NEW.why_it_matters, NEW.context_prefix);
            END;

            CREATE TRIGGER stories_fts_delete AFTER DELETE ON stories BEGIN
                INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
                VALUES ('delete', OLD.id, OLD.headline, OLD.summary, OLD.key_facts, OLD.why_it_matters, OLD.context_prefix);
            END;

            CREATE TRIGGER stories_fts_update AFTER UPDATE ON stories BEGIN
                INSERT INTO stories_fts(stories_fts, rowid, headline, summary, key_facts, why_it_matters, context_prefix)
                VALUES ('delete', OLD.id, OLD.headline, OLD.summary, OLD.key_facts, OLD.why_it_matters, OLD.context_prefix);
                INSERT INTO stories_fts(rowid, headline, summary, key_facts, why_it_matters, context_prefix)
                VALUES (NEW.id, NEW.headline, NEW.summary, NEW.key_facts, NEW.why_it_matters, NEW.context_prefix);
            END;"
        )?;

        // Step 3: Recreate entities table with expanded entity types
        let mut needs_entity_rebuild = false;
        if let Ok(mut stmt) = conn.prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='entities'") {
            if let Ok(sql) = stmt.query_row([], |row| row.get::<_, String>(0)) {
                needs_entity_rebuild = !sql.contains("insider_trade");
            }
        }

        if needs_entity_rebuild {
            conn.execute_batch(
                "CREATE TABLE entities_new (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    name            TEXT NOT NULL,
                    name_normalized TEXT NOT NULL,
                    entity_type     TEXT NOT NULL CHECK(entity_type IN (
                        'company', 'person', 'topic', 'product', 'regulation',
                        'insider_trade', 'contract_award', 'patent_cluster',
                        'lobbying_disclosure', 'institutional_holding',
                        'private_placement', 'material_event', 'regulatory_action'
                    )),
                    sector          TEXT,
                    first_seen      TEXT NOT NULL,
                    last_seen       TEXT NOT NULL,
                    mention_count   INTEGER NOT NULL DEFAULT 1,
                    sentiment_avg   REAL NOT NULL DEFAULT 0.0,
                    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
                );

                INSERT INTO entities_new (
                    id, name, name_normalized, entity_type, sector,
                    first_seen, last_seen, mention_count, sentiment_avg, created_at
                )
                SELECT
                    id, name, name_normalized, entity_type, sector,
                    first_seen, last_seen, mention_count, sentiment_avg, created_at
                FROM entities;

                DROP TABLE entities;
                ALTER TABLE entities_new RENAME TO entities;

                CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_name_type ON entities(name_normalized, entity_type);
                CREATE INDEX IF NOT EXISTS idx_entities_sector ON entities(sector);
                CREATE INDEX IF NOT EXISTS idx_entities_mention_count ON entities(mention_count DESC);
                CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(entity_type);"
            )?;
        }

        // Step 4: Create financial infrastructure tables
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entity_tickers (
                entity_id   INTEGER PRIMARY KEY REFERENCES entities(id),
                ticker      TEXT NOT NULL,
                exchange    TEXT,
                cik         TEXT,
                is_public   INTEGER DEFAULT 1,
                confidence  REAL DEFAULT 1.0,
                last_verified TEXT,
                created_at  TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_entity_tickers_ticker ON entity_tickers(ticker);
            CREATE INDEX IF NOT EXISTS idx_entity_tickers_cik ON entity_tickers(cik);

            CREATE TABLE IF NOT EXISTS entity_prices (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_id   INTEGER REFERENCES entities(id),
                ticker      TEXT NOT NULL,
                date        TEXT NOT NULL,
                open        REAL,
                close       REAL NOT NULL,
                high        REAL,
                low         REAL,
                volume      INTEGER,
                change_1d   REAL,
                change_7d   REAL,
                change_30d  REAL,
                created_at  TEXT DEFAULT (datetime('now')),
                UNIQUE(ticker, date)
            );
            CREATE INDEX IF NOT EXISTS idx_entity_prices_entity ON entity_prices(entity_id, date);
            CREATE INDEX IF NOT EXISTS idx_entity_prices_ticker ON entity_prices(ticker, date);

            CREATE TABLE IF NOT EXISTS cross_signals (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_id           INTEGER REFERENCES entities(id),
                ticker              TEXT,
                compound_score      REAL NOT NULL,
                insider_signal      REAL DEFAULT 0,
                institutional_flow  REAL DEFAULT 0,
                news_momentum       REAL DEFAULT 0,
                government_signal   REAL DEFAULT 0,
                search_trend        REAL DEFAULT 0,
                patent_signal       REAL DEFAULT 0,
                supply_chain        REAL DEFAULT 0,
                political_signal    REAL DEFAULT 0,
                source_diversity    INTEGER DEFAULT 0,
                convergence_detected INTEGER DEFAULT 0,
                computed_at         TEXT DEFAULT (datetime('now'))
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_cross_signals_entity_date ON cross_signals(entity_id, date(computed_at));
            CREATE INDEX IF NOT EXISTS idx_cross_signals_entity ON cross_signals(entity_id);
            CREATE INDEX IF NOT EXISTS idx_cross_signals_convergence ON cross_signals(convergence_detected);
            CREATE INDEX IF NOT EXISTS idx_cross_signals_score ON cross_signals(compound_score);

            CREATE TABLE IF NOT EXISTS paper_trades (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_id       INTEGER REFERENCES entities(id),
                ticker          TEXT NOT NULL,
                direction       TEXT NOT NULL CHECK(direction IN ('long', 'short')),
                entry_price     REAL NOT NULL,
                entry_date      TEXT NOT NULL,
                exit_price      REAL,
                exit_date       TEXT,
                position_size   REAL NOT NULL,
                confidence      REAL NOT NULL,
                signal_profile  TEXT NOT NULL,
                prediction_id   INTEGER REFERENCES insights(id),
                alpaca_order_id TEXT,
                status          TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open', 'closed', 'stopped_out', 'expired')),
                pnl             REAL,
                pnl_pct         REAL,
                created_at      TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_paper_trades_status ON paper_trades(status);
            CREATE INDEX IF NOT EXISTS idx_paper_trades_entity ON paper_trades(entity_id);

            CREATE TABLE IF NOT EXISTS backtest_results (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                signal_profile  TEXT NOT NULL,
                start_date      TEXT NOT NULL,
                end_date        TEXT NOT NULL,
                total_signals   INTEGER NOT NULL,
                hit_rate        REAL NOT NULL,
                avg_return      REAL,
                max_drawdown    REAL,
                sharpe_ratio    REAL,
                avg_holding_days REAL,
                details         TEXT,
                created_at      TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS financial_dedup (
                source_type TEXT NOT NULL,
                source_id   TEXT NOT NULL,
                story_id    INTEGER REFERENCES stories(id),
                fetched_at  TEXT DEFAULT (datetime('now')),
                PRIMARY KEY (source_type, source_id)
            );"
        )?;

        // Step 5: Add signal dimension columns
        let signal_cols = [
            "insider_buy_volume", "institutional_flow", "contract_value",
            "patent_filing_rate", "search_trend_delta", "import_volume_delta",
            "regulatory_sentiment", "lobbying_spend_delta", "source_diversity",
        ];
        for col in &signal_cols {
            if !column_exists(conn, "signals", col) {
                let col_type = if *col == "source_diversity" { "INTEGER" } else { "REAL" };
                conn.execute_batch(&format!(
                    "ALTER TABLE signals ADD COLUMN {} {} DEFAULT 0;", col, col_type
                ))?;
            }
        }

        // Re-enable foreign keys
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        // Record migration
        conn.execute("INSERT INTO schema_migrations (version) VALUES (17)", [])?;
    }

    // Migration 18: Entity resolution — canonical entity deduplication
    if !applied.contains(&18) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_018)?;

        // Add canonical_id column to entities if not exists
        if !column_exists(&tx, "entities", "canonical_id") {
            tx.execute_batch("ALTER TABLE entities ADD COLUMN canonical_id INTEGER REFERENCES entity_canonical(id);")?;
            tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_entities_canonical ON entities(canonical_id);")?;
        }

        tx.execute("INSERT INTO schema_migrations (version) VALUES (18)", [])?;
        tx.commit()?;
    }

    // Migration 19: Position management columns + portfolio snapshots
    if !applied.contains(&19) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_019)?;

        if !column_exists(&tx, "paper_trades", "high_water_mark") {
            tx.execute_batch("ALTER TABLE paper_trades ADD COLUMN high_water_mark REAL;")?;
        }
        if !column_exists(&tx, "paper_trades", "trailing_stop") {
            tx.execute_batch("ALTER TABLE paper_trades ADD COLUMN trailing_stop REAL;")?;
        }
        if !column_exists(&tx, "paper_trades", "original_compound_score") {
            tx.execute_batch("ALTER TABLE paper_trades ADD COLUMN original_compound_score REAL;")?;
        }
        if !column_exists(&tx, "paper_trades", "scale_in_count") {
            tx.execute_batch("ALTER TABLE paper_trades ADD COLUMN scale_in_count INTEGER DEFAULT 0;")?;
        }

        tx.execute("INSERT INTO schema_migrations (version) VALUES (19)", [])?;
        tx.commit()?;
    }

    if !applied.contains(&21) {
        if !column_exists(conn, "paper_trades", "trade_journal") {
            conn.execute_batch(MIGRATION_021)?;
        }
        conn.execute("INSERT INTO schema_migrations (version) VALUES (21)", [])?;
    }

    // Migration 23: Add 'whoop' to freedom_stories CHECK constraint (rebuild table).
    if !applied.contains(&23) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_023)?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES (23)", [])?;
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
