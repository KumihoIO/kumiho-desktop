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
const CE_CONFIG_BACKUP_TEMP_FILE: &str = "server.toml.setup-backup.tmp";
const CE_CONFIG_NEW_MARKER: &str = "server.toml.setup-new";
const CE_CONFIG_CANDIDATE_FILE: &str = "server.toml.setup-candidate";
const CE_PROCESS_MARKER_FILE: &str = "ce-process.json";
const CE_PROCESS_INTENT_FILE: &str = "ce-process-starting";
const NEO4J_MIN_PASSWORD_LENGTH: usize = 8;

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
    /// Desktop has a durable process/launch record, even if 9190 is not bound yet.
    pub managed: bool,
    /// This Desktop session still owns the native child handle and can stop it safely.
    pub stoppable: bool,
    pub port: u16,
    pub version: Option<String>,
    pub mode: Option<String>,
    pub installed: bool,
    pub configured: bool,
    /// The compiled-in CE concurrent-connection cap (fixed; shown for context).
    pub max_connections: u32,
}

#[tauri::command]
pub fn ce_status(state: State<'_, AppState>) -> CeStatus {
    let live = http_get_json(CE_PORT, "/api/_live");
    let home = kumiho_home();
    let configured = home
        .as_ref()
        .map(|home| home.join(CE_CONFIG_FILE).exists())
        .unwrap_or(false);
    let managed = home
        .as_ref()
        .map(|home| {
            home.join(CE_PROCESS_MARKER_FILE).exists()
                || home.join(CE_PROCESS_INTENT_FILE).exists()
        })
        .unwrap_or(false);
    let stoppable = state
        .ce
        .lock()
        .map(|tracked| tracked.is_some())
        .unwrap_or(false);
    CeStatus {
        reachable: live.is_some() || port_open(CE_PORT),
        managed,
        stoppable,
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

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
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
    file.sync_all().map_err(|e| e.to_string())?;
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
        restrict_private_file(to)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(to)
            .and_then(|destination| destination.sync_all())
            .map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

fn move_file_durable(source: &Path, destination: &Path, replace: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let source_wide: Vec<u16> = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let destination_wide: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let flags = MOVEFILE_WRITE_THROUGH
            | if replace {
                MOVEFILE_REPLACE_EXISTING
            } else {
                0
            };
        // SAFETY: both paths are owned, NUL-terminated buffers that remain alive
        // for the duration of the synchronous Win32 call.
        let moved = unsafe { MoveFileExW(source_wide.as_ptr(), destination_wide.as_ptr(), flags) };
        if moved == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = replace;
        std::fs::rename(source, destination).map_err(|e| e.to_string())
    }
}

fn retired_path(path: &Path) -> std::path::PathBuf {
    let mut retired = path.as_os_str().to_os_string();
    retired.push(".retired");
    retired.into()
}

/// Remove a recovery-critical name by first moving it durably out of the
/// transaction namespace. If a later delete is lost in a power failure, only
/// the ignored `.retired` tombstone can reappear, never a live rollback record.
fn retire_file_durable(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let retired = retired_path(path);
    remove_if_present(&retired)?;
    move_file_durable(path, &retired, true)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    remove_if_present(&retired)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn write_private_file_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    remove_if_present(&temporary)?;
    if let Err(error) = write_private_file(&temporary, contents) {
        let _ = remove_if_present(&temporary);
        return Err(error);
    }
    // The lifecycle lock and single-instance app guarantee one writer. Refuse
    // to replace an existing recovery record: rename is then atomic on every
    // supported platform, including Windows.
    if path.exists() {
        let _ = remove_if_present(&temporary);
        return Err(format!("recovery record already exists: {}", path.display()));
    }
    if let Err(error) = move_file_durable(&temporary, path, false) {
        let _ = remove_if_present(&temporary);
        return Err(error.to_string());
    }
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn publish_private_file(source: &Path, destination: &Path) -> Result<(), String> {
    restrict_private_file(source)?;
    move_file_durable(source, destination, true)?;
    restrict_private_file(destination)?;
    if let Some(parent) = destination.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn ce_process_marker_path(home: &Path) -> std::path::PathBuf {
    home.join(CE_PROCESS_MARKER_FILE)
}

fn ce_process_intent_path(home: &Path) -> std::path::PathBuf {
    home.join(CE_PROCESS_INTENT_FILE)
}

fn remove_ce_process_records(home: &Path) -> Result<(), String> {
    remove_if_present(&ce_process_marker_path(home))?;
    remove_if_present(&ce_process_intent_path(home))?;
    sync_directory(home)
}

fn any_ce_process_running() -> Result<bool, String> {
    #[cfg(windows)]
    {
        let output = command("tasklist")
            .args(["/FI", "IMAGENAME eq kumiho_server.exe", "/FO", "CSV", "/NH"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        return Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.split(',')
                .next()
                .map(|name| name.trim().trim_matches('"').eq_ignore_ascii_case("kumiho_server.exe"))
                .unwrap_or(false)
        }));
    }
    #[cfg(unix)]
    {
        let output = command("/usr/bin/pgrep")
            .args(["-x", "kumiho_server"])
            .output()
            .map_err(|e| e.to_string())?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    }
}

fn uncertain_ce_launch(home: &Path) -> Result<bool, String> {
    let intent = ce_process_intent_path(home);
    if !intent.exists() {
        return Ok(false);
    }
    if active_ce_process_marker(home)?.is_some() {
        return Ok(false);
    }
    if any_ce_process_running()? {
        Ok(true)
    } else {
        remove_if_present(&intent)?;
        Ok(false)
    }
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
    let output = command("/bin/ps")
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
    if identity.is_empty() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !error.is_empty() {
            return Err(error);
        }
    }
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
    write_private_file_atomic(&path, &marker)
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
    remove_if_present(&ce_process_marker_path(home))?;
    sync_directory(home)
}

fn ce_process_identity_matches(expected: &str, current: Option<&str>) -> bool {
    current == Some(expected) && expected.to_ascii_lowercase().contains("kumiho_server")
}

fn is_ce_process_identity(identity: &str) -> bool {
    identity.to_ascii_lowercase().contains("kumiho_server")
}

fn active_ce_process_marker_with<F, G>(
    home: &Path,
    process_identity_for_pid: F,
    any_ce_process_is_running: G,
) -> Result<Option<CeProcessMarker>, String>
where
    F: FnOnce(u32) -> Result<Option<String>, String>,
    G: FnOnce() -> Result<bool, String>,
{
    let Some(marker) = read_ce_process_marker(home)? else {
        return Ok(None);
    };
    match process_identity_for_pid(marker.pid)? {
        identity if ce_process_identity_matches(&marker.identity, identity.as_deref()) => {
            // A complete marker supersedes the pre-spawn intent record.
            remove_if_present(&ce_process_intent_path(home))?;
            Ok(Some(marker))
        }
        Some(identity) if is_ce_process_identity(&identity) => {
            return Err(format!(
                "kumiho_server PID {} is still present, but its start identity changed; recovery was preserved for manual inspection",
                marker.pid
            ));
        }
        _ => {
            if any_ce_process_is_running()? {
                return Err("the recorded CE PID is gone or changed, but another kumiho_server process exists; recovery was preserved for manual inspection".into());
            }
            remove_ce_process_marker(home)?;
            Ok(None)
        }
    }
}

fn active_ce_process_marker(home: &Path) -> Result<Option<CeProcessMarker>, String> {
    active_ce_process_marker_with(home, process_identity, any_ce_process_running)
}

fn recorded_ce_running(home: &Path) -> Result<bool, String> {
    Ok(active_ce_process_marker(home)?.is_some())
}

fn stop_child_and_confirm(child: &mut std::process::Child) -> Result<(), String> {
    if child.try_wait().map_err(|e| e.to_string())?.is_some() {
        return Ok(());
    }

    if let Err(kill_error) = child.kill() {
        return match child.try_wait().map_err(|e| e.to_string())? {
            Some(_) => Ok(()),
            None => Err(format!("could not stop kumiho_server: {kill_error}")),
        };
    }

    match child.wait() {
        Ok(_) => Ok(()),
        Err(wait_error) => match child.try_wait().map_err(|e| e.to_string())? {
            Some(_) => Ok(()),
            None => Err(format!(
                "kumiho_server termination could not be confirmed: {wait_error}"
            )),
        },
    }
}

fn stop_tracked_child(
    tracked: &mut Option<std::process::Child>,
) -> Result<bool, String> {
    let Some(child) = tracked.as_mut() else {
        return Ok(false);
    };
    stop_child_and_confirm(child)?;
    tracked.take();
    Ok(true)
}

fn rollback_setup_config_at(home: &Path) -> Result<bool, String> {
    let config = home.join(CE_CONFIG_FILE);
    let backup = home.join(CE_CONFIG_BACKUP_FILE);
    let backup_temp = home.join(CE_CONFIG_BACKUP_TEMP_FILE);
    let marker = home.join(CE_CONFIG_NEW_MARKER);
    let candidate = home.join(CE_CONFIG_CANDIDATE_FILE);
    let restored = if backup.exists() {
        copy_private_file(&backup, &candidate)?;
        publish_private_file(&candidate, &config)?;
        true
    } else if marker.exists() {
        retire_file_durable(&config)?;
        true
    } else {
        false
    };
    retire_file_durable(&backup)?;
    remove_if_present(&backup_temp)?;
    retire_file_durable(&marker)?;
    remove_if_present(&candidate)?;
    sync_directory(home)?;
    Ok(restored)
}

fn stage_setup_config_at(home: &Path, contents: &str) -> Result<(), String> {
    rollback_setup_config_at(home)?;
    let config = home.join(CE_CONFIG_FILE);
    let backup = home.join(CE_CONFIG_BACKUP_FILE);
    let backup_temp = home.join(CE_CONFIG_BACKUP_TEMP_FILE);
    let marker = home.join(CE_CONFIG_NEW_MARKER);
    let candidate = home.join(CE_CONFIG_CANDIDATE_FILE);
    write_private_file(&candidate, contents.as_bytes())?;
    let preserve_result = if config.exists() {
        let result = copy_private_file(&config, &backup_temp).and_then(|()| {
            move_file_durable(&backup_temp, &backup, false).and_then(|()| sync_directory(home))
        });
        if result.is_err() {
            let _ = remove_if_present(&backup_temp);
            let _ = remove_if_present(&backup);
        }
        result
    } else {
        write_private_file_atomic(&marker, b"new config")
    };
    if let Err(error) = preserve_result {
        let _ = remove_if_present(&candidate);
        return Err(error);
    }
    if let Err(error) = publish_private_file(&candidate, &config) {
        let _ = rollback_setup_config_at(home);
        return Err(error);
    }
    Ok(())
}

fn commit_setup_config_at(home: &Path) -> Result<bool, String> {
    let backup = home.join(CE_CONFIG_BACKUP_FILE);
    let backup_temp = home.join(CE_CONFIG_BACKUP_TEMP_FILE);
    let marker = home.join(CE_CONFIG_NEW_MARKER);
    let candidate = home.join(CE_CONFIG_CANDIDATE_FILE);
    let pending = backup.exists() || marker.exists();
    remove_if_present(&candidate)?;
    remove_if_present(&backup_temp)?;
    retire_file_durable(&backup)?;
    retire_file_durable(&marker)?;
    sync_directory(home)?;
    Ok(pending)
}

fn setup_config_pending_at(home: &Path) -> bool {
    home.join(CE_CONFIG_BACKUP_FILE).exists() || home.join(CE_CONFIG_NEW_MARKER).exists()
}

fn neo4j_password_error(password: &str) -> Option<String> {
    let length = password.trim().chars().count();
    if length == 0 {
        Some("a Neo4j password is required".into())
    } else if length < NEO4J_MIN_PASSWORD_LENGTH {
        Some(format!(
            "Neo4j password must be at least {NEO4J_MIN_PASSWORD_LENGTH} characters"
        ))
    } else {
        None
    }
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
    if let Some(error) = neo4j_password_error(&neo4j_password) {
        return Err(error);
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
    let start_guard = state.ce_start.lock().map_err(|e| e.to_string())?;
    // Mirror brain_start's guard: a second spawn would lose the bind race AND
    // truncate the log the running server is still writing.
    if port_open(CE_PORT) {
        return Ok(format!("kumiho_server already serving on {CE_PORT}"));
    }
    let home = kumiho_home().ok_or("no home directory")?;
    let tracked_process_exited = {
        let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
        if let Some(child) = tracked.as_mut() {
            match child.try_wait().map_err(|e| e.to_string())? {
                None => return Ok(format!("kumiho_server already starting on {CE_PORT}")),
                Some(_) => {
                    tracked.take();
                    true
                }
            }
        } else {
            false
        }
    };
    if tracked_process_exited {
        remove_ce_process_records(&home)?;
    }
    let bin = ce_binary().ok_or("kumiho_server is not installed yet")?;
    let cfg = home.join("server.toml");
    if !cfg.exists() {
        return Err("not configured yet — finish setup first".into());
    }
    if recorded_ce_running(&home)? {
        return Ok(format!("recorded kumiho_server already starting on {CE_PORT}"));
    }
    if uncertain_ce_launch(&home)? {
        return Err("an interrupted Community Edition launch may still be running. The pending config was preserved; wait for that kumiho_server process to exit or stop it explicitly, then retry".into());
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
    let intent_path = ce_process_intent_path(&home);
    write_private_file(&intent_path, b"starting")?;
    sync_directory(&home)?;
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let cleanup = remove_if_present(&intent_path);
            return Err(match cleanup {
                Ok(()) => error.to_string(),
                Err(cleanup_error) => format!(
                    "{error}; the failed-launch recovery record could not be removed: {cleanup_error}"
                ),
            });
        }
    };
    let pid = child.id();
    *tracked = Some(child);
    drop(tracked);
    if let Err(error) = write_ce_process_marker(&home, pid) {
        let cleanup = (|| -> Result<(), String> {
            let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
            stop_tracked_child(&mut tracked)?;
            drop(tracked);
            remove_ce_process_records(&home)
        })();
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => format!(
                "{error}; kumiho_server cleanup could not be confirmed, so the pending config was preserved: {cleanup_error}"
            ),
        });
    }
    // A complete PID + process-start identity marker now owns recovery. A
    // leftover intent is harmless and will be cleared when that marker is read.
    let _ = remove_if_present(&intent_path);
    drop(start_guard);
    // Watch until the server serves or dies, so config/auth failures surface as
    // the actual error instead of a generic health-wait timeout. Ten seconds
    // because a wrong Neo4j password only kills the server after driver
    // retries (seconds, not milliseconds); a healthy server exits this loop as
    // soon as it binds.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let status = {
            // Re-enter the lifecycle lock before mutating the Child/records so
            // a retry cannot publish a newer marker between those two steps.
            let _start_guard = state.ce_start.lock().map_err(|e| e.to_string())?;
            let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
            let Some(child) = tracked.as_mut() else {
                return Err("kumiho_server was stopped during startup".into());
            };
            if child.id() != pid {
                return Err("a newer kumiho_server start replaced this startup attempt".into());
            }
            let status = child.try_wait().map_err(|e| e.to_string())?;
            if status.is_some() {
                tracked.take();
                drop(tracked);
                remove_ce_process_records(&home)?;
            }
            status
        };
        if let Some(status) = status {
            let tail = ce_log_tail();
            let mut msg = format!("kumiho_server exited during startup ({status})");
            if !tail.is_empty() {
                msg.push_str(&format!(":\n{tail}"));
            }
            return Err(msg);
        }
        if port_open(CE_PORT) {
            return Ok(format!("kumiho_server serving on {CE_PORT}"));
        }
    }
    Ok(format!("kumiho_server starting on {CE_PORT}"))
}

