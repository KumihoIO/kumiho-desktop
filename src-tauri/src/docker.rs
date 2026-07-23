//! Docker pillar — manage the CE server's dependencies (Neo4j + Redis).
//!
//! We do NOT assume our own container names: an existing Neo4j/Redis container
//! (however it was created — `kumiho-neo4j`, plain `neo4j`, the community
//! compose's `kumiho-ce-neo4j`, …) is discovered and managed. A container is
//! only created when nothing suitable exists and the port is free.

use crate::util::command;
use serde::Serialize;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Candidate container names, most-specific first. The first entry is the one
/// we create ourselves when nothing exists.
const NEO4J_NAMES: &[&str] = &["kumiho-ce-neo4j", "kumiho-neo4j", "neo4j"];
const REDIS_NAMES: &[&str] = &["kumiho-ce-redis", "kumiho-redis", "redis"];
const NEO4J_DEFAULT: u16 = 7687;
const REDIS_DEFAULT: u16 = 6379;

/// Is something already accepting connections on this loopback port?
fn port_serving(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

fn docker_available() -> bool {
    command("docker")
        .arg("--version")
        .output()
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
    command("docker")
        .args(["inspect", "-f", "{{.State.Status}}", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn container_running(name: &str) -> bool {
    command("docker")
        .args(["inspect", "-f", "{{.State.Running}}", name])
        .output()
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

/// Bring up Neo4j (and optional Redis): reuse a served port, else start an
/// existing container, else create one. A password is only needed to CREATE.
#[tauri::command]
pub fn docker_up(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
) -> Result<String, String> {
    let need_neo4j = !port_serving(neo4j_port);
    let need_redis = use_redis && !port_serving(redis_port);

    if (need_neo4j || need_redis) && !docker_available() {
        return Err(docker_missing_message());
    }

    let mut notes: Vec<String> = Vec::new();

    if need_neo4j {
        let pw = neo4j_password.replace(['"', ' '], "");
        let create = vec![
            "run".into(), "-d".into(),
            "--name".into(), NEO4J_NAMES[0].to_string(),
            "--restart".into(), "unless-stopped".into(),
            "-p".into(), format!("127.0.0.1:{neo4j_port}:7687"),
            "-e".into(), format!("NEO4J_AUTH=neo4j/{pw}"),
            "neo4j:5".into(),
        ];
        notes.push(start_or_create(NEO4J_NAMES, &create, !neo4j_password.trim().is_empty(), "Neo4j")?);
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
            notes.push(start_or_create(REDIS_NAMES, &create, true, "Redis")?);
        } else {
            notes.push(format!("Redis already serving {redis_port} — reusing"));
        }
    }

    Ok(notes.join("; "))
}

/// Start an existing container (any known name), else create ours. `can_create`
/// is false when we'd need a password we don't have.
fn start_or_create(
    cands: &[&str],
    create_args: &[String],
    can_create: bool,
    label: &str,
) -> Result<String, String> {
    if let Some(name) = find_container(cands) {
        let out = command("docker")
            .args(["start", &name])
            .output()
            .map_err(|e| format!("docker not available: {e}"))?;
        if out.status.success() {
            return Ok(format!("{label} container '{name}' started"));
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // Only recreate a container we own; never silently destroy the user's.
        if name != cands[0] {
            return Err(format!("could not start '{name}': {err}"));
        }
        let _ = command("docker").args(["rm", "-f", &name]).output();
    }
    if !can_create {
        return Err(format!(
            "a Neo4j password is required to create the {label} database (no existing container found)"
        ));
    }
    let args: Vec<&str> = create_args.iter().map(|s| s.as_str()).collect();
    let out = command("docker")
        .args(&args)
        .output()
        .map_err(|e| format!("docker not available: {e}"))?;
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
            let out = command("docker").args(["stop", &name]).output();
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
