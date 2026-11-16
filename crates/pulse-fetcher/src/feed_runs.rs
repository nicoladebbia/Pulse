//! Per-feed counts for every fetch run (`feed_runs`, migration 048): whether each
//! news feed answered, what it handed over, how much of that the run could use and
//! how much was left once repeats were dropped.
//!
//! A feed used to be visible only through the articles that survived, so one that
//! failed, went quiet or only ever repeated other feeds left no trace but a log line.
//!
//! Two halves. The sources call [`watch`] (or [`note`]) around each feed's fetch,
//! which collects into a process-wide list; the pipeline calls [`record`] once the
//! repeats are gone and [`attach`] once a briefing exists.
//!
//! Strictly observational, like `candidates`: nothing the briefing depends on reads
//! any of it, and `record` / `attach` log and drop every failure.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use rusqlite::Connection;

use crate::sources::RawArticle;

/// Rows older than this are deleted when a run records its own. About 240 feeds
/// and three or four runs a day; half a year is enough to tell a dead feed from
/// a slow one.
pub const KEEP_DAYS: i64 = 180;

/// An error is stored as the source reported it, capped: a failed request repeats
/// its whole URL, and a Google News query runs to several hundred characters.
const ERROR_MAX_BYTES: usize = 300;

/// One feed's fetch, as its source saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetch {
    /// The id the feed puts on its articles (`RawArticle.feed_id`).
    pub feed_id: String,
    pub kind: &'static str,
    pub name: String,
    pub sector: String,
    /// `None`: the fetch worked.
    pub error: Option<String>,
    pub millis: i64,
    /// Entries in the feed's answer, before any filter. `None` when the fetch failed.
    pub entries: Option<i64>,
}

/// The fetches of the collect in progress.
///
/// Process-wide for the reason `sources::SOURCES_DONE` gives: two collects never
/// overlap. `begin` empties it at the top of each one and `record` takes what the
/// collect left.
static NOTES: Mutex<Vec<Fetch>> = Mutex::new(Vec::new());

fn notes() -> std::sync::MutexGuard<'static, Vec<Fetch>> {
    // A panic while the list was held leaves it readable; losing a run's counts
    // to a poisoned lock would be the worse outcome.
    NOTES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forget the previous collect's fetches. Called at the top of every collect.
pub fn begin() {
    notes().clear();
}

/// Note one feed's fetch: `outcome` is the number of entries in its answer, or
/// the error that stopped it.
pub fn note(
    feed_id: String,
    kind: &'static str,
    name: &str,
    sector: &str,
    started: Instant,
    outcome: Result<usize, &anyhow::Error>,
) {
    let (error, entries) = match outcome {
        Ok(entries) => (None, Some(entries as i64)),
        Err(e) => (Some(crate::article_text::clip(&format!("{e:#}"), ERROR_MAX_BYTES).to_string()), None),
    };
    notes().push(Fetch {
        feed_id,
        kind,
        name: name.to_string(),
        sector: sector.to_string(),
        error,
        millis: started.elapsed().as_millis() as i64,
        entries,
    });
}

/// Run one feed's fetch, note how it went, and hand back its articles. The fetch
/// returns its articles and the number of entries the feed's answer held.
pub async fn watch(
    feed_id: String,
    kind: &'static str,
    name: &str,
    sector: &str,
    fetch: impl std::future::Future<Output = anyhow::Result<(Vec<RawArticle>, usize)>>,
) -> anyhow::Result<Vec<RawArticle>> {
    let started = Instant::now();
    let result = fetch.await;
    note(
        feed_id,
        kind,
        name,
        sector,
        started,
        result.as_ref().map(|(_, entries)| *entries),
    );
    result.map(|(articles, _)| articles)
}

/// The time a run's rows are stamped with: UTC, in SQLite's own format so
/// `datetime()` arithmetic and plain string comparison both work on it.
pub fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// News articles per feed at one stage of a run.
pub type Tally = HashMap<String, usize>;

