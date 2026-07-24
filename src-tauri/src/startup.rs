//! "Launch at login" — per-OS autostart via tauri-plugin-autostart (Windows Run
//! key / macOS LaunchAgent / Linux .desktop). Driven from Rust so the frontend
//! just flips a checkbox. Pairs with the `autostart_infra` desktop-config flag
//! (CE server + DBs on launch) to make the app a terminal-free way to run the
//! whole Kumiho stack from sign-in.

use tauri_plugin_autostart::ManagerExt;

#[tauri::command]
pub fn autostart_get(app: tauri::AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn autostart_set(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let al = app.autolaunch();
    if enabled {
        al.enable().map_err(|e| e.to_string())
    } else {
        al.disable().map_err(|e| e.to_string())
    }
}
