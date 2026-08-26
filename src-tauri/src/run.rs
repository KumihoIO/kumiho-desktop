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
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tauri::{Manager, State};

const CE_PORT: u16 = 9190;
const BRAIN_PORT: u16 = 8090;
const CE_CONFIG_FILE: &str = "server.toml";
const CE_CONFIG_BACKUP_FILE: &str = "server.toml.setup-backup";
const CE_CONFIG_NEW_MARKER: &str = "server.toml.setup-new";
const CE_CONFIG_CANDIDATE_FILE: &str = "server.toml.setup-candidate";
const CE_PROCESS_MARKER_FILE: &str = "ce-process.json";

#[derive(Deserialize, Serialize)]
struct CeProcessMarker {
    pid: u32,
    identity: String,
}

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

pub(crate) fn escape_toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn restrict_private_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        if path.exists() {
            restrict_private_file(path)?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;

    file.write_all(contents).map_err(|e| e.to_string())?;
    restrict_private_file(path)
}

fn copy_private_file(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // If the destination already exists, tighten it before truncating it.
        // A newly-created destination receives 0600 atomically from open(2), so
        // password-bearing backups are never briefly world-readable.
        if to.exists() {
            restrict_private_file(to)?;
        }
        let mut source = std::fs::File::open(from).map_err(|e| e.to_string())?;
        let mut destination = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(to)
            .map_err(|e| e.to_string())?;
        restrict_private_file(to)?;
        std::io::copy(&mut source, &mut destination).map_err(|e| e.to_string())?;
        destination.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(from, to).map_err(|e| e.to_string())?;
        restrict_private_file(to)
    }
}

fn ce_process_marker_path(home: &Path) -> std::path::PathBuf {
    home.join(CE_PROCESS_MARKER_FILE)
}

fn process_identity(pid: u32) -> Result<Option<String>, String> {
    #[cfg(windows)]
    let output = {
        let script = format!(
            "$p=Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
             if ($null -ne $p) {{ [Console]::Out.Write(\
             $p.StartTime.ToUniversalTime().Ticks.ToString() + '|' + $p.ProcessName) }}"
        );
        command("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .map_err(|e| e.to_string())?
    };
    #[cfg(unix)]
    let output = command("ps")
        .args([
            "-p",
            &pid.to_string(),
            "-o",
            "lstart=",
            "-o",
            "comm=",
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return match output.status.code() {
            Some(1) => Ok(None),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        };
    }
    let identity = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!identity.is_empty()).then_some(identity))
}

fn write_ce_process_marker(home: &Path, pid: u32) -> Result<(), String> {
    let identity = process_identity(pid)?.ok_or("spawned CE process disappeared")?;
    if !identity.to_ascii_lowercase().contains("kumiho_server") {
        return Err("spawned CE process identity did not match kumiho_server".into());
    }
    let marker = serde_json::to_vec(&CeProcessMarker { pid, identity })
        .map_err(|e| e.to_string())?;
    let path = ce_process_marker_path(home);
    if let Err(error) = write_private_file(&path, &marker) {
        let _ = remove_if_present(&path);
        return Err(error);
    }
    Ok(())
}

fn read_ce_process_marker(home: &Path) -> Result<Option<CeProcessMarker>, String> {
    let path = ce_process_marker_path(home);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("invalid CE process marker: {e}"))
}

fn remove_ce_process_marker(home: &Path) -> Result<(), String> {
    remove_if_present(&ce_process_marker_path(home))
}

fn recorded_ce_running(home: &Path) -> Result<bool, String> {
    let Some(marker) = read_ce_process_marker(home)? else {
        return Ok(false);
    };
    match process_identity(marker.pid)? {
        Some(identity) if identity == marker.identity => Ok(true),
        _ => {
            remove_ce_process_marker(home)?;
            Ok(false)
        }
    }
}

