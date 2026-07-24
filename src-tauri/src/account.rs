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

/// The stored cloud token, for the code that actually has to connect with it
/// (the SDK reads `KUMIHO_AUTH_TOKEN`, not our keychain entry).
pub fn cloud_token() -> Option<String> {
    entry().ok().and_then(|e| e.get_password().ok())
}

#[derive(Serialize)]
pub struct AccountStatus {
    /// A cloud token is present in the credential store.
    pub has_token: bool,
    /// The Claude/Codex launcher is pointed at the local CE server.
    pub ce_mode: bool,
    /// Why the credential store could not be read, when it failed outright.
    pub error: Option<String>,
}

#[tauri::command]
pub fn account_status() -> AccountStatus {
    let (has_token, error) = match entry() {
        Err(e) => (false, Some(e)),
        Ok(en) => match en.get_password() {
            Ok(_) => (true, None),
            Err(keyring::Error::NoEntry) => (false, None),
            Err(e) => (false, Some(e.to_string())),
        },
    };
    AccountStatus {
        has_token,
        ce_mode: std::env::var("KUMIHO_CLAUDE_MODE")
            .map(|v| v == "ce")
            .unwrap_or(false),
        error,
    }
}

/// The token itself — used by the app to test the cloud connection.
#[tauri::command]
pub fn account_token() -> Option<String> {
    cloud_token()
}

#[tauri::command]
pub fn account_set_token(token: String) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("token is empty".into());
    }
    let en = entry()?;
    en.set_password(token)
        .map_err(|e| format!("could not write to the OS credential store: {e}"))?;
    // Read it straight back: a silent write failure is worse than a loud one.
    match en.get_password() {
        Ok(v) if v == token => Ok(()),
        Ok(_) => Err("the credential store returned a different value after saving".into()),
        Err(e) => Err(format!("saved, but reading it back failed: {e}")),
    }
}

#[tauri::command]
pub fn account_clear_token() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