/// Count news articles per feed. The financial sources give every filing its own
/// feed id, so counting them here would make one row per filing.
pub fn tally<'a>(articles: impl IntoIterator<Item = &'a RawArticle>) -> Tally {
    let mut counts = Tally::new();
    for a in articles {
        if a.source_type == "news" {
            *counts.entry(a.feed_id.clone()).or_default() += 1;
        }
    }
    counts
}

/// One row of `feed_runs`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub feed_id: String,
    pub kind: String,
    pub name: String,
    pub sector: String,
    pub error: Option<String>,
    pub millis: Option<i64>,
    pub entries: Option<i64>,
    pub fetched: usize,
    pub offered: usize,
    pub pooled: usize,
}

/// One row per feed: the feeds that were fetched, in the order they answered,
/// then any feed that handed over articles without a noted fetch (a source that
/// does not report its fetches yet), by id.
///
/// Two fetches under one id become one row, since the table holds one row per
/// feed per run: it failed if either did, took as long as the slower, and holds
/// the entries of both.
pub fn rows(fetches: Vec<Fetch>, fetched: &Tally, offered: &Tally, pooled: &Tally) -> Vec<Row> {
    let count = |tally: &Tally, id: &str| tally.get(id).copied().unwrap_or(0);
    let mut out: Vec<Row> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for f in fetches {
        if let Some(&i) = at.get(&f.feed_id) {
            let row = &mut out[i];
            row.error = row.error.take().or(f.error);
            row.millis = row.millis.max(Some(f.millis));
            row.entries = match (row.entries, f.entries) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            };
            continue;
        }
        at.insert(f.feed_id.clone(), out.len());
        out.push(Row {
            fetched: count(fetched, &f.feed_id),
            offered: count(offered, &f.feed_id),
            pooled: count(pooled, &f.feed_id),
            feed_id: f.feed_id,
            kind: f.kind.to_string(),
            name: f.name,
            sector: f.sector,
            error: f.error,
            millis: Some(f.millis),
            entries: f.entries,
        });
    }

    let noted: HashSet<&str> = at.keys().map(String::as_str).collect();
    let mut unnoted: Vec<&String> = fetched.keys().filter(|id| !noted.contains(id.as_str())).collect();
    unnoted.sort();
    let unnoted: Vec<Row> = unnoted
        .into_iter()
        .map(|id| Row {
            feed_id: id.clone(),
            kind: "other".to_string(),
            name: String::new(),
            sector: String::new(),
            error: None,
            millis: None,
            entries: None,
            fetched: count(fetched, id),
            offered: count(offered, id),
            pooled: count(pooled, id),
        })
        .collect();
    out.extend(unnoted);
    out
}

/// Write a run's rows and drop the rows past `KEEP_DAYS`. Returns the rows written.
pub fn save(conn: &Connection, run_at: &str, mode: &str, rows: &[Row]) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut written = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO feed_runs (
                run_at, mode, feed_id, kind, name, sector, ok, error, millis,
                entries, fetched, offered, pooled
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for r in rows {
            written += stmt.execute(rusqlite::params![
                run_at,
                mode,
                r.feed_id,
                r.kind,
                r.name,
                r.sector,
                r.error.is_none(),
                r.error,
                r.millis,
                r.entries,
                r.fetched as i64,
                r.offered as i64,
                r.pooled as i64,
            ])?;
        }
    }
    tx.execute(
        "DELETE FROM feed_runs WHERE run_at < datetime(?1, ?2)",
        rusqlite::params![run_at, format!("-{KEEP_DAYS} days")],
    )?;
    tx.commit()?;
    Ok(written)
}

/// Point a run's rows at the briefing the run went on to make. Returns the rows changed.
pub fn link(conn: &Connection, run_at: &str, mode: &str, briefing_id: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE feed_runs SET briefing_id = ?3 WHERE run_at = ?1 AND mode = ?2",
        rusqlite::params![run_at, mode, briefing_id],
    )
}

