//! Docker pillar — manage the CE server's dependencies (Neo4j + Redis).
//!
//! We do NOT assume our own container names: an existing Neo4j/Redis container
//! (however it was created — `kumiho-neo4j`, plain `neo4j`, the community
//! compose's `kumiho-ce-neo4j`, …) is discovered and managed. A container is
//! only created when nothing suitable exists and the port is free.

use crate::util::{command, kumiho_home};
use serde::Serialize;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::process::{Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Candidate container names, most-specific first. The first entry is the one
/// we create ourselves when nothing exists.
const NEO4J_NAMES: &[&str] = &["kumiho-ce-neo4j", "kumiho-neo4j", "neo4j"];
const REDIS_NAMES: &[&str] = &["kumiho-ce-redis", "kumiho-redis", "redis"];
const NEO4J_DEFAULT: u16 = 7687;
const REDIS_DEFAULT: u16 = 6379;
const NEO4J_MIN_PASSWORD_LENGTH: usize = 8;
const DOCKER_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const DOCKER_STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const DOCKER_ACTION_TIMEOUT: Duration = Duration::from_secs(60);
const DOCKER_SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DOCKER_MIN_TIMEOUT_MS: u64 = 1_000;
const DOCKER_MAX_TIMEOUT_MS: u64 = 15 * 60 * 1_000;

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

#[derive(Clone, Copy)]
struct DockerDeadline {
    expires_at: Instant,
}

impl DockerDeadline {
    fn after(duration: Duration) -> Self {
        Self {
            expires_at: Instant::now() + duration,
        }
    }

    fn remaining(self) -> io::Result<Duration> {
        self.expires_at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Docker operation timed out"))
    }

    fn capped(self, duration: Duration) -> Self {
        Self {
            expires_at: self.expires_at.min(Instant::now() + duration),
        }
    }
}

fn spawn_output_reader(mut reader: impl Read + Send + 'static) -> Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    receiver
}

fn receive_output(
    reader: Receiver<io::Result<Vec<u8>>>,
    deadline: DockerDeadline,
) -> io::Result<Vec<u8>> {
    let remaining = deadline.remaining()?;
    match reader.recv_timeout(remaining) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Docker output drain timed out",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "Docker output reader stopped unexpectedly",
        )),
    }
}

fn docker_command_output(
    program: &str,
    args: &[&str],
    deadline: DockerDeadline,
) -> io::Result<Output> {
    let mut child = command(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Docker stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Docker stderr was not captured"))?;
    let stdout_reader = spawn_output_reader(stdout);
    let stderr_reader = spawn_output_reader(stderr);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    status,
                    stdout: receive_output(stdout_reader, deadline)?,
                    stderr: receive_output(stderr_reader, deadline)?,
                });
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        let remaining = match deadline.remaining() {
            Ok(remaining) => remaining,
            Err(timeout) => {
                if let Err(kill_error) = child.kill() {
                    if child.try_wait()?.is_none() {
                        return Err(io::Error::other(format!(
                            "{timeout}; Docker CLI termination failed: {kill_error}"
                        )));
                    }
                } else {
                    child.wait()?;
                }
                // Reader results arrive over channels. Do not join here: a
                // descendant may have inherited a pipe even after the direct
                // Docker CLI child has been terminated.
                return Err(timeout);
            }
        };
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

