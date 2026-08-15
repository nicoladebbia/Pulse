use std::sync::atomic::{AtomicBool, Ordering};
use serde::Serialize;

static FETCHING: AtomicBool = AtomicBool::new(false);

/// A run whose progress file hasn't been touched in this long is treated as no
/// longer running (crashed / hard-killed). Measured from real fetch logs: the
/// longest silence between progress writes on a HEALTHY run is Phase 12 market
/// prices (~2m19s) and a Phase-4 analyze re-roll (2 back-to-back 70B generations,
/// un-heartbeatable). 240s clears both with margin. Erring high is deliberate: a
/// healthy run flickering to "interrupted" destroys the bar's credibility, while a
/// crashed run showing "running" a few minutes too long costs nothing.
const STALE_AFTER_SECS: i64 = 240;

#[derive(Debug, Clone, Serialize)]
pub struct FetchStatus {
    /// True iff a fetch is actively running right now (any process — app OR launchd).
    pub running: bool,
    pub stage: Option<String>,
    pub stage_label: Option<String>,
    pub percent: Option<u8>,
    pub detail: Option<String>,
    pub elapsed_secs: Option<u64>,
    pub eta_secs: Option<u64>,
    /// Terminal state of the most recent run: "complete" | "failed" | "interrupted"
    /// | "running" | "idle". Drives the always-visible last-run line in the UI.
    pub last_status: String,
    /// Human-readable reason when last_status == "failed" (e.g. "Groq blocked").
    pub last_reason: Option<String>,
    /// RFC3339 timestamp of the last progress write, for "Updated 2h ago".
    pub last_at: Option<String>,
}

