use std::sync::atomic::{AtomicBool, Ordering};
use serde::Serialize;

static FETCHING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize)]
pub struct FetchStatus {
    pub running: bool,
    pub stage: Option<String>,
    pub stage_label: Option<String>,
    pub percent: Option<u8>,
    pub detail: Option<String>,
    pub elapsed_secs: Option<u64>,
    pub eta_secs: Option<u64>,
}

#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}

#[tauri::command]
pub async fn trigger_manual_fetch() -> Result<String, String> {
    if FETCHING.swap(true, Ordering::SeqCst) {
        return Err("Fetch already in progress".to_string());
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

    // Clean up any stale progress file
    let progress_path = progress_file_path();
    let _ = std::fs::remove_file(&progress_path);

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

        let failed = match result {
            Ok(output) => {
                if output.status.success() {
                    tracing::info!("Manual fetch completed successfully");
                    false
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!("Manual fetch failed: {}", stderr);
                    // Write failure status so frontend can detect it
                    let fail_json = serde_json::json!({
                        "stage": "failed",
                        "stage_label": "Fetch failed",
                        "percent": 0,
                        "error": stderr.chars().take(200).collect::<String>(),
                    });
                    let _ = std::fs::write(progress_file_path(), fail_json.to_string());
                    true
                }
            }
            Err(e) => {
                tracing::warn!("Manual fetch error: {}", e);
                let fail_json = serde_json::json!({
                    "stage": "failed",
                    "stage_label": "Fetch failed",
                    "percent": 0,
                    "error": e.to_string(),
                });
                let _ = std::fs::write(progress_file_path(), fail_json.to_string());
                true
            }
        };

        // Clean up progress file only on success
        if !failed {
            let _ = std::fs::remove_file(progress_file_path());
        }
        FETCHING.store(false, Ordering::SeqCst);
    });

    Ok("Fetch started".to_string())
}

#[tauri::command]
pub fn get_fetch_status() -> Result<FetchStatus, String> {
    let running = FETCHING.load(Ordering::SeqCst);

    if !running {
        // Check if a failure progress file was left behind
        let progress_path = progress_file_path();
        if let Ok(json_str) = std::fs::read_to_string(&progress_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if val["stage"].as_str() == Some("failed") {
                    // Clean up the failure file after reading it once
                    let _ = std::fs::remove_file(&progress_path);
                    return Ok(FetchStatus {
                        running: false,
                        stage: Some("failed".to_string()),
                        stage_label: val["stage_label"].as_str().map(String::from),
                        percent: Some(0),
                        detail: val["error"].as_str().map(String::from),
                        elapsed_secs: None,
                        eta_secs: None,
                    });
                }
            }
        }
        return Ok(FetchStatus {
            running: false,
            stage: None,
            stage_label: None,
            percent: None,
            detail: None,
            elapsed_secs: None,
            eta_secs: None,
        });
    }

    // Try to read progress file
    let progress_path = progress_file_path();
    let progress = std::fs::read_to_string(&progress_path).ok();

    if let Some(json_str) = progress {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
            let percent = val["percent"].as_u64().map(|p| p as u8);
            let started_at = val["started_at"].as_str().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s).ok()
            });

            let elapsed_secs = started_at.map(|sa| {
                (chrono::Utc::now() - sa.with_timezone(&chrono::Utc)).num_seconds().max(0) as u64
            });

            let eta_secs = match (elapsed_secs, percent) {
                (Some(elapsed), Some(pct)) if pct > 2 => {
                    let total_est = elapsed as f64 / (pct as f64 / 100.0);
                    Some((total_est - elapsed as f64).max(0.0) as u64)
                }
                _ => None,
            };

            return Ok(FetchStatus {
                running: true,
                stage: val["stage"].as_str().map(String::from),
                stage_label: val["stage_label"].as_str().map(String::from),
                percent,
                detail: val["detail"].as_str().map(String::from),
                elapsed_secs,
                eta_secs,
            });
        }
    }

    // Running but no progress file yet
    Ok(FetchStatus {
        running: true,
        stage: Some("starting".to_string()),
        stage_label: Some("Starting fetch...".to_string()),
        percent: Some(0),
        detail: None,
        elapsed_secs: Some(0),
        eta_secs: None,
    })
}

fn progress_file_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.pulse.app")
        .join("fetch-progress.json")
}
