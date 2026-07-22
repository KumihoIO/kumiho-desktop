//! Connect pillar — install the Kumiho Memory plugin into each host agent
//! (Claude Code, ChatGPT/Codex, OpenClaw). We detect which host CLIs are on
//! PATH and run each host's own install flow.
//!
//! NOTE: the exact per-host install invocations are first-cut and should be
//! reconciled with each host's current CLI (marketplace vs. direct install).

use crate::util::command;
use serde::Serialize;

#[derive(Serialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub installed: bool,
}

/// Is `cmd` resolvable on PATH? (`where` on Windows, `which` elsewhere.)
fn on_path(cmd: &str) -> bool {
    let probe = if cfg!(windows) { "where" } else { "which" };
    command(probe)
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tauri::command]
pub fn connect_hosts() -> Vec<Host> {
    vec![
        Host { id: "claude".into(), name: "Claude Code".into(), installed: on_path("claude") },
        Host { id: "codex".into(), name: "ChatGPT / Codex".into(), installed: on_path("codex") },
        Host { id: "openclaw".into(), name: "OpenClaw".into(), installed: on_path("openclaw") },
    ]
}

#[tauri::command]
pub fn connect_install(host: String) -> Result<String, String> {
    let (cmd, args): (&str, &[&str]) = match host.as_str() {
        "claude" => ("claude", &["plugin", "install", "kumiho-memory@kumiho-plugins"]),
        "codex" => ("codex", &["plugin", "marketplace", "add", "KumihoIO/kumiho-plugins"]),
        "openclaw" => ("openclaw", &["plugins", "install", "kumiho-memory"]),
        other => return Err(format!("unknown host: {other}")),
    };
    let out = command(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch `{cmd}`: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}
