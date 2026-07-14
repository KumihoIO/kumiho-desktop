//! kumiho-brain — real-time WebGL dashboard for the living Kumiho memory graph.
//!
//! `cargo run` → connects to your tenant via the `kumiho` SDK bootstrap chain,
//! crawls the memory graph into a snapshot, subscribes to the live event
//! stream, and serves the WebGL2 frontend + WebSocket feed.

mod access;
mod config;
mod fetch;
mod live;
mod model;
mod trace;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use config::Config;
use live::send;
use model::{GraphStore, StreamEvent};
use std::sync::Arc;
use tokio::sync::{broadcast, watch, RwLock};

const INDEX_HTML: &str = include_str!("../static/index.html");
const BRAIN_CSS: &str = include_str!("../static/brain.css");
const BRAIN_JS: &str = include_str!("../static/brain.js");
const GL_JS: &str = include_str!("../static/gl.js");
// JWST backdrops (NASA/ESA/CSA/STScI, public domain — credited in gl.js).
const PILLARS_WEBP: &[u8] = include_bytes!("../static/pillars.webp");
const SOUTHERN_RING_WEBP: &[u8] = include_bytes!("../static/southern-ring.webp");

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    client: kumiho::Client,
    store: Arc<RwLock<GraphStore>>,
    tx: broadcast::Sender<String>,
    ready_rx: watch::Receiver<bool>,
    live_rx: watch::Receiver<bool>,
    endpoint_label: Arc<String>,
}

impl AppState {
    async fn snapshot_event(&self) -> StreamEvent {
        let g = self.store.read().await;
        StreamEvent::Snapshot {
            generated_at: now_ms(),
            endpoint: self.endpoint_label.to_string(),
            spaces: g.spaces.clone(),
            nodes: g.live_nodes(),
            edges: g.edges.clone(),
            tenant: g.tenant.clone(),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kumiho_brain=info,kumiho=warn".into()),
        )
        .init();

    let cfg = match Config::from_env_and_args() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("kumiho-brain: {e}");
            std::process::exit(2);
        }
    };

    let (client, endpoint_label) = match connect(&cfg).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("kumiho-brain: cannot configure the Kumiho client: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("kumiho client ready ({endpoint_label})");

    let store = Arc::new(RwLock::new(GraphStore::default()));
    let (tx, _) = broadcast::channel::<String>(4096);
    let (ready_tx, ready_rx) = watch::channel(false);
    let (live_tx, live_rx) = watch::channel(false);

    // Live feed first: writes that land during the crawl are still captured
    // (the store upsert is idempotent, so overlap with the crawl is harmless).
    tokio::spawn(
        live::LiveFeed {
            client: client.clone(),
            cfg: (*cfg).clone(),
            store: store.clone(),
            tx: tx.clone(),
        }
        .run(live_tx),
    );
    tokio::spawn(live::heartbeat(tx.clone()));

    // Snapshot crawl with retry — the dashboard stays up (and says "syncing")
    // even if the server is briefly unreachable.
    {
        let (client, cfg, store, tx) = (client.clone(), cfg.clone(), store.clone(), tx.clone());
        tokio::spawn(async move {
            loop {
                let progress = {
                    let tx = tx.clone();
                    let last = std::sync::atomic::AtomicUsize::new(0);
                    move |done: usize, total: usize| {
                        // throttle: every 25 items or at completion
                        let prev = last.swap(done, std::sync::atomic::Ordering::Relaxed);
                        if done == total || done / 25 != prev / 25 {
                            send(
                                &tx,
                                &StreamEvent::Status {
                                    core: false,
                                    live: false,
                                    info: format!("syncing memory {done}/{total}"),
                                },
                            );
                        }
                    }
                };
                match fetch::load_snapshot(&client, &cfg, &store, progress).await {
                    Ok(stats) => {
                        tracing::info!(
                            "snapshot ready: {} memories, {} interlinks ({} skipped) in {} ms",
                            stats.items,
                            stats.edges,
                            stats.skipped,
                            stats.elapsed_ms
                        );
                        let _ = ready_tx.send(true);
                        send(
                            &tx,
                            &StreamEvent::Status {
                                core: true,
                                live: false,
                                info: format!(
                                    "snapshot: {} memories · {} interlinks",
                                    stats.items, stats.edges
                                ),
                            },
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("snapshot failed (retrying in 10s): {e}");
                        send(
                            &tx,
                            &StreamEvent::Status {
                                core: false,
                                live: false,
                                info: format!("snapshot failed: {e} — retrying"),
                            },
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    }
                }
            }
        });
    }

    let state = AppState {
        cfg: cfg.clone(),
        client,
        store,
        tx,
        ready_rx,
        live_rx,
        endpoint_label: Arc::new(endpoint_label),
    };

    // Non-loopback binds expose the whole memory graph — gate them behind an
    // access key unless --no-auth was given explicitly.
    let remote_bind = !access::is_loopback_bind(&cfg.bind);
    let access_key: Option<std::sync::Arc<String>> = if remote_bind && !cfg.no_auth {
        Some(std::sync::Arc::new(access::resolve_key(cfg.key.as_deref())))
    } else {
        if remote_bind {
            tracing::warn!(
                "--no-auth: serving the memory graph unauthenticated on {}",
                cfg.bind
            );
        }
        None
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/static/:file", get(static_file))
        .route("/api/healthz", get(healthz))
        .route("/api/snapshot", get(snapshot))
        .route("/api/search", get(search))
        .route("/api/node/:id", get(node_detail))
        .route("/api/traverse", get(traverse))
        .route("/api/path", get(path_between))
        .route("/api/revisions/:id", get(revisions))
        .route("/api/revision/:id", get(revision))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            access_key.clone(),
            access::guard,
        ));

    // Bind, sliding to the next free port when the default is taken (an
    // explicit --port/env choice fails loudly instead).
    let mut port = cfg.port;
    let listener = loop {
        match tokio::net::TcpListener::bind(format!("{}:{port}", cfg.bind)).await {
            Ok(l) => break l,
            Err(e)
                if e.kind() == std::io::ErrorKind::AddrInUse
                    && !cfg.port_explicit
                    && port < cfg.port + 10 =>
            {
                tracing::info!("port {port} is busy — trying {}", port + 1);
                port += 1;
            }
            Err(e) => {
                eprintln!("kumiho-brain: cannot bind {}:{port}: {e}", cfg.bind);
                std::process::exit(1);
            }
        }
    };
    let local_url = format!("http://127.0.0.1:{port}");
    print!("\n  🦊🧠  Kumiho Brain — {local_url}");
    println!(
        "{}",
        if port != cfg.port {
            format!("   (port {} was busy)", cfg.port)
        } else {
            String::new()
        }
    );
    if let Some(key) = &access_key {
        let host = access::lan_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| cfg.bind.clone());
        println!("        remote: http://{host}:{port}/?key={key}");
        println!("        (the key is needed once per browser; local clients never need it)");
    }
    println!();
    if cfg.open {
        open_browser(&local_url);
    }
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .expect("server error");
}

