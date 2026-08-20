//! Run pillar — install, configure, start/stop, and monitor the local CE server
//! (`kumiho_server`, HTTP+gRPC on 127.0.0.1:9190), plus the Brain view (8090).
//!
//! Grounded in the real CE surface: config is a `~/.kumiho/server.toml` we write
//! from the setup modal (keys per the onboard wizard) so we never need the
//! interactive `onboard`; health is the server's own `/api/_live` + `/api/_health`.
//! Note: CE hard-caps concurrent connections at 4 (compiled in — CE_MAX_CONNECTIONS,
//! not tunable); when memory calls stall while the port still listens it is
//! connection-starved, and a restart clears it.

use crate::util::{ce_binary, command, kumiho_home};
use crate::AppState;
use serde::Serialize;
use std::io::{Read, Seek, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::Stdio;
use std::time::Duration;
use tauri::State;

const CE_PORT: u16 = 9190;
const BRAIN_PORT: u16 = 8090;

fn port_open(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

/// Minimal loopback HTTP GET → parse a JSON body. No TLS (loopback only), no deps.
fn http_get_json(port: u16, path: &str) -> Option<serde_json::Value> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(2000))).ok()?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

#[derive(Serialize)]
pub struct CeStatus {
    pub reachable: bool,
    pub port: u16,
    pub version: Option<String>,
    pub mode: Option<String>,
    pub installed: bool,
    pub configured: bool,
    /// The compiled-in CE concurrent-connection cap (fixed; shown for context).
    pub max_connections: u32,
}

#[tauri::command]
pub fn ce_status() -> CeStatus {
    let live = http_get_json(CE_PORT, "/api/_live");
    let configured = kumiho_home()
        .map(|h| h.join("server.toml").exists())
        .unwrap_or(false);
    CeStatus {
        reachable: live.is_some() || port_open(CE_PORT),
        port: CE_PORT,
        version: live
            .as_ref()
            .and_then(|v| v.get("version"))
            .and_then(|v| v.as_str())
            .map(String::from),
        mode: live
            .as_ref()
            .and_then(|v| v.get("deployment_mode"))
            .and_then(|v| v.as_str())
            .map(String::from),
        installed: ce_binary().is_some(),
        configured,
        max_connections: 4,
    }
}

/// Dependency/readiness detail from the server's own `/api/_health`.
#[tauri::command]
pub fn ce_health() -> Option<serde_json::Value> {
    http_get_json(CE_PORT, "/api/_health")
}

/// Run the Community Edition installer (one-liner from the community releases).
/// Best-effort: the vendor installer hands off to interactive `onboard`, which we
/// don't need — so we ignore its tail and confirm success by the binary landing.
#[tauri::command]
pub fn ce_install() -> Result<String, String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = command("powershell");
        c.args([
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
            "irm https://github.com/KumihoIO/kumiho-server-community/releases/latest/download/install.ps1 | iex",
        ]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = command("sh");
        c.args([
            "-c",
            "curl -fsSL https://github.com/KumihoIO/kumiho-server-community/releases/latest/download/install.sh | sh",
        ]);
        c
    };
    let _ = cmd.stdin(Stdio::null()).output();
    if ce_binary().is_some() {
        Ok("Community Edition installed".into())
    } else {
        Err("installer did not produce ~/.kumiho/bin/kumiho_server".into())
    }
}

