//! 9miho application lifecycle.
//!
//! Release installers carry the private 9miho prebuild as a signed sidecar.
//! "Install" copies that immutable payload into ~/.kumiho/apps/9miho; "Start"
//! launches it against the explicit CE/Cloud choice already owned by Desktop.

use crate::config::desktop_config_get;
use crate::util::{command, kumiho_home};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Manager, State};

const MIHO_PORT: u16 = 9999;
const BUNDLED_BUILD: &str = include_str!("../9miho-version.json");

#[derive(Deserialize)]
struct BuildInfo {
    version: String,
}

fn build_info() -> BuildInfo {
    serde_json::from_str(BUNDLED_BUILD).expect("valid src-tauri/9miho-version.json")
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "9miho.exe"
    } else {
        "9miho"
    }
}

fn bundled_binary() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(binary_name());
    candidate.exists().then_some(candidate)
}

fn install_root() -> Option<std::path::PathBuf> {
    kumiho_home().map(|home| home.join("apps").join("9miho"))
}

fn installed_binary() -> Option<std::path::PathBuf> {
    let candidate = install_root()?.join("bin").join(binary_name());
    candidate.exists().then_some(candidate)
}

fn installed_version_at(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("manifest.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

fn installed_version() -> Option<String> {
    installed_version_at(&install_root()?)
}

fn write_install_manifest(root: &Path) -> Result<(), String> {
    fs::write(root.join("manifest.json"), BUNDLED_BUILD).map_err(|e| e.to_string())
}

fn newer_version_available(installed: &str, bundled: &str) -> bool {
    match (
        semver::Version::parse(installed),
        semver::Version::parse(bundled),
    ) {
        (Ok(installed), Ok(bundled)) => bundled > installed,
        _ => installed != bundled,
    }
}

fn health_ok() -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], MIHO_PORT).into();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .is_err()
    {
        return false;
    }
    let request = concat!(
        "GET /api/healthz HTTP/1.1\r\n",
        "Host: 127.0.0.1\r\n",
        "Accept: application/json\r\n",
        "Connection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    // Do not use `read_to_string`: it waits for EOF, so a healthy keep-alive
    // response can be misreported as a timeout after its complete body arrived.
    // Return as soon as the health JSON is complete instead.
    let mut response = Vec::with_capacity(512);
    let mut chunk = [0_u8; 512];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return health_response_ok(&response),
            Ok(read) => {
                response.extend_from_slice(&chunk[..read]);
                if health_response_ok(&response) {
                    return true;
                }
                if response.len() > 64 * 1024 {
                    return false;
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return false;
            }
            Err(_) => return false,
        }
    }
}

fn health_response_ok(response: &[u8]) -> bool {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let Some(status_line) = headers.lines().next() else {
        return false;
    };
    if !(status_line.starts_with("HTTP/1.1 200") || status_line.starts_with("HTTP/1.0 200")) {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&response[header_end + 4..])
        .ok()
        .and_then(|body| body.get("status")?.as_str().map(str::to_owned))
        .is_some_and(|status| status == "ok")
}

