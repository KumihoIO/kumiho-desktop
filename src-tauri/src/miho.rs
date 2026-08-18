//! 9miho application lifecycle.
//!
//! Release installers carry the private 9miho prebuild as a signed sidecar.
//! "Install" copies that immutable payload into ~/.kumiho/apps/9miho; "Start"
//! launches it against the explicit CE/Cloud choice already owned by Desktop.

use crate::config::desktop_config_get;
use crate::util::{command, kumiho_home};
use crate::AppState;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Manager, State};

const MIHO_PORT: u16 = 9999;
const BUNDLED_BUILD: &str = include_str!("../9miho-version.json");
const UPDATE_FEED_URL: &str =
    "https://github.com/KumihoIO/9miho-release/releases/latest/download/latest.json";
// 9miho component-only Minisign key. The private source repository signs each
// archive before publishing it to the public binary-only release repository.
const UPDATE_PUBLIC_KEY: &str = "RWQVYAIfinTi8U4QHo9OKjUXrg/VcKAMm9McT7PFNhjwUHwy5TUKLFys";
const MAX_UPDATE_BYTES: u64 = 512 * 1024 * 1024;
/// How long a 9miho gets to shut down gracefully before we force it.
const STOP_GRACE: Duration = Duration::from_secs(8);
/// The same, on the app-quit path — quitting must not visibly hang.
const EXIT_GRACE: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct BuildInfo {
    version: String,
}

#[derive(Clone, Deserialize)]
struct RuntimeRelease {
    url: String,
    signature: String,
    sha256: String,
}

#[derive(Deserialize)]
struct RuntimeFeed {
    version: String,
    platforms: HashMap<String, RuntimeRelease>,
}

#[derive(Deserialize)]
struct RuntimeArchiveManifest {
    version: String,
    platform: String,
    arch: String,
    api_port: u16,
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

fn runtime_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn runtime_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

fn runtime_target() -> String {
    format!("{}-{}", runtime_platform(), runtime_arch())
}

fn parse_update_feed(text: &str) -> Result<(String, RuntimeRelease), String> {
    let feed: RuntimeFeed =
        serde_json::from_str(text).map_err(|e| format!("invalid 9miho update feed: {e}"))?;
    semver::Version::parse(&feed.version)
        .map_err(|e| format!("invalid 9miho release version: {e}"))?;
    let target = runtime_target();
    let release = feed
        .platforms
        .get(&target)
        .cloned()
        .ok_or_else(|| format!("9miho {target} update is not published yet"))?;
    if !release
        .url
        .starts_with("https://github.com/KumihoIO/9miho-release/releases/")
    {
        return Err("9miho update URL is not an official Kumiho runtime release".into());
    }
    if release.sha256.len() != 64 || !release.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("9miho update feed contains an invalid SHA-256".into());
    }
    if !release.signature.starts_with("untrusted comment:") {
        return Err("9miho update feed contains a non-Minisign signature".into());
    }
    Ok((feed.version, release))
}

fn fetch_update_feed() -> Result<(String, RuntimeRelease), String> {
    let response = ureq::get(UPDATE_FEED_URL)
        .set("Cache-Control", "no-cache")
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("could not check 9miho updates: {e}"))?;
    let text = response
        .into_string()
        .map_err(|e| format!("could not read 9miho update feed: {e}"))?;
    parse_update_feed(&text)
}

