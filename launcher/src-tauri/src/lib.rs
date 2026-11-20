pub mod commands;
pub mod config;
pub mod download;
pub mod error;
pub mod java;
pub mod launch;
pub mod mojang;
pub mod neoforge;
pub mod pack;
pub mod pipeline;
pub mod report;
pub mod settings;
pub mod status;
pub mod uuid;
pub mod version;

use std::sync::{atomic::AtomicBool, Arc, Mutex};
use tauri::Manager;

pub fn run() {
    let cfg = config::AppConfig::load();
    let client = download::build_client().expect("cliente HTTP");
    let state = commands::AppState {
        settings: Mutex::new(settings::Settings::load(&cfg)),
        cfg: cfg.clone(),
        client,
        busy: Arc::new(AtomicBool::new(false)),
        kill: Arc::new(Mutex::new(None)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .setup(move |app| {
            // Nome do launcher vem do config embutido.
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(&cfg.name);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_init,
            commands::save_settings,
            commands::refresh_info,
            commands::server_status,
            commands::play,
            commands::stop_game,
            commands::repair,
            commands::clear_cache,
            commands::open_game_dir,
            commands::open_url,
            commands::check_update,
            commands::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o launcher");
}