#[cfg(test)]
mod tests {
    use super::{
        build_info, health_response_ok, installed_version_at, newer_version_available,
        write_install_manifest,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn accepts_complete_health_response_without_waiting_for_eof() {
        assert!(health_response_ok(
            b"HTTP/1.1 200 OK\r\ncontent-length: 15\r\n\r\n{\"status\":\"ok\"}"
        ));
    }

    #[test]
    fn rejects_partial_or_unhealthy_responses() {
        assert!(!health_response_ok(
            b"HTTP/1.1 200 OK\r\ncontent-length: 15\r\n\r\n{\"status\":"
        ));
        assert!(!health_response_ok(
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 20\r\n\r\n{\"status\":\"starting\"}"
        ));
    }

    #[test]
    fn only_offers_a_newer_bundled_9miho_version() {
        assert!(newer_version_available("0.1.3", "0.3.0"));
        assert!(!newer_version_available("0.3.0", "0.3.0"));
        assert!(!newer_version_available("0.4.0", "0.3.0"));
    }

    #[test]
    fn installed_manifest_reports_the_version_after_an_update() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kumiho-desktop-miho-manifest-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create test install root");

        write_install_manifest(&root).expect("write installed manifest");
        assert_eq!(installed_version_at(&root), Some(build_info().version));

        fs::remove_dir_all(root).expect("remove test install root");
    }
}

#[derive(Serialize)]
pub struct MihoStatus {
    pub reachable: bool,
    pub port: u16,
    pub bundled: bool,
    pub installed: bool,
    pub version: Option<String>,
    pub bundled_version: String,
    pub update_available: bool,
}

#[tauri::command]
pub fn miho_status() -> MihoStatus {
    let bundled_version = build_info().version;
    let version = installed_version();
    MihoStatus {
        reachable: health_ok(),
        port: MIHO_PORT,
        bundled: bundled_binary().is_some(),
        installed: installed_binary().is_some(),
        update_available: version
            .as_deref()
            .is_some_and(|v| newer_version_available(v, &bundled_version)),
        version,
        bundled_version,
    }
}

#[tauri::command]
pub fn miho_install(state: State<AppState>) -> Result<String, String> {
    miho_stop(state)?;
    let source = bundled_binary().ok_or(
        "this development build does not contain 9miho; use a Kumiho Desktop release installer",
    )?;
    let root = install_root().ok_or("no home directory")?;
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("data")).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("logs")).map_err(|e| e.to_string())?;
    let destination = bin_dir.join(binary_name());
    fs::copy(&source, &destination)
        .map_err(|e| format!("could not install {}: {e}", destination.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&destination)
            .map_err(|e| e.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&destination, permissions).map_err(|e| e.to_string())?;
    }
    write_install_manifest(&root)?;
    Ok(format!("9miho {} installed", build_info().version))
}

#[tauri::command]
pub fn miho_start(state: State<AppState>) -> Result<String, String> {
    if health_ok() {
        return Ok("9miho already serving on 9999".into());
    }
    let addr: SocketAddr = ([127, 0, 0, 1], MIHO_PORT).into();
    if TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok() {
        return Err("port 9999 is occupied by a process that is not 9miho".into());
    }
    let binary = installed_binary().ok_or("9miho is not installed yet")?;
    let root = install_root().ok_or("no home directory")?;
    let cfg = desktop_config_get();
    if cfg.mode != "ce" && cfg.mode != "cloud" {
        return Err("choose Community Edition or Kumiho Cloud before starting 9miho".into());
    }

    let log_path = root.join("logs").join("9miho.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| e.to_string())?;
    let stderr = stdout.try_clone().map_err(|e| e.to_string())?;

    let mut child = command(binary.to_str().ok_or("invalid 9miho install path")?);
    child
        .args(["--kumiho", cfg.mode.as_str()])
        .current_dir(&root)
        .env("MIHO_PORT", MIHO_PORT.to_string())
        .env("STORAGE_ROOT", root.join("data"))
        .env("KUMIHO_MODE", &cfg.mode)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    if cfg.mode == "ce" {
        child
            .env("KUMIHO_CLAUDE_MODE", "ce")
            .env(
                "KUMIHO_URL",
                format!("http://127.0.0.1:{}", cfg.server_port),
            )
            .env(
                "KUMIHO_LOCAL_SERVER_ENDPOINT",
                format!("127.0.0.1:{}", cfg.server_port),
            )
            .env_remove("KUMIHO_AUTH_TOKEN");
    } else {
        let token = crate::account::cloud_token()
            .ok_or("Kumiho Cloud is selected but no service token is stored")?;
        child
            .env("KUMIHO_AUTH_TOKEN", token)
            .env_remove("KUMIHO_CLAUDE_MODE")
            .env_remove("KUMIHO_LOCAL_SERVER_ENDPOINT");
    }

    let child = child.spawn().map_err(|e| e.to_string())?;
    *state.miho.lock().map_err(|e| e.to_string())? = Some(child);
    Ok(format!("9miho starting on {MIHO_PORT}"))
}

fn stop_tracked(process: &Mutex<Option<std::process::Child>>) -> Result<(), String> {
    let child = process.lock().map_err(|e| e.to_string())?.take();
    let Some(mut child) = child else {
        return Ok(());
    };
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = command("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
    Ok(())
}

pub fn kill_tracked_miho(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let _ = stop_tracked(&state.miho);
}

#[tauri::command]
pub fn miho_stop(state: State<AppState>) -> Result<String, String> {
    stop_tracked(&state.miho)?;
    Ok("9miho stopped".into())
}