fn stop_recorded_ce(home: &Path) -> Result<bool, String> {
    let Some(marker) = read_ce_process_marker(home)? else {
        return Ok(false);
    };
    match process_identity(marker.pid)? {
        Some(identity) if identity == marker.identity => {}
        _ => {
            remove_ce_process_marker(home)?;
            return Ok(false);
        }
    }

    #[cfg(windows)]
    {
        let output = command("taskkill")
            .args(["/PID", &marker.pid.to_string(), "/F"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success()
            && process_identity(marker.pid)?.as_deref() == Some(marker.identity.as_str())
        {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(marker.pid as i32, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.to_string());
            }
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match process_identity(marker.pid)? {
            Some(identity) if identity == marker.identity => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                remove_ce_process_marker(home)?;
                return Ok(true);
            }
        }
    }
    Err("recorded kumiho_server did not exit within 5 seconds".into())
}

fn rollback_setup_config_at(home: &Path) -> Result<bool, String> {
    let config = home.join(CE_CONFIG_FILE);
    let backup = home.join(CE_CONFIG_BACKUP_FILE);
    let marker = home.join(CE_CONFIG_NEW_MARKER);
    let candidate = home.join(CE_CONFIG_CANDIDATE_FILE);
    let restored = if backup.exists() {
        copy_private_file(&backup, &config)?;
        true
    } else if marker.exists() {
        remove_if_present(&config)?;
        true
    } else {
        false
    };
    remove_if_present(&backup)?;
    remove_if_present(&marker)?;
    remove_if_present(&candidate)?;
    Ok(restored)
}

fn stage_setup_config_at(home: &Path, contents: &str) -> Result<(), String> {
    rollback_setup_config_at(home)?;
    let config = home.join(CE_CONFIG_FILE);
    let backup = home.join(CE_CONFIG_BACKUP_FILE);
    let marker = home.join(CE_CONFIG_NEW_MARKER);
    let candidate = home.join(CE_CONFIG_CANDIDATE_FILE);
    write_private_file(&candidate, contents.as_bytes())?;
    let preserve_result = if config.exists() {
        let result = copy_private_file(&config, &backup);
        if result.is_err() {
            let _ = remove_if_present(&backup);
        }
        result
    } else {
        std::fs::write(&marker, []).map_err(|e| e.to_string())
    };
    if let Err(error) = preserve_result {
        let _ = remove_if_present(&candidate);
        return Err(error);
    }
    if let Err(error) = copy_private_file(&candidate, &config) {
        let _ = rollback_setup_config_at(home);
        return Err(error);
    }
    remove_if_present(&candidate)
}

fn commit_setup_config_at(home: &Path) -> Result<bool, String> {
    let backup = home.join(CE_CONFIG_BACKUP_FILE);
    let marker = home.join(CE_CONFIG_NEW_MARKER);
    let candidate = home.join(CE_CONFIG_CANDIDATE_FILE);
    let pending = backup.exists() || marker.exists();
    remove_if_present(&candidate)?;
    if backup.exists() {
        remove_if_present(&marker)?;
        remove_if_present(&backup)?;
    } else {
        remove_if_present(&backup)?;
        remove_if_present(&marker)?;
    }
    Ok(pending)
}

fn setup_config_pending_at(home: &Path) -> bool {
    home.join(CE_CONFIG_BACKUP_FILE).exists() || home.join(CE_CONFIG_NEW_MARKER).exists()
}

/// Stage `~/.kumiho/server.toml` from the setup modal (bypasses interactive
/// onboard). The UI commits it only after CE connects; otherwise the previous
/// config is restored. Keys match the onboard wizard's output.
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
    let mut toml = String::new();
    toml.push_str("deployment_mode = \"self_hosted_ce\"\n");
    toml.push_str("eula_accepted = true\n");
    toml.push_str("eula_version = \"1.1\"\n");
    toml.push_str(&format!("server_addr = \"127.0.0.1:{server_port}\"\n"));
    toml.push_str(&format!("neo4j_port = {neo4j_port}\n"));
    toml.push_str("db_name = \"neo4j\"\n");
    toml.push_str("db_user = \"neo4j\"\n");
    toml.push_str(&format!(
        "db_pass = \"{}\"\n",
        escape_toml_basic_string(neo4j_password.trim())
    ));
    toml.push_str(&format!(
        "local_user = \"{}\"\n",
        escape_toml_basic_string(local_user.trim())
    ));
    toml.push_str(&format!(
        "local_email = \"{}\"\n",
        escape_toml_basic_string(local_email.trim())
    ));
    if let Some(rp) = redis_port {
        toml.push_str(&format!("redis_port = {rp}\n"));
    }
    let path = home.join(CE_CONFIG_FILE);
    stage_setup_config_at(&home, &toml)?;
    Ok(format!("wrote {}", path.display()))
}

