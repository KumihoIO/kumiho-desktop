//! Connect pillar — install and update the Kumiho Memory plugin in each host.
//! The host CLI owns its plugin files; Desktop only queries the CLI's JSON
//! status and invokes the host's supported install/update flow.

use crate::util::command;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

const MARKETPLACE_URL: &str =
    "https://raw.githubusercontent.com/KumihoIO/kumiho-plugins/main/.claude-plugin/marketplace.json";
const OPENCLAW_LATEST_URL: &str = "https://registry.npmjs.org/@kumiho%2Fopenclaw-kumiho/latest";

#[derive(Serialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    /// The host CLI itself is available on PATH.
    pub installed: bool,
    pub plugin_installed: bool,
    pub version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub error: Option<String>,
}

fn on_path(cmd: &str) -> bool {
    let probe = if cfg!(windows) { "where" } else { "which" };
    command(probe)
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn command_json(cmd: &str, args: &[&str]) -> Result<Value, String> {
    let output = command(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch `{cmd}`: {e}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if error.is_empty() {
            format!("`{cmd}` exited with {}", output.status)
        } else {
            error
        });
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("`{cmd}` returned invalid JSON: {e}"))
}

fn plugin_entries(value: &Value) -> Option<&Vec<Value>> {
    value.as_array().or_else(|| {
        value
            .get("installed")
            .or_else(|| value.get("plugins"))
            .and_then(Value::as_array)
    })
}

fn plugin_version_from_json(value: &Value, ids: &[&str]) -> Option<String> {
    plugin_entries(value)?.iter().find_map(|entry| {
        let matches = ["id", "pluginId", "name", "package"]
            .iter()
            .filter_map(|key| entry.get(key).and_then(Value::as_str))
            .any(|candidate| ids.iter().any(|id| candidate == *id));
        matches
            .then(|| entry.get("version")?.as_str().map(str::to_owned))
            .flatten()
    })
}

fn installed_plugin_version(host: &str) -> Result<Option<String>, String> {
    match host {
        "claude" => command_json("claude", &["plugin", "list", "--json"]).map(|json| {
            plugin_version_from_json(&json, &["kumiho-memory@kumiho-plugins", "kumiho-memory"])
        }),
        "codex" => command_json("codex", &["plugin", "list", "--json"]).map(|json| {
            plugin_version_from_json(&json, &["kumiho-memory@kumiho-plugins", "kumiho-memory"])
        }),
        "openclaw" => command_json("openclaw", &["plugins", "list", "--json"]).map(|json| {
            plugin_version_from_json(
                &json,
                &[
                    "@kumiho/openclaw-kumiho",
                    "openclaw-kumiho",
                    "kumiho-memory",
                ],
            )
        }),
        other => Err(format!("unknown host: {other}")),
    }
}

