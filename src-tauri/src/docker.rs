//! Docker pillar — manage the CE server's dependencies (Neo4j + Redis).
//!
//! We do NOT assume our own container names: an existing Neo4j/Redis container
//! (however it was created — `kumiho-neo4j`, plain `neo4j`, the community
//! compose's `kumiho-ce-neo4j`, …) is discovered and managed. A container is
//! only created when nothing suitable exists and the port is free.

use crate::util::{command, kumiho_home};
use serde::Serialize;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Candidate container names, most-specific first. The first entry is the one
/// we create ourselves when nothing exists.
const NEO4J_NAMES: &[&str] = &["kumiho-ce-neo4j", "kumiho-neo4j", "neo4j"];
const REDIS_NAMES: &[&str] = &["kumiho-ce-redis", "kumiho-redis", "redis"];
const NEO4J_DEFAULT: u16 = 7687;
const REDIS_DEFAULT: u16 = 6379;
const NEO4J_MIN_PASSWORD_LENGTH: usize = 8;

#[cfg(target_os = "macos")]
const DOCKER_FALLBACKS: &[&str] = &[
    "/usr/local/bin/docker",
    "/opt/homebrew/bin/docker",
    "/Applications/Docker.app/Contents/Resources/bin/docker",
];

#[cfg(not(target_os = "macos"))]
const DOCKER_FALLBACKS: &[&str] = &[];

fn docker_output_with<T>(
    args: &[&str],
    fallbacks: &[&str],
    mut output: impl FnMut(&str, &[&str]) -> io::Result<T>,
    is_success: impl Fn(&T) -> bool,
) -> io::Result<T> {
    let mut unsuccessful = None;
    let mut first_error = None;
    let mut substantive_error = None;
    let mut selected = None;
    let mut selected_probe = None;
    for program in std::iter::once("docker").chain(fallbacks.iter().copied()) {
        match output(program, &["--version"]) {
            Ok(value) if is_success(&value) => {
                selected = Some(program.to_string());
                selected_probe = Some(value);
                break;
            }
            Ok(value) => unsuccessful = Some(value),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(io::Error::new(error.kind(), error.to_string()));
                }
                if error.kind() != io::ErrorKind::NotFound {
                    substantive_error = Some(error);
                }
            }
        }
    }
    if let Some(program) = selected {
        if args == ["--version"] {
            return Ok(selected_probe.expect("a selected Docker CLI has a probe result"));
        }
        // Selection is read-only. Execute a state-changing command exactly once
        // on the selected runtime, even when it returns a non-zero status.
        return output(&program, args);
    }
    if let Some(value) = unsuccessful {
        Ok(value)
    } else {
        Err(substantive_error
            .or(first_error)
            .unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
    }
}

fn docker_output(args: &[&str]) -> io::Result<std::process::Output> {
    docker_output_with(
        args,
        DOCKER_FALLBACKS,
        |program, args| command(program).args(args).output(),
        |result| result.status.success(),
    )
}

fn neo4j_create_password_error(password: &str) -> Option<&'static str> {
    let length = password.trim().chars().count();
    if length == 0 {
        Some("a Neo4j password is required to create a new database")
    } else if length < NEO4J_MIN_PASSWORD_LENGTH {
        Some("Neo4j password must be at least 8 characters to create a new database")
    } else {
        None
    }
}

