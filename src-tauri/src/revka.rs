//! Revka application lifecycle.
//!
//! Unlike 9miho, Revka ships no bundled payload: the binary is downloaded at
//! runtime from the public KumihoIO/Revka GitHub releases (calver tags such as
//! v2026.6.30), verified against the release SHA256SUMS fail-closed, and
//! installed into ~/.kumiho/apps/revka/bin. An existing standalone install at
//! ~/.revka/bin (the official install.ps1/install.sh location) is detected and
//! reused. "Start" launches `revka daemon` bound to loopback :42617.

use crate::util::{command, kumiho_home};
use crate::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Manager, State};

const REVKA_PORT: u16 = 42617;
const RELEASE_API_URL: &str = "https://api.github.com/repos/KumihoIO/Revka/releases/latest";
const REVKA_PAIRING_URL: &str = "http://127.0.0.1:42617/admin/paircode";
const REVKA_PAIRING_NEW_URL: &str = "http://127.0.0.1:42617/admin/paircode/new";
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
/// How long a Revka gets to shut down gracefully before we force it.
const STOP_GRACE: Duration = Duration::from_secs(8);
/// The same, on the app-quit path — quitting must not visibly hang.
const EXIT_GRACE: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct ActiveWorkspaceState {
    config_dir: String,
}

#[derive(Deserialize)]
struct PairingResponse {
    success: bool,
    pairing_required: bool,
    pairing_code: Option<String>,
    #[serde(default)]
    message: String,
}

/// The release asset published for this machine's platform/architecture.
/// Mirrors Revka's own installer policy: Windows ARM64 runs the x86_64 build
/// under emulation, so there is deliberately no aarch64 mapping for Windows.
fn release_asset_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "revka-x86_64-pc-windows-msvc.zip"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "revka-aarch64-apple-darwin.tar.gz"
        } else {
            "revka-x86_64-apple-darwin.tar.gz"
        }
    } else if cfg!(target_arch = "aarch64") {
        "revka-aarch64-unknown-linux-gnu.tar.gz"
    } else {
        "revka-x86_64-unknown-linux-gnu.tar.gz"
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "revka.exe"
    } else {
        "revka"
    }
}

/// Release tag "v2026.6.30" → version "2026.6.30". A prerelease/build suffix
/// ("-rc1") is preserved so comparisons still work via the string fallback.
fn tag_version(tag: &str) -> Result<String, String> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let core = version.split(['-', '+']).next().unwrap_or(version);
    if core.is_empty()
        || !core.chars().all(|c| c.is_ascii_digit() || c == '.')
        || !core.contains('.')
        || core.split('.').any(|segment| segment.is_empty())
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!("invalid Revka release tag: {tag}"));
    }
    Ok(version.to_owned())
}

fn parse_release(text: &str) -> Result<(String, GhRelease), String> {
    let release: GhRelease =
        serde_json::from_str(text).map_err(|e| format!("invalid Revka release feed: {e}"))?;
    let version = tag_version(&release.tag_name)?;
    let wanted = release_asset_name();
    if !release.assets.iter().any(|asset| asset.name == wanted) {
        return Err(format!("Revka {wanted} is not published yet"));
    }
    Ok((version, release))
}

fn fetch_latest_release() -> Result<(String, GhRelease), String> {
    let response = ureq::get(RELEASE_API_URL)
        .set("User-Agent", "kumiho-desktop")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("could not check Revka releases: {e}"))?;
    let text = response
        .into_string()
        .map_err(|e| format!("could not read Revka release feed: {e}"))?;
    parse_release(&text)
}

/// Pull the expected digest for `asset_name` out of a SHA256SUMS manifest.
/// Lines look like `<hex>  name` or `<hex> *name`; anything else fails closed.
fn checksum_for(sums: &str, asset_name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let filename = fields.next()?.trim_start_matches('*');
        if filename == asset_name
            && digest.len() == 64
            && digest.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

fn download(url: &str, what: &str) -> Result<Vec<u8>, String> {
    // Pin the host and repository path exactly like the 9miho feed does: the
    // release JSON alone must never choose where bytes come from.
    if !url.starts_with("https://github.com/KumihoIO/Revka/releases/") {
        return Err("Revka download URL is not an official Kumiho release".into());
    }
    let response = ureq::get(url)
        .set("User-Agent", "kumiho-desktop")
        .timeout(Duration::from_secs(300))
        .call()
        .map_err(|e| format!("could not download {what}: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_DOWNLOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not read {what}: {e}"))?;
    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(format!("{what} is unexpectedly large"));
    }
    Ok(bytes)
}

/// Download the archive and verify it against the release SHA256SUMS before
/// anything touches disk. No checksum entry is a hard failure, never a warning.
fn download_release(release: &GhRelease) -> Result<Vec<u8>, String> {
    let asset_name = release_asset_name();
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| format!("Revka {asset_name} is not published yet"))?;
    let sums_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS")
        .ok_or("Revka release has no SHA256SUMS; refusing to install an unverified binary")?;
    let sums = String::from_utf8(download(&sums_asset.browser_download_url, "SHA256SUMS")?)
        .map_err(|_| "SHA256SUMS is not valid UTF-8".to_string())?;
    let expected = checksum_for(&sums, asset_name)
        .ok_or("SHA256SUMS has no entry for this platform's archive; refusing to install")?;
    let bytes = download(&asset.browser_download_url, asset_name)?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "Revka archive checksum mismatch (got {actual}, expected {expected})"
        ));
    }
    Ok(bytes)
}

