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
    let available = command("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    // Reachable counts whether it's our container or a pre-existing server.
    DockerStatus {
        available,
        neo4j: port_serving(NEO4J_DEFAULT) || (available && container_running(NEO4J_CONTAINER)),
        redis: port_serving(REDIS_DEFAULT) || (available && container_running(REDIS_CONTAINER)),
    }
}

/// Bring up Neo4j (and optional Redis). If a port is already served, reuse it
/// instead of creating a conflicting container.
#[tauri::command]
pub fn docker_up(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
) -> Result<String, String> {
    let mut notes: Vec<String> = Vec::new();

    if port_serving(neo4j_port) {
        notes.push(format!("Neo4j already serving {neo4j_port} — reusing"));
    } else {
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
    }

    if use_redis {
        if port_serving(redis_port) {
            notes.push(format!("Redis already serving {redis_port} — reusing"));
        } else {
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
