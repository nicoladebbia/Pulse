//! Writes the phone snapshot (`pulse-snapshot` crate) after the runs that change
//! what the phone shows. It only reads: the database is opened read-only and the
//! broker is asked for the account, never to trade. A failure is logged and
//! never fails the run that called it.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// Build and write the snapshot. `Ok(None)` when it is switched off
/// (`PULSE_SNAPSHOT=off`). Without broker data (no keys, offline) the snapshot
/// is still written with the parts the database has.
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
    let snapshot = pulse_snapshot::build(&conn, today, chrono::Utc::now(), live.as_ref()).map_err(anyhow::Error::msg)?;
    drop(conn);
    let path = pulse_snapshot::write_atomic(&dir, &snapshot).map_err(anyhow::Error::msg)?;
    Ok(Some(path))
}

pub async fn refresh_nonfatal(db_path: &Path) {
    match refresh(db_path).await {
        Ok(Some(path)) => tracing::info!("Phone snapshot written to {}", path.display()),
        Ok(None) => {}
        Err(e) => tracing::warn!("Phone snapshot failed: {e}"),
    }
}