#[tauri::command]
pub fn ce_stop(state: State<'_, AppState>, force: Option<bool>) -> Result<String, String> {
    let _start_guard = state.ce_start.lock().map_err(|e| e.to_string())?;
    let home = kumiho_home().ok_or("no home directory")?;
    {
        let mut tracked = state.ce.lock().map_err(|e| e.to_string())?;
        if stop_tracked_child(&mut tracked)? {
            drop(tracked);
            remove_ce_process_records(&home)?;
            return Ok("tracked kumiho_server stopped".into());
        }
    }

    if let Some(marker) = active_ce_process_marker(&home)? {
        let context = if force.unwrap_or(false) {
            "The pending config was preserved."
        } else {
            "Desktop will not signal a cross-session PID because it cannot do so race-free."
        };
        return Err(format!(
            "kumiho_server from a previous Desktop session is still running (PID {}). {context} Stop that PID in Activity Monitor (macOS), Task Manager (Windows), or System Monitor (Linux), then retry",
            marker.pid
        ));
    }

    if force.unwrap_or(false) {
        if uncertain_ce_launch(&home)? {
            return Err("an interrupted Community Edition launch may still be running. The pending config was preserved; stop that specific kumiho_server process, then retry".into());
        }
        if port_open(CE_PORT) {
            return Err("the server on port 9190 was not started by this Desktop session. The pending config was preserved; stop that specific process manually before retrying".into());
        }
        return Ok("kumiho_server is not running".into());
    }

    if uncertain_ce_launch(&home)? {
        return Err("an interrupted Community Edition launch may still be running; stop that specific kumiho_server process manually before retrying".into());
    }
    if !port_open(CE_PORT) {
        Ok("kumiho_server is not running".into())
    } else {
        Err("the server on port 9190 was not started by this Desktop version; stop that specific process manually before retrying".into())
    }
}

