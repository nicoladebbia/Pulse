//! Writes the phone snapshot (`pulse-snapshot` crate) after the runs that change
//! what the phone shows. It only reads: the database is opened read-only (the
//! standalone mode first brings its schema up to date) and the broker is asked
//! for the account, never to trade. A failure is logged and never fails the run
//! that called it.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Build and write the snapshot. `Ok(None)` when it is switched off
/// (`PULSE_SNAPSHOT=off`). Without broker data (no keys, offline) the snapshot
/// is still written with the parts the database has, and the account figures
/// of the copy already on disk (marked with when they were read).
pub async fn refresh(db_path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(dir) = pulse_snapshot::output_dir() else {
        return Ok(None);
    };
    let live = match pulse_snapshot::live::fetch().await {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!("Snapshot: no live account data ({e}), writing the rest");
            None
        }
    };
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let today = chrono::Local::now().date_naive();
    let previous = if live.is_none() { pulse_snapshot::read_previous(&dir) } else { None };
    let snapshot = pulse_snapshot::build_from(&conn, today, chrono::Utc::now(), live.as_ref(), previous.as_ref()).map_err(anyhow::Error::msg)?;
    drop(conn);
    let path = pulse_snapshot::write_atomic(&dir, &snapshot).map_err(anyhow::Error::msg)?;
    Ok(Some(path))
}

/// Bring the schema up to date before a standalone snapshot run: a database the
/// app has not opened since an update lacks the newest tables the snapshot reads.
/// The hook after the other modes skips this (they migrated already, and a
/// writer holding the database must not cost the snapshot its 10 s busy wait).
pub fn migrate(db_path: &Path) {
    let run = || -> anyhow::Result<()> {
        let conn = Connection::open(db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        crate::db::run_migrations(&conn)
    };
    if let Err(e) = run() {
        tracing::warn!("Snapshot: could not update the database schema ({e}), continuing");
    }
}

pub async fn refresh_nonfatal(db_path: &Path) {
    match refresh(db_path).await {
        Ok(Some(path)) => tracing::info!("Phone snapshot written to {}", path.display()),
        Ok(None) => {}
        Err(e) => tracing::warn!("Phone snapshot failed: {e}"),
    }
}
