//! The desktop app's own state (~/.kumiho/desktop.json) — which drives the
//! first-run setup modal (shown until `configured` is true) and remembers the
//! chosen mode + CE ports.

use crate::util::kumiho_home;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct DesktopConfig {
    pub configured: bool,
    /// "ce" | "cloud" | ""
    pub mode: String,
    pub server_port: u16,
    pub neo4j_port: u16,
    pub redis_port: u16,
    pub use_redis: bool,
    pub local_user: String,
    /// Bring the CE server + its databases up automatically when the app starts
    /// (CE mode only). `serde(default)` keeps older desktop.json files — and the
    /// setup wizard's explicit 7-field writes — deserializing cleanly.
    #[serde(default)]
    pub autostart_infra: bool,
    /// Vector search / embedding — optional, opt-in.
    #[serde(default)]
    pub embedding_enabled: bool,
    #[serde(default = "default_embedding_provider")]
    pub embedding_provider: String,
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    #[serde(default = "default_embedding_dimensions")]
    pub embedding_dimensions: u32,
    #[serde(default)]
    pub embedding_endpoint: String,
    /// None = auto (true for OpenAI default endpoint, false otherwise).
    #[serde(default)]
    pub embedding_send_dimensions: Option<bool>,
    #[serde(default = "default_embedding_batch_size")]
    pub embedding_batch_size: u32,
}

fn default_embedding_provider() -> String {
    "openai".to_string()
}

fn default_embedding_model() -> String {
    "text-embedding-3-small".to_string()
}

fn default_embedding_dimensions() -> u32 {
    1536
}

fn default_embedding_batch_size() -> u32 {
    20
}

impl Default for DesktopConfig {
    fn default() -> Self {
        DesktopConfig {
            configured: false,
            mode: String::new(),
            server_port: 9190,
            neo4j_port: 7687,
            redis_port: 6379,
            use_redis: true,
            local_user: String::new(),
            autostart_infra: false,
            embedding_enabled: false,
            embedding_provider: default_embedding_provider(),
            embedding_model: default_embedding_model(),
            embedding_dimensions: default_embedding_dimensions(),
            embedding_endpoint: String::new(),
            embedding_send_dimensions: None,
            embedding_batch_size: default_embedding_batch_size(),
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    kumiho_home().map(|h| h.join("desktop.json"))
}

/// This app's version — shown in the window title / header.
#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
pub fn desktop_config_get() -> DesktopConfig {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn desktop_config_set(cfg: DesktopConfig) -> Result<(), String> {
    let p = config_path().ok_or("no home directory")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(&p, json).map_err(|e| e.to_string())
}