fn download_release(release: &RuntimeRelease) -> Result<Vec<u8>, String> {
    let response = ureq::get(&release.url)
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("could not download 9miho update: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_UPDATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read 9miho update: {e}"))?;
    if bytes.len() as u64 > MAX_UPDATE_BYTES {
        return Err("9miho update archive is unexpectedly large".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(&release.sha256) {
        return Err(format!(
            "9miho update checksum mismatch (got {actual}, expected {})",
            release.sha256
        ));
    }
    let key = PublicKey::from_base64(UPDATE_PUBLIC_KEY)
        .map_err(|e| format!("invalid embedded updater public key: {e}"))?;
    let signature = Signature::decode(&release.signature)
        .map_err(|e| format!("invalid 9miho update signature: {e}"))?;
    key.verify(&bytes, &signature, false)
        .map_err(|e| format!("9miho update signature verification failed: {e}"))?;
    Ok(bytes)
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

fn unpack_runtime(
    archive_bytes: Vec<u8>,
    expected_version: &str,
    staging: &Path,
) -> Result<String, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(archive_bytes)).map_err(|e| e.to_string())?;
    let manifest_text = {
        let mut file = archive
            .by_name("manifest.json")
            .map_err(|e| format!("9miho archive has no manifest.json: {e}"))?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| format!("could not read 9miho archive manifest: {e}"))?;
        text
    };
    let manifest: RuntimeArchiveManifest = serde_json::from_str(&manifest_text)
        .map_err(|e| format!("invalid 9miho archive manifest: {e}"))?;
    if manifest.version != expected_version
        || manifest.platform != runtime_platform()
        || manifest.arch != runtime_arch()
        || manifest.api_port != MIHO_PORT
    {
        return Err(format!(
            "9miho archive does not match this release (version={}, platform={}, arch={}, port={})",
            manifest.version, manifest.platform, manifest.arch, manifest.api_port
        ));
    }
    fs::create_dir_all(staging).map_err(|e| e.to_string())?;
    let mut source = archive
        .by_name(binary_name())
        .map_err(|e| format!("9miho archive has no {}: {e}", binary_name()))?;
    let staged_binary = staging.join(binary_name());
    let mut destination = fs::File::create(&staged_binary).map_err(|e| e.to_string())?;
    std::io::copy(&mut source, &mut destination).map_err(|e| e.to_string())?;
    destination.sync_all().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged_binary, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(manifest_text)
}

fn replace_runtime(root: &Path, staged_binary: &Path, manifest: &str) -> Result<(), String> {
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let destination = bin_dir.join(binary_name());
    let backup = bin_dir.join(format!("{}.previous", binary_name()));
    let old_manifest = fs::read(root.join("manifest.json")).ok();
    if backup.exists() {
        fs::remove_file(&backup).map_err(|e| e.to_string())?;
    }
    let had_previous = destination.exists();
    if had_previous {
        fs::rename(&destination, &backup)
            .map_err(|e| format!("could not stage the installed 9miho for replacement: {e}"))?;
    }
    if let Err(error) = fs::rename(staged_binary, &destination) {
        if had_previous {
            let _ = fs::rename(&backup, &destination);
        }
        return Err(format!("could not activate the 9miho update: {error}"));
    }
    if let Err(error) = fs::write(root.join("manifest.json"), manifest) {
        let _ = fs::remove_file(&destination);
        if had_previous {
            let _ = fs::rename(&backup, &destination);
        }
        match old_manifest {
            Some(old) => {
                let _ = fs::write(root.join("manifest.json"), old);
            }
            None => {
                let _ = fs::remove_file(root.join("manifest.json"));
            }
        }
        return Err(format!(
            "could not record the installed 9miho version: {error}"
        ));
    }
    if backup.exists() {
        fs::remove_file(backup).map_err(|e| e.to_string())?;
    }
    Ok(())
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

/// One `/api/healthz` round trip. `Some(bytes)` only when 9miho answered with a
/// complete, healthy response — so callers can also read fields out of the body.
fn health_probe() -> Option<Vec<u8>> {
    let addr: SocketAddr = ([127, 0, 0, 1], MIHO_PORT).into();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return None;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .is_err()
    {
        return None;
    }
    let request = concat!(
        "GET /api/healthz HTTP/1.1\r\n",
        "Host: 127.0.0.1\r\n",
        "Accept: application/json\r\n",
        "Connection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return None;
    }
    // Do not use `read_to_string`: it waits for EOF, so a healthy keep-alive
    // response can be misreported as a timeout after its complete body arrived.
    // Return as soon as the health JSON is complete instead.
    let mut response = Vec::with_capacity(512);
    let mut chunk = [0_u8; 512];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return health_response_ok(&response).then_some(response),
            Ok(read) => {
                response.extend_from_slice(&chunk[..read]);
                if health_response_ok(&response) {
                    return Some(response);
                }
                if response.len() > 64 * 1024 {
                    return None;
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return None;
            }
            Err(_) => return None,
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

/// The version reported inside a healthy `/api/healthz` body, when 9miho
/// publishes one. Older runtimes omit it, hence the stamp fallback below.
fn health_response_version(response: &[u8]) -> Option<String> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    serde_json::from_slice::<serde_json::Value>(&response[header_end + 4..])
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_owned)
}

#[derive(Serialize, Deserialize)]
struct RuntimeStamp {
    pid: u32,
    version: String,
}

fn runtime_stamp_path() -> Option<std::path::PathBuf> {
    Some(install_root()?.join("runtime.json"))
}

/// Record which build we just spawned. `manifest.json` describes what is ON DISK;
/// this describes what is actually RUNNING. Conflating the two is what let an
/// update silently leave the previous 9miho serving on 9999.
fn write_runtime_stamp(pid: u32, version: &str) {
    let Some(path) = runtime_stamp_path() else {
        return;
    };
    if let Ok(text) = serde_json::to_string(&RuntimeStamp {
        pid,
        version: version.to_owned(),
    }) {
        let _ = fs::write(path, text);
    }
}

fn read_runtime_stamp() -> Option<RuntimeStamp> {
    let text = fs::read_to_string(runtime_stamp_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn clear_runtime_stamp() {
    if let Some(path) = runtime_stamp_path() {
        let _ = fs::remove_file(path);
    }
}

/// `(something healthy is on 9999, which version it is serving)`.
fn runtime_state() -> (bool, Option<String>) {
    let Some(response) = health_probe() else {
        return (false, None);
    };
    let serving = health_response_version(&response)
        .or_else(|| read_runtime_stamp().map(|stamp| stamp.version));
    (true, serving)
}

fn is_stale(installed: Option<&str>, reachable: bool, serving: Option<&str>) -> bool {
    if !reachable {
        return false;
    }
    let Some(installed) = installed else {
        return false;
    };
    // No stamp and no version in the health body means we cannot PROVE the
    // running process is the installed build. Treat that as stale: one extra
    // restart is cheap, silently serving the previous build is the bug.
    match serving {
        Some(serving) => serving != installed,
        None => true,
    }
}

fn wait_for_port_closed(timeout: Duration) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], MIHO_PORT).into();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Kill 9miho processes this Desktop session does not own — the ones left behind
/// by a previous session, which `state.miho` knows nothing about.
fn kill_orphan_miho(force: bool) {
    let Some(binary) = installed_binary() else {
        return;
    };
    #[cfg(windows)]
    {
        // Windows has no graceful signal for a console process: taskkill without
        // /F sends WM_CLOSE, which 9miho never sees, so the polite attempt would
        // only cost us the full STOP_GRACE before we forced it anyway.
        let _ = (binary, force);
        let _ = command("taskkill")
            .args(["/IM", binary_name(), "/T", "/F"])
            .output();
    }
    #[cfg(not(windows))]
    {
        let Some(path) = binary.to_str() else {
            return;
        };
        let signal = if force { "-KILL" } else { "-TERM" };
        let _ = command("pkill").args([signal, "-f", path]).output();
    }
}

fn terminate_tracked(
    process: &Mutex<Option<std::process::Child>>,
    grace: Duration,
) -> Result<(), String> {
    let child = process.lock().map_err(|e| e.to_string())?.take();
    let Some(mut child) = child else {
        return Ok(());
    };
    #[cfg(windows)]
    {
        let _ = grace;
        let pid = child.id().to_string();
        let _ = command("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .output();
    }
    #[cfg(unix)]
    {
        // SIGTERM before SIGKILL: the PyInstaller onefile runtime only removes
        // its extracted _MEI temp directory on a graceful exit, so SIGKILL leaks
        // ~40MB of temp per run. std::process::Child::kill is SIGKILL only.
        // SAFETY: kill(2) on a pid this process owns and has not yet reaped; the
        // worst case is ESRCH, which we ignore.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(150));
                }
                _ => {
                    let _ = child.kill();
                    break;
                }
            }
        }
    }
    let _ = child.wait();
    Ok(())
}

