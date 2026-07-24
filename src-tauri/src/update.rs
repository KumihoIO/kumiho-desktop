//! In-app auto-update via the Tauri updater. Signed artifacts + a `latest.json`
//! manifest are published on each GitHub release; the app checks that manifest,
//! and downloads/installs a newer signed build in place. Driven from Rust so the
//! frontend only invokes two commands.
//!
//! Before swapping the app's files we kill the Brain sidecar: a live
//! kumiho-brain(.exe) locks its own file, which is what made reinstalls fail.

use tauri_plugin_updater::UpdaterExt;

#[derive(serde::Serialize)]
pub struct UpdateInfo {
    pub available: bool,
    pub version: Option<String>,
    pub current: String,
    pub notes: Option<String>,
}

/// Is there a newer signed release than the running build?
#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> Result<UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(u)) => Ok(UpdateInfo {
            available: true,
            version: Some(u.version.clone()),
            current,
            notes: u.body.clone(),
        }),
        Ok(None) => Ok(UpdateInfo { available: false, version: None, current, notes: None }),
        Err(e) => Err(e.to_string()),
    }
}

/// Download + install the newest release, then relaunch. Frees the Brain sidecar
/// first so its files aren't locked while the installer swaps them.
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("already up to date")?;
    // Kill the Brain in the on-download-finish hook — i.e. AFTER the (possibly
    // long) download succeeds and just BEFORE the installer swaps files. Killing
    // it up front would leave the main view blank if the download then failed.
    update
        .download_and_install(|_, _| {}, || crate::run::kill_brain())
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}
