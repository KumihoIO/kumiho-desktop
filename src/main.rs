//! kumiho-brain — real-time WebGL dashboard for the living Kumiho memory graph.
//!
//! `cargo run` → connects to your tenant via the `kumiho` SDK bootstrap chain,
//! crawls the memory graph into a snapshot, subscribes to the live event
//! stream, and serves the WebGL2 frontend + WebSocket feed.

mod config;
mod fetch;
mod live;
mod model;

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

    let app = Router::new()
        .route("/", get(index))
        .route("/static/:file", get(static_file))
        .route("/api/healthz", get(healthz))
        .route("/api/snapshot", get(snapshot))
        .route("/api/node/:id", get(node_detail))
        .route("/ws", get(ws_upgrade))
        .with_state(state);

    let bind = std::env::var("KUMIHO_BRAIN_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let addr = format!("{bind}:{}", cfg.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("kumiho-brain: cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!("\n  🦊🧠  Kumiho Brain — http://{addr}\n");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server error");
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

async fn index(State(app): State<AppState>) -> Html<String> {
    Html(load_static(&app, "index.html").await)
}

async fn static_file(Path(file): Path<String>, State(app): State<AppState>) -> Response {
    let (body, mime) = match file.as_str() {
        "brain.css" => (load_static(&app, "brain.css").await, "text/css"),
        "brain.js" => (load_static(&app, "brain.js").await, "text/javascript"),
        "gl.js" => (load_static(&app, "gl.js").await, "text/javascript"),
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    ([(header::CONTENT_TYPE, format!("{mime}; charset=utf-8"))], body).into_response()
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

async fn node_detail(Path(id): Path<u32>, State(app): State<AppState>) -> Response {
    match fetch::fetch_detail(&app.client, &app.cfg, &app.store, id).await {
        Some(d) => Json(d).into_response(),
        None => (StatusCode::NOT_FOUND, "no such node").into_response(),
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
            info: if core { "ready".into() } else { "syncing memory…".into() },
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
