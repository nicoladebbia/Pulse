//! What each auto-trade run did with each candidate, for the Signals page's
//! "Why didn't it buy?" card (`trade_decisions`, migration 040).
//!
//! Writes are best-effort: a failed log line must never stop a trade.

use rusqlite::Connection;

/// Rows older than this are pruned when a run starts. Long enough for the
/// learning report to see what the skipped signals did afterwards.
const KEEP_DAYS: i64 = 180;

pub struct DecisionLog<'a> {
    conn: &'a Connection,
    run_at: String,
}

impl<'a> DecisionLog<'a> {
    /// One log per run; every row it writes shares the run's timestamp.
    pub fn start(conn: &'a Connection) -> Self {
        conn.execute(
            "DELETE FROM trade_decisions WHERE run_at < datetime('now', 'localtime', ?1)",
            [format!("-{KEEP_DAYS} days")],
        )
        .ok();
        let run_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        Self { conn, run_at }
    }

    fn write(&self, ticker: &str, name: &str, score: Option<f64>, outcome: &str, reason: &str, detail: &str) {
        if let Err(e) = self.conn.execute(
            "INSERT INTO trade_decisions (run_at, ticker, name, score, outcome, reason, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![self.run_at, ticker, name, score, outcome, reason, detail],
        ) {
            tracing::debug!("trade_decisions write failed: {}", e);
        }
    }

    pub fn skip(&self, ticker: &str, name: &str, score: f64, reason: &str, detail: impl AsRef<str>) {
        self.write(ticker, name, Some(score), "skipped", reason, detail.as_ref());
    }

    pub fn bought(&self, ticker: &str, name: &str, score: f64, detail: impl AsRef<str>) {
        self.write(ticker, name, Some(score), "bought", "bought", detail.as_ref());
    }

    pub fn preview(&self, ticker: &str, name: &str, score: f64, detail: impl AsRef<str>) {
        self.write(ticker, name, Some(score), "preview", "preview", detail.as_ref());
    }

    /// The whole run stopped here; later candidates were not looked at.
    pub fn stopped(&self, reason: &str, detail: impl AsRef<str>) {
        self.write("", "", None, "run_stopped", reason, detail.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../migrations/040_signals_page.sql").split("ALTER TABLE").next().unwrap())
            .unwrap();
        conn
    }

    #[test]
    fn a_run_writes_one_timestamp_and_prunes_old_rows() {
        let conn = db();
        conn.execute(
            "INSERT INTO trade_decisions (run_at, outcome, reason) VALUES ('2020-01-01T10:00:00', 'skipped', 'old')",
            [],
        )
        .unwrap();
        let log = DecisionLog::start(&conn);
        log.skip("AAA", "Aaa Inc", 0.4, "earnings", "reports Thursday");
        log.bought("BBB", "Bbb Inc", 0.5, "10 shares");
        log.stopped("max_positions", "40 open");
        let rows: Vec<(String, String, String)> = conn
            .prepare("SELECT run_at, ticker, reason FROM trade_decisions ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows.len(), 3, "the 2020 row is pruned");
        assert!(rows.iter().all(|r| r.0 == rows[0].0));
        assert_eq!((rows[2].1.as_str(), rows[2].2.as_str()), ("", "max_positions"));
    }
}