fn extract_binary(archive_bytes: &[u8], staging: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(staging).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(archive_bytes))
            .map_err(|e| format!("invalid Revka archive: {e}"))?;
        let mut names: Vec<String> = (0..archive.len())
            .filter_map(|index| {
                archive
                    .by_index(index)
                    .ok()
                    .map(|file| file.name().to_owned())
            })
            .collect();
        names.sort_by_key(|name| (name.matches('/').count(), name.len()));
        for name in names {
            // Match the final path segment only — `ends_with` would also
            // accept "xrevka.exe" or "evil/../revka.exe".
            if Path::new(&name)
                .file_name()
                .is_some_and(|n| n == binary_name())
            {
                let mut source = archive
                    .by_name(&name)
                    .map_err(|e| format!("could not reopen {}: {e}", name))?;
                let destination = staging.join(binary_name());
                let mut out = fs::File::create(&destination).map_err(|e| e.to_string())?;
                std::io::copy(&mut source, &mut out).map_err(|e| e.to_string())?;
                out.sync_all().map_err(|e| e.to_string())?;
                return Ok(destination);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let decoder = flate2::read::GzDecoder::new(archive_bytes);
        let mut archive = tar::Archive::new(decoder);
        let mut candidates: Vec<PathBuf> = archive
            .entries()
            .map_err(|e| format!("invalid Revka archive: {e}"))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.path().ok().map(|path| path.into_owned()))
            .filter(|path| path.file_name().is_some_and(|name| name == binary_name()))
            .collect();
        candidates.sort_by_key(|path| (path.components().count(), path.to_string_lossy().len()));
        if let Some(entry_path) = candidates.first() {
            let decoder = flate2::read::GzDecoder::new(archive_bytes);
            let mut archive = tar::Archive::new(decoder);
            for entry in archive
                .entries()
                .map_err(|e| format!("invalid Revka archive: {e}"))?
            {
                let mut entry = entry.map_err(|e| e.to_string())?;
                if entry
                    .path()
                    .ok()
                    .is_some_and(|path| path.as_ref() == entry_path.as_path())
                {
                    let destination = staging.join(binary_name());
                    let mut out = fs::File::create(&destination).map_err(|e| e.to_string())?;
                    std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                    out.sync_all().map_err(|e| e.to_string())?;
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
                        .map_err(|e| e.to_string())?;
                    return Ok(destination);
                }
            }
        }
    }
    Err(format!(
        "Revka release archive does not contain {}",
        binary_name()
    ))
}

/// Where Desktop manages its own Revka: `~/.kumiho/apps/revka`.
fn install_root() -> Option<PathBuf> {
    kumiho_home().map(|home| home.join("apps").join("revka"))
}

fn expand_revka_path(raw: &str, home: &Path) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        home.join(rest)
    } else {
        PathBuf::from(trimmed)
    }
}

/// Revka gives a non-empty HOME precedence over the platform user directory,
/// including on Windows. Match that policy so Desktop validates the same
/// workspace that the directly-spawned wizard just wrote.
fn revka_home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    dirs::home_dir()
}

/// Resolve the same config/workspace pair used by the released Revka CLI.
/// Desktop only needs the files created by onboarding, but it must honor
/// custom and persisted workspaces or a real existing setup looks unfinished.
fn revka_runtime_dirs() -> Option<(PathBuf, PathBuf)> {
    let home = revka_home_dir()?;
    let default_config = home.join(".revka");

    if let Ok(raw) = std::env::var("REVKA_CONFIG_DIR") {
        if !raw.trim().is_empty() {
            let config = expand_revka_path(&raw, &home);
            return Some((config.clone(), config.join("workspace")));
        }
    }

    if let Ok(raw) = std::env::var("REVKA_WORKSPACE") {
        if !raw.trim().is_empty() {
            let requested = expand_revka_path(&raw, &home);
            if requested.join("config.toml").is_file() {
                return Some((requested.clone(), requested.join("workspace")));
            }
            if let Some(parent) = requested.parent() {
                let legacy = parent.join(".revka");
                if legacy.join("config.toml").is_file()
                    || requested
                        .file_name()
                        .is_some_and(|name| name == "workspace")
                {
                    return Some((legacy, requested));
                }
            }
            return Some((requested.clone(), requested.join("workspace")));
        }
    }

    let active = default_config.join("active_workspace.toml");
    if let Ok(raw) = fs::read_to_string(active) {
        if let Ok(state) = toml::from_str::<ActiveWorkspaceState>(&raw) {
            if !state.config_dir.trim().is_empty() {
                let config = expand_revka_path(&state.config_dir, &home);
                let config = if config.is_absolute() {
                    config
                } else {
                    default_config.join(config)
                };
                return Some((config.clone(), config.join("workspace")));
            }
        }
    }

    Some((default_config.clone(), default_config.join("workspace")))
}

fn onboarding_artifacts_complete_at(config_dir: &Path, workspace_dir: &Path) -> bool {
    config_dir.join("config.toml").is_file()
        && ["AGENTS.md", "USER.md", "TOOLS.md"]
            .iter()
            .all(|name| workspace_dir.join(name).is_file())
}

