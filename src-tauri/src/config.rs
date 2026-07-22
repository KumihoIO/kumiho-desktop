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
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    kumiho_home().map(|h| h.join("desktop.json"))
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
