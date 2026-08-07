//! PyPI-owned Kumiho Memory engine lifecycle.
//!
//! Host plugins are adapters (managed in `connect`). Their Python engines live
//! in host-owned virtual environments and are all upgraded from the canonical
//! `kumiho-memory` PyPI release without touching a global Python installation.

use crate::util::command;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const PYPI_URL: &str = "https://pypi.org/pypi/kumiho-memory/json";
const LOCK_STALE_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Serialize)]
pub struct MemoryEnvironment {
    pub id: String,
    pub name: String,
    pub python: String,
    pub version: String,
    pub sdk_version: Option<String>,
}

#[derive(Serialize)]
pub struct MemoryStatus {
    pub environments: Vec<MemoryEnvironment>,
    pub latest_version: Option<String>,
    pub update_available: bool,
}

#[derive(Serialize)]
pub struct MemoryUpdateResult {
    pub status: MemoryStatus,
    pub updated: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Deserialize)]
struct Probe {
    memory: String,
    kumiho: Option<String>,
}

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn candidate_venvs(home: &Path, local_app_data: Option<&Path>) -> Vec<(String, String, PathBuf)> {
    let mut candidates = vec![
        (
            "claude".into(),
            "Claude Code".into(),
            home.join(".claude")
                .join("plugins")
                .join("data")
                .join("kumiho-memory-kumiho-plugins")
                .join("venv"),
        ),
        (
            "codex".into(),
            "ChatGPT / Codex".into(),
            home.join(".codex")
                .join("plugins")
                .join("data")
                .join("kumiho-memory-kumiho-plugins")
                .join("venv"),
        ),
        (
            "openclaw".into(),
            "OpenClaw / shared".into(),
            home.join(".kumiho").join("venv"),
        ),
    ];
    if cfg!(windows) {
        if let Some(local) = local_app_data {
            candidates.push((
                "legacy-claude".into(),
                "Claude legacy".into(),
                local.join("kumiho-claude").join("venv"),
            ));
        }
    } else {
        candidates.push((
            "legacy-claude".into(),
            "Claude legacy".into(),
            home.join(".cache").join("kumiho-claude").join("venv"),
        ));
    }
    candidates
}

fn probe_environment(id: String, name: String, venv: PathBuf) -> Option<MemoryEnvironment> {
    let python = venv_python(&venv);
    if !python.exists() {
        return None;
    }
    let script = concat!(
        "import importlib.metadata as m,json; ",
        "v=lambda n: m.version(n) if any(d.metadata.get('Name','').lower()==n for d in m.distributions()) else None; ",
        "print(json.dumps({'memory':v('kumiho-memory'),'kumiho':v('kumiho')}))"
    );
    let output = command(python.to_string_lossy().as_ref())
        .args(["-c", script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let probe: Probe = serde_json::from_slice(&output.stdout).ok()?;
    if probe.memory.is_empty() {
        return None;
    }
    Some(MemoryEnvironment {
        id,
        name,
        python: python.to_string_lossy().to_string(),
        version: probe.memory,
        sdk_version: probe.kumiho,
    })
}

fn environments() -> Vec<MemoryEnvironment> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let local = dirs::data_local_dir();
    let mut seen = HashSet::new();
    candidate_venvs(&home, local.as_deref())
        .into_iter()
        .filter_map(|(id, name, venv)| {
            let python = venv_python(&venv);
            let key = fs::canonicalize(&python).unwrap_or(python);
            seen.insert(key)
                .then(|| probe_environment(id, name, venv))?
        })
        .collect()
}

fn latest_version() -> Result<String, String> {
    let response = ureq::get(PYPI_URL)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("could not check PyPI: {e}"))?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let version = json
        .get("info")
        .and_then(|info| info.get("version"))
        .and_then(serde_json::Value::as_str)
        .ok_or("PyPI response has no kumiho-memory version")?;
    semver::Version::parse(version).map_err(|e| format!("invalid PyPI version: {e}"))?;
    Ok(version.to_owned())
}