/// Best-effort default-browser launch; failures are silent (headless boxes).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let (cmd, args) = ("open", vec![url.to_string()]);
    #[cfg(target_os = "windows")]
    let (cmd, args) = (
        "cmd",
        vec!["/C".into(), "start".into(), String::new(), url.to_string()],
    );
    #[cfg(all(unix, not(target_os = "macos")))]
    let (cmd, args) = ("xdg-open", vec![url.to_string()]);
    let _ = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Build the SDK client per config: `--local` → loopback CE, `--endpoint` →
/// explicit, else the standard bootstrap chain (token → discovery → CE probe).
async fn connect(cfg: &Config) -> Result<(kumiho::Client, String), kumiho::Error> {
    if cfg.local {
        return match kumiho::Client::from_local_ce().await? {
            Some(c) => Ok((c, "local self-hosted CE".into())),
            None => Err(kumiho::Error::Discovery(
                "--local: no self-hosted CE server detected on loopback".into(),
            )),
        };
    }
    let mut b = kumiho::Client::builder();
    let label;
    if let Some(ep) = &cfg.endpoint {
        b = b.endpoint(ep.clone());
        label = ep.clone();
    } else {
        label = "auto (discovery / local CE)".to_string();
    }
    if let Some(t) = &cfg.tenant {
        b = b.tenant_hint(t.clone());
    }
    Ok((b.build().await?, label))
}

async fn index(State(app): State<AppState>) -> Response {
    (
        [(header::CACHE_CONTROL, "no-cache")],
        Html(load_static(&app, "index.html").await),
    )
        .into_response()
}

