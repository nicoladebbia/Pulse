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
                .expect("failed to get app data dir");
            std::fs::create_dir_all(&app_data).ok();

            let db_path = app_data.join("pulse.db");
            eprintln!("Pulse DB path: {}", db_path.display());

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
