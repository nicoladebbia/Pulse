use super::*;

/// How often the heartbeat re-stamps `updated_at` while a run is in flight.
/// Must stay well under the app's STALE_AFTER_SECS (240s) so a live run never
/// drifts into the "interrupted" window between two ticks.
pub(crate) const HEARTBEAT_SECS: u64 = 15;

/// Serializes every write to the progress file inside this process. The heartbeat
/// task and the pipeline's own stage writes both read-modify-write the same file;
/// without this the heartbeat can rename a stale-stage record over a newer one, and
/// two `atomic_write`s can collide. Held only around synchronous fs calls — never
/// across an `.await`.
pub(crate) static PROGRESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Distinguishes the temp file of concurrent writers. A shared `…tmp` path lets one
/// writer truncate another's file mid-write, and `get_fetch_status` treats a JSON
/// parse error as "idle" — so a torn write silently BLANKS the progress bar instead
/// of erroring visibly.
pub(crate) static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write `json` to `path` atomically (unique temp file + rename), serialized against
/// every other progress write in this process.
pub(crate) fn write_progress_json(path: &Path, json: &serde_json::Value) {
    let _guard = PROGRESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    write_progress_json_locked(path, json);
}

/// Atomic write assuming PROGRESS_LOCK is already held by the caller.
pub(crate) fn write_progress_json_locked(path: &Path, json: &serde_json::Value) {
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
    if std::fs::write(&tmp, serde_json::to_string(json).unwrap_or_default()).is_ok() {
        if std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Stops the heartbeat when the run leaves scope — including the `?` early-returns
/// (cost cap, mid-run abort). Otherwise a bailed run's non-terminal record would keep
/// being stamped "fresh" and read as still-running forever.
pub(crate) struct HeartbeatGuard(pub tokio::task::JoinHandle<()>);

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Keep the progress file's `updated_at` fresh for as long as this run is in flight.
///
/// The app classifies an in-progress record whose `updated_at` is older than
/// STALE_AFTER_SECS (240s) as "Fetch stopped unexpectedly". But Phase 1 calls
/// `start_stage(1)` ONCE and then collects sources for 9–120 minutes (measured from
/// fetch-stdout.log over 2026-08-10..14: 53s, 9m, 14m, 16m, 16m, 31m, 34m, 65m,
/// 120m), writing nothing in between — so a healthy run showed the red "interrupted"
/// badge on essentially every slow-network day while it was still working.
///
/// This makes silence mean what the app already assumes it means: the process is
/// gone. Only a dead (or hard-killed) fetcher stops stamping the file, so the 240s
/// crash detector stays intact — see the `heartbeat` tests in commands/fetch.rs.
pub(crate) fn spawn_heartbeat(path: std::path::PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_SECS)).await;

            // Re-read instead of caching: the pipeline's own writes own stage/percent,
            // and the heartbeat must only ever touch `updated_at`. The whole
            // read-modify-write runs under PROGRESS_LOCK (no `.await` inside) so it can
            // never rename a stale-stage record over a newer one the pipeline just wrote.
            let done = {
                let _guard = PROGRESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                match std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                {
                    Some(mut val) => {
                        // Terminal record → the run is over. Stop ticking so a finished
                        // (or failed) run is never held artificially "fresh".
                        if matches!(val["stage"].as_str(), Some("complete") | Some("failed")) {
                            true
                        } else {
                            // Phase 1 is the one stage that reports nothing while it
                            // runs (the 14 sources are join!ed, so nothing logs until
                            // they all return). Surface the completed-source count so
                            // the bar moves instead of sitting at 0% for minutes —
                            // "not lying" and "not looking stuck" are different bugs.
                            if val["stage"].as_str() == Some("collecting") {
                                let done = crate::sources::SOURCES_DONE
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                    .min(crate::sources::SOURCE_COUNT);
                                val["detail"] = serde_json::json!(format!(
                                    "{}/{} sources",
                                    done,
                                    crate::sources::SOURCE_COUNT
                                ));
                            }
                            val["updated_at"] =
                                serde_json::json!(chrono::Utc::now().to_rfc3339());
                            write_progress_json_locked(&path, &val);
                            false
                        }
                    }
                    // Unreadable/garbled right now (mid-rename, or not written yet) —
                    // skip this tick rather than inventing a record.
                    None => false,
                }
            };
            if done {
                return;
            }
        }
    })
}

