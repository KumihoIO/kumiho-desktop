// Kumiho Desktop — control center for Kumiho Memory.
//
// First run shows a setup modal (CE or Cloud). CE: install the server, run
// Neo4j + Redis in Docker, configure ports, start/stop. Cloud: token to connect.
// See: the Brain graph view (kumiho-brain spawned as a child, shown in an iframe).
// Cross-platform (Windows / macOS / Linux) via std process + conditional bits.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod config;
mod connect;
mod docker;
mod run;
mod upgrade;
mod util;

use std::sync::Mutex;

/// App-wide state: the Brain dashboard child we started (so we can stop it).
#[derive(Default)]
pub struct AppState {
    pub brain: Mutex<Option<std::process::Child>>,
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            // First-run + app config
            config::app_version,
            config::desktop_config_get,
            config::desktop_config_set,
            // Run — CE server lifecycle + Brain
            run::ce_status,
            run::ce_health,
            run::ce_install,
            run::ce_configure,
            run::ce_start,
            run::ce_stop,
            run::brain_status,
            run::brain_start,
            run::brain_stop,
            // Docker — Neo4j + Redis dependencies
            docker::docker_status,
            docker::docker_up,
            docker::docker_down,
            // Account — cloud token in the OS keychain
            account::account_status,
            account::account_token,
            account::account_set_token,
            account::account_clear_token,
            // Connect — per-host plugin install
            connect::connect_hosts,
            connect::connect_install,
            // Upgrade — CE vs Cloud
            upgrade::upgrade_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kumiho Desktop");
}
