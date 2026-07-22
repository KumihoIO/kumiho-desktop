// Kumiho Desktop — Tauri control-center wrapping the Brain axum server.
//
// v1 strategy (see MIGRATION notes): keep axum, wrap in a native webview.
// The Brain graph view (orb + WS event stream) is served UNCHANGED by the
// embedded axum server on loopback; the four pillars below are new native
// Tauri commands layered alongside. Nothing about the See path is rewritten.
//
// WIP: the axum server (this repo's root `kumiho-brain` crate) must be
// refactored into a lib exposing `serve(addr)` before the setup hook can call
// it. Until then this shell compiles standalone with stubbed commands.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

const BRAIN_ADDR: &str = "127.0.0.1:8090";

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|_app| {
            // See pillar: bring up the Brain axum server before the window
            // navigates to it (tauri.conf.json window.url = BRAIN_ADDR).
            // TODO: gate window.show() on /api/healthz to avoid a first-load blank.
            tauri::async_runtime::spawn(async {
                // TODO: brain::serve(BRAIN_ADDR).await  (root crate → lib)
                let _ = BRAIN_ADDR;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ce_status, ce_start, ce_stop,          // Run
            account_get_token, account_set_token,  // Account
            connect_install_plugin,                // Connect
            upgrade_status,                        // Upgrade
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kumiho Desktop");
}

// --- Run: CE server lifecycle -------------------------------------------------
// Owns ~/.kumiho/bin/kumiho_server: download, start/stop, health @127.0.0.1:9190.
// MUST include wedge detection + one-click restart / raise-concurrency
// (recurring concurrency-starvation incidents — see MIGRATION §7).
#[tauri::command]
async fn ce_status() -> Result<serde_json::Value, String> {
    // TODO: probe 9190 (TCP + /api/_live), report edition/version + slot pressure.
    Ok(serde_json::json!({ "reachable": false }))
}
#[tauri::command]
async fn ce_start() -> Result<(), String> {
    // TODO: spawn kumiho_server detached via tauri-plugin-shell sidecar,
    //       redirect stdout/stderr to ~/.kumiho/kumiho_server.out/.err.log.
    Ok(())
}
#[tauri::command]
async fn ce_stop() -> Result<(), String> {
    // TODO: terminate only the kumiho_server child we spawned.
    Ok(())
}

// --- Account: cloud token in the OS keychain ---------------------------------
// NO plaintext ~/.kumiho token file — use keyring (Keychain / Credential Manager / libsecret).
#[tauri::command]
fn account_get_token() -> Result<Option<String>, String> {
    // TODO: keyring::Entry::new("kumiho", "cloud-token")?.get_password()
    Ok(None)
}
#[tauri::command]
fn account_set_token(_token: String) -> Result<(), String> {
    // TODO: keyring set_password
    Ok(())
}

// --- Connect: per-host plugin install ----------------------------------------
// Claude / Codex / OpenClaw: run the host CLI (shell) + write MCP config (fs).
#[tauri::command]
async fn connect_install_plugin(_host: String) -> Result<(), String> {
    // TODO: host-specific install flow.
    Ok(())
}

// --- Upgrade: CE <-> Cloud ----------------------------------------------------
#[tauri::command]
fn upgrade_status() -> Result<serde_json::Value, String> {
    // TODO: compare local CE vs cloud tenant, surface upgrade CTA + deeplink.
    Ok(serde_json::json!({ "edition": "ce" }))
}