fn require_full_onboarding_at(config_dir: &Path, workspace_dir: &Path) -> Result<(), String> {
    if onboarding_artifacts_complete_at(config_dir, workspace_dir) {
        Ok(())
    } else {
        Err(
            "Revka exited without completing the full workspace setup. Run onboarding again and choose Full onboarding when an existing config is detected."
                .into(),
        )
    }
}

/// Compatibility signal for users who completed onboarding outside Desktop.
/// A config alone is deliberately insufficient: Revka creates one from
/// `load_or_init()` before onboarding and only the wizard adds this scaffold.
fn onboarding_artifacts_complete() -> bool {
    revka_runtime_dirs()
        .is_some_and(|(config, workspace)| onboarding_artifacts_complete_at(&config, &workspace))
}

fn onboarding_complete() -> bool {
    onboarding_artifacts_complete()
}

/// A pre-existing install from Revka's own installer scripts.
fn standalone_binary() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let candidate = home.join(".revka").join("bin").join(binary_name());
    candidate.exists().then_some(candidate)
}

fn managed_binary() -> Option<PathBuf> {
    let candidate = install_root()?.join("bin").join(binary_name());
    candidate.exists().then_some(candidate)
}

/// Managed install wins; otherwise reuse the standalone one.
fn installed_binary() -> Option<PathBuf> {
    managed_binary().or_else(standalone_binary)
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
    if let Some(root) = install_root() {
        if let Some(version) = installed_version_at(&root) {
            return Some(version);
        }
    }
    standalone_version()
}

/// Standalone installs carry no Desktop manifest, so ask the binary once per
/// session. `revka --version` prints "<name> <version>". Only successes are
/// cached — a transient failure (AV lock, cold runtime) must not pin "unknown"
/// for the whole session.
fn standalone_version() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    if let Some(cached) = CACHE.get() {
        return cached.clone();
    }
    let binary = standalone_binary()?;
    let Some(path) = binary.to_str() else {
        return None; // non-UTF-8 home directory: skip rather than panic
    };
    let output = command(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.split_whitespace().next_back().map(str::to_owned);
    if let Some(version) = version {
        let _ = CACHE.set(Some(version.clone()));
        return Some(version);
    }
    None
}

fn write_manifest(root: &Path, version: &str) -> Result<(), String> {
    let manifest = serde_json::to_string_pretty(&serde_json::json!({ "version": version }))
        .expect("serializable");
    fs::write(root.join("manifest.json"), manifest).map_err(|e| e.to_string())
}

fn replace_runtime(root: &Path, staged_binary: &Path, version: &str) -> Result<(), String> {
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
            .map_err(|e| format!("could not stage the installed Revka for replacement: {e}"))?;
    }
    if let Err(error) = fs::rename(staged_binary, &destination) {
        if had_previous {
            let _ = fs::rename(&backup, &destination);
        }
        return Err(format!("could not activate the Revka update: {error}"));
    }
    if let Err(error) = write_manifest(root, version) {
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
            "could not record the installed Revka version: {error}"
        ));
    }
    if backup.exists() {
        fs::remove_file(backup).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn newer_version_available(installed: &str, latest: &str) -> bool {
    match (
        semver::Version::parse(installed),
        semver::Version::parse(latest),
    ) {
        (Ok(installed), Ok(latest)) => latest > installed,
        _ => installed != latest,
    }
}

/// One `/health` round trip. True only when the gateway answered HTTP 200.
fn health_probe() -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], REVKA_PORT).into();
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
        "GET /health HTTP/1.1\r\n",
        "Host: 127.0.0.1\r\n",
        "Accept: application/json\r\n",
        "Connection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    // Read until a complete health body has arrived — like the 9miho probe,
    // never wait for EOF on a keep-alive response.
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
    let status_200 = headers.lines().next().is_some_and(|status_line| {
        status_line.starts_with("HTTP/1.1 200") || status_line.starts_with("HTTP/1.0 200")
    });
    if !status_200 {
        return false;
    }
    // The gateway answers {"status":"ok"}; requiring it keeps a random
    // loopback service squatting on 42617 from counting as Revka.
    serde_json::from_slice::<serde_json::Value>(&response[header_end + 4..])
        .ok()
        .and_then(|body| body.get("status")?.as_str().map(str::to_owned))
        .is_some_and(|status| status == "ok")
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct RevkaPairingInfo {
    pub success: bool,
    pub pairing_required: bool,
    /// One-time six-digit code. It is returned to the webview only and is
    /// never written to a Desktop log, config file, or runtime stamp.
    pub pairing_code: Option<String>,
    pub message: String,
}

fn parse_pairing_response(text: &str) -> Result<RevkaPairingInfo, String> {
    let response: PairingResponse =
        serde_json::from_str(text).map_err(|e| format!("invalid Revka pairing response: {e}"))?;
    if let Some(code) = response.pairing_code.as_deref() {
        if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Revka returned an invalid pairing code".into());
        }
    }
    Ok(RevkaPairingInfo {
        success: response.success,
        pairing_required: response.pairing_required,
        pairing_code: response.pairing_code,
        message: response.message,
    })
}

fn validate_pairing_http(
    status: u16,
    issue_new: bool,
    info: RevkaPairingInfo,
) -> Result<RevkaPairingInfo, String> {
    let pairing_disabled = issue_new
        && status == 400
        && !info.success
        && !info.pairing_required
        && info.pairing_code.is_none();
    if pairing_disabled {
        return Ok(info);
    }
    if !(200..300).contains(&status) || !info.success {
        return Err(if info.message.is_empty() {
            format!("Revka pairing request failed with HTTP {status}")
        } else {
            info.message
        });
    }
    if issue_new && info.pairing_required && info.pairing_code.is_none() {
        return Err("Revka did not return a new pairing code".into());
    }
    Ok(info)
}