/// Stop every 9miho on this machine and do not return until 9999 is free.
/// Caller must hold [`lifecycle_lock`].
fn stop_miho(state: &AppState) -> Result<(), String> {
    terminate_tracked(&state.miho, STOP_GRACE)?;
    kill_orphan_miho(false);
    if !wait_for_port_closed(STOP_GRACE) {
        kill_orphan_miho(true);
        if !wait_for_port_closed(Duration::from_secs(3)) {
            return Err(
                "9miho is still holding 127.0.0.1:9999 — stop it manually and try again".into(),
            );
        }
    }
    clear_runtime_stamp();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        binary_name, build_info, download_release, fetch_update_feed, health_response_ok,
        health_response_version, installed_version_at, is_stale, newer_version_available,
        parse_update_feed, replace_runtime, runtime_target, write_install_manifest,
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
    fn reads_the_version_out_of_a_health_body() {
        assert_eq!(
            health_response_version(
                b"HTTP/1.1 200 OK\r\ncontent-length: 34\r\n\r\n{\"status\":\"ok\",\"version\":\"0.13.4\"}"
            )
            .as_deref(),
            Some("0.13.4")
        );
        assert_eq!(
            health_response_version(
                b"HTTP/1.1 200 OK\r\ncontent-length: 15\r\n\r\n{\"status\":\"ok\"}"
            ),
            None
        );
    }

    #[test]
    fn a_runtime_serving_a_different_build_than_the_one_installed_is_stale() {
        // The regression this guards: update wrote 0.13.4 to disk while 0.13.1
        // kept answering on 9999, and Desktop reported success.
        assert!(is_stale(Some("0.13.4"), true, Some("0.13.1")));
        assert!(!is_stale(Some("0.13.4"), true, Some("0.13.4")));
        // Nothing listening: nothing to retire.
        assert!(!is_stale(Some("0.13.4"), false, None));
        // Unknown serving version cannot be proven current, so restart it.
        assert!(is_stale(Some("0.13.4"), true, None));
        // Nothing installed: leave whatever is running alone.
        assert!(!is_stale(None, true, None));
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

    #[test]
    fn online_feed_selects_the_current_platform_release() {
        let target = runtime_target();
        let feed = format!(
            r#"{{"version":"0.4.0","platforms":{{"{target}":{{"url":"https://github.com/KumihoIO/9miho-release/releases/download/9miho-v0.4.0/9miho.zip","signature":"untrusted comment: signature from minisign secret key\nRUTestSignature","sha256":"{}"}}}}}}"#,
            "a".repeat(64)
        );
        let (version, release) = parse_update_feed(&feed).expect("valid feed");
        assert_eq!(version, "0.4.0");
        assert!(release.url.ends_with("9miho.zip"));
    }

    #[test]
    fn runtime_replacement_updates_binary_and_manifest_together() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kumiho-desktop-miho-replace-{}-{unique}",
            std::process::id()
        ));
        let bin = root.join("bin");
        let staging = root.join("staging");
        fs::create_dir_all(&bin).expect("create bin");
        fs::create_dir_all(&staging).expect("create staging");
        fs::write(bin.join(binary_name()), b"old").expect("write old binary");
        fs::write(root.join("manifest.json"), r#"{"version":"0.3.0"}"#)
            .expect("write old manifest");
        fs::write(staging.join(binary_name()), b"new").expect("write staged binary");

        replace_runtime(
            &root,
            &staging.join(binary_name()),
            r#"{"version":"0.4.0"}"#,
        )
        .expect("replace runtime");

        assert_eq!(
            fs::read(bin.join(binary_name())).expect("read binary"),
            b"new"
        );
        assert_eq!(installed_version_at(&root).as_deref(), Some("0.4.0"));
        fs::remove_dir_all(root).expect("remove test install root");
    }

    #[test]
    #[ignore = "requires the public 9miho release feed"]
    fn live_public_update_feed_has_a_valid_signed_archive() {
        let (version, release) = fetch_update_feed().expect("fetch public feed");
        assert!(semver::Version::parse(&version).is_ok());
        let bytes = download_release(&release).expect("download and verify signed archive");
        assert!(!bytes.is_empty());
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
    /// The build answering on 9999, which is not necessarily `version` (that one
    /// is read from manifest.json on disk).
    pub serving_version: Option<String>,
    /// A 9miho is running, but it is not the build we have installed — it needs a
    /// restart before the update actually takes effect.
    pub stale: bool,
}