/// Stage weights (approximate % of total pipeline time)
pub(crate) const STAGE_WEIGHTS: &[(u8, &str, &str)] = &[
    (5,  "collecting",         "Collecting sources"),
    (2,  "deduplicating",      "Deduplicating articles"),
    (40, "summarizing",        "Summarizing stories"),
    (10, "analyzing",          "Cross-sector analysis"),
    (3,  "executive_summary",  "Executive summary"),
    (7,  "contextual",         "Contextual prefixes"),
    (5,  "embeddings",         "Generating embeddings"),
    (2,  "writing_db",         "Writing to database"),
    (18, "entities",           "Extracting entities"),
    (8,  "deep_summaries",     "Deep analysis (top stories)"),
];

pub(crate) struct ProgressWriter {
    path: std::path::PathBuf,
    started_at: String,
    current_stage: usize,
}

impl ProgressWriter {
    pub fn new(db_path: &Path) -> Self {
        let dir = db_path.parent().unwrap_or(Path::new("."));
        Self {
            path: dir.join("fetch-progress.json"),
            started_at: chrono::Utc::now().to_rfc3339(),
            current_stage: 0,
        }
    }

    pub fn start_stage(&mut self, stage_num: usize) {
        self.current_stage = stage_num;
        self.write_progress(None, 0.0);
    }

    pub fn update_detail(&self, detail: &str, sub_pct: f64) {
        self.write_progress(Some(detail), sub_pct);
    }

