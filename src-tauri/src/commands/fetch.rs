use std::sync::atomic::{AtomicBool, Ordering};

static FETCHING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub fn trigger_manual_fetch() -> Result<String, String> {
    if FETCHING.swap(true, Ordering::SeqCst) {
        return Err("Fetch already in progress".to_string());
    }

    // TODO: Spawn pulse-fetcher sidecar binary
    // For now, just return immediately
    FETCHING.store(false, Ordering::SeqCst);

    Ok("Fetch triggered".to_string())
}

#[tauri::command]
pub fn get_fetch_status() -> Result<bool, String> {
    Ok(FETCHING.load(Ordering::SeqCst))
}