/// Is something already accepting connections on this loopback port?
fn port_serving(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

fn docker_available() -> bool {
    docker_output(&["--version"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Actionable, platform-aware guidance when Docker is needed but missing.
fn docker_missing_message() -> String {
    let how = if cfg!(target_os = "linux") {
        "install it — e.g. `curl -fsSL https://get.docker.com | sh`, then `sudo usermod -aG docker $USER` (re-login)"
    } else {
        "install Docker Desktop from https://www.docker.com/products/docker-desktop and start it"
    };
    format!(
        "Docker isn't installed or running — {how}. \
         Or run Neo4j 5.x (and optionally Redis) yourself; Kumiho reuses whatever is already listening on those ports."
    )
}

fn container_exists(name: &str) -> bool {
    docker_output(&["inspect", "-f", "{{.State.Status}}", name])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn container_running(name: &str) -> bool {
    docker_output(&["inspect", "-f", "{{.State.Running}}", name])
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// The first existing container among the candidates — so we manage the one you
/// already have instead of creating a duplicate under a different name.
fn find_container(cands: &[&str]) -> Option<String> {
    cands
        .iter()
        .find(|n| container_exists(n))
        .map(|n| (*n).to_string())
}

#[derive(Serialize)]
pub struct DockerStatus {
    pub available: bool,
    pub neo4j: bool,
    pub redis: bool,
}

#[tauri::command]
pub fn docker_status() -> DockerStatus {
    let available = docker_available();
    let running = |cands: &[&str], port: u16| {
        port_serving(port)
            || (available && find_container(cands).map(|n| container_running(&n)).unwrap_or(false))
    };
    DockerStatus {
        available,
        neo4j: running(NEO4J_NAMES, NEO4J_DEFAULT),
        redis: running(REDIS_NAMES, REDIS_DEFAULT),
    }
}

/// The Neo4j password saved at setup (`~/.kumiho/server.toml` `db_pass`), so
/// starting the databases never has to re-prompt for it.
fn stored_neo4j_password() -> Option<String> {
    let text = std::fs::read_to_string(kumiho_home()?.join("server.toml")).ok()?;
    for line in text.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "db_pass" {
            continue;
        }
        if let Some(value) = decode_saved_toml_string(raw_value) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Decode the two TOML escapes produced by `run::ce_configure` for saved
/// passwords. Keeping this inverse explicit avoids changing a password that
/// contains a quote or backslash when the databases are started again later.
fn decode_saved_toml_string(raw: &str) -> Option<String> {
    let inner = raw.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut decoded = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => decoded.push('\\'),
            '"' => decoded.push('"'),
            _ => return None,
        }
    }
    Some(decoded)
}

/// Bring up Neo4j (and optional Redis): reuse a served port, else start an
/// existing container, else create one. A password is only needed to CREATE —
/// and we fall back to the one saved at setup so the user isn't re-prompted.
#[tauri::command]
pub fn docker_up(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
) -> Result<String, String> {
    let neo4j_password = if neo4j_password.trim().is_empty() {
        stored_neo4j_password().unwrap_or_default()
    } else {
        neo4j_password
    };
    let need_neo4j = !port_serving(neo4j_port);
    let need_redis = use_redis && !port_serving(redis_port);

    if (need_neo4j || need_redis) && !docker_available() {
        return Err(docker_missing_message());
    }

    let mut notes: Vec<String> = Vec::new();

    if need_neo4j {
        let pw = neo4j_password.trim();
        let create = vec![
            "run".into(), "-d".into(),
            "--name".into(), NEO4J_NAMES[0].to_string(),
            "--restart".into(), "unless-stopped".into(),
            "-p".into(), format!("127.0.0.1:{neo4j_port}:7687"),
            "-e".into(), format!("NEO4J_AUTH=neo4j/{pw}"),
            "neo4j:5".into(),
        ];
        notes.push(start_or_create(
            NEO4J_NAMES,
            &create,
            neo4j_create_password_error(&neo4j_password),
            "Neo4j",
        )?);
    } else {
        notes.push(format!("Neo4j already serving {neo4j_port} — reusing"));
    }

    if use_redis {
        if need_redis {
            let create = vec![
                "run".into(), "-d".into(),
                "--name".into(), REDIS_NAMES[0].to_string(),
                "--restart".into(), "unless-stopped".into(),
                "-p".into(), format!("127.0.0.1:{redis_port}:6379"),
                "redis:7".into(),
            ];
            notes.push(start_or_create(REDIS_NAMES, &create, None, "Redis")?);
        } else {
            notes.push(format!("Redis already serving {redis_port} — reusing"));
        }
    }

    Ok(notes.join("; "))
}

/// Start an existing container (any known name), else create ours. `create_error`
/// explains why a new container cannot be created, such as a missing password.
fn start_or_create(
    cands: &[&str],
    create_args: &[String],
    create_error: Option<&str>,
    label: &str,
) -> Result<String, String> {
    if let Some(name) = find_container(cands) {
        let out =
            docker_output(&["start", &name]).map_err(|e| format!("docker not available: {e}"))?;
        if out.status.success() {
            return Ok(format!("{label} container '{name}' started"));
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "could not start '{name}': {err}. Kumiho kept the existing container and its data; reset it explicitly only if the data is disposable"
        ));
    }
    if let Some(error) = create_error {
        return Err(error.to_string());
    }
    let args: Vec<&str> = create_args.iter().map(|s| s.as_str()).collect();
    let out = docker_output(&args).map_err(|e| format!("docker not available: {e}"))?;
    if out.status.success() {
        Ok(format!("{label} container '{}' created", cands[0]))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[tauri::command]
pub fn docker_down() -> Result<String, String> {
    let mut stopped: Vec<String> = Vec::new();
    for cands in [NEO4J_NAMES, REDIS_NAMES] {
        if let Some(name) = find_container(cands) {
            let out = docker_output(&["stop", &name]);
            if out.map(|o| o.status.success()).unwrap_or(false) {
                stopped.push(name);
            }
        }
    }
    if stopped.is_empty() {
        Ok("no Kumiho database containers found to stop".into())
    } else {
        Ok(format!("stopped {}", stopped.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_on_process_path_takes_precedence_over_fallbacks() {
        let mut calls = Vec::new();
        let result = docker_output_with(
            &["--version"],
            &["/fallback/docker"],
            |program, args| {
                calls.push((program.to_string(), args.join(" ")));
                Ok("path docker")
            },
            |_| true,
        );

        assert_eq!(result.unwrap(), "path docker");
        assert_eq!(calls, [("docker".into(), "--version".into())]);
    }

    #[test]
    fn missing_path_docker_uses_macos_fallback_and_forwards_arguments() {
        let mut calls = Vec::new();
        let result = docker_output_with(
            &["inspect", "neo4j"],
            &["/usr/local/bin/docker"],
            |program, args| {
                calls.push((program.to_string(), args.join(" ")));
                if program == "docker" {
                    Err(io::Error::from(io::ErrorKind::NotFound))
                } else {
                    Ok("fallback docker")
                }
            },
            |_| true,
        );

        assert_eq!(result.unwrap(), "fallback docker");
        assert_eq!(
            calls,
            [
                ("docker".into(), "--version".into()),
                ("/usr/local/bin/docker".into(), "--version".into()),
                ("/usr/local/bin/docker".into(), "inspect neo4j".into()),
            ]
        );
    }

    #[test]
    fn missing_docker_checks_each_macos_fallback_in_order() {
        let fallbacks = [
            "/usr/local/bin/docker",
            "/opt/homebrew/bin/docker",
            "/Applications/Docker.app/Contents/Resources/bin/docker",
        ];
        let mut calls = Vec::new();
        let result = docker_output_with(
            &["--version"],
            &fallbacks,
            |program, args| {
                calls.push((program.to_string(), args.join(" ")));
                if program == fallbacks[2] {
                    Ok("Docker Desktop bundle")
                } else {
                    Err(io::Error::from(io::ErrorKind::NotFound))
                }
            },
            |_| true,
        );

        assert_eq!(result.unwrap(), "Docker Desktop bundle");
        assert_eq!(
            calls,
            [
                ("docker".into(), "--version".into()),
                (fallbacks[0].into(), "--version".into()),
                (fallbacks[1].into(), "--version".into()),
                (fallbacks[2].into(), "--version".into()),
            ]
        );
    }

    #[test]
    fn spawn_error_uses_the_next_known_docker_runtime() {
        let mut calls = Vec::new();
        let result = docker_output_with(
            &["--version"],
            &["/fallback/docker"],
            |program, _| {
                calls.push(program.to_string());
                if program == "docker" {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                } else {
                    Ok("fallback docker")
                }
            },
            |_| true,
        );

        assert_eq!(result.unwrap(), "fallback docker");
        assert_eq!(calls, ["docker", "/fallback/docker"]);
    }

    #[test]
    fn unsuccessful_cli_uses_the_next_known_docker_runtime() {
        let mut calls = Vec::new();
        let result = docker_output_with(
            &["--version"],
            &["/fallback/docker"],
            |program, _| {
                calls.push(program.to_string());
                Ok((program != "docker", program.to_string()))
            },
            |result| result.0,
        );

        assert_eq!(result.unwrap().1, "/fallback/docker");
        assert_eq!(calls, ["docker", "/fallback/docker"]);
    }

    #[test]
    fn state_changing_command_runs_once_on_the_selected_runtime() {
        let mut calls = Vec::new();
        let result = docker_output_with(
            &["start", "neo4j"],
            &["/fallback/docker", "/another/docker"],
            |program, args| {
                calls.push((program.to_string(), args.join(" ")));
                if args == ["--version"] {
                    Ok((program == "/fallback/docker", "probe"))
                } else {
                    Ok((false, "command failed"))
                }
            },
            |result| result.0,
        );

        assert_eq!(result.unwrap().1, "command failed");
        assert_eq!(
            calls,
            [
                ("docker".into(), "--version".into()),
                ("/fallback/docker".into(), "--version".into()),
                ("/fallback/docker".into(), "start neo4j".into()),
            ]
        );
    }

    #[test]
    fn new_neo4j_container_requires_an_eight_character_password() {
        assert!(neo4j_create_password_error("").is_some());
        assert!(neo4j_create_password_error("1234567").is_some());
        assert_eq!(neo4j_create_password_error("12345678"), None);
        assert_eq!(neo4j_create_password_error("abcd efgh"), None);
    }

    #[test]
    fn saved_neo4j_password_decodes_quotes_backslashes_and_spaces() {
        let logical = r#"pa\ss"word 한글"#;
        let encoded = format!("\"{}\"", crate::run::escape_toml_basic_string(logical));
        assert_eq!(
            decode_saved_toml_string(&encoded).as_deref(),
            Some(logical)
        );
        assert_eq!(
            decode_saved_toml_string(r#""eight chars""#).as_deref(),
            Some("eight chars")
        );
        assert_eq!(decode_saved_toml_string(r#""bad\nvalue""#), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_macos_fallbacks_include_cli_and_app_bundle_locations() {
        assert_eq!(
            DOCKER_FALLBACKS,
            [
                "/usr/local/bin/docker",
                "/opt/homebrew/bin/docker",
                "/Applications/Docker.app/Contents/Resources/bin/docker",
            ]
        );
    }
}
