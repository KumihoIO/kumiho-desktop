//! Runtime configuration: CLI flags + `KUMIHO_BRAIN_*` environment variables.

use crate::model::MemoryClass;

const DEFAULT_PORT: u16 = 8090;
const DEFAULT_CONCURRENCY: usize = 24;
const DEFAULT_PAGE_SIZE: i32 = 200;
/// How many newest revisions per item to scan for edges at snapshot time.
/// Older revisions rarely carry links their neighbours' latest revisions don't
/// also see (edges are fetched in `Both` directions); `--edge-revs 0` = all.
const DEFAULT_EDGE_REVS: usize = 3;

const CONV_KINDS: &[&str] = &["conversation", "fact", "entity"];
const CODE_KINDS: &[&str] = &["code_decision", "code_anchor", "code_evidence"];

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    /// True when the port came from --port/env (disables auto-fallback).
    pub port_explicit: bool,
    /// Open the dashboard in the default browser once serving.
    pub open: bool,
    /// Interface to listen on (default loopback; the dashboard serves the
    /// whole memory graph, so non-loopback binds require an access key).
    pub bind: String,
    /// Access key for non-loopback clients (else generated + persisted).
    pub key: Option<String>,
    /// Explicitly serve without any access key on a non-loopback bind.
    pub no_auth: bool,
    /// Explicit gRPC endpoint (else the SDK bootstrap chain decides).
    pub endpoint: Option<String>,
    /// Pin discovery to a tenant slug/id.
    pub tenant: Option<String>,
    /// Force the loopback self-hosted CE server.
    pub local: bool,
    pub conv_kinds: Vec<String>,
    pub code_kinds: Vec<String>,
    pub concurrency: usize,
    pub page_size: i32,
    pub edge_revs: usize,
    /// Serve frontend from this directory instead of the embedded copy.
    pub static_dir: Option<String>,
}

impl Config {
    pub fn classify(&self, kind: &str) -> Option<MemoryClass> {
        if self.conv_kinds.iter().any(|k| k == kind) {
            Some(MemoryClass::Conversation)
        } else if self.code_kinds.iter().any(|k| k == kind) {
            Some(MemoryClass::Code)
        } else {
            None
        }
    }

    /// All memory kinds the dashboard tracks.
    pub fn kinds(&self) -> impl Iterator<Item = &String> {
        self.conv_kinds.iter().chain(self.code_kinds.iter())
    }

    pub fn from_env_and_args() -> Result<Config, String> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let list = |v: String| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        };
        let env_port = env("KUMIHO_BRAIN_PORT")
            .map(|v| v.parse::<u16>().map_err(|_| "invalid KUMIHO_BRAIN_PORT"))
            .transpose()?;
        let mut cfg = Config {
            port_explicit: env_port.is_some(),
            port: env_port.unwrap_or(DEFAULT_PORT),
            open: false,
            bind: env("KUMIHO_BRAIN_BIND").unwrap_or_else(|| "127.0.0.1".into()),
            key: env("KUMIHO_BRAIN_KEY"),
            no_auth: false,
            endpoint: env("KUMIHO_BRAIN_ENDPOINT"),
            tenant: env("KUMIHO_BRAIN_TENANT"),
            local: false,
            conv_kinds: env("KUMIHO_BRAIN_CONV_KINDS")
                .map(list)
                .unwrap_or_else(|| CONV_KINDS.iter().map(|s| s.to_string()).collect()),
            code_kinds: env("KUMIHO_BRAIN_CODE_KINDS")
                .map(list)
                .unwrap_or_else(|| CODE_KINDS.iter().map(|s| s.to_string()).collect()),
            concurrency: env("KUMIHO_BRAIN_CONCURRENCY")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_CONCURRENCY)
                .clamp(1, 128),
            page_size: DEFAULT_PAGE_SIZE,
            edge_revs: env("KUMIHO_BRAIN_EDGE_REVS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_EDGE_REVS),
            static_dir: env("KUMIHO_BRAIN_STATIC_DIR"),
        };

        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let mut take = |name: &str| {
                args.next()
                    .ok_or_else(|| format!("{name} requires a value"))
            };
            match a.as_str() {
                "--port" => {
                    cfg.port = take("--port")?.parse().map_err(|_| "invalid --port")?;
                    cfg.port_explicit = true;
                }
                "--open" | "-o" => cfg.open = true,
                "--version" | "-V" => {
                    println!("kumiho-brain {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                "--bind" => cfg.bind = take("--bind")?,
                "--key" => cfg.key = Some(take("--key")?),
                "--no-auth" => cfg.no_auth = true,
                "--endpoint" => cfg.endpoint = Some(take("--endpoint")?),
                "--tenant" => cfg.tenant = Some(take("--tenant")?),
                "--local" => cfg.local = true,
                "--edge-revs" => {
                    cfg.edge_revs = take("--edge-revs")?
                        .parse()
                        .map_err(|_| "invalid --edge-revs")?
                }
                "--static-dir" => cfg.static_dir = Some(take("--static-dir")?),
                "--help" | "-h" => {
                    println!("{HELP}");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other} (see --help)")),
            }
        }
        Ok(cfg)
    }
}

const HELP: &str = "\
kumiho-brain — real-time WebGL dashboard for the living Kumiho memory graph

USAGE:
  kumiho-brain [--open] [--port N] [--bind ADDR] [--key SECRET] [--no-auth]
               [--endpoint HOST:PORT] [--tenant SLUG] [--local]
               [--edge-revs N] [--static-dir DIR] [--version]

  Connects like every Kumiho SDK client: explicit --endpoint, else bearer token
  (~/.kumiho) + control-plane discovery to your cloud tenant, else a loopback
  self-hosted CE server. --local skips the token and forces the CE probe.

  --open (-o) launches your browser once serving. If the default port is busy
  the next free one is used automatically (explicit --port disables that).

REMOTE ACCESS:
  The dashboard serves your whole memory graph, so it binds 127.0.0.1 by
  default. `--bind 0.0.0.0` opens it to the network behind an access key:
  --key/KUMIHO_BRAIN_KEY if set, else one is generated and persisted at
  $KUMIHO_CONFIG_DIR/kumiho-brain.key (the startup banner prints the full
  URL). Remote clients open http://host:port/?key=… once — a cookie keeps
  the session; loopback clients never need the key. --no-auth disables the
  gate entirely (not recommended).

ENV:
  KUMIHO_BRAIN_PORT, KUMIHO_BRAIN_BIND, KUMIHO_BRAIN_KEY,
  KUMIHO_BRAIN_ENDPOINT, KUMIHO_BRAIN_TENANT,
  KUMIHO_BRAIN_CONV_KINDS, KUMIHO_BRAIN_CODE_KINDS (comma-separated),
  KUMIHO_BRAIN_CONCURRENCY, KUMIHO_BRAIN_EDGE_REVS, KUMIHO_BRAIN_STATIC_DIR
  plus the standard SDK vars (KUMIHO_AUTH_TOKEN, KUMIHO_SERVER_ENDPOINT, …).";