/// Avoid leaving an unbound startup child behind when Desktop exits normally.
/// A healthy committed CE server is intentionally allowed to keep serving.
pub fn kill_pending_ce(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let Ok(_start_guard) = state.ce_start.lock() else {
        return;
    };
    let pending = kumiho_home()
        .map(|home| setup_config_pending_at(&home))
        .unwrap_or(false);
    if !pending && port_open(CE_PORT) {
        return;
    }
    if let Ok(mut tracked) = state.ce.lock() {
        if stop_tracked_child(&mut tracked).unwrap_or(false) {
            if let Some(home) = kumiho_home() {
                let _ = remove_ce_process_records(&home);
            }
            return;
        }
    };
    // Never kill a cross-session PID during an app-exit callback. Without the
    // live Child handle, recovery stays conservative and preserves its records.
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
        assert!(!home.join(CE_CONFIG_BACKUP_TEMP_FILE).exists());
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
        assert!(!home.join(CE_CONFIG_BACKUP_TEMP_FILE).exists());
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
    fn incomplete_backup_temp_is_never_used_for_rollback() {
        let home = test_home("partial-backup-temp");
        let config = home.join(CE_CONFIG_FILE);
        let backup_temp = home.join(CE_CONFIG_BACKUP_TEMP_FILE);
        std::fs::write(&config, "db_pass = \"original\"\n").unwrap();
        std::fs::write(&backup_temp, "partial").unwrap();

        assert!(!rollback_setup_config_at(&home).unwrap());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "db_pass = \"original\"\n"
        );
        assert!(!backup_temp.exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn process_marker_rejects_an_unrelated_or_changed_process() {
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
        assert!(
            active_ce_process_marker_with(&home, |_| Ok(Some(identity.clone())), || Ok(false))
                .unwrap()
                .is_none()
        );
        assert!(!marker_path.exists());

        let stale = serde_json::to_vec(&CeProcessMarker {
            pid,
            identity: format!("{identity}-kumiho_server-different-process"),
        })
        .unwrap();
        write_private_file(&marker_path, &stale).unwrap();
        assert!(
            active_ce_process_marker_with(&home, |_| Ok(Some(identity)), || Ok(false))
                .unwrap()
                .is_none()
        );
        assert!(!marker_path.exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn process_identity_match_requires_exact_identity_and_ce_name() {
        let expected = "Mon Aug 26 01:30:48 2026 /opt/kumiho_server";
        assert!(ce_process_identity_matches(expected, Some(expected)));
        assert!(!ce_process_identity_matches(
            expected,
            Some("Mon Aug 26 01:30:49 2026 /opt/kumiho_server")
        ));
        assert!(!ce_process_identity_matches("123|notepad", Some("123|notepad")));
        assert!(!ce_process_identity_matches(expected, None));
    }

    #[test]
    fn atomic_recovery_record_never_replaces_an_existing_record() {
        let home = test_home("atomic-process-marker");
        let marker_path = ce_process_marker_path(&home);
        write_private_file_atomic(&marker_path, b"first").unwrap();
        assert!(write_private_file_atomic(&marker_path, b"second").is_err());
        assert_eq!(std::fs::read(&marker_path).unwrap(), b"first");
        assert!(!marker_path.with_extension("tmp").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stopping_a_tracked_child_removes_the_handle_only_after_exit() {
        #[cfg(windows)]
        let child = command("powershell")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .spawn()
            .unwrap();
        #[cfg(unix)]
        let child = command("/bin/sleep").arg("30").spawn().unwrap();

        let mut tracked = Some(child);
        assert!(stop_tracked_child(&mut tracked).unwrap());
        assert!(tracked.is_none());
    }

    #[test]
    fn neo4j_config_password_requires_eight_unicode_characters() {
        assert!(neo4j_password_error("").is_some());
        assert!(neo4j_password_error("1234567").is_some());
        assert_eq!(neo4j_password_error("12345678"), None);
        assert_eq!(neo4j_password_error("여덟글자암호예요"), None);
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
        assert!(!home.join(CE_CONFIG_BACKUP_TEMP_FILE).exists());
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