/// Write `~/.kumiho/server.toml` from the setup modal (bypasses interactive
/// onboard). Keys match the onboard wizard's output.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn ce_configure(
    server_port: u16,
    neo4j_port: u16,
    neo4j_password: String,
    redis_port: Option<u16>,
    local_user: String,
    local_email: String,
    eula_accepted: bool,
) -> Result<String, String> {
    if !eula_accepted {
        return Err("the EULA must be accepted to run Community Edition".into());
    }
    if neo4j_password.trim().is_empty() {
        return Err("a Neo4j password is required".into());
    }
    let home = kumiho_home().ok_or("no home directory")?;
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut toml = String::new();
    toml.push_str("deployment_mode = \"self_hosted_ce\"\n");
    toml.push_str("eula_accepted = true\n");
    toml.push_str("eula_version = \"1.1\"\n");
    toml.push_str(&format!("server_addr = \"127.0.0.1:{server_port}\"\n"));
    toml.push_str(&format!("neo4j_port = {neo4j_port}\n"));
    toml.push_str("db_name = \"neo4j\"\n");
    toml.push_str("db_user = \"neo4j\"\n");
    toml.push_str(&format!("db_pass = \"{}\"\n", esc(neo4j_password.trim())));
    toml.push_str(&format!("local_user = \"{}\"\n", esc(local_user.trim())));
    toml.push_str(&format!("local_email = \"{}\"\n", esc(local_email.trim())));
    if let Some(rp) = redis_port {
        toml.push_str(&format!("redis_port = {rp}\n"));
    }
    let path = home.join("server.toml");
    std::fs::write(&path, toml).map_err(|e| e.to_string())?;
    Ok(format!("wrote {}", path.display()))
}

/// `~/.kumiho/logs/kumiho_server.log` — fresh per start, written by `ce_start`.
fn ce_log_path() -> Option<std::path::PathBuf> {
    Some(kumiho_home()?.join("logs").join("kumiho_server.log"))
}

/// The last non-empty lines of the server log, reading at most the final 16KB
/// so a long-lived log stays cheap to tail.
fn ce_log_tail_text(max_lines: usize) -> String {
    let Some(path) = ce_log_path() else {
        return String::new();
    };
    let Ok(mut file) = std::fs::File::open(&path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(std::io::SeekFrom::Start(len.saturating_sub(16 * 1024)));
    let mut buf = String::new();
    let _ = file.read_to_string(&mut buf);
    let lines: Vec<&str> = buf.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

/// The tail of the CE server log, for the UI to show when a start goes wrong.
#[tauri::command]
pub fn ce_log_tail() -> String {
    ce_log_tail_text(40)
}

/// Start the CE server with our written config (`KUMIHO_CONFIG`). Databases must
/// be up first (see the docker pillar).
#[tauri::command]
pub fn ce_start() -> Result<String, String> {
    let bin = ce_binary().ok_or("kumiho_server is not installed yet")?;
    let home = kumiho_home().ok_or("no home directory")?;
    let cfg = home.join("server.toml");
    if !cfg.exists() {
        return Err("not configured yet — finish setup first".into());
    }
    // Log to a file, not null: a server that dies on a bad config or a wrong
    // Neo4j password must leave its reason somewhere the UI can surface — a
    // silent death here is indistinguishable from "never came up".
    std::fs::create_dir_all(home.join("logs")).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(ce_log_path().ok_or("no home directory")?)
        .map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = command(bin.to_str().ok_or("bad path")?)
        .env("KUMIHO_CONFIG", &cfg)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|e| e.to_string())?;
    // Catch an immediate death so the caller gets the actual reason instead of
    // a generic health-wait timeout.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(250));
        if let Ok(Some(status)) = child.try_wait() {
            let tail = ce_log_tail_text(15);
            return Err(if tail.is_empty() {
                format!("kumiho_server exited immediately ({status})")
            } else {
                format!("kumiho_server exited immediately ({status}):\n{tail}")
            });
        }
    }
    Ok("kumiho_server starting on 9190".into())
}

#[tauri::command]
pub fn ce_stop() -> Result<String, String> {
    #[cfg(windows)]
    {
        let out = command("taskkill")
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
        command("pkill")
            .args(["-f", "kumiho_server"])
            .output()
            .map_err(|e| e.to_string())?;
        Ok("kumiho_server stopped".into())
    }
}

// --- Brain (See pillar) ------------------------------------------------------

