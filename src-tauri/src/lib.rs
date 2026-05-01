mod commands;
pub mod db;
mod models;
pub mod services;

use tauri::Manager;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub struct ChatAbortFlag(pub Arc<AtomicBool>);

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

            // Startup maintenance: migrate prediction dates and expire stale predictions
            if let Ok(migrated) = services::predictions::migrate_prediction_dates(&conn) {
                if migrated > 0 {
                    eprintln!("Migrated {} prediction dates to ISO format", migrated);
                }
            }
            if let Ok(expired) = services::predictions::expire_stale_predictions(&conn) {
                if expired > 0 {
                    eprintln!("Expired {} stale predictions", expired);
                }
            }

            app.manage(db::DbState(std::sync::Mutex::new(conn)));
            app.manage(ChatAbortFlag(Arc::new(AtomicBool::new(false))));
            app.manage(Arc::new(services::live_prices::LivePriceState::new()));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::briefing::get_today_briefing,
            commands::briefing::get_briefing_by_date,
            commands::briefing::get_briefing_by_id,
            commands::briefing::list_briefings,
            commands::stories::get_stories_by_sector,
            commands::stories::get_story_detail,
            commands::stories::get_story_headlines,
            commands::search::full_text_search,
            commands::fetch::trigger_manual_fetch,
            commands::fetch::get_fetch_status,
            commands::chat::chat_send,
            commands::chat::chat_send_stream,
            commands::chat::chat_list_threads,
            commands::chat::chat_get_thread,
            commands::chat::chat_delete_thread,
            commands::chat::get_chat_context,
            commands::chat::submit_chat_feedback,
            commands::chat::get_feedback_stats,
            commands::chat::chat_cancel_stream,
            commands::freedoms::get_today_freedoms,
            commands::freedoms::get_freedoms_by_date,
            commands::trends::get_trends,
            commands::trends::get_story_trend_badges,
            commands::trends::get_intelligence_counts,
            commands::trends::get_story_entities,
            commands::ideas::generate_ideas,
            commands::ideas::get_ideas,
            commands::ideas::update_idea_status,
            commands::predictions::get_all_predictions,
            commands::predictions::get_prediction_stats,
            commands::predictions::manually_validate_prediction,
            commands::predictions::expire_stale_predictions,
            commands::predictions::get_calibration_stats,
            commands::usage::get_api_usage,
            commands::usage::get_tavily_quota,
            commands::usage::get_financial_quotas,
            commands::cross_signals::get_cross_signals,
            commands::cross_signals::get_convergence_alerts,
            commands::cross_signals::get_entity_prices,
            commands::cross_signals::refresh_prices,
            commands::cross_signals::get_financial_events,
            commands::cross_signals::get_signal_evidence,
            commands::cross_signals::get_source_health,
            commands::trading::get_portfolio,
            commands::trading::get_paper_trades,
            commands::trading::execute_trade,
            commands::trading::get_portfolio_analytics,
            commands::trading::get_trade_journal,
            commands::trading::run_backtest,
            commands::trading::get_backtest_history,
            commands::trading::auto_backtest_if_due,
            commands::trading::get_auto_trade_status,
            commands::trading::start_price_stream,
            commands::trading::stop_price_stream,
            commands::trading::get_stream_status,
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
