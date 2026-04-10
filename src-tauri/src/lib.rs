mod commands;
mod db;
mod models;
mod services;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let app_data = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("failed to get app data dir: {e}"))?;
            std::fs::create_dir_all(&app_data).ok();

            let db_path = app_data.join("pulse.db");

            // Load .env from the project directory for API keys
            let project_env = dirs::home_dir()
                .unwrap_or_default()
                .join("Projects/Pulse/.env");
            if project_env.exists() {
                dotenvy::from_path(&project_env).ok();
            }

            // Log to file since macOS swallows stderr for .app bundles
            let log_dir = dirs::home_dir().unwrap_or_default().join("Library/Logs/Pulse");
            std::fs::create_dir_all(&log_dir).ok();
            let log_path = log_dir.join("app-debug.log");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                use std::io::Write;
                let _ = writeln!(f, "\n--- Pulse started at {} ---", chrono::Local::now());
                let _ = writeln!(f, "DB path: {}", db_path.display());
                let _ = writeln!(f, "DB exists: {}", db_path.exists());
            }

            let conn = db::connection::initialize(&db_path)
                .map_err(|e| format!("failed to initialize database: {e}"))?;
            app.manage(db::DbState(std::sync::Mutex::new(conn)));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::briefing::get_today_briefing,
            commands::briefing::get_briefing_by_date,
            commands::briefing::list_briefings,
            commands::stories::get_stories_by_sector,
            commands::stories::get_story_detail,
            commands::search::full_text_search,
            commands::fetch::trigger_manual_fetch,
            commands::fetch::get_fetch_status,
            commands::fetch::ping,
            commands::chat::ask_pulse,
            commands::chat::chat_send,
            commands::chat::chat_send_stream,
            commands::chat::chat_list_threads,
            commands::chat::chat_get_thread,
            commands::chat::chat_delete_thread,
            commands::chat::get_chat_context,
            commands::freedoms::get_today_freedoms,
            commands::freedoms::get_freedoms_by_date,
            commands::trends::get_trends,
            commands::ideas::generate_ideas,
            commands::ideas::get_ideas,
            commands::ideas::update_idea_status,
            commands::usage::get_api_usage,
            commands::usage::get_tavily_quota,
        ])
        .build(tauri::generate_context!())
        .expect("error building Pulse")
        .run(|app, event| {
            // Handle macOS dock click — focus existing window instead of creating new one
            if let tauri::RunEvent::Reopen { has_visible_windows, .. } = event {
                if !has_visible_windows {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        });
}