fn open(db_path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open(db_path)?;
    // As the briefing writer opens it, since `record` may be the connection that
    // brings the schema up to date in its place.
    conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

/// Record the collect that just finished: its noted fetches and the three counts.
/// Called once the repeats are gone, which is before the run can stop for having
/// too little that is new, so a thin or failed run still leaves its rows.
///
/// A run on a machine with no database yet records nothing: creating the
/// database is the briefing writer's job, not this one's.
pub fn record(db_path: &Path, run_at: &str, mode: &str, fetched: &Tally, offered: &Tally, pooled: &Tally) {
    let fetches = std::mem::take(&mut *notes());
    if !db_path.exists() {
        return;
    }
    let rows = rows(fetches, fetched, offered, pooled);
    let written = open(db_path).and_then(|conn| {
        // The run reaches this before the briefing writer has brought the schema
        // up to date, so the first run after an update would find no table.
        crate::db::run_migrations(&conn)?;
        Ok(save(&conn, run_at, mode, &rows)?)
    });
    match written {
        Ok(n) => {
            let failed = rows.iter().filter(|r| r.error.is_some()).count();
            tracing::info!("Recorded {} feed runs ({} feeds failed)", n, failed);
        }
        Err(e) => tracing::warn!("Feed run record failed (non-fatal): {:#}", e),
    }
}

/// Point the rows `record` wrote for this run at its briefing.
pub fn attach(db_path: &Path, run_at: &str, mode: &str, briefing_id: i64) {
    if let Err(e) = open(db_path).and_then(|conn| Ok(link(&conn, run_at, mode, briefing_id)?)) {
        tracing::warn!("Feed run link failed (non-fatal): {:#}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(feed_id: &str, source_type: &str) -> RawArticle {
        RawArticle {
            title: "t".into(),
            url: "https://a.com/x".into(),
            source_name: "src".into(),
            source_url: String::new(),
            published_at: None,
            content_snippet: String::new(),
            sector: "ai".into(),
            feed_id: feed_id.into(),
            language: "en".into(),
            source_type: source_type.into(),
            financial_metadata: None,
        }
    }

    fn fetch(feed_id: &str, error: Option<&str>, millis: i64, entries: Option<i64>) -> Fetch {
        Fetch {
            feed_id: feed_id.into(),
            kind: "rss",
            name: format!("name of {feed_id}"),
            sector: "ai".into(),
            error: error.map(str::to_string),
            millis,
            entries,
        }
    }

    fn counts(pairs: &[(&str, usize)]) -> Tally {
        pairs.iter().map(|(id, n)| (id.to_string(), *n)).collect()
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    /// The noted fetches are one list for the whole process, and tests run side by
    /// side: a test that empties the list holds this while it does.
    static EMPTIES_THE_NOTES: Mutex<()> = Mutex::new(());

    fn alone() -> std::sync::MutexGuard<'static, ()> {
        EMPTIES_THE_NOTES.lock().unwrap_or_else(|e| e.into_inner())
    }

    type Stored = (String, String, i64, Option<String>, Option<i64>, Option<i64>, i64, i64, i64, Option<i64>);

    fn stored(conn: &Connection) -> Vec<Stored> {
        conn.prepare(
            "SELECT feed_id, kind, ok, error, millis, entries, fetched, offered, pooled, briefing_id
             FROM feed_runs ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn counts_news_per_feed_and_leaves_the_filings_out() {
        let articles = vec![
            article("rss_a", "news"),
            article("rss_a", "news"),
            article("rss_b", "news"),
            article("fec_123", "financial"),
        ];
        assert_eq!(tally(&articles), counts(&[("rss_a", 2), ("rss_b", 1)]));
    }

    /// The four kinds of feed the table exists to tell apart.
    #[test]
    fn a_row_says_what_became_of_each_feed() {
        let fetches = vec![
            fetch("rss_working", None, 120, Some(20)),
            fetch("rss_failed", Some("HTTP 403: no feed found"), 80, None),
            fetch("rss_nothing_recent", None, 60, Some(15)),
            fetch("rss_other_mode", None, 90, Some(10)),
        ];
        let fetched = counts(&[("rss_working", 12), ("rss_other_mode", 10)]);
        let offered = counts(&[("rss_working", 12)]);
        let pooled = counts(&[("rss_working", 5)]);

        let got = rows(fetches, &fetched, &offered, &pooled);
        let brief: Vec<_> = got
            .iter()
            .map(|r| (r.feed_id.as_str(), r.error.is_some(), r.entries, r.fetched, r.offered, r.pooled))
            .collect();
        assert_eq!(
            brief,
            vec![
                ("rss_working", false, Some(20), 12, 12, 5),
                ("rss_failed", true, None, 0, 0, 0),
                ("rss_nothing_recent", false, Some(15), 0, 0, 0),
                ("rss_other_mode", false, Some(10), 10, 0, 0),
            ]
        );
        assert_eq!(got[0].name, "name of rss_working");
        assert_eq!(got[0].millis, Some(120));
    }

    /// A source that never reports its fetches still shows up through its articles,
    /// rather than vanishing from the table.
    #[test]
    fn a_feed_with_articles_and_no_noted_fetch_still_gets_a_row() {
        let fetched = counts(&[("new_z", 4), ("new_a", 2)]);
        let offered = counts(&[("new_z", 3)]);
        let got = rows(vec![fetch("rss_a", None, 10, Some(1))], &fetched, &offered, &Tally::new());

        let ids: Vec<_> = got.iter().map(|r| r.feed_id.as_str()).collect();
        assert_eq!(ids, vec!["rss_a", "new_a", "new_z"], "noted feeds first, the rest by id");
        let z = &got[2];
        assert_eq!((z.kind.as_str(), z.millis, z.entries), ("other", None, None));
        assert_eq!((z.fetched, z.offered, z.pooled), (4, 3, 0));
        assert!(z.error.is_none());
    }

    /// The table holds one row per feed per run, so a second fetch under the same id
    /// must fold into the first instead of failing the whole run's insert.
    #[test]
    fn two_fetches_under_one_id_make_one_row() {
        let fetches = vec![
            fetch("rss_a", None, 100, Some(20)),
            fetch("rss_a", Some("timed out"), 900, None),
            fetch("rss_b", None, 5, Some(1)),
            fetch("rss_a", None, 50, Some(7)),
        ];
        let got = rows(fetches, &counts(&[("rss_a", 9)]), &Tally::new(), &Tally::new());

        assert_eq!(got.len(), 2);
        let a = &got[0];
        assert_eq!(a.error.as_deref(), Some("timed out"), "it failed if any of them did");
        assert_eq!(a.millis, Some(900), "as slow as the slowest");
        assert_eq!(a.entries, Some(27));
        assert_eq!(a.fetched, 9);

        let conn = db();
        assert_eq!(save(&conn, "2026-11-14 13:00:00", "daily", &got).unwrap(), 2);
    }

    #[test]
    fn saved_rows_read_back_as_written() {
        let conn = db();
        let got = rows(
            vec![
                fetch("rss_a", None, 120, Some(20)),
                fetch("rss_b", Some("HTTP 404: no feed found"), 80, None),
            ],
            &counts(&[("rss_a", 12)]),
            &counts(&[("rss_a", 11)]),
            &counts(&[("rss_a", 4)]),
        );
        assert_eq!(save(&conn, "2026-11-14 13:00:00", "daily", &got).unwrap(), 2);

        assert_eq!(
            stored(&conn),
            vec![
                ("rss_a".into(), "rss".into(), 1, None, Some(120), Some(20), 12, 11, 4, None),
                (
                    "rss_b".into(),
                    "rss".into(),
                    0,
                    Some("HTTP 404: no feed found".into()),
                    Some(80),
                    None,
                    0,
                    0,
                    0,
                    None
                ),
            ]
        );
    }

    /// The daily run and the freedoms run of one morning fetch the same feeds, and
    /// only the run that made the briefing may be pointed at it.
    #[test]
    fn a_briefing_is_linked_to_its_own_run_only() {
        let conn = db();
        let one = rows(vec![fetch("rss_a", None, 1, Some(1))], &Tally::new(), &Tally::new(), &Tally::new());
        save(&conn, "2026-11-14 13:00:00", "daily", &one).unwrap();
        save(&conn, "2026-11-14 13:00:00", "freedoms", &one).unwrap();
        save(&conn, "2026-11-15 02:00:00", "daily", &one).unwrap();

        assert_eq!(link(&conn, "2026-11-14 13:00:00", "daily", 77).unwrap(), 1);

        let linked: Vec<(String, String, Option<i64>)> = conn
            .prepare("SELECT run_at, mode, briefing_id FROM feed_runs ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            linked,
            vec![
                ("2026-11-14 13:00:00".into(), "daily".into(), Some(77)),
                ("2026-11-14 13:00:00".into(), "freedoms".into(), None),
                ("2026-11-15 02:00:00".into(), "daily".into(), None),
            ]
        );
    }

    #[test]
    fn rows_past_the_keep_window_are_dropped_when_a_run_is_saved() {
        let conn = db();
        let one = rows(vec![fetch("rss_a", None, 1, Some(1))], &Tally::new(), &Tally::new(), &Tally::new());
        save(&conn, "2026-01-01 13:00:00", "daily", &one).unwrap();
        // 179 days before the newest run below: inside the window.
        save(&conn, "2026-05-19 13:00:00", "daily", &one).unwrap();
        save(&conn, "2026-11-14 13:00:00", "daily", &one).unwrap();

        let kept: Vec<String> = conn
            .prepare("SELECT run_at FROM feed_runs ORDER BY run_at")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kept, vec!["2026-05-19 13:00:00", "2026-11-14 13:00:00"]);
    }

    #[test]
    fn a_long_error_is_stored_capped_and_whole_characters_only() {
        // Three bytes each, and the cap is not a multiple of three once the first
        // letter is in front: the cut has to step back to a whole character.
        let long = anyhow::anyhow!("x{}", "\u{2019}".repeat(ERROR_MAX_BYTES));
        let _alone = alone();
        begin();
        note("rss_long".into(), "rss", "Long", "ai", Instant::now(), Err(&long));
        note("rss_fine".into(), "rss", "Fine", "ai", Instant::now(), Ok(3));
        let got = std::mem::take(&mut *notes());

        let long = got.iter().find(|f| f.feed_id == "rss_long").expect("the failed fetch was noted");
        let stored = long.error.as_deref().expect("a failed fetch keeps its error");
        assert_eq!(stored.len(), ERROR_MAX_BYTES - 2, "cut at the last whole character under the cap");
        assert!(stored.starts_with("x\u{2019}") && stored.ends_with('\u{2019}'));
        assert_eq!(long.entries, None);
        let fine = got.iter().find(|f| f.feed_id == "rss_fine").expect("the good fetch was noted");
        assert_eq!((fine.error.as_deref(), fine.entries), (None, Some(3)));
    }

    #[tokio::test]
    async fn watching_a_fetch_passes_its_articles_and_its_error_through() {
        let good = watch("watch_good".into(), "rss", "Good", "ai", async {
            Ok((vec![article("watch_good", "news")], 9))
        })
        .await
        .unwrap();
        assert_eq!(good.len(), 1);

        let bad = watch("watch_bad".into(), "rss", "Bad", "ai", async { anyhow::bail!("HTTP 500: nope") }).await;
        assert_eq!(bad.unwrap_err().to_string(), "HTTP 500: nope");
    }

    /// The whole path the pipeline uses, on a real file: record, then attach.
    #[test]
    fn a_run_is_recorded_in_the_database_file_and_linked_to_its_briefing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pulse.db");
        // A database from before this migration, as the first run after an update finds it.
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE marker (x INTEGER);").unwrap();

        let fetched = counts(&[("record_feed", 6)]);
        let offered = counts(&[("record_feed", 5)]);
        let pooled = counts(&[("record_feed", 2)]);
        let _alone = alone();
        record(&path, "2026-11-14 13:00:00", "daily", &fetched, &offered, &pooled);
        attach(&path, "2026-11-14 13:00:00", "daily", 41);

        let conn = Connection::open(&path).unwrap();
        let row = stored(&conn).into_iter().find(|r| r.0 == "record_feed").expect("the feed has a row");
        assert_eq!((row.6, row.7, row.8, row.9), (6, 5, 2, Some(41)));
    }

    #[test]
    fn a_run_with_no_database_yet_records_nothing_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        let _alone = alone();
        record(&path, "2026-11-14 13:00:00", "daily", &Tally::new(), &Tally::new(), &Tally::new());
        assert!(!path.exists());
    }
}