async fn static_file(Path(file): Path<String>, State(app): State<AppState>) -> Response {
    // binary image assets: embedded, disk-overridable, cacheable (they only
    // change with a release)
    let image: Option<&'static [u8]> = match file.as_str() {
        "pillars.webp" => Some(PILLARS_WEBP),
        "southern-ring.webp" => Some(SOUTHERN_RING_WEBP),
        _ => None,
    };
    if let Some(bytes) = image {
        let body = match &app.cfg.static_dir {
            Some(dir) => tokio::fs::read(format!("{dir}/{file}"))
                .await
                .unwrap_or_else(|_| bytes.to_vec()),
            None => bytes.to_vec(),
        };
        return (
            [
                (header::CONTENT_TYPE, "image/webp".to_string()),
                (header::CACHE_CONTROL, "public, max-age=604800".to_string()),
            ],
            body,
        )
            .into_response();
    }
    let (body, mime) = match file.as_str() {
        "brain.css" => (load_static(&app, "brain.css").await, "text/css"),
        "brain.js" => (load_static(&app, "brain.js").await, "text/javascript"),
        "gl.js" => (load_static(&app, "gl.js").await, "text/javascript"),
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, format!("{mime}; charset=utf-8")),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        body,
    )
        .into_response()
}

/// Embedded frontend, with a disk override for frontend development
/// (`--static-dir` avoids recompiling on every CSS/JS tweak).
async fn load_static(app: &AppState, name: &str) -> String {
    if let Some(dir) = &app.cfg.static_dir {
        if let Ok(s) = tokio::fs::read_to_string(format!("{dir}/{name}")).await {
            return s;
        }
    }
    match name {
        "index.html" => INDEX_HTML.into(),
        "brain.css" => BRAIN_CSS.into(),
        "brain.js" => BRAIN_JS.into(),
        "gl.js" => GL_JS.into(),
        _ => String::new(),
    }
}

async fn healthz(State(app): State<AppState>) -> Json<serde_json::Value> {
    let g = app.store.read().await;
    Json(serde_json::json!({
        "status": "ok",
        "core": *app.ready_rx.borrow(),
        "live": *app.live_rx.borrow(),
        "nodes": g.live_nodes().len(),
        "edges": g.edges.len(),
    }))
}

