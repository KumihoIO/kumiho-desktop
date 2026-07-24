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
    // Verify through a FRESH handle, not `en`. Reading back on the same handle
    // can succeed even when nothing persisted — an in-memory fallback isolates
    // state per handle, which is exactly how keyring's mock backend hid a
    // missing platform vault in 0.1.2 (write "succeeded", `account_status` and
    // `cloud_token` on new handles then saw nothing). Distinguish NoEntry (truly
    // did not persist) from a transient read error so we never mislabel one.
    match entry()?.get_password() {
        Ok(v) if v == token => Ok(()),
        Ok(_) => Err("the credential store returned a different value after saving".into()),
        Err(keyring::Error::NoEntry) => {
            Err("the token did not persist to the OS credential store".into())
        }
        Err(e) => Err(format!("saved, but reading it back to confirm failed: {e}")),
    }
}

/// Validate a cloud service token against the control plane and resolve its
/// tenant. Done natively rather than from the webview: control.kumiho.cloud's
/// CORS preflight omits `Access-Control-Allow-Origin`, so a browser `fetch` from
/// the app origin is blocked and would falsely read as "unreachable". The Brain's
/// real connection uses `KUMIHO_AUTH_TOKEN` (also native), so this mirrors it.
#[derive(Serialize)]
pub struct CloudProbe {
    /// The control plane accepted the token (2xx).
    pub ok: bool,
    /// HTTP status (0 = the request never completed).
    pub status: u16,
    pub tenant: Option<String>,
    pub error: Option<String>,
}

const DISCOVERY_URL: &str = "https://control.kumiho.cloud/api/discovery/tenant";

#[tauri::command]
pub fn cloud_probe(token: String) -> CloudProbe {
    let token = token.trim();
    if token.is_empty() {
        return CloudProbe { ok: false, status: 0, tenant: None, error: Some("no token".into()) };
    }
    let resp = ureq::post(DISCOVERY_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(8))
        .send_string("{}");
    match resp {
        Ok(r) => {
            let status = r.status();
            // Parse with our own serde_json (ureq's into_json is behind a feature
            // we don't need to pull just for this).
            let tenant = r
                .into_string()
                .ok()
                .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
                .and_then(|j| {
                    j.get("tenant_name")
                        .or_else(|| j.get("tenant_id"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                });
            CloudProbe { ok: (200..300).contains(&status), status, tenant, error: None }
        }
        // ureq surfaces non-2xx as Error::Status — that's a real answer (401/403),
        // not a transport failure, so report the code with ok=false.
        Err(ureq::Error::Status(code, _)) => {
            CloudProbe { ok: false, status: code, tenant: None, error: None }
        }
        Err(e) => CloudProbe { ok: false, status: 0, tenant: None, error: Some(e.to_string()) },
    }
}

#[tauri::command]
pub fn account_clear_token() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