fn docker_output(args: &[&str], deadline: DockerDeadline) -> io::Result<Output> {
    docker_output_with(
        args,
        DOCKER_FALLBACKS,
        |program, command_args| {
            let command_deadline = if command_args == ["--version"] {
                deadline.capped(DOCKER_PROBE_TIMEOUT)
            } else {
                deadline
            };
            docker_command_output(program, command_args, command_deadline)
        },
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

fn docker_available(deadline: DockerDeadline) -> io::Result<bool> {
    docker_output(&["--version"], deadline).map(|output| output.status.success())
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

fn container_exists(name: &str, deadline: DockerDeadline) -> bool {
    docker_output(&["inspect", "-f", "{{.State.Status}}", name], deadline)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn container_running(name: &str, deadline: DockerDeadline) -> bool {
    docker_output(&["inspect", "-f", "{{.State.Running}}", name], deadline)
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// The first existing container among the candidates — so we manage the one you
/// already have instead of creating a duplicate under a different name.
fn find_container(cands: &[&str], deadline: DockerDeadline) -> Option<String> {
    cands
        .iter()
        .find(|n| container_exists(n, deadline))
        .map(|n| (*n).to_string())
}

fn find_container_checked(
    cands: &[&str],
    deadline: DockerDeadline,
) -> Result<Option<String>, String> {
    for name in cands {
        let output = docker_output(&["inspect", "-f", "{{.State.Status}}", name], deadline)
            .map_err(|error| format!("Docker inspect failed: {error}"))?;
        if output.status.success() {
            return Ok(Some((*name).to_string()));
        }
    }
    Ok(None)
}

#[derive(Serialize)]
pub struct DockerStatus {
    pub available: bool,
    pub neo4j: bool,
    pub redis: bool,
}

fn docker_status_blocking(deadline: DockerDeadline) -> DockerStatus {
    let available = docker_available(deadline).unwrap_or(false);
    let running = |cands: &[&str], port: u16| {
        port_serving(port)
            || (available
                && find_container(cands, deadline)
                    .map(|name| container_running(&name, deadline))
                    .unwrap_or(false))
    };
    DockerStatus {
        available,
        neo4j: running(NEO4J_NAMES, NEO4J_DEFAULT),
        redis: running(REDIS_NAMES, REDIS_DEFAULT),
    }
}

#[tauri::command]
pub async fn docker_status() -> DockerStatus {
    let deadline = DockerDeadline::after(DOCKER_STATUS_TIMEOUT);
    tauri::async_runtime::spawn_blocking(move || docker_status_blocking(deadline))
        .await
        .unwrap_or(DockerStatus {
            available: false,
            neo4j: false,
            redis: false,
        })
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
pub async fn docker_up(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
    timeout_ms: Option<u64>,
) -> Result<String, String> {
    let timeout = timeout_ms
        .map(|value| value.clamp(DOCKER_MIN_TIMEOUT_MS, DOCKER_MAX_TIMEOUT_MS))
        .map(Duration::from_millis)
        .unwrap_or(DOCKER_SETUP_TIMEOUT);
    let deadline = DockerDeadline::after(timeout);
    tauri::async_runtime::spawn_blocking(move || {
        docker_up_blocking(neo4j_port, redis_port, neo4j_password, use_redis, deadline)
    })
    .await
    .map_err(|error| format!("Docker worker failed: {error}"))?
}

fn docker_up_blocking(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
    deadline: DockerDeadline,
) -> Result<String, String> {
    let neo4j_password = if neo4j_password.trim().is_empty() {
        stored_neo4j_password().unwrap_or_default()
    } else {
        neo4j_password
    };
    let need_neo4j = !port_serving(neo4j_port);
    let need_redis = use_redis && !port_serving(redis_port);

    if need_neo4j || need_redis {
        let available =
            docker_available(deadline).map_err(|error| format!("Docker check failed: {error}"))?;
        if !available {
            return Err(docker_missing_message());
        }
    }

    let mut notes: Vec<String> = Vec::new();

    if need_neo4j {
        let pw = neo4j_password.trim();
        let create = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            NEO4J_NAMES[0].to_string(),
            "--restart".into(),
            "unless-stopped".into(),
            "-p".into(),
            format!("127.0.0.1:{neo4j_port}:7687"),
            "-e".into(),
            format!("NEO4J_AUTH=neo4j/{pw}"),
            "neo4j:5".into(),
        ];
        notes.push(start_or_create(
            NEO4J_NAMES,
            &create,
            neo4j_create_password_error(&neo4j_password),
            "Neo4j",
            deadline,
        )?);
    } else {
        notes.push(format!("Neo4j already serving {neo4j_port} — reusing"));
    }

    if use_redis {
        if need_redis {
            let create = vec![
                "run".into(),
                "-d".into(),
                "--name".into(),
                REDIS_NAMES[0].to_string(),
                "--restart".into(),
                "unless-stopped".into(),
                "-p".into(),
                format!("127.0.0.1:{redis_port}:6379"),
                "redis:7".into(),
            ];
            notes.push(start_or_create(
                REDIS_NAMES,
                &create,
                None,
                "Redis",
                deadline,
            )?);
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
    deadline: DockerDeadline,
) -> Result<String, String> {
    if let Some(name) = find_container(cands, deadline) {
        let out = docker_output(&["start", &name], deadline)
            .map_err(|e| format!("Docker command failed: {e}"))?;
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
    let out = docker_output(&args, deadline).map_err(|e| format!("Docker command failed: {e}"))?;
    if out.status.success() {
        Ok(format!("{label} container '{}' created", cands[0]))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[tauri::command]
pub async fn docker_down() -> Result<String, String> {
    let deadline = DockerDeadline::after(DOCKER_ACTION_TIMEOUT);
    tauri::async_runtime::spawn_blocking(move || docker_down_blocking(deadline))
        .await
        .map_err(|error| format!("Docker worker failed: {error}"))?
}

fn docker_down_blocking(deadline: DockerDeadline) -> Result<String, String> {
    if !docker_available(deadline).map_err(|error| format!("Docker check failed: {error}"))? {
        return Err(docker_missing_message());
    }
    let daemon = docker_output(&["info", "--format", "{{.ServerVersion}}"], deadline)
        .map_err(|error| format!("Docker daemon check failed: {error}"))?;
    if !daemon.status.success() {
        let error = String::from_utf8_lossy(&daemon.stderr).trim().to_string();
        return Err(if error.is_empty() {
            "Docker daemon is not ready".into()
        } else {
            format!("Docker daemon is not ready: {error}")
        });
    }

    let mut stopped: Vec<String> = Vec::new();
    for cands in [NEO4J_NAMES, REDIS_NAMES] {
        if let Some(name) = find_container_checked(cands, deadline)? {
            let out = docker_output(&["stop", &name], deadline)
                .map_err(|error| format!("could not stop '{name}': {error}"))?;
            if !out.status.success() {
                let error = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(format!(
                    "could not stop '{name}': {}",
                    if error.is_empty() {
                        "Docker returned a non-zero status"
                    } else {
                        &error
                    }
                ));
            }
            stopped.push(name);
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
        assert_eq!(decode_saved_toml_string(&encoded).as_deref(), Some(logical));
        assert_eq!(
            decode_saved_toml_string(r#""eight chars""#).as_deref(),
            Some("eight chars")
        );
        assert_eq!(decode_saved_toml_string(r#""bad\nvalue""#), None);
    }

    #[test]
    fn bounded_docker_command_terminates_a_hung_direct_cli_child() {
        #[cfg(windows)]
        let (program, args): (&str, &[&str]) = (
            "powershell",
            &["-NoProfile", "-Command", "Start-Sleep -Seconds 30"],
        );
        #[cfg(unix)]
        let (program, args): (&str, &[&str]) = ("/bin/sleep", &["30"]);

        let started = Instant::now();
        let error = docker_command_output(
            program,
            args,
            DockerDeadline::after(Duration::from_millis(100)),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn bounded_docker_command_does_not_wait_for_descendant_inherited_pipe() {
        // Run a short-lived copy of this test binary which starts another copy
        // that inherits its stdout/stderr and stays alive. Joining the reader
        // threads would therefore ignore the command deadline until the
        // descendant exits.
        let test_executable = std::env::current_exe().expect("current test executable");
        let test_program = test_executable
            .to_str()
            .expect("UTF-8 test executable path");
        let started = Instant::now();
        let error = docker_command_output(
            test_program,
            &[
                "--ignored",
                "--exact",
                "docker::tests::descendant_pipe_spawner_helper",
                "--nocapture",
            ],
            DockerDeadline::after(Duration::from_secs(1)),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    #[ignore = "process helper for the descendant-pipe regression test"]
    fn descendant_pipe_spawner_helper() {
        let test_executable = std::env::current_exe().expect("current test executable");
        std::process::Command::new(test_executable)
            .args([
                "--ignored",
                "--exact",
                "docker::tests::descendant_pipe_holder_helper",
                "--nocapture",
            ])
            .spawn()
            .expect("spawn descendant pipe holder");
    }

    #[test]
    #[ignore = "process helper for the descendant-pipe regression test"]
    fn descendant_pipe_holder_helper() {
        std::thread::sleep(Duration::from_secs(4));
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
