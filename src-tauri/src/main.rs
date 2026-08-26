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
mod memory;
mod miho;
mod run;
mod startup;
mod update;
mod upgrade;
mod util;
mod window;

use std::sync::Mutex;

/// App-wide state: the Brain dashboard child we started (so we can stop it).
#[derive(Default)]
pub struct AppState {
    pub brain: Mutex<Option<std::process::Child>>,
    pub miho: Mutex<Option<std::process::Child>>,
    /// Held across 9miho's check-then-spawn so two UI paths cannot both decide
    /// the port is free and each launch a runtime.
    pub miho_start: Mutex<()>,
}

fn main() {
    tauri::Builder::default()
        // Must be the FIRST plugin. A second launch (notably the updater
        // relaunching the app on Windows) hands the window back to the running
        // instance instead of spawning a duplicate that would race the Brain
        // on 8090.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window::show_main(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
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
            run::ce_configure_commit,
            run::ce_configure_pending,
            run::ce_configure_rollback,
            run::ce_start,
            run::ce_stop,
            run::ce_log_tail,
            run::brain_status,
            run::brain_start,
            run::brain_stop,
            // Kumiho Memory engine — host-owned Python venvs + PyPI updates
            memory::memory_status,
            memory::memory_check_update,
            memory::memory_update,
            // 9miho — bundled install + explicit CE/Cloud launch
            miho::miho_status,
            miho::miho_check_update,
            miho::miho_install,
            miho::miho_update,
            miho::miho_start,
            miho::miho_stop,
            // Docker — Neo4j + Redis dependencies
            docker::docker_status,
            docker::docker_up,
            docker::docker_down,
            // Account — cloud token in the OS keychain
            account::account_status,
            account::account_token,
            account::account_set_token,
            account::account_clear_token,
            account::cloud_probe,
            // Connect — per-host plugin install
            connect::connect_hosts,
            connect::connect_check_updates,
            connect::connect_install,
            connect::connect_update,
            // Upgrade — CE vs Cloud
            upgrade::upgrade_status,
            // Update — in-app auto-update
            update::check_update,
            update::install_update,
            // Startup — launch at login
            startup::autostart_get,
            startup::autostart_set,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Kumiho Desktop")
        .run(|app, event| match event {
            // The Brain runs as a detached child that nothing else reaps. Left
            // alive past the app it keeps kumiho-brain(.exe) locked, so the next
            // install/update can't replace it (the reinstall conflict). Kill it
            // as we exit.
            tauri::RunEvent::ExitRequested { .. } => {
                run::kill_brain();
                miho::kill_tracked_miho(app);
            }
            // Clicking the Dock icon is how macOS asks for the window back once
            // the last one was closed. Without this the app stays running with
            // nothing on screen and no way in.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => window::show_main(app),
            _ => {}
        });
}
