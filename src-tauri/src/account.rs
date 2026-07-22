//! Account pillar — Kumiho Cloud token, stored in the OS credential store
//! (Keychain / Windows Credential Manager / libsecret) via `keyring`.
//! Never a plaintext file — the desktop app is distributed, so secrets live
//! in the platform vault.

use serde::Serialize;

const SERVICE: &str = "io.kumiho.desktop";
const ACCOUNT: &str = "cloud-token";

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct AccountStatus {
    /// A cloud token is present in the keychain.
    pub has_token: bool,
    /// The Claude/Codex launcher is pointed at the local CE server.
    pub ce_mode: bool,
}

#[tauri::command]
pub fn account_status() -> AccountStatus {
    let has_token = entry().ok().and_then(|e| e.get_password().ok()).is_some();
    let ce_mode = std::env::var("KUMIHO_CLAUDE_MODE")
        .map(|v| v == "ce")
        .unwrap_or(false);
    AccountStatus { has_token, ce_mode }
}

#[tauri::command]
pub fn account_set_token(token: String) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("token is empty".into());
    }
    entry()?.set_password(token).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn account_clear_token() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