#[derive(Serialize)]
pub struct MihoUpdateInfo {
    pub installed_version: Option<String>,
    pub bundled_version: String,
    pub latest_version: String,
    pub update_available: bool,
}

#[tauri::command]
pub fn miho_status() -> MihoStatus {
    let bundled_version = build_info().version;
    let version = installed_version();
    // One probe, both answers: whether anything is serving and what it is.
    let (reachable, serving_version) = runtime_state();
    MihoStatus {
        reachable,
        port: MIHO_PORT,
        bundled: bundled_binary().is_some(),
        installed: installed_binary().is_some(),
        update_available: version
            .as_deref()
            .is_some_and(|v| newer_version_available(v, &bundled_version)),
        stale: is_stale(version.as_deref(), reachable, serving_version.as_deref()),
        serving_version,
        version,
        bundled_version,
    }
}

#[tauri::command]
pub fn miho_check_update() -> Result<MihoUpdateInfo, String> {
    let installed_version = installed_version();
    let bundled_version = build_info().version;
    let (latest_version, _) = fetch_update_feed()?;
    let update_available = installed_version
        .as_deref()
        .map(|installed| newer_version_available(installed, &latest_version))
        .unwrap_or(true);
    Ok(MihoUpdateInfo {
        installed_version,
        bundled_version,
        latest_version,
        update_available,
    })
}