fn pairing_request(issue_new: bool) -> Result<RevkaPairingInfo, String> {
    if !health_probe() {
        return Err("Revka is not ready on 127.0.0.1:42617".into());
    }
    let response = if issue_new {
        ureq::post(REVKA_PAIRING_NEW_URL)
            .timeout(Duration::from_secs(5))
            .call()
    } else {
        ureq::get(REVKA_PAIRING_URL)
            .timeout(Duration::from_secs(5))
            .call()
    };
    let (status, response) = match response {
        Ok(response) => (response.status(), response),
        Err(ureq::Error::Status(status, response)) => (status, response),
        Err(error) => return Err(format!("could not reach Revka pairing service: {error}")),
    };
    let text = response
        .into_string()
        .map_err(|e| format!("could not read Revka pairing response: {e}"))?;
    let info = parse_pairing_response(&text)?;
    // Only POST /new uses HTTP 400 as a valid pairing-disabled response.
    validate_pairing_http(status, issue_new, info)
}

#[derive(Serialize, Deserialize)]
struct RuntimeStamp {
    pid: u32,
    version: String,
    identity: String,
}

fn runtime_stamp_path() -> Option<PathBuf> {
    Some(install_root()?.join("runtime.json"))
}

fn is_revka_process_identity(identity: &str) -> bool {
    let Some(name) = identity.split(['|', '/', '\\', ' ', '\t']).next_back() else {
        return false;
    };
    name.eq_ignore_ascii_case(binary_name())
        || name.eq_ignore_ascii_case(binary_name().trim_end_matches(".exe"))
}

fn runtime_identity_matches(expected: &str, current: Option<&str>) -> bool {
    current == Some(expected) && is_revka_process_identity(expected)
}

fn write_runtime_stamp(pid: u32, version: &str) -> Result<(), String> {
    let path = runtime_stamp_path().ok_or("no home directory")?;
    let identity = crate::run::process_identity(pid)?
        .ok_or("spawned Revka process disappeared before it could be recorded")?;
    if !is_revka_process_identity(&identity) {
        return Err("spawned process identity did not match Revka".into());
    }
    let text = serde_json::to_string(&RuntimeStamp {
        pid,
        version: version.to_owned(),
        identity,
    })
    .map_err(|e| e.to_string())?;
    // Temp file + rename: a concurrent reader must never see truncated JSON
    // and misread the running daemon as unknown/stale.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("could not record the running Revka process: {e}")
    })
}

fn read_runtime_stamp() -> Result<Option<RuntimeStamp>, String> {
    let Some(path) = runtime_stamp_path() else {
        return Ok(None);
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("invalid Revka runtime record: {e}"))
}

fn clear_runtime_stamp() {
    if let Some(path) = runtime_stamp_path() {
        let _ = fs::remove_file(path);
    }
}

/// `(something healthy is on 42617, which version our stamp says it is)`.
fn runtime_state() -> (bool, Option<String>) {
    if !health_probe() {
        return (false, None);
    }
    (
        true,
        validated_runtime_stamp()
            .ok()
            .flatten()
            .map(|stamp| stamp.version),
    )
}

fn is_stale(installed: Option<&str>, reachable: bool, serving: Option<&str>) -> bool {
    if !reachable {
        return false;
    }
    let Some(installed) = installed else {
        return false;
    };
    // No stamp means we cannot PROVE the running daemon is ours — treat it as
    // stale so one restart retires whatever answered.
    match serving {
        Some(serving) => serving != installed,
        None => true,
    }
}

fn wait_for_port_closed(timeout: Duration) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], REVKA_PORT).into();
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

fn validated_runtime_stamp() -> Result<Option<RuntimeStamp>, String> {
    let Some(stamp) = read_runtime_stamp()? else {
        return Ok(None);
    };
    match crate::run::process_identity(stamp.pid)? {
        current if runtime_identity_matches(&stamp.identity, current.as_deref()) => Ok(Some(stamp)),
        _ => {
            // The recorded process is gone. A reused PID — even another Revka
            // process — is not ours and must never inherit this record.
            clear_runtime_stamp();
            Ok(None)
        }
    }
}

fn terminate_tracked(
    process: &Mutex<Option<std::process::Child>>,
    grace: Duration,
) -> Result<bool, String> {
    let child = process.lock().map_err(|e| e.to_string())?.take();
    let Some(mut child) = child else {
        return Ok(false);
    };
    #[cfg(windows)]
    {
        let _ = grace;
        // Child keeps the Windows process handle, so this cannot hit a PID
        // reuse replacement the way a later `taskkill /PID` subprocess could.
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            child.kill().map_err(|e| e.to_string())?;
        }
    }
    #[cfg(unix)]
    {
        // std::process::Child::kill is SIGKILL on Unix. Give Revka a normal
        // shutdown first so stop, update, and app exit do not always wait out
        // the grace period and then force-kill it.
        // SAFETY: this PID belongs to the live Child handle we just removed
        // from Desktop state; ESRCH is harmless and handled by try_wait.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(150));
                }
                _ => {
                    if let Err(error) = child.kill() {
                        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                            return Err(format!("could not stop Revka: {error}"));
                        }
                    }
                    break;
                }
            }
        }
    }
    child.wait().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Stop only a Revka daemon represented by this process's live Child handle.