#[tauri::command]
pub fn ce_configure_commit() -> Result<String, String> {
    let home = kumiho_home().ok_or("no home directory")?;
    let committed = commit_setup_config_at(&home)?;
    Ok(if committed {
        "Community Edition config committed".into()
    } else {
        "no pending Community Edition config".into()
    })
}

#[tauri::command]
pub fn ce_configure_rollback() -> Result<String, String> {
    let home = kumiho_home().ok_or("no home directory")?;
    let restored = rollback_setup_config_at(&home)?;
    Ok(if restored {
        "previous Community Edition config restored".into()
    } else {
        "no pending Community Edition config".into()
    })
}

#[tauri::command]
pub fn ce_configure_pending() -> Result<bool, String> {
    let home = kumiho_home().ok_or("no home directory")?;
    Ok(setup_config_pending_at(&home))
}

/// `~/.kumiho/logs/kumiho_server.log` — fresh per start, written by `ce_start`.
fn ce_log_path() -> Option<std::path::PathBuf> {
    Some(kumiho_home()?.join("logs").join("kumiho_server.log"))
}

/// The last `max_lines` non-empty lines of the file, reading at most the final
/// 16KB. Decoded lossily: server output is not guaranteed UTF-8 (Windows
/// codepages, a cut that splits a codepoint), and a strict decode here would
/// silently drop the whole tail. NULs are stripped — a truncated-under-a-live-
/// writer log refills the gap with them.
fn log_tail_text(path: &std::path::Path, max_lines: usize) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(std::io::SeekFrom::Start(len.saturating_sub(16 * 1024)));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).replace('\0', "");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

/// The tail of the CE server log, for the UI to show when a start goes wrong.
#[tauri::command]
pub fn ce_log_tail() -> String {
    ce_log_path()
        .map(|p| log_tail_text(&p, 40))
        .unwrap_or_default()
}