async fn snapshot(State(app): State<AppState>) -> Response {
    let ev = app.snapshot_event().await;
    match serde_json::to_string(&ev) {
        Ok(json) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            json,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Tier-2 semantic search: the server's hybrid ranked retrieval
/// (`Client::search`, the engine behind memory recall) mapped onto loaded
/// node ids. The frontend unions these with its instant substring tier.
async fn search(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let q = params.get("q").map(|s| s.trim()).unwrap_or("");
    if q.chars().count() < 2 {
        return Json(serde_json::json!({ "hits": [], "took_ms": 0 })).into_response();
    }
    let kinds: Vec<String> = match params.get("kind").map(String::as_str) {
        Some("conversation") => app.cfg.conv_kinds.clone(),
        Some("code") => app.cfg.code_kinds.clone(),
        _ => app.cfg.kinds().cloned().collect(),
    };
    let t0 = std::time::Instant::now();
    let futs = kinds.into_iter().map(|kind| {
        let client = app.client.clone();
        let q = q.to_string();
        async move {
            client
                .search(&q, "", &kind, false, false, false, 0.2, Some(24), None)
                .await
        }
    });
    let pages = futures::future::join_all(futs).await;

    let g = app.store.read().await;
    let mut best: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
    for page in pages {
        let page = match page {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("semantic search page failed: {e}");
                continue;
            }
        };
        for hit in page {
            let item = model::item_uri(hit.item.kref.uri()).to_string();
            if let Some(id) = g.node_id(&item) {
                let e = best.entry(id).or_insert(hit.score);
                if hit.score > *e {
                    *e = hit.score;
                }
            }
        }
    }
    let mut hits: Vec<model::SearchHit> = best
        .into_iter()
        .filter_map(|(id, score)| {
            let n = g.nodes.get(id as usize)?;
            (!n.dead).then(|| model::SearchHit {
                id,
                title: n.title.clone(),
                kind: n.kind,
                score,
            })
        })
        .collect();
    drop(g);
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(40);
    let took = t0.elapsed().as_millis() as u64;
    tracing::debug!("semantic search: {} hits in {took} ms", hits.len());
    Json(serde_json::json!({ "hits": hits, "took_ms": took })).into_response()
}

async fn node_detail(Path(id): Path<u32>, State(app): State<AppState>) -> Response {
    match fetch::fetch_detail(&app.client, &app.cfg, &app.store, id).await {
        Some(d) => Json(d).into_response(),
        None => (StatusCode::NOT_FOUND, "no such node").into_response(),
    }
}

type Params = std::collections::HashMap<String, String>;

fn param_id(params: &Params, key: &str) -> Option<u32> {
    params.get(key)?.parse().ok()
}

/// Bounded typed-edge BFS over the loaded graph (the Why/Impact explorer).
async fn traverse(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<Params>,
) -> Response {
    let Some(from) = param_id(&params, "from") else {
        return (StatusCode::BAD_REQUEST, "from required").into_response();
    };
    let types = trace::parse_types(params.get("edges").map(String::as_str));
    let dir = trace::Dir::parse(params.get("dir").map(String::as_str));
    let depth = param_id(&params, "depth").unwrap_or(2);
    let g = app.store.read().await;
    if g.nodes.get(from as usize).map(|n| n.dead).unwrap_or(true) {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }
    let sub = trace::bfs(
        &g.edges,
        |id| g.nodes.get(id as usize).map(|n| n.dead).unwrap_or(true),
        from,
        &types,
        dir,
        depth,
        trace::NODE_CAP,
    );
    Json(serde_json::json!({ "origin": from, "nodes": sub.nodes, "edges": sub.edges, "truncated": sub.truncated }))
        .into_response()
}

/// Undirected shortest chain between two memories ("how are these related?").
async fn path_between(
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<Params>,
) -> Response {
    let (Some(from), Some(to)) = (param_id(&params, "from"), param_id(&params, "to")) else {
        return (StatusCode::BAD_REQUEST, "from and to required").into_response();
    };
    let g = app.store.read().await;
    let dead = |id: u32| g.nodes.get(id as usize).map(|n| n.dead).unwrap_or(true);
    if dead(from) || dead(to) {
        return (StatusCode::NOT_FOUND, "no such node").into_response();
    }
    match trace::shortest_path(&g.edges, dead, from, to, 6) {
        Some((chain, edges)) => {
            Json(serde_json::json!({ "found": true, "nodes": chain, "edges": edges }))
                .into_response()
        }
        None => Json(serde_json::json!({ "found": false })).into_response(),
    }
}

/// Revision lineage for a memory (newest first) — the time-travel index.
async fn revisions(Path(id): Path<u32>, State(app): State<AppState>) -> Response {
    match fetch::fetch_revisions(&app.client, &app.store, id).await {
        Some(revs) => Json(serde_json::json!({ "revisions": revs })).into_response(),
        None => (StatusCode::NOT_FOUND, "no such node").into_response(),
    }
}

/// One historical revision's content ("what did I used to think?").
async fn revision(
    Path(id): Path<u32>,
    State(app): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<Params>,
) -> Response {
    let Some(r) = param_id(&params, "r") else {
        return (StatusCode::BAD_REQUEST, "r required").into_response();
    };
    match fetch::fetch_revision(&app.client, &app.store, id, r as i32).await {
        Some(rev) => Json(rev).into_response(),
        None => (StatusCode::NOT_FOUND, "no such revision").into_response(),
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(app): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| ws_conn(socket, app))
}

async fn ws_conn(mut socket: WebSocket, app: AppState) {
    let send_ev = |ev: &StreamEvent| serde_json::to_string(ev).unwrap_or_default();
    if socket
        .send(Message::Text(send_ev(&StreamEvent::Hello {
            proto: 1,
            server: format!("kumiho-brain/{}", env!("CARGO_PKG_VERSION")),
        })))
        .await
        .is_err()
    {
        return;
    }
    let core = *app.ready_rx.borrow();
    let live = *app.live_rx.borrow();
    let _ = socket
        .send(Message::Text(send_ev(&StreamEvent::Status {
            core,
            live,
            info: if core {
                "ready".into()
            } else {
                "syncing memory…".into()
            },
        })))
        .await;

    // Subscribe before serializing the snapshot: deltas racing the snapshot
    // may arrive twice, which the frontend applies idempotently.
    let mut rx = app.tx.subscribe();
    let mut ready_rx = app.ready_rx.clone();
    let mut sent_snapshot = false;
    if *ready_rx.borrow() {
        let snap = send_ev(&app.snapshot_event().await);
        if socket.send(Message::Text(snap)).await.is_err() {
            return;
        }
        sent_snapshot = true;
    }

    loop {
        tokio::select! {
            changed = ready_rx.changed(), if !sent_snapshot => {
                if changed.is_err() { break; }
                if *ready_rx.borrow() {
                    let snap = send_ev(&app.snapshot_event().await);
                    if socket.send(Message::Text(snap)).await.is_err() { break; }
                    sent_snapshot = true;
                }
            }
            msg = rx.recv() => match msg {
                Ok(txt) => {
                    if socket.send(Message::Text(txt)).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("ws client lagged {n} events; resyncing");
                    let snap = send_ev(&app.snapshot_event().await);
                    if socket.send(Message::Text(snap)).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}   // ignore pings/client chatter
                Some(Err(_)) => break,
            }
        }
    }
}