/// A record from a previous Desktop session is useful for status, but cannot
/// make a later PID signal race-free; fail closed instead of risking another
/// Revka command or a PID-reuse replacement.
/// Caller must hold [`lifecycle_lock`].
fn stop_revka(state: &AppState) -> Result<(), String> {
    let stopped_tracked = terminate_tracked(&state.revka, STOP_GRACE)?;
    if stopped_tracked {
        if wait_for_port_closed(Duration::from_secs(3)) {
            clear_runtime_stamp();
            return Ok(());
        }
        return Err(
            "Revka's tracked process exited, but 127.0.0.1:42617 is still occupied; stop that specific process manually and try again".into(),
        );
    }

    match validated_runtime_stamp() {
        Ok(Some(stamp)) => Err(format!(
            "Revka from a previous Desktop session is still running (PID {}). Desktop will not signal a cross-session PID; stop that specific process manually and try again",
            stamp.pid
        )),
        Ok(None) => {
            if wait_for_port_closed(Duration::from_millis(400)) {
                clear_runtime_stamp();
                Ok(())
            } else {
                Err(
                    "127.0.0.1:42617 is held by a process Desktop did not start; stop that specific process manually and try again".into(),
                )
            }
        }
        Err(error) => {
            if wait_for_port_closed(Duration::from_millis(400)) {
                clear_runtime_stamp();
                Ok(())
            } else {
                Err(format!(
                    "Revka on 127.0.0.1:42617 could not be matched to Desktop's process record ({error}); stop that specific process manually and try again"
                ))
            }
        }
    }
}

/// Serializes every mutation of Revka's lifecycle — stop, binary swap, spawn.
fn lifecycle_lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, ()>, String> {
    state.revka_start.lock().map_err(|e| e.to_string())
}