/// Start the CE server with our written config (`KUMIHO_CONFIG`). Databases must
/// be up first (see the docker pillar). Async: the startup watch below blocks
/// for up to ten seconds, which must not park the main thread.
#[tauri::command]
pub async fn ce_start(state: State<'_, AppState>) -> Result<String, String> {
    let _start_guard = state.ce_start.lock().map_err(|e| e.to_string())?;
    // Mirror brain_start's guard: a second spawn would lose the bind race AND
    // truncate the log the running server is still writing.
    if port_open(CE_PORT) {
        return Ok(format!("kumiho_server already serving on {CE_PORT}"));
    }
    {
        let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
        if let Some(child) = tracked.as_mut() {
            match child.try_wait().map_err(|e| e.to_string())? {
                None => return Ok(format!("kumiho_server already starting on {CE_PORT}")),
                Some(_) => {
                    tracked.take();
                }
            }
        }
    }
    let bin = ce_binary().ok_or("kumiho_server is not installed yet")?;
    let home = kumiho_home().ok_or("no home directory")?;
    let cfg = home.join("server.toml");
    if !cfg.exists() {
        return Err("not configured yet — finish setup first".into());
    }
    if recorded_ce_running(&home)? {
        return Ok(format!("recorded kumiho_server already starting on {CE_PORT}"));
    }
    // Log to a file, not null: a server that dies on a bad config or a wrong
    // Neo4j password must leave its reason somewhere the UI can surface. Best
    // effort — an unwritable log file must not block the start itself.
    let log_files = std::fs::create_dir_all(home.join("logs"))
        .ok()
        .and_then(|_| std::fs::File::create(home.join("logs").join("kumiho_server.log")).ok())
        .and_then(|out| out.try_clone().ok().map(|err| (out, err)));
    let mut cmd = command(bin.to_str().ok_or("bad path")?);
    cmd.env("KUMIHO_CONFIG", &cfg);
    match log_files {
        Some((out, err)) => cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err)),
        None => cmd.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    *tracked = Some(child);
    drop(tracked);
    if let Err(error) = write_ce_process_marker(&home, pid) {
        if let Some(mut child) = state.ce.lock().map_err(|e| e.to_string())?.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        return Err(error);
    }
    // Watch until the server serves or dies, so config/auth failures surface as
    // the actual error instead of a generic health-wait timeout. Ten seconds
    // because a wrong Neo4j password only kills the server after driver
    // retries (seconds, not milliseconds); a healthy server exits this loop as
    // soon as it binds.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if port_open(CE_PORT) {
            return Ok(format!("kumiho_server serving on {CE_PORT}"));
        }
        let status = {
            let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
            let Some(child) = tracked.as_mut() else {
                remove_ce_process_marker(&home)?;
                return Err("kumiho_server was stopped during startup".into());
            };
            let status = child.try_wait().map_err(|e| e.to_string())?;
            if status.is_some() {
                tracked.take();
            }
            status
        };
        if let Some(status) = status {
            remove_ce_process_marker(&home)?;
            let tail = ce_log_tail();
            let mut msg = format!("kumiho_server exited during startup ({status})");
            if !tail.is_empty() {
                msg.push_str(&format!(":\n{tail}"));
            }
            return Err(msg);
        }
    }
    Ok(format!("kumiho_server starting on {CE_PORT}"))
}

#[tauri::command]
pub fn ce_stop(state: State<'_, AppState>, force: Option<bool>) -> Result<String, String> {
    let home = kumiho_home().ok_or("no home directory")?;
    if let Some(mut child) = state.ce.lock().map_err(|e| e.to_string())?.take() {
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            child.kill().map_err(|e| e.to_string())?;
            child.wait().map_err(|e| e.to_string())?;
        }
        remove_ce_process_marker(&home)?;
        return Ok("tracked kumiho_server stopped".into());
    }
    if stop_recorded_ce(&home)? {
        return Ok("recorded kumiho_server stopped".into());
    }
    if force.unwrap_or(false) || !port_open(CE_PORT) {
        Ok("kumiho_server is not running".into())
    } else {
        Err("the server on port 9190 was not started by this Desktop version; stop that specific process manually before retrying".into())
    }
}