    pub fn finish(&self) {
        let json = serde_json::json!({
            "stage": "complete",
            "stage_label": "Complete",
            "stage_num": STAGE_WEIGHTS.len(),
            "total_stages": STAGE_WEIGHTS.len(),
            "percent": 100,
            "detail": null,
            "started_at": self.started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = self.atomic_write(&json);
    }

    /// Write a fresh "running" record at the very start of a run. This clears any
    /// terminal state (failed/interrupted) a PREVIOUS run left behind, so a fixed
    /// failure never haunts the next fetch. Must fire before the pre-Phase-1 network
    /// calls (form4/enrich) — otherwise the file shows the old state during them.
    pub fn start_run(&self) {
        let json = serde_json::json!({
            "stage": "starting",
            "stage_label": "Starting fetch…",
            "stage_num": 0,
            "total_stages": STAGE_WEIGHTS.len(),
            "percent": 0,
            "detail": null,
            "started_at": self.started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = self.atomic_write(&json);
    }

    /// Re-stamp the "starting" record with a fresh updated_at + detail. Keeps the file
    /// fresh during the pre-Phase-1 SEC calls (Form4 fetch/enrich), which can run tens of
    /// seconds with no start_stage() — without this, heavy EDGAR throttling at the very
    /// start of a healthy run could be misread as an interruption.
    pub fn heartbeat_starting(&self, detail: &str) {
        let json = serde_json::json!({
            "stage": "starting",
            "stage_label": "Starting fetch…",
            "stage_num": 0,
            "total_stages": STAGE_WEIGHTS.len(),
            "percent": 0,
            "detail": detail,
            "started_at": self.started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = self.atomic_write(&json);
    }

    fn write_progress(&self, detail: Option<&str>, sub_pct: f64) {
        let idx = self.current_stage.saturating_sub(1).min(STAGE_WEIGHTS.len() - 1);
        let (weight, stage_id, stage_label) = STAGE_WEIGHTS[idx];

        // percent = sum of completed stage weights + current stage partial
        let completed_weight: u8 = STAGE_WEIGHTS.iter().take(idx).map(|(w, _, _)| w).sum();
        let percent = (completed_weight as f64 + (weight as f64 * sub_pct / 100.0)).min(99.0) as u8;

        let json = serde_json::json!({
            "stage": stage_id,
            "stage_label": stage_label,
            "stage_num": self.current_stage,
            "total_stages": STAGE_WEIGHTS.len(),
            "percent": percent,
            "detail": detail,
            "started_at": self.started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = self.atomic_write(&json);
    }

    fn atomic_write(&self, json: &serde_json::Value) -> std::io::Result<()> {
        write_progress_json(&self.path, json);
        Ok(())
    }
}

/// The canonical progress-file path (same as `ProgressWriter::new`), so callers that
/// don't hold a ProgressWriter (e.g. main.rs on an early bail) can still find it.
pub(crate) fn progress_file_path(db_path: &Path) -> std::path::PathBuf {
    db_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("fetch-progress.json")
}

/// Standalone durable-failure writer. Used by `ProgressWriter::fail` AND directly by
/// main.rs when `pipeline::run` returns Err (the cost-cap bail can fire before any
/// ProgressWriter method was called, so main.rs owns writing the terminal state).
pub(crate) fn write_failed_state(path: &Path, reason: &str) {
    let json = serde_json::json!({
        "stage": "failed",
        "stage_label": "Fetch failed",
        "percent": 0,
        "reason": reason.chars().take(200).collect::<String>(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    write_progress_json(path, &json);
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    /// The heartbeat must move `updated_at` on a live in-progress record — this is the
    /// state transition the whole fix exists for (a steady-state test would pass even
    /// with the heartbeat doing nothing).
    #[tokio::test(start_paused = true)]
    async fn heartbeat_refreshes_stale_in_progress_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fetch-progress.json");
        let old = "2026-08-14T17:10:47+00:00";
        std::fs::write(
            &path,
            serde_json::json!({
                "stage": "collecting", "stage_label": "Collecting sources",
                "percent": 0, "started_at": old, "updated_at": old,
            })
            .to_string(),
        )
        .unwrap();

        let handle = spawn_heartbeat(path.clone());
        tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_SECS * 3)).await;
        handle.abort();

        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_ne!(
            val["updated_at"].as_str().unwrap(),
            old,
            "heartbeat must refresh updated_at on a live run"
        );
        assert_eq!(
            val["stage"].as_str(),
            Some("collecting"),
            "heartbeat must not disturb stage"
        );
    }

    /// A terminal record must never be kept artificially fresh.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_stops_on_terminal_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fetch-progress.json");
        let done = "2026-08-14T17:10:47+00:00";
        std::fs::write(
            &path,
            serde_json::json!({
                "stage": "complete", "percent": 100, "updated_at": done,
            })
            .to_string(),
        )
        .unwrap();

        let handle = spawn_heartbeat(path.clone());
        tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_SECS * 3)).await;

        let val: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(val["updated_at"].as_str(), Some(done));
        assert!(handle.is_finished(), "heartbeat must exit on a terminal record");
    }

    /// Concurrent writers must never leave a half-written file: `get_fetch_status`
    /// classifies a parse error as "idle", so a torn write blanks the bar silently.
    #[test]
    fn concurrent_writes_never_produce_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fetch-progress.json");
        write_progress_json(&path, &serde_json::json!({"stage": "starting"}));

        std::thread::scope(|scope| {
            for writer in 0..4 {
                let path = path.clone();
                scope.spawn(move || {
                    for i in 0..250 {
                        write_progress_json(
                            &path,
                            &serde_json::json!({
                                "stage": "collecting",
                                "detail": format!("writer {writer} iter {i}"),
                                "percent": i % 100,
                                "updated_at": chrono::Utc::now().to_rfc3339(),
                            }),
                        );
                    }
                });
            }
            // Reader races the writers the way the app's poll does.
            scope.spawn(|| {
                for _ in 0..1000 {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        assert!(
                            serde_json::from_str::<serde_json::Value>(&text).is_ok(),
                            "progress file must always parse; got: {text}"
                        );
                    }
                }
            });
        });

        // No temp files may be left behind in the app-support directory.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leaked temp files: {leftovers:?}");
    }
}
