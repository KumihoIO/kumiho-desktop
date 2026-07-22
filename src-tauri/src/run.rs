//! Run pillar — lifecycle for the CE server (`kumiho_server`, gRPC @9190) and
//! the Brain dashboard (`kumiho-brain`, HTTP @8090, the See pillar).
//!
//! The recurring failure mode is concurrency-starvation: the CE server keeps
//! listening on 9190 but resets connections. `ce_status` surfaces reachability
//! so the UI can offer a one-click restart (`ce_stop` + `ce_start`).

use crate::AppState;
use serde::Serialize;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tauri::State;

const CE_PORT: u16 = 9190;
const BRAIN_PORT: u16 = 8090;

/// TCP-connect probe on loopback (a wedged CE still accepts TCP, but this at
/// least distinguishes "process gone" from "listening").
fn port_open(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

fn kumiho_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".kumiho"))
}

#[derive(Serialize)]
pub struct PortStatus {
    pub reachable: bool,
    pub port: u16,
}

#[tauri::command]
pub fn ce_status() -> PortStatus {
    PortStatus { reachable: port_open(CE_PORT), port: CE_PORT }
}

/// Relaunch the CE server via the on-disk launcher `~/.kumiho/start-kumiho-server.ps1`
/// (the Defender-safe, log-redirecting path established during the wedge fixes).
#[tauri::command]
pub fn ce_start() -> Result<String, String> {
    let script = kumiho_home()
        .map(|h| h.join("start-kumiho-server.ps1"))
        .ok_or("no home directory")?;
    if !script.exists() {
        return Err(format!("launcher not found: {}", script.display()));
    }
    #[cfg(windows)]
    {
        Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok("kumiho_server launch requested".into())
    }
    #[cfg(not(windows))]
    {
        let _ = &script;
        Err("CE start currently wires the Windows launcher only".into())
    }
}

#[tauri::command]
pub fn ce_stop() -> Result<String, String> {
    #[cfg(windows)]
    {
        let out = Command::new("taskkill")
            .args(["/IM", "kumiho_server.exe", "/F"])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok("kumiho_server stopped".into())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    #[cfg(not(windows))]
    {
        Command::new("pkill")
            .arg("-f")
            .arg("kumiho_server")
            .status()
            .map_err(|e| e.to_string())?;
        Ok("stop requested".into())
    }
}

// --- Brain (See pillar) ------------------------------------------------------

fn brain_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "kumiho-brain.exe" } else { "kumiho-brain" };
    let p = kumiho_home()?.join("bin").join(name);
    p.exists().then_some(p)
}

#[tauri::command]
pub fn brain_status() -> PortStatus {
    PortStatus { reachable: port_open(BRAIN_PORT), port: BRAIN_PORT }
}

#[tauri::command]
pub fn brain_start(state: State<AppState>) -> Result<String, String> {
    if port_open(BRAIN_PORT) {
        return Ok("brain already serving on 8090".into());
    }
    let bin = brain_binary()
        .ok_or("kumiho-brain not found in ~/.kumiho/bin — install or build it first")?;
    let child = Command::new(bin)
        .args(["--port", &BRAIN_PORT.to_string()])
        .spawn()
        .map_err(|e| e.to_string())?;
    *state.brain.lock().map_err(|e| e.to_string())? = Some(child);
    Ok("brain starting on 8090".into())
}

#[tauri::command]
pub fn brain_stop(state: State<AppState>) -> Result<String, String> {
    if let Some(mut child) = state.brain.lock().map_err(|e| e.to_string())?.take() {
        let _ = child.kill();
    }
    Ok("brain stopped".into())
}