#[tauri::command]
pub async fn trigger_manual_fetch() -> Result<String, String> {
    if FETCHING.swap(true, Ordering::SeqCst) {
        return Err("Fetch already in progress".to_string());
    }

    // FETCHING only knows about THIS app's spawns. A launchd slot can be mid-run right
    // now, and the fetcher it would spawn exits immediately on the single-instance lock —
    // leaving write_starting()'s record to go stale and paint a false "interrupted" over
    // a healthy run. Defer to whoever is already running.
    if let Ok(status) = get_fetch_status() {
        if status.running {
            FETCHING.store(false, Ordering::SeqCst);
            return Err("Fetch already in progress".to_string());
        }
    }

    // Find the fetcher binary (release or debug)
    let fetcher_path = {
        let release = dirs::home_dir()
            .unwrap_or_default()
            .join("Projects/Pulse/target/release/pulse-fetcher");
        let debug = dirs::home_dir()
            .unwrap_or_default()
            .join("Projects/Pulse/target/debug/pulse-fetcher");
        if release.exists() {
            release
        } else if debug.exists() {
            debug
        } else {
            FETCHING.store(false, Ordering::SeqCst);
            return Err("pulse-fetcher binary not found".to_string());
        }
    };

    // Immediately overwrite the progress file with a fresh "running" record so the
    // UI lights up on click, before the fetcher process has booted (form4/enrich run
    // for several seconds before the fetcher writes its own first progress line). This
    // also clears any stale "failed"/"interrupted" state from a previous run so a fixed
    // failure never haunts the next fetch.
    write_starting();

    // Spawn the fetcher in a background task
    let path = fetcher_path.clone();
    tokio::spawn(async move {
        tracing::info!("Manual fetch started: {}", path.display());

        let result = tokio::process::Command::new(&path)
            .arg("--mode")
            .arg("daily")
            .arg("--force")
            .current_dir(
                dirs::home_dir()
                    .unwrap_or_default()
                    .join("Projects/Pulse"),
            )
            .output()
            .await;

        match result {
            Ok(output) => {
                if output.status.success() {
                    // The fetcher itself wrote "complete"; leave the file as-is so the
                    // UI can show "Updated just now". Do NOT delete it.
                    tracing::info!("Manual fetch completed successfully");
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!("Manual fetch failed: {}", stderr);
                    // Only stamp a failure if the fetcher didn't already record one of
                    // its own (a clean bail writes stage:"failed" with a real reason).
                    if !file_is_failed() {
                        write_failed(&last_line(&stderr));
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Manual fetch spawn error: {}", e);
                write_failed(&e.to_string());
            }
        }

        FETCHING.store(false, Ordering::SeqCst);
    });

    Ok("Fetch started".to_string())
}

#[tauri::command]
pub fn get_fetch_status() -> Result<FetchStatus, String> {
    let progress_path = progress_file_path();

    // Absent file → nothing has ever run (or it was cleared). Idle, not error.
    let json_str = match std::fs::read_to_string(&progress_path) {
        Ok(s) => s,
        Err(_) => return Ok(idle_status()),
    };
    let val: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return Ok(idle_status()),
    };

    Ok(classify_status(&val, chrono::Utc::now()))
}

/// Pure classification of a progress-file JSON into a FetchStatus at time `now`. This is
/// the load-bearing freshness logic (stale in-progress ⇒ interrupted) — kept pure so it's
/// unit-testable without touching the real filesystem/clock.
fn classify_status(val: &serde_json::Value, now: chrono::DateTime<chrono::Utc>) -> FetchStatus {
    let stage = val["stage"].as_str().unwrap_or("").to_string();
    let updated_at = val["updated_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let age_secs = updated_at.map(|ua| (now - ua.with_timezone(&chrono::Utc)).num_seconds().max(0));
    let last_at = val["updated_at"].as_str().map(String::from);

    // Terminal states, verbatim from the file.
    if stage == "complete" {
        return FetchStatus {
            running: false,
            stage: Some(stage),
            stage_label: val["stage_label"].as_str().map(String::from),
            percent: Some(100),
            detail: None,
            elapsed_secs: None,
            eta_secs: None,
            last_status: "complete".to_string(),
            last_reason: None,
            last_at,
        };
    }
    if stage == "failed" {
        // Accept both "reason" (fetcher clean bail) and "error" (app spawn failure).
        let reason = val["reason"]
            .as_str()
            .or_else(|| val["error"].as_str())
            .map(String::from);
        return FetchStatus {
            running: false,
            stage: Some(stage),
            stage_label: val["stage_label"].as_str().map(String::from),
            percent: val["percent"].as_u64().map(|p| p as u8),
            detail: reason.clone(),
            elapsed_secs: None,
            eta_secs: None,
            last_status: "failed".to_string(),
            last_reason: reason,
            last_at,
        };
    }

    // In-progress stage. Fresh → running; stale → the process died without recording
    // a terminal state (hard kill / panic / SIGKILL) → interrupted.
    let is_stale = age_secs.map(|a| a >= STALE_AFTER_SECS).unwrap_or(true);
    if is_stale {
        return FetchStatus {
            running: false,
            stage: Some(stage),
            stage_label: Some("Interrupted".to_string()),
            percent: val["percent"].as_u64().map(|p| p as u8),
            detail: Some("Fetch stopped unexpectedly".to_string()),
            elapsed_secs: None,
            eta_secs: None,
            last_status: "interrupted".to_string(),
            last_reason: Some("Fetch stopped unexpectedly".to_string()),
            last_at,
        };
    }

    // Genuinely running.
    let percent = val["percent"].as_u64().map(|p| p as u8);
    let started_at = val["started_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let elapsed_secs = started_at.map(|sa| (now - sa.with_timezone(&chrono::Utc)).num_seconds().max(0) as u64);
    let eta_secs = match (elapsed_secs, percent) {
        (Some(elapsed), Some(pct)) if pct > 2 => {
            let total_est = elapsed as f64 / (pct as f64 / 100.0);
            Some((total_est - elapsed as f64).max(0.0) as u64)
        }
        _ => None,
    };

    FetchStatus {
        running: true,
        stage: val["stage"].as_str().map(String::from),
        stage_label: val["stage_label"].as_str().map(String::from),
        percent,
        detail: val["detail"].as_str().map(String::from),
        elapsed_secs,
        eta_secs,
        last_status: "running".to_string(),
        last_reason: None,
        last_at,
    }
}

fn idle_status() -> FetchStatus {
    FetchStatus {
        running: false,
        stage: None,
        stage_label: None,
        percent: None,
        detail: None,
        elapsed_secs: None,
        eta_secs: None,
        last_status: "idle".to_string(),
        last_reason: None,
        last_at: None,
    }
}

/// Overwrite the progress file with a fresh "running" record (clears prior terminal state).
fn write_starting() {
    let now = chrono::Utc::now().to_rfc3339();
    let json = serde_json::json!({
        "stage": "starting",
        "stage_label": "Starting fetch…",
        "stage_num": 0,
        "total_stages": 10,
        "percent": 0,
        "detail": null,
        "started_at": now,
        "updated_at": now,
    });
    let _ = atomic_write(&json.to_string());
}

fn write_failed(reason: &str) {
    let json = serde_json::json!({
        "stage": "failed",
        "stage_label": "Fetch failed",
        "percent": 0,
        "reason": reason.chars().take(200).collect::<String>(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    let _ = atomic_write(&json.to_string());
}

/// True if the current progress file already records a terminal failure (so the app
/// wrapper doesn't clobber the fetcher's more specific reason with a generic one).
fn file_is_failed() -> bool {
    std::fs::read_to_string(progress_file_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["stage"].as_str().map(|s| s == "failed"))
        .unwrap_or(false)
}

fn last_line(s: &str) -> String {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Fetch failed")
        .to_string()
}

fn atomic_write(contents: &str) -> std::io::Result<()> {
    let path = progress_file_path();
    // Unique temp name: the fetcher process writes this same file (its own heartbeat +
    // stage writes). A shared "…tmp" path lets one process truncate the other's file
    // mid-write, and classify_status treats a parse error as "idle" — so a torn write
    // silently blanks the bar rather than erroring visibly.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    let result = std::fs::rename(&tmp, &path);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn progress_file_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.pulse.app")
        .join("fetch-progress.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use serde_json::json;

    // A fixed "now" so tests are deterministic; ages are computed relative to it.
    fn now() -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ts(secs_ago: i64) -> String {
        (now() - Duration::seconds(secs_ago)).to_rfc3339()
    }

    #[test]
    fn fresh_in_progress_reads_running() {
        // Embeddings stage updated 30s ago (well under the 240s stale window).
        let v = json!({
            "stage": "embeddings", "stage_label": "Generating embeddings",
            "percent": 60, "detail": "Embedding batch 3/8",
            "started_at": ts(400), "updated_at": ts(30),
        });
        let s = classify_status(&v, now());
        assert!(s.running, "fresh in-progress must be running");
        assert_eq!(s.last_status, "running");
        assert_eq!(s.percent, Some(60));
    }

    #[test]
    fn stale_in_progress_reads_interrupted() {
        // Same stage but last write 300s ago (>240s) — the process died mid-run.
        let v = json!({
            "stage": "embeddings", "stage_label": "Generating embeddings",
            "percent": 60, "started_at": ts(600), "updated_at": ts(300),
        });
        let s = classify_status(&v, now());
        assert!(!s.running, "stale in-progress must NOT be running");
        assert_eq!(s.last_status, "interrupted");
        assert_eq!(s.percent, Some(60), "keeps last percent for the UI");
    }

    #[test]
    fn just_under_threshold_still_running() {
        // 239s < 240s — boundary must still read running (a healthy Phase-12/analyze gap).
        let v = json!({
            "stage": "analyzing", "percent": 50,
            "started_at": ts(300), "updated_at": ts(239),
        });
        assert!(classify_status(&v, now()).running);
    }

    #[test]
    fn complete_reads_complete_not_running() {
        let v = json!({
            "stage": "complete", "stage_label": "Complete",
            "percent": 100, "updated_at": ts(120),
        });
        let s = classify_status(&v, now());
        assert!(!s.running);
        assert_eq!(s.last_status, "complete");
        assert_eq!(s.percent, Some(100));
    }

    #[test]
    fn failed_with_reason_surfaces_reason() {
        // Fetcher clean bail writes "reason".
        let v = json!({
            "stage": "failed", "stage_label": "Fetch failed",
            "reason": "Daily run aborted: Groq blocked", "updated_at": ts(7200),
        });
        let s = classify_status(&v, now());
        assert!(!s.running);
        assert_eq!(s.last_status, "failed");
        assert_eq!(s.last_reason.as_deref(), Some("Daily run aborted: Groq blocked"));
        assert_eq!(s.last_at.as_deref(), Some(ts(7200).as_str()));
    }

    #[test]
    fn failed_with_error_key_also_surfaces() {
        // App spawn-failure path historically writes "error" instead of "reason".
        let v = json!({
            "stage": "failed", "error": "pulse-fetcher exited 1", "updated_at": ts(60),
        });
        let s = classify_status(&v, now());
        assert_eq!(s.last_status, "failed");
        assert_eq!(s.last_reason.as_deref(), Some("pulse-fetcher exited 1"));
    }

    #[test]
    fn missing_updated_at_treated_as_stale() {
        // A malformed/legacy in-progress record with no updated_at must not read running.
        let v = json!({ "stage": "summarizing", "percent": 20 });
        let s = classify_status(&v, now());
        assert!(!s.running);
        assert_eq!(s.last_status, "interrupted");
    }

    #[test]
    fn starting_stage_is_running_when_fresh() {
        // The write_starting()/start_run() record uses stage:"starting".
        let v = json!({
            "stage": "starting", "stage_label": "Starting fetch…",
            "percent": 0, "started_at": ts(2), "updated_at": ts(2),
        });
        assert!(classify_status(&v, now()).running);
    }

    // --- The regression this whole heartbeat exists for ---
    //
    // Phase 1 writes progress ONCE and then collects for 9–120 minutes (measured from
    // fetch-stdout.log, 2026-08-10..14). Before the fetcher heartbeat, every such run
    // read "interrupted" while it was still working — the red banner Nicola kept seeing.
    // These two tests pin BOTH directions: heartbeated-long-run stays running, and a
    // genuinely dead process still gets caught.

    #[test]
    fn hour_long_heartbeated_collect_reads_running() {
        // 62 minutes into Phase 1, still 0%, but the heartbeat stamped 15s ago.
        let v = json!({
            "stage": "collecting", "stage_label": "Collecting sources",
            "stage_num": 1, "percent": 0,
            "started_at": ts(3720), "updated_at": ts(15),
        });
        let s = classify_status(&v, now());
        assert!(s.running, "a heartbeated long collect must NOT read interrupted");
        assert_eq!(s.last_status, "running");
        assert_ne!(s.detail.as_deref(), Some("Fetch stopped unexpectedly"));
    }

    #[test]
    fn killed_mid_collect_still_reads_interrupted() {
        // Same long collect, but the process was killed: no heartbeat for 5 minutes.
        // The 240s crash detector must still fire — the heartbeat makes silence
        // meaningful, it does not suppress the interrupted state.
        let v = json!({
            "stage": "collecting", "stage_label": "Collecting sources",
            "stage_num": 1, "percent": 0,
            "started_at": ts(3720), "updated_at": ts(300),
        });
        let s = classify_status(&v, now());
        assert!(!s.running);
        assert_eq!(s.last_status, "interrupted");
        assert_eq!(s.detail.as_deref(), Some("Fetch stopped unexpectedly"));
    }
}