fn newer(installed: &str, latest: &str) -> bool {
    match (
        semver::Version::parse(installed),
        semver::Version::parse(latest),
    ) {
        (Ok(installed), Ok(latest)) => latest > installed,
        _ => installed != latest,
    }
}

fn status(latest: Option<String>) -> MemoryStatus {
    let environments = environments();
    let update_available = latest.as_deref().is_some_and(|latest| {
        environments
            .iter()
            .any(|environment| newer(&environment.version, latest))
    });
    MemoryStatus {
        environments,
        latest_version: latest,
        update_available,
    }
}

#[tauri::command]
pub fn memory_status() -> MemoryStatus {
    status(None)
}

#[tauri::command]
pub fn memory_check_update() -> Result<MemoryStatus, String> {
    Ok(status(Some(latest_version()?)))
}

struct ProvisionLock {
    path: PathBuf,
}

impl ProvisionLock {
    fn acquire(python: &Path) -> Result<Self, String> {
        let venv = python
            .parent()
            .and_then(Path::parent)
            .ok_or("invalid venv path")?;
        let path = venv
            .parent()
            .ok_or("invalid venv parent")?
            .join("provision.lock");
        if path.exists() {
            let stale = path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > LOCK_STALE_AFTER);
            if stale {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
            } else {
                return Err("another process is provisioning this Memory environment".into());
            }
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                std::io::Write::write_all(&mut file, std::process::id().to_string().as_bytes())
            })
            .map_err(|e| format!("could not lock the Memory environment: {e}"))?;
        Ok(Self { path })
    }
}

impl Drop for ProvisionLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn update_environment(environment: &MemoryEnvironment) -> Result<(), String> {
    let python = PathBuf::from(&environment.python);
    let _lock = ProvisionLock::acquire(&python)?;
    let output = command(&environment.python)
        .args([
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--upgrade",
            "kumiho[mcp]",
            "kumiho-memory[all]",
        ])
        .output()
        .map_err(|e| format!("could not start pip: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if error.is_empty() {
            format!("pip exited with {}", output.status)
        } else {
            error
        })
    }
}

#[tauri::command]
pub fn memory_update() -> Result<MemoryUpdateResult, String> {
    let latest = latest_version()?;
    let before = environments();
    if before.is_empty() {
        return Err("no host-owned Kumiho Memory Python environment was found".into());
    }
    let mut updated = Vec::new();
    let mut errors = Vec::new();
    for environment in before
        .iter()
        .filter(|environment| newer(&environment.version, &latest))
    {
        match update_environment(environment) {
            Ok(()) => updated.push(environment.name.clone()),
            Err(error) => errors.push(format!("{}: {error}", environment.name)),
        }
    }
    let after = status(Some(latest));
    Ok(MemoryUpdateResult {
        status: after,
        updated,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::{candidate_venvs, memory_check_update, newer};
    use std::path::Path;

    #[test]
    fn discovers_host_owned_venvs_without_global_python() {
        let candidates = candidate_venvs(Path::new("/home/test"), None);
        assert!(candidates.iter().any(|(id, _, path)| {
            id == "claude" && path.ends_with("kumiho-memory-kumiho-plugins/venv")
        }));
        assert!(candidates
            .iter()
            .any(|(id, _, path)| id == "openclaw" && path.ends_with(".kumiho/venv")));
    }

    #[test]
    fn pypi_update_is_strictly_newer() {
        assert!(newer("1.2.0", "1.2.1"));
        assert!(!newer("1.2.1", "1.2.1"));
        assert!(!newer("1.3.0", "1.2.1"));
    }

    #[test]
    #[ignore = "requires PyPI and a provisioned host environment"]
    fn live_pypi_status_reports_installed_engine_versions() {
        let status = memory_check_update().expect("check PyPI");
        assert!(status.latest_version.is_some());
        assert!(!status.environments.is_empty());
        assert!(status
            .environments
            .iter()
            .all(|environment| semver::Version::parse(&environment.version).is_ok()));
    }
}
