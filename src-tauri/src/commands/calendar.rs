//! The Calendar page. The data is built by `pulse_calendar::view` (shared with
//! the iPhone snapshot); this is only the command.

use tauri::State;

use crate::db::DbState;
use pulse_calendar::view::{CalendarData, DEFAULT_DAYS, build_calendar};


/// Earnings and macro dates from two days ago through the next `days` (default
/// 30, at most 90). Reads only the local DB.
#[tauri::command]
pub fn get_calendar(db: State<'_, DbState>, days: Option<i64>) -> Result<CalendarData, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    build_calendar(&conn, chrono::Local::now().date_naive(), days.unwrap_or(DEFAULT_DAYS))
}

