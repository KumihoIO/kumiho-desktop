//! Docker pillar — run the CE server's dependencies (Neo4j + Redis) as local
//! containers, OR reuse whatever is already serving those ports (e.g. an
//! existing Neo4j from a prior setup). Container names mirror the community
//! `deploy/docker-compose.yml`: kumiho-ce-neo4j / kumiho-ce-redis.

use crate::util::command;
use serde::Serialize;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const NEO4J_CONTAINER: &str = "kumiho-ce-neo4j";
const REDIS_CONTAINER: &str = "kumiho-ce-redis";
const NEO4J_DEFAULT: u16 = 7687;
const REDIS_DEFAULT: u16 = 6379;

/// Is something already accepting connections on this loopback port?
fn port_serving(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
}

/// Is a working `docker` CLI on PATH (and its daemon reachable enough to answer)?
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
        "install it — e.g. `sudo apt install docker.io`, then `sudo systemctl enable --now docker` and `sudo usermod -aG docker $USER` (re-login)"
    } else if cfg!(target_os = "macos") {
        "install Docker Desktop from https://www.docker.com/products/docker-desktop and start it"
    } else {
        "install Docker Desktop from https://www.docker.com/products/docker-desktop and start it (make sure the whale icon says it's running)"
    };
    format!(
        "Docker isn't installed or running — {how}. \
         Or run Neo4j 5.x (and optionally Redis) yourself; Kumiho reuses whatever is already listening on those ports."
    )
}

fn container_running(name: &str) -> bool {
    command("docker")
        .args(["inspect", "-f", "{{.State.Running}}", name])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

fn container_exists(name: &str) -> bool {
    command("docker")
        .args(["inspect", "-f", "{{.State.Status}}", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
    // Reachable counts whether it's our container or a pre-existing server.
    DockerStatus {
        available,
        neo4j: port_serving(NEO4J_DEFAULT) || (available && container_running(NEO4J_CONTAINER)),
        redis: port_serving(REDIS_DEFAULT) || (available && container_running(REDIS_CONTAINER)),
    }
}

/// Bring up Neo4j (and optional Redis). If a port is already served, reuse it
/// instead of creating a conflicting container. If a DB is actually needed and
/// Docker isn't available, return actionable guidance rather than a raw error.
#[tauri::command]
pub fn docker_up(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
) -> Result<String, String> {
    let need_neo4j = !port_serving(neo4j_port);
    let need_redis = use_redis && !port_serving(redis_port);

    // Only require Docker if we actually have to create a container.
    if (need_neo4j || need_redis) && !docker_available() {
        return Err(docker_missing_message());
    }

    let mut notes: Vec<String> = Vec::new();

    if need_neo4j {
        if neo4j_password.trim().is_empty() {
            return Err("a Neo4j password is required to create the database".into());
        }
        run_or_start(
            NEO4J_CONTAINER,
            &[
                "run", "-d",
                "--name", NEO4J_CONTAINER,
                "--restart", "unless-stopped",
                "-p", &format!("127.0.0.1:{neo4j_port}:7687"),
                "-e", &format!("NEO4J_AUTH=neo4j/{}", neo4j_password.replace(['"', ' '], "")),
                "neo4j:5",
            ],
        )?;
        notes.push(format!("Neo4j starting on {neo4j_port}"));
    } else {
        notes.push(format!("Neo4j already serving {neo4j_port} — reusing"));
    }

    if use_redis {
        if need_redis {
            run_or_start(
                REDIS_CONTAINER,
                &[
                    "run", "-d",
                    "--name", REDIS_CONTAINER,
                    "--restart", "unless-stopped",
                    "-p", &format!("127.0.0.1:{redis_port}:6379"),
                    "redis:7",
                ],
            )?;
            notes.push(format!("Redis starting on {redis_port}"));
        } else {
            notes.push(format!("Redis already serving {redis_port} — reusing"));
        }
    }

    Ok(notes.join("; "))
}

/// `docker start` an existing container; if that fails (e.g. a stale port map),
/// remove it and create it fresh. Otherwise `docker run` a new one.
fn run_or_start(name: &str, run_args: &[&str]) -> Result<(), String> {
    if container_exists(name) {
        let out = command("docker")
            .args(["start", name])
            .output()
            .map_err(|e| format!("docker not available: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        // Broken/misconfigured container (e.g. stale port binding) — recreate it.
        let _ = command("docker").args(["rm", "-f", name]).output();
    }
    let out = command("docker")
        .args(run_args)
        .output()
        .map_err(|e| format!("docker not available: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[tauri::command]
pub fn docker_down() -> Result<String, String> {
    // Stop only our own containers (leave a reused, user-managed DB alone).
    let _ = command("docker")
        .args(["stop", NEO4J_CONTAINER, REDIS_CONTAINER])
        .output();
    Ok("databases stopped".into())
}