fn brain_binary() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "kumiho-brain.exe" } else { "kumiho-brain" };
    // 1) bundled sidecar sitting next to the app executable (installed builds).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent().map(|d| d.join(name)) {
            if p.exists() {
                return Some(p);
            }
        }
    }
    // 2) a manually-installed copy (dev / `cargo tauri dev`).
    let p = kumiho_home()?.join("bin").join(name);
    p.exists().then_some(p)
}

#[derive(Serialize)]
pub struct PortStatus {
    pub reachable: bool,
    pub port: u16,
}

#[tauri::command]
pub fn brain_status() -> PortStatus {
    PortStatus { reachable: port_open(BRAIN_PORT), port: BRAIN_PORT }
}

/// (mode, server_port) from ~/.kumiho/desktop.json (defaults: "", 9190).
fn desktop_mode_port() -> (String, u16) {
    let v: Option<serde_json::Value> = kumiho_home()
        .and_then(|h| std::fs::read_to_string(h.join("desktop.json")).ok())
        .and_then(|s| serde_json::from_str(&s).ok());
    let mode = v
        .as_ref()
        .and_then(|v| v.get("mode"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let port = v
        .as_ref()
        .and_then(|v| v.get("server_port"))
        .and_then(|p| p.as_u64())
        .unwrap_or(9190) as u16;
    (mode, port)
}

#[tauri::command]
pub fn brain_start(state: State<AppState>) -> Result<String, String> {
    if port_open(BRAIN_PORT) {
        return Ok("brain already serving on 8090".into());
    }
    let bin = brain_binary()
        .ok_or("kumiho-brain not found in ~/.kumiho/bin — install or build it first")?;
    let mut cmd = command(bin.to_str().ok_or("bad path")?);
    cmd.args(["--port", &BRAIN_PORT.to_string()]);

    // In CE mode, force Brain at the LOCAL server. Otherwise the SDK bootstrap
    // follows the ambient KUMIHO_AUTH_TOKEN to the cloud tenant and renders cloud
    // memory instead of the local CE graph.
    let (mode, server_port) = desktop_mode_port();
    if mode == "ce" {
        cmd.arg("--local");
        cmd.env("KUMIHO_CLAUDE_MODE", "ce");
        cmd.env("KUMIHO_LOCAL_SERVER_ENDPOINT", format!("127.0.0.1:{server_port}"));
        cmd.env_remove("KUMIHO_AUTH_TOKEN");
    } else if let Some(token) = crate::account::cloud_token() {
        // Cloud mode: hand the saved token to the SDK bootstrap. It reads
        // KUMIHO_AUTH_TOKEN — it has no idea our copy lives in the OS keychain,
        // which is why cloud connections silently did nothing before.
        cmd.env("KUMIHO_AUTH_TOKEN", token);
        cmd.env_remove("KUMIHO_CLAUDE_MODE");
    }

    let child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    *state.brain.lock().map_err(|e| e.to_string())? = Some(child);
    Ok("brain starting on 8090".into())
}

/// Kill any Brain sidecar — tracked or orphaned — by process name. Used on a
/// restart (to free 8090), before an update (so kumiho-brain(.exe) isn't locked
/// while the installer swaps files), and on app exit.
pub fn kill_brain() {
    #[cfg(windows)]
    {
        let _ = command("taskkill").args(["/IM", "kumiho-brain.exe", "/F"]).output();
    }
    #[cfg(not(windows))]
    {
        let _ = command("pkill").args(["-f", "kumiho-brain"]).output();
    }
}

#[tauri::command]
pub fn brain_stop(state: State<AppState>) -> Result<String, String> {
    if let Some(mut child) = state.brain.lock().map_err(|e| e.to_string())?.take() {
        let _ = child.kill();
    }
    // Also clear any orphaned Brain (e.g. one left by a previous app session, or
    // an old one connected to the cloud) so a restart truly frees 8090 and
    // relaunches in the current mode.
    kill_brain();
    Ok("brain stopped".into())
}
