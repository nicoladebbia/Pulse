mod commands;
mod db;
mod models;
mod services;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let app_data = app
                .path()
                .app_data_dir()
                .expect("failed to get app data dir");
            std::fs::create_dir_all(&app_data).ok();

            let db_path = app_data.join("pulse.db");
            let conn = db::connection::initialize(&db_path)
                .expect("failed to initialize database");
            app.manage(db::DbState(std::sync::Mutex::new(conn)));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::briefing::get_today_briefing,
            commands::briefing::get_briefing_by_date,
            commands::stories::get_stories_by_sector,
            commands::stories::get_story_detail,
            commands::search::full_text_search,
            commands::fetch::trigger_manual_fetch,
            commands::fetch::get_fetch_status,
        ])
        .run(tauri::generate_context!())
        .expect("error running Pulse");
}
