use tauri::State;
use crate::db::DbState;
use crate::services::{api_usage, brave_search};

#[tauri::command]
pub fn get_api_usage(db: State<DbState>, days: u32) -> Result<api_usage::UsageStats, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    api_usage::get_usage(&conn, days).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_tavily_quota(db: State<DbState>) -> Result<brave_search::TavilyQuota, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    Ok(brave_search::get_quota(&conn))
}
