// Kumiho Desktop — control center for Kumiho Memory.
//
// v1 architecture: the Brain graph view (See pillar) is the existing kumiho-brain
// axum server, spawned as a child process and shown in an <iframe>; the other
// four pillars (Connect / Run / Account / Upgrade) are native Tauri commands.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod connect;
mod run;
mod upgrade;

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
            // Run — CE server + Brain lifecycle
            run::ce_status,
            run::ce_start,
            run::ce_stop,
            run::brain_status,
            run::brain_start,
            run::brain_stop,
            // Account — cloud token in the OS keychain
            account::account_status,
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
