use tauri::State;

use crate::db::DbState;
use crate::services::predictions::{self, Prediction, PredictionStats};

#[tauri::command]
pub fn get_all_predictions(db: State<DbState>) -> Result<Vec<Prediction>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    predictions::get_all_predictions(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_prediction_stats(db: State<DbState>) -> Result<PredictionStats, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    predictions::get_prediction_stats(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn manually_validate_prediction(
    db: State<DbState>,
    id: i64,
    outcome: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    predictions::manually_validate_prediction(&conn, id, &outcome).map_err(|e| e.to_string())
}