#[tauri::command]
pub fn miho_update(state: State<AppState>) -> Result<String, String> {
    let (latest_version, release) = fetch_update_feed()?;
    if installed_version()
        .as_deref()
        .is_some_and(|installed| !newer_version_available(installed, &latest_version))
    {
        return Ok(format!("9miho {latest_version} is already installed"));
    }
    let bytes = download_release(&release)?;
    let root = install_root().ok_or("no home directory")?;
    fs::create_dir_all(root.join("data")).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("logs")).map_err(|e| e.to_string())?;
    let staging = root.join(format!(
        ".update-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let app: &AppState = &state;
    let result = (|| {
        let manifest = unpack_runtime(bytes, &latest_version, &staging)?;
        // Held from here so no start can slip between the stop and the swap.
        let _lifecycle = lifecycle_lock(app)?;
        // Must stop BEFORE swapping the binary, and must actually stop: an orphan
        // from a previous Desktop session keeps serving the old build otherwise.
        stop_miho(app)?;
        replace_runtime(&root, &staging.join(binary_name()), &manifest)?;
        // ...and restart here rather than leaving it to the frontend, so the
        // update is never reported as done while nothing is running the new bits.
        // Past this point the new build IS installed, so a restart that fails
        // (no mode chosen yet, cloud token gone) must not be reported as a failed
        // update — that sends the user back to re-download what already landed.
        Ok(match start_miho(app) {
            Ok(_) => format!("9miho {latest_version} updated and restarted"),
            Err(error) => {
                format!("9miho {latest_version} updated, but it did not restart: {error}")
            }
        })
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[tauri::command]
pub fn miho_install(state: State<AppState>) -> Result<String, String> {
    let app: &AppState = &state;
    let _lifecycle = lifecycle_lock(app)?;
    stop_miho(app)?;
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
    // Installed either way; only the restart can still fail. See miho_update.
    let version = build_info().version;
    Ok(match start_miho(app) {
        Ok(_) => format!("9miho {version} installed and restarted"),
        Err(error) => format!("9miho {version} installed, but it did not restart: {error}"),
    })
}

#[tauri::command]
pub fn miho_start(state: State<AppState>) -> Result<String, String> {
    let _lifecycle = lifecycle_lock(&state)?;
    start_miho(&state)
}

/// Serializes every mutation of 9miho's lifecycle — stop, binary swap, spawn.
/// Three UI paths can start at once (Apps install, product tab, empty-state
/// button) and a 40MB PyInstaller onefile takes seconds to extract before it
/// binds, so unguarded callers all saw a closed port and every one of them
/// spawned. Stop and install/update take the same lock: a start that slips
/// between an update's stop and its swap spawns the build about to be replaced,
/// and on Windows that runtime holds the binary open so the swap fails outright.
fn lifecycle_lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, ()>, String> {
    state.miho_start.lock().map_err(|e| e.to_string())
}

/// Caller must hold [`lifecycle_lock`].
fn start_miho(state: &AppState) -> Result<String, String> {
    let (reachable, serving) = runtime_state();
    if reachable {
        if !is_stale(installed_version().as_deref(), true, serving.as_deref()) {
            return Ok("9miho already serving on 9999".into());
        }
        // Healthy, but not the build we have on disk — the previous runtime
        // survived an update. Retiring it here is the whole point of this guard.
        stop_miho(state)?;
    }

    // A child we already spawned that has not finished booting yet. Never spawn a
    // second one: it loses the bind race, and storing it would orphan the first.
    {
        let mut tracked = state.miho.lock().map_err(|e| e.to_string())?;
        match tracked.as_mut().map(std::process::Child::try_wait) {
            Some(Ok(None)) => return Ok("9miho is already starting on 9999".into()),
            Some(_) => *tracked = None,
            None => {}
        }
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
    write_runtime_stamp(
        child.id(),
        installed_version().as_deref().unwrap_or_default(),
    );
    *state.miho.lock().map_err(|e| e.to_string())? = Some(child);
    Ok(format!("9miho starting on {MIHO_PORT}"))
}

/// App shutdown: retire the child we own so it does not become the orphan that
/// the next session's update has to fight.
pub fn kill_tracked_miho(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let _ = terminate_tracked(&state.miho, EXIT_GRACE);
    clear_runtime_stamp();
}

#[tauri::command]
pub fn miho_stop(state: State<AppState>) -> Result<String, String> {
    let _lifecycle = lifecycle_lock(&state)?;
    stop_miho(&state)?;
    Ok("9miho stopped".into())
}