fn fetch_json(url: &str) -> Result<Value, String> {
    let response = ureq::get(url)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| e.to_string())?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn marketplace_latest_version() -> Result<String, String> {
    let json = fetch_json(MARKETPLACE_URL)?;
    json.get("plugins")
        .and_then(Value::as_array)
        .and_then(|plugins| {
            plugins
                .iter()
                .find(|plugin| plugin.get("name").and_then(Value::as_str) == Some("kumiho-memory"))
        })
        .and_then(|plugin| plugin.get("version"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("official marketplace has no kumiho-memory version".into())
}

fn openclaw_latest_version() -> Result<String, String> {
    fetch_json(OPENCLAW_LATEST_URL)?
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("npm registry has no @kumiho/openclaw-kumiho version".into())
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

fn host_status(id: &str, name: &str, latest: Option<&Result<String, String>>) -> Host {
    let cli_installed = on_path(id);
    let (version, mut error) = if cli_installed {
        match installed_plugin_version(id) {
            Ok(version) => (version, None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, None)
    };
    let latest_version = match latest {
        Some(Ok(version)) => Some(version.clone()),
        Some(Err(latest_error)) => {
            error = Some(latest_error.clone());
            None
        }
        None => None,
    };
    let update_available = version
        .as_deref()
        .zip(latest_version.as_deref())
        .is_some_and(|(installed, latest)| newer(installed, latest));
    Host {
        id: id.into(),
        name: name.into(),
        installed: cli_installed,
        plugin_installed: version.is_some(),
        version,
        latest_version,
        update_available,
        error,
    }
}

fn hosts(check_latest: bool) -> Vec<Host> {
    let marketplace = check_latest.then(marketplace_latest_version);
    let openclaw = check_latest.then(openclaw_latest_version);
    vec![
        host_status("claude", "Claude Code", marketplace.as_ref()),
        host_status("codex", "ChatGPT / Codex", marketplace.as_ref()),
        host_status("openclaw", "OpenClaw", openclaw.as_ref()),
    ]
}

#[tauri::command]
pub fn connect_hosts() -> Vec<Host> {
    hosts(false)
}

#[tauri::command]
pub fn connect_check_updates() -> Vec<Host> {
    hosts(true)
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = command(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch `{cmd}`: {e}"))?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if stdout.is_empty() {
            format!("{cmd} completed")
        } else {
            stdout
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("`{cmd}` exited with {}", output.status)
        } else {
            stderr
        })
    }
}

fn marketplace_add(cmd: &str) -> Result<(), String> {
    match run(
        cmd,
        &["plugin", "marketplace", "add", "KumihoIO/kumiho-plugins"],
    ) {
        Ok(_) => Ok(()),
        Err(error)
            if error.to_ascii_lowercase().contains("already")
                || error.to_ascii_lowercase().contains("exist") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[tauri::command]
pub fn connect_install(host: String) -> Result<String, String> {
    match host.as_str() {
        "claude" => {
            marketplace_add("claude")?;
            run(
                "claude",
                &["plugin", "install", "kumiho-memory@kumiho-plugins"],
            )
        }
        "codex" => {
            marketplace_add("codex")?;
            run("codex", &["plugin", "add", "kumiho-memory@kumiho-plugins"])
        }
        "openclaw" => run(
            "openclaw",
            &["plugins", "install", "@kumiho/openclaw-kumiho"],
        ),
        other => Err(format!("unknown host: {other}")),
    }
}

#[tauri::command]
pub fn connect_update(host: String) -> Result<String, String> {
    if installed_plugin_version(&host)?.is_none() {
        return connect_install(host);
    }
    match host.as_str() {
        "claude" => {
            run(
                "claude",
                &["plugin", "marketplace", "update", "kumiho-plugins"],
            )?;
            run(
                "claude",
                &["plugin", "update", "kumiho-memory@kumiho-plugins"],
            )
        }
        "codex" => {
            run(
                "codex",
                &["plugin", "marketplace", "upgrade", "kumiho-plugins"],
            )?;
            // Codex has no plugin-update command. Re-adding an installed plugin
            // replaces its marketplace snapshot in place (verified on 0.144.5).
            run("codex", &["plugin", "add", "kumiho-memory@kumiho-plugins"])
        }
        "openclaw" => run(
            "openclaw",
            &["plugins", "install", "@kumiho/openclaw-kumiho"],
        ),
        other => Err(format!("unknown host: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{newer, plugin_version_from_json};

    #[test]
    fn reads_claude_and_codex_installed_versions() {
        let claude = serde_json::json!([
            {"id":"kumiho-memory@kumiho-plugins","version":"0.19.2"}
        ]);
        let codex = serde_json::json!({"installed":[
            {"pluginId":"kumiho-memory@kumiho-plugins","version":"0.20.0"}
        ]});
        let ids = ["kumiho-memory@kumiho-plugins", "kumiho-memory"];
        assert_eq!(
            plugin_version_from_json(&claude, &ids).as_deref(),
            Some("0.19.2")
        );
        assert_eq!(
            plugin_version_from_json(&codex, &ids).as_deref(),
            Some("0.20.0")
        );
    }

    #[test]
    fn only_marks_a_strictly_newer_plugin_release() {
        assert!(newer("0.19.2", "0.20.0"));
        assert!(!newer("0.19.2", "0.19.2"));
        assert!(!newer("0.20.0", "0.19.2"));
    }
}
