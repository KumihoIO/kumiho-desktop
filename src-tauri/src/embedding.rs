//! Embedding pillar — CE vector-search credentials and backfill.
//!
//! The embedding *API key* lives in the OS credential store (same vault as the
//! cloud token) so it is never written to `server.toml` or `desktop.json`.
//! Non-secret fields (provider, model, endpoint, dimensions) live in
//! `DesktopConfig` (`~/.kumiho/desktop.json`) and are injected as env vars
//! when Desktop spawns `kumiho_server`.

use serde::Serialize;

const SERVICE: &str = "io.kumiho.desktop";
const ACCOUNT: &str = "ce-embedding-api-key";

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
}

/// The stored embedding API key, if any.
pub fn embedding_api_key() -> Option<String> {
    entry().ok().and_then(|e| e.get_password().ok())
}

#[derive(Serialize)]
pub struct EmbeddingKeyStatus {
    /// An embedding API key is present in the credential store.
    pub has_key: bool,
    /// Why the credential store could not be read, when it failed outright.
    pub error: Option<String>,
}

#[tauri::command]
pub fn embedding_key_status() -> EmbeddingKeyStatus {
    let (has_key, error) = match entry() {
        Err(e) => (false, Some(e)),
        Ok(en) => match en.get_password() {
            Ok(_) => (true, None),
            Err(keyring::Error::NoEntry) => (false, None),
            Err(e) => (false, Some(e.to_string())),
        },
    };
    EmbeddingKeyStatus { has_key, error }
}

#[tauri::command]
pub fn embedding_key_get() -> Option<String> {
    embedding_api_key()
}

#[tauri::command]
pub fn embedding_key_set(api_key: String) -> Result<(), String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("embedding API key is empty".into());
    }
    let en = entry()?;
    en.set_password(api_key)
        .map_err(|e| format!("could not write to the OS credential store: {e}"))?;
    // Verify through a FRESH handle — same pattern as account.rs.
    match entry()?.get_password() {
        Ok(v) if v == api_key => Ok(()),
        Ok(_) => Err("the credential store returned a different value after saving".into()),
        Err(keyring::Error::NoEntry) => {
            Err("the embedding key did not persist to the OS credential store".into())
        }
        Err(e) => Err(format!("saved, but reading it back to confirm failed: {e}")),
    }
}

#[tauri::command]
pub fn embedding_key_clear() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Validation helpers (shared between Rust and JS via Tauri)
// ---------------------------------------------------------------------------

/// Placeholder presets shown in the UI so users can pick quickly.
#[derive(Serialize, Clone)]
pub struct EmbeddingPreset {
    pub label: String,
    pub provider: String,
    pub model: String,
    pub dimensions: u32,
    pub endpoint: String,
    pub send_dimensions: Option<bool>,
}

