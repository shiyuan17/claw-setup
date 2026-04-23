pub mod commands;
pub mod config;
pub mod daemon;
pub mod logging;
pub mod oauth;
pub mod provider;
pub mod proxy;
pub mod runtime;
pub mod system;

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::detect_installation,
            commands::resolve_conflict,
            commands::setup_get_launch_at_login,
            commands::verify_key,
            commands::save_config,
            commands::complete_setup,
            commands::gateway_status,
            commands::restart_gateway,
            commands::test_openclaw_chat,
            commands::kimi_oauth_login,
            commands::kimi_oauth_cancel,
            commands::kimi_oauth_logout,
            commands::kimi_oauth_status,
            commands::open_external,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run claw-setup");
}