/// Avoid leaving an unbound startup child behind when Desktop exits normally.
/// A healthy committed CE server is intentionally allowed to keep serving.
pub fn kill_pending_ce(app: &tauri::AppHandle) {
    let pending = kumiho_home()
        .map(|home| setup_config_pending_at(&home))
        .unwrap_or(false);
    if !pending && port_open(CE_PORT) {
        return;
    }
    let state = app.state::<AppState>();
    if let Ok(mut tracked) = state.ce.lock() {
        if let Some(mut child) = tracked.take() {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(home) = kumiho_home() {
                let _ = remove_ce_process_marker(&home);
            }
            return;
        }
    };
    if let Some(home) = kumiho_home() {
        let _ = stop_recorded_ce(&home);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn tail_survives_a_multibyte_char_split_by_the_16kb_cut() {
        // 2 + 3×6000 + 12 bytes: the 16KB cut lands mid-em-dash. A strict
        // decode of the tail fails and loses everything; lossy must not.
        let mut bytes = b"aa".to_vec();
        bytes.extend_from_slice("—".repeat(6000).as_bytes());
        bytes.extend_from_slice(b"\nfinal line\n");
        let path = write_temp("kumiho_tail_utf8.log", &bytes);
        let tail = log_tail_text(&path, 5);
        std::fs::remove_file(&path).ok();
        assert!(tail.ends_with("final line"), "{tail:?}");
    }

    #[test]
    fn tail_of_a_missing_file_is_empty_and_nuls_are_stripped() {
        let missing = std::env::temp_dir().join("kumiho_tail_no_such.log");
        assert_eq!(log_tail_text(&missing, 5), "");
        let path = write_temp("kumiho_tail_nul.log", b"\0\0\0\nreal error line\n\0\0");
        let tail = log_tail_text(&path, 5);
        std::fs::remove_file(&path).ok();
        assert_eq!(tail, "real error line");
    }

    #[test]
    fn tail_keeps_only_the_last_non_empty_lines() {
        let path = write_temp("kumiho_tail_window.log", b"one\ntwo\n\nthree\n");
        let tail = log_tail_text(&path, 2);
        std::fs::remove_file(&path).ok();
        assert_eq!(tail, "two\nthree");
    }

    fn test_home(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "kumiho-desktop-run-test-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn setup_config_restores_the_previous_file_until_committed() {
        let home = test_home("existing-config");
        let config = home.join(CE_CONFIG_FILE);
        std::fs::write(&config, "db_pass = \"original\"\n").unwrap();

        stage_setup_config_at(&home, "db_pass = \"candidate\"\n").unwrap();
        assert!(setup_config_pending_at(&home));
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "db_pass = \"candidate\"\n"
        );
        assert!(rollback_setup_config_at(&home).unwrap());
        assert!(!setup_config_pending_at(&home));
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "db_pass = \"original\"\n"
        );

        stage_setup_config_at(&home, "db_pass = \"accepted\"\n").unwrap();
        assert!(setup_config_pending_at(&home));
        assert!(commit_setup_config_at(&home).unwrap());
        assert!(!setup_config_pending_at(&home));
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "db_pass = \"accepted\"\n"
        );
        assert!(!home.join(CE_CONFIG_BACKUP_FILE).exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn failed_first_setup_removes_the_uncommitted_config() {
        let home = test_home("new-config");
        let config = home.join(CE_CONFIG_FILE);

        stage_setup_config_at(&home, "db_pass = \"candidate\"\n").unwrap();
        assert!(config.exists());
        assert!(rollback_setup_config_at(&home).unwrap());
        assert!(!config.exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn process_marker_requires_the_exact_pid_identity() {
        let home = test_home("process-marker");
        let pid = std::process::id();
        let identity = process_identity(pid).unwrap().unwrap();
        let marker_path = ce_process_marker_path(&home);
        let marker = serde_json::to_vec(&CeProcessMarker {
            pid,
            identity: identity.clone(),
        })
        .unwrap();
        write_private_file(&marker_path, &marker).unwrap();
        assert!(recorded_ce_running(&home).unwrap());

        let stale = serde_json::to_vec(&CeProcessMarker {
            pid,
            identity: format!("{identity}-different-process"),
        })
        .unwrap();
        write_private_file(&marker_path, &stale).unwrap();
        assert!(!recorded_ce_running(&home).unwrap());
        assert!(!marker_path.exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn setup_config_and_backup_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let home = test_home("private-config");
        let config = home.join(CE_CONFIG_FILE);
        let backup = home.join(CE_CONFIG_BACKUP_FILE);
        std::fs::write(&config, "db_pass = \"original\"\n").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644))
            .unwrap();

        stage_setup_config_at(&home, "db_pass = \"candidate\"\n").unwrap();
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );

        rollback_setup_config_at(&home).unwrap();
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