#[tauri::command]
pub fn embedding_presets() -> Vec<EmbeddingPreset> {
    vec![
        EmbeddingPreset {
            label: "OpenAI — text-embedding-3-small (1536)".into(),
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            dimensions: 1536,
            endpoint: "".into(),
            send_dimensions: Some(true),
        },
        EmbeddingPreset {
            label: "OpenAI — text-embedding-3-large (3072)".into(),
            provider: "openai".into(),
            model: "text-embedding-3-large".into(),
            dimensions: 3072,
            endpoint: "".into(),
            send_dimensions: Some(true),
        },
        EmbeddingPreset {
            label: "Cloudflare — bge-m3 (1024)".into(),
            provider: "openai".into(),
            model: "bge-m3".into(),
            dimensions: 1024,
            endpoint: "https://api.cloudflare.com/client/v4/accounts/<account_id>/ai/run/@cf/baai/bge-m3".into(),
            send_dimensions: Some(false),
        },
        EmbeddingPreset {
            label: "Self-hosted — BGE-M3 via TEI/vLLM (1024)".into(),
            provider: "openai".into(),
            model: "BAAI/bge-m3".into(),
            dimensions: 1024,
            endpoint: "http://localhost:8080/v1/embeddings".into(),
            send_dimensions: Some(false),
        },
        EmbeddingPreset {
            label: "Custom OpenAI-compatible".into(),
            provider: "openai".into(),
            model: "".into(),
            dimensions: 1536,
            endpoint: "".into(),
            send_dimensions: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Backfill / status — talks to the CE server's admin endpoints
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct EmbeddingStatus {
    pub enabled: bool,
    pub embedding_enabled: bool,
    pub has_key: bool,
    pub provider: String,
    pub model: String,
    pub dimensions: u32,
    pub endpoint: String,
    pub total_revisions: Option<u64>,
    pub with_embedding: Option<u64>,
    pub without_embedding: Option<u64>,
    pub error: Option<String>,
}

fn ce_base_url() -> String {
    // desktop.json server_port is authoritative; fall back to 9190.
    let port: u16 = crate::util::kumiho_home()
        .and_then(|h| std::fs::read_to_string(h.join("desktop.json")).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("server_port").and_then(|p| p.as_u64()).map(|p| p as u16))
        .unwrap_or(9190);
    format!("http://127.0.0.1:{port}")
}

#[tauri::command]
pub fn embedding_status() -> EmbeddingStatus {
    let cfg = crate::config::desktop_config_get();
    let has_key = embedding_api_key().is_some();
    let enabled = cfg.embedding_enabled && has_key;

    // Try to get live counts from the server if reachable.
    let mut total = None;
    let mut with_emb = None;
    let mut without_emb = None;
    let mut error = None;

    if enabled {
        let url = format!("{}/api/_admin/embedding-status", ce_base_url());
        match ureq::get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .call()
        {
            Ok(resp) => {
                if let Ok(body) = resp.into_string() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        total = v.get("total_revisions").and_then(|x| x.as_u64());
                        with_emb = v.get("with_embedding").and_then(|x| x.as_u64());
                        without_emb = v.get("without_embedding").and_then(|x| x.as_u64());
                    }
                }
            }
            Err(ureq::Error::Status(code, resp)) => {
                // 404 means the server doesn't have the endpoint yet — not an error
                // for the UI, just indicates backfill isn't available on this build.
                if code != 404 {
                    error = Some(format!("server returned HTTP {code}: {}", resp.into_string().unwrap_or_default()));
                }
            }
            Err(e) => {
                error = Some(e.to_string());
            }
        }
    }

    EmbeddingStatus {
        enabled,
        embedding_enabled: cfg.embedding_enabled,
        has_key,
        provider: cfg.embedding_provider.clone(),
        model: cfg.embedding_model.clone(),
        dimensions: cfg.embedding_dimensions,
        endpoint: cfg.embedding_endpoint.clone(),
        total_revisions: total,
        with_embedding: with_emb,
        without_embedding: without_emb,
        error,
    }
}

#[derive(Serialize)]
pub struct BackfillResult {
    pub total: u64,
    pub embedded: u64,
    pub errors: u64,
    pub message: String,
}

#[tauri::command]
pub fn embedding_backfill(batch_size: Option<u32>) -> Result<BackfillResult, String> {
    let cfg = crate::config::desktop_config_get();
    if !cfg.embedding_enabled {
        return Err("vector search is not enabled — enable it in Settings first".into());
    }
    if embedding_api_key().is_none() {
        return Err("no embedding API key is stored — add one in Settings first".into());
    }

    let url = format!("{}/api/_admin/backfill-embeddings", ce_base_url());
    let batch = batch_size.unwrap_or(cfg.embedding_batch_size).clamp(1, 100);

    let body = serde_json::json!({ "batch_size": batch }).to_string();
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(300))
        .send_string(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let detail = resp.into_string().unwrap_or_default();
                if code == 404 {
                    "this kumiho_server build does not support backfill yet — update the CE server to the latest release".to_string()
                } else {
                    format!("backfill failed (HTTP {code}): {detail}")
                }
            }
            other => format!("backfill request failed: {other}"),
        })?;

    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    Ok(BackfillResult {
        total: v.get("total").and_then(|x| x.as_u64()).unwrap_or(0),
        embedded: v.get("embedded").and_then(|x| x.as_u64()).unwrap_or(0),
        errors: v.get("errors").and_then(|x| x.as_u64()).unwrap_or(0),
        message: v.get("message").and_then(|x| x.as_str()).unwrap_or("backfill complete").to_string(),
    })
}

#[tauri::command]
pub fn embedding_rebuild_index() -> Result<String, String> {
    let url = format!("{}/api/_admin/rebuild-vector-index", ce_base_url());
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(60))
        .send_string("{}")
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let detail = resp.into_string().unwrap_or_default();
                if code == 404 {
                    "this kumiho_server build does not support index rebuild yet — update the CE server".to_string()
                } else {
                    format!("rebuild failed (HTTP {code}): {detail}")
                }
            }
            other => format!("rebuild request failed: {other}"),
        })?;

    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
    Ok(v.get("message").and_then(|x| x.as_str()).unwrap_or("vector index rebuilt").to_string())
}
