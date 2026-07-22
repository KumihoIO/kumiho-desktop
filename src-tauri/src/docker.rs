//! Docker pillar — run the CE server's dependencies (Neo4j + Redis) as local
//! containers. Mirrors the community `deploy/docker-compose.yml` (containers
//! kumiho-ce-neo4j / kumiho-ce-redis, published to loopback) using plain
//! `docker run` so no compose file needs to ship.

use crate::util::command;
use serde::Serialize;

const NEO4J_CONTAINER: &str = "kumiho-ce-neo4j";
const REDIS_CONTAINER: &str = "kumiho-ce-redis";

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
    DockerStatus {
        available,
        neo4j: available && container_running(NEO4J_CONTAINER),
        redis: available && container_running(REDIS_CONTAINER),
    }
}

/// Start (or create) the Neo4j and optional Redis containers on the given ports.
#[tauri::command]
pub fn docker_up(
    neo4j_port: u16,
    redis_port: u16,
    neo4j_password: String,
    use_redis: bool,
) -> Result<String, String> {
    if neo4j_password.trim().is_empty() {
        return Err("a Neo4j password is required".into());
    }
    // Neo4j 5.x
    run_or_start(
        NEO4J_CONTAINER,
        &[
            "run", "-d",
            "--name", NEO4J_CONTAINER,
            "--restart", "unless-stopped",
            "-p", &format!("127.0.0.1:{neo4j_port}:7687"),
            "-p", "127.0.0.1:7474:7474",
            "-e", &format!("NEO4J_AUTH=neo4j/{}", neo4j_password.replace(['"', ' '], "")),
            "neo4j:5",
        ],
    )?;
    if use_redis {
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
    }
    Ok("databases starting".into())
}

/// `docker start <name>` if the container exists, else `docker run ...` to create it.
fn run_or_start(name: &str, run_args: &[&str]) -> Result<(), String> {
    if container_exists(name) {
        let out = command("docker")
            .args(["start", name])
            .output()
            .map_err(|e| format!("docker not available: {e}"))?;
        return if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        };
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
    // Stop (keep data volumes). Ignore per-container errors so a missing one is fine.
    let _ = command("docker")
        .args(["stop", NEO4J_CONTAINER, REDIS_CONTAINER])
        .output();
    Ok("databases stopped".into())
}