/// Caller must hold [`lifecycle_lock`].
fn start_revka(state: &AppState) -> Result<String, String> {
    if !onboarding_complete() {
        return Err(
            "Revka setup is required — open Revka in Kumiho Desktop and run onboarding first"
                .into(),
        );
    }
    let (reachable, serving) = runtime_state();
    if reachable && !is_stale(installed_version().as_deref(), true, serving.as_deref()) {
        return Ok("Revka already serving on 42617".into());
    }

    {
        let mut tracked = state.revka.lock().map_err(|e| e.to_string())?;
        match tracked.as_mut().map(std::process::Child::try_wait) {
            Some(Ok(None)) if !reachable => return Ok("Revka is already starting on 42617".into()),
            Some(Ok(None)) => {}
            Some(_) => *tracked = None,
            None => {}
        }
    }

    if reachable {
        stop_revka(state)?;
    } else if runtime_stamp_path().is_some_and(|path| path.exists()) {
        // A prior Desktop may have exited while its daemon was still starting.
        // A validated cross-session process is reported for manual recovery;
        // a legacy/corrupt record is discarded only while the port is free.
        if !wait_for_port_closed(Duration::from_millis(400)) {
            return Err(
                "127.0.0.1:42617 is occupied, but the saved Revka process record cannot be verified; stop that specific process manually and try again".into(),
            );
        }
        match validated_runtime_stamp() {
            Ok(Some(_)) => stop_revka(state)?,
            Ok(None) => {}
            Err(_) => clear_runtime_stamp(),
        }
    }

    let addr: SocketAddr = ([127, 0, 0, 1], REVKA_PORT).into();
    if TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok() {
        return Err("port 42617 is occupied by a process that is not Revka".into());
    }
    let binary = installed_binary().ok_or("Revka is not installed yet")?;
    let root = install_root().ok_or("no home directory")?;
    fs::create_dir_all(root.join("logs")).map_err(|e| e.to_string())?;

    let log_path = root.join("logs").join("revka.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| e.to_string())?;
    let stderr = stdout.try_clone().map_err(|e| e.to_string())?;

    let child = command(binary.to_str().ok_or("invalid Revka install path")?)
        .args([
            "daemon",
            "--host",
            "127.0.0.1",
            "--port",
            &REVKA_PORT.to_string(),
        ])
        .current_dir(&root)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut child = child;
    if let Err(error) = write_runtime_stamp(child.id(), &installed_version().unwrap_or_default()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    *state.revka.lock().map_err(|e| e.to_string())? = Some(child);
    Ok(format!("Revka starting on {REVKA_PORT}"))
}

/// App shutdown: retire the daemon we own so it does not hold the binary lock
/// the next session's update has to fight.
pub fn kill_tracked_revka(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    if matches!(terminate_tracked(&state.revka, EXIT_GRACE), Ok(true)) {
        clear_runtime_stamp();
    }
}

#[derive(Serialize)]
pub struct RevkaStatus {
    pub reachable: bool,
    pub port: u16,
    /// `true` when the binary comes from ~/.kumiho/apps/revka rather than a
    /// pre-existing standalone install.
    pub managed: bool,
    pub installed: bool,
    /// Installation and setup are intentionally separate. A default
    /// config.toml can exist before the interactive wizard has ever run.
    pub onboarded: bool,
    pub version: Option<String>,
    /// The build answering on 42617 according to our stamp — None for daemons
    /// Desktop did not spawn.
    pub serving_version: Option<String>,
    pub stale: bool,
}

#[derive(Serialize)]
pub struct RevkaUpdateInfo {
    pub installed_version: Option<String>,
    pub latest_version: String,
    pub update_available: bool,
}

#[tauri::command]
pub fn revka_status() -> RevkaStatus {
    let version = installed_version();
    let (reachable, serving_version) = runtime_state();
    RevkaStatus {
        reachable,
        port: REVKA_PORT,
        managed: managed_binary().is_some(),
        installed: installed_binary().is_some(),
        onboarded: onboarding_complete(),
        stale: is_stale(version.as_deref(), reachable, serving_version.as_deref()),
        serving_version,
        version,
    }
}

#[tauri::command]
pub fn revka_check_update() -> Result<RevkaUpdateInfo, String> {
    let installed_version = installed_version();
    let (latest_version, _) = fetch_latest_release()?;
    let update_available = installed_version
        .as_deref()
        .map(|installed| newer_version_available(installed, &latest_version))
        .unwrap_or(true);
    Ok(RevkaUpdateInfo {
        installed_version,
        latest_version,
        update_available,
    })
}

/// Download the latest prebuilt Revka, verify it against the release
/// checksums, swap it in, and bring the daemon back up. Used for both first
/// installs and updates — there is no bundled payload to distinguish.
#[tauri::command]
pub fn revka_install(state: State<AppState>) -> Result<String, String> {
    let app: &AppState = &state;
    // Fetch, download, verify, and extract with NO locks held: a slow or
    // failed download must never cost the user their running daemon (the
    // old build keeps serving until the swap section below).
    let (latest_version, release) = fetch_latest_release()?;
    let bytes = download_release(&release)?;
    let root = install_root().ok_or("no home directory")?;
    fs::create_dir_all(root.join("logs")).map_err(|e| e.to_string())?;
    let staging = root.join(format!(
        ".update-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let result = (|| {
        let staged_binary = extract_binary(&bytes, &staging)?;
        // ONE lock section covers stop → swap → start, so no start can slip
        // between the stop and the activate.
        let _lifecycle = lifecycle_lock(app)?;
        stop_revka(app)?;
        replace_runtime(&root, &staged_binary, &latest_version)?;
        Ok(if onboarding_complete() {
            match start_revka(app) {
                Ok(_) => format!("Revka {latest_version} installed and restarted"),
                Err(error) => {
                    format!("Revka {latest_version} installed, but it did not restart: {error}")
                }
            }
        } else {
            format!("Revka {latest_version} installed — setup required")
        })
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[tauri::command]
pub fn revka_start(state: State<AppState>) -> Result<String, String> {
    let _lifecycle = lifecycle_lock(&state)?;
    start_revka(&state)
}

#[tauri::command]
pub fn revka_stop(state: State<AppState>) -> Result<String, String> {
    let _lifecycle = lifecycle_lock(&state)?;
    stop_revka(&state)?;
    Ok("Revka stopped".into())
}

/// The UI calls this only after the directly-spawned wizard exits successfully.
/// Revka's provider-only path also exits zero, so require the full workspace
/// scaffold before Desktop advances to daemon startup and dashboard pairing.
#[tauri::command]
pub fn revka_onboard_complete() -> Result<(), String> {
    let (config, workspace) = revka_runtime_dirs().ok_or("no home directory")?;
    require_full_onboarding_at(&config, &workspace)
}

/// Fetch the current active code, issuing one only when pairing is enabled and
/// no code exists. This keeps a still-valid one-time code stable across UI
/// retries instead of rotating it on every click.
#[tauri::command]
pub fn revka_pairing_prepare() -> Result<RevkaPairingInfo, String> {
    if !onboarding_complete() {
        return Err("Finish Revka onboarding before pairing the dashboard".into());
    }
    let current = pairing_request(false)?;
    if !current.pairing_required || current.pairing_code.is_some() {
        return Ok(current);
    }
    pairing_request(true)
}

/// Explicit user action: replace any active one-time code with a fresh code.
#[tauri::command]
pub fn revka_pairing_new() -> Result<RevkaPairingInfo, String> {
    if !onboarding_complete() {
        return Err("Finish Revka onboarding before pairing the dashboard".into());
    }
    pairing_request(true)
}

#[derive(Serialize)]
pub struct OnboardStarted {
    /// Human-readable process label shown in the terminal header.
    pub shell: String,
}

/// Open the embedded onboarding terminal with Revka itself as the PTY child.
/// The wizard is fully interactive and its actual process exit reaches the UI.
#[tauri::command]
pub fn revka_onboard_start(
    state: State<AppState>,
    on_data: tauri::ipc::Channel<crate::pty::PtyEvent>,
) -> Result<OnboardStarted, String> {
    let binary = installed_binary().ok_or("Revka is not installed yet")?;
    crate::pty::spawn_command_session(&state, &binary, &["onboard"], on_data)?;
    Ok(OnboardStarted {
        shell: "Revka CLI".into(),
    })
}

#[tauri::command]
pub fn revka_pty_write(state: State<AppState>, data: String) -> Result<(), String> {
    crate::pty::write_input(&state, &data)
}

#[tauri::command]
pub fn revka_pty_resize(state: State<AppState>, rows: u16, cols: u16) -> Result<(), String> {
    crate::pty::resize(&state, rows, cols)
}

#[tauri::command]
pub fn revka_pty_stop(state: State<AppState>) -> Result<(), String> {
    crate::pty::stop_session(&state)
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    use super::extract_binary;
    use super::{
        checksum_for, health_response_ok, installed_version_at, is_stale, newer_version_available,
        onboarding_artifacts_complete_at, parse_pairing_response, parse_release,
        release_asset_name, replace_runtime, require_full_onboarding_at, runtime_identity_matches,
        tag_version, validate_pairing_http,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_release(asset: &str) -> String {
        format!(
            r#"{{"tag_name":"v2026.6.30","assets":[
                {{"name":"install.sh","browser_download_url":"https://github.com/KumihoIO/Revka/releases/download/v2026.6.30/install.sh"}},
                {{"name":"{asset}","browser_download_url":"https://github.com/KumihoIO/Revka/releases/download/v2026.6.30/{asset}"}},
                {{"name":"SHA256SUMS","browser_download_url":"https://github.com/KumihoIO/Revka/releases/download/v2026.6.30/SHA256SUMS"}}]}}"#
        )
    }

    #[test]
    fn every_platform_maps_to_a_published_style_asset() {
        assert_eq!(
            release_asset_name(),
            if cfg!(target_os = "windows") {
                "revka-x86_64-pc-windows-msvc.zip"
            } else if cfg!(target_os = "macos") {
                if cfg!(target_arch = "aarch64") {
                    "revka-aarch64-apple-darwin.tar.gz"
                } else {
                    "revka-x86_64-apple-darwin.tar.gz"
                }
            } else if cfg!(target_arch = "aarch64") {
                "revka-aarch64-unknown-linux-gnu.tar.gz"
            } else {
                "revka-x86_64-unknown-linux-gnu.tar.gz"
            }
        );
    }

    #[test]
    fn release_feed_selects_this_platforms_archive() {
        let (version, release) =
            parse_release(&sample_release(release_asset_name())).expect("valid release");
        assert_eq!(version, "2026.6.30");
        assert!(release
            .assets
            .iter()
            .any(|asset| asset.name == release_asset_name()));
    }

    #[test]
    fn missing_platform_archive_fails_closed() {
        let (version, _) =
            parse_release(&sample_release(release_asset_name())).expect("valid release");
        assert_eq!(version, "2026.6.30");
        assert!(parse_release(&sample_release("revka-s390x-unknown-linux-gnu.tar.gz")).is_err());
    }

    #[test]
    fn tags_strip_the_v_prefix_and_reject_garbage() {
        assert_eq!(tag_version("v2026.6.30"), Ok("2026.6.30".into()));
        assert_eq!(tag_version("2026.6.30"), Ok("2026.6.30".into()));
        assert!(tag_version("").is_err());
        assert!(tag_version("vabc").is_err());
        assert!(tag_version("v2026").is_err());
    }

    #[test]
    fn sums_lookup_handles_both_text_and_binary_markers() {
        let sums = "3c2aee5a59f975a96035c2d05f0f2d4d90096a1cd403dc1d77fa6044742f3fd2  revka-x86_64-pc-windows-msvc.zip\n\
                    77a444267ba952b8385d023f6ee3e11a53704e9ddad9663edf386bebaaf2325f *revka-x86_64-unknown-linux-gnu.tar.gz\n";
        assert_eq!(
            checksum_for(sums, "revka-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("3c2aee5a59f975a96035c2d05f0f2d4d90096a1cd403dc1d77fa6044742f3fd2")
        );
        assert_eq!(
            checksum_for(sums, "revka-x86_64-unknown-linux-gnu.tar.gz").as_deref(),
            Some("77a444267ba952b8385d023f6ee3e11a53704e9ddad9663edf386bebaaf2325f")
        );
        assert_eq!(checksum_for(sums, "install.sh"), None);
        assert_eq!(checksum_for("", "anything"), None);
    }

    #[test]
    fn calver_comparison_understands_newer_releases() {
        assert!(newer_version_available("2026.6.30", "2027.1.5"));
        assert!(!newer_version_available("2026.6.30", "2026.6.30"));
        assert!(!newer_version_available("2026.12.31", "2026.6.30"));
    }

    #[test]
    fn a_daemon_serving_a_different_build_than_installed_is_stale() {
        assert!(is_stale(Some("2026.6.30"), true, Some("2026.1.1")));
        assert!(!is_stale(Some("2026.6.30"), true, Some("2026.6.30")));
        assert!(!is_stale(Some("2026.6.30"), false, None));
        // Unknown serving version cannot be proven current, so restart it.
        assert!(is_stale(Some("2026.6.30"), true, None));
        assert!(!is_stale(None, true, None));
    }

    #[test]
    fn runtime_identity_must_match_the_recorded_revka_process_exactly() {
        let expected = if cfg!(windows) {
            "638921234567890000|revka"
        } else {
            "Sun Aug 31 08:00:00 2026 revka"
        };
        assert!(runtime_identity_matches(expected, Some(expected)));
        assert!(!runtime_identity_matches(
            expected,
            Some(if cfg!(windows) {
                "638921234567890001|revka"
            } else {
                "Sun Aug 31 08:00:01 2026 revka"
            })
        ));
        assert!(!runtime_identity_matches(
            if cfg!(windows) {
                "638921234567890000|notepad"
            } else {
                "Sun Aug 31 08:00:00 2026 sleep"
            },
            Some(if cfg!(windows) {
                "638921234567890000|notepad"
            } else {
                "Sun Aug 31 08:00:00 2026 sleep"
            })
        ));
    }

    #[test]
    fn accepts_a_200_health_response_only_with_an_ok_body() {
        assert!(health_response_ok(
            b"HTTP/1.1 200 OK\r\ncontent-length: 15\r\n\r\n{\"status\":\"ok\"}"
        ));
        // 200 with a foreign body: not Revka, don't count it as reachable.
        assert!(!health_response_ok(
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}"
        ));
        assert!(!health_response_ok(
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 2\r\n\r\n{}"
        ));
        assert!(!health_response_ok(b"HTTP/1.1 200 OK\r\n")); // headers incomplete
    }

    #[test]
    fn a_default_config_without_the_onboarding_scaffold_is_not_ready() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kumiho-desktop-revka-onboarding-{}-{unique}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::write(root.join("config.toml"), b"default_provider = 'openrouter'")
            .expect("write config");
        fs::write(workspace.join("IDENTITY.md"), b"identity").expect("write identity");
        fs::write(workspace.join("SOUL.md"), b"soul").expect("write soul");
        assert!(!onboarding_artifacts_complete_at(&root, &workspace));
        assert!(require_full_onboarding_at(&root, &workspace).is_err());

        for name in ["AGENTS.md", "USER.md", "TOOLS.md"] {
            fs::write(workspace.join(name), name.as_bytes()).expect("write scaffold");
        }
        assert!(onboarding_artifacts_complete_at(&root, &workspace));
        assert!(require_full_onboarding_at(&root, &workspace).is_ok());
        fs::remove_dir_all(root).expect("remove test workspace");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_release_archive_extracts_the_revka_binary() {
        use flate2::{write::GzEncoder, Compression};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kumiho-desktop-revka-archive-{}-{unique}",
            std::process::id()
        ));
        let staging = root.join("staging");
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let payload = b"revka-test-binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "nested/revka", payload.as_slice())
            .expect("append test binary");
        let encoder = builder.into_inner().expect("finish tar archive");
        let archive = encoder.finish().expect("finish gzip archive");

        let extracted = extract_binary(&archive, &staging).expect("extract Revka binary");
        assert_eq!(fs::read(extracted).expect("read extracted binary"), payload);
        fs::remove_dir_all(root).expect("remove test archive");
    }

    #[test]
    fn pairing_response_accepts_only_six_numeric_digits() {
        let info = parse_pairing_response(
            r#"{"success":true,"pairing_required":true,"pairing_code":"481205","message":"ready"}"#,
        )
        .expect("valid pairing response");
        assert!(info.pairing_required);
        assert_eq!(info.pairing_code.as_deref(), Some("481205"));
        assert!(parse_pairing_response(
            r#"{"success":true,"pairing_required":true,"pairing_code":"abc123","message":"bad"}"#
        )
        .is_err());
        assert!(parse_pairing_response(
            r#"{"success":true,"pairing_required":true,"pairing_code":"12345","message":"bad"}"#
        )
        .is_err());
    }

    #[test]
    fn pairing_disabled_is_a_valid_code_free_state() {
        let info = parse_pairing_response(
            r#"{"success":false,"pairing_required":false,"pairing_code":null,"message":"disabled"}"#,
        )
        .expect("pairing disabled response");
        assert!(!info.pairing_required);
        assert_eq!(info.pairing_code, None);
        assert!(validate_pairing_http(400, true, info).is_ok());

        let invalid_failure = parse_pairing_response(
            r#"{"success":false,"pairing_required":false,"pairing_code":null,"message":"failure"}"#,
        )
        .expect("structured failure response");
        assert!(validate_pairing_http(500, true, invalid_failure).is_err());

        let missing_code = parse_pairing_response(
            r#"{"success":true,"pairing_required":true,"pairing_code":null,"message":"missing"}"#,
        )
        .expect("structured missing-code response");
        assert!(validate_pairing_http(200, true, missing_code).is_err());
    }

    #[test]
    fn tags_allow_prerelease_suffixes_but_reject_garbage() {
        assert_eq!(tag_version("v2026.7.1-rc1"), Ok("2026.7.1-rc1".into()));
        assert!(tag_version("v2026..30").is_err());
        assert!(tag_version("v2026/30").is_err());
    }

    #[test]
    fn runtime_replacement_updates_binary_and_manifest_together() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kumiho-desktop-revka-replace-{}-{unique}",
            std::process::id()
        ));
        let bin = root.join("bin");
        let fs_bin = bin.join(if cfg!(windows) { "revka.exe" } else { "revka" });
        fs::create_dir_all(&bin).expect("create bin");
        fs::write(&fs_bin, b"old").expect("write old binary");
        write_manifest_helper(&root, "2026.1.1");

        let staged = root.join("staging");
        fs::create_dir_all(&staged).expect("create staging");
        fs::write(staged.join(fs_bin.file_name().unwrap()), b"new").expect("write staged binary");

        replace_runtime(
            &root,
            &staged.join(fs_bin.file_name().unwrap()),
            "2026.6.30",
        )
        .expect("replace runtime");

        assert_eq!(fs::read(&fs_bin).expect("read binary"), b"new");
        assert_eq!(installed_version_at(&root).as_deref(), Some("2026.6.30"));
        fs::remove_dir_all(root).expect("remove test install root");
    }

    fn write_manifest_helper(root: &std::path::Path, version: &str) {
        fs::write(
            root.join("manifest.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .expect("write old manifest");
    }
}
