//! The live feed: `EventStream` subscription → graph deltas → WebSocket fan-out.
//!
//! The server pushes `revision.created` / `item.*.created` / `space.created`
//! events, but **no edge events** (confirmed gap — see README): edge creation
//! is silent. Compensation: every `revision.created` re-reads that revision's
//! edges and diffs them against the store, which catches links in practice
//! because writers create edges alongside the revisions they link.

use crate::config::Config;
use crate::model::{item_uri, GraphStore, NodeSeed, StreamEvent};
use futures::StreamExt;
use kumiho::{Client, EdgeDirection, Event};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};

pub struct LiveFeed {
    pub client: Client,
    pub cfg: Config,
    pub store: Arc<RwLock<GraphStore>>,
    pub tx: broadcast::Sender<String>,
}

pub fn send(tx: &broadcast::Sender<String>, ev: &StreamEvent) {
    if let Ok(json) = serde_json::to_string(ev) {
        let _ = tx.send(json); // no receivers is fine
    }
}

impl LiveFeed {
    /// Run forever: subscribe, consume, resume from the last cursor on drop,
    /// exponential backoff on failure.
    pub async fn run(self, live_status: tokio::sync::watch::Sender<bool>) {
        let mut cursor: Option<String> = None;
        let mut backoff = Duration::from_secs(1);
        loop {
            match self
                .client
                .event_stream("", "", cursor.clone(), None, false)
                .await
            {
                Ok(stream) => {
                    tracing::info!("event stream connected (cursor={cursor:?})");
                    let _ = live_status.send(true);
                    send(
                        &self.tx,
                        &StreamEvent::Status {
                            core: true,
                            live: true,
                            info: "event stream connected".into(),
                        },
                    );
                    backoff = Duration::from_secs(1);
                    futures::pin_mut!(stream);
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(ev) => {
                                if ev.cursor.is_some() {
                                    cursor = ev.cursor.clone();
                                }
                                self.handle(ev).await;
                            }
                            Err(e) => {
                                tracing::warn!("event stream error: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("event stream connect failed: {e}"),
            }
            let _ = live_status.send(false);
            send(
                &self.tx,
                &StreamEvent::Status {
                    core: true,
                    live: false,
                    info: format!("event stream reconnecting in {}s", backoff.as_secs()),
                },
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    async fn handle(&self, ev: Event) {
        let rk = ev.routing_key.as_str();
        if rk == "revision.created" {
            self.on_revision_created(&ev).await;
        } else if rk.starts_with("item.") && rk.ends_with(".deleted") {
            self.on_item_deleted(&ev).await;
        } else {
            tracing::debug!("event ignored: {rk} {}", ev.kref.uri());
        }
    }

    async fn on_revision_created(&self, ev: &Event) {
        let rev_uri = ev.kref.uri().to_string();
        let item_kref = item_uri(&rev_uri).to_string();
        // Kind gate first — the stream carries every tenant write (workflow
        // runs, images, …), not just memory.
        let kind = ev.kref.kind().to_string();
        let Some(class) = self.cfg.classify(&kind) else {
            return;
        };

        let rev = match self.client.get_revision(&rev_uri).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("get_revision({rev_uri}): {e}");
                return;
            }
        };
        let md = |k: &str| rev.metadata.get(k).cloned().unwrap_or_default();

        // Item lookup is best-effort: `get_item_by_kref` re-validates the kref
        // and rejects some server-accepted URIs (e.g. Unicode names — SDK gap),
        // so fall back to fields derivable from the revision + event.
        let item = self.client.get_item_by_kref(&item_kref).await.ok();
        let title = {
            let t = md("title");
            if !t.is_empty() {
                t
            } else if let Some(i) = &item {
                i.item_name.clone()
            } else {
                ev.kref.item_name().to_string()
            }
        };
        let source = ["source_client", "client", "agent", "created_by"]
            .iter()
            .map(|k| md(k))
            .find(|v| !v.is_empty())
            .or_else(|| item.as_ref().map(|i| i.username.clone()))
            .unwrap_or_else(|| ev.author.clone());
        let space = ev.kref.space();
        let space_path = if space.is_empty() {
            format!("/{}", ev.kref.project())
        } else {
            format!("/{}/{}", ev.kref.project(), space)
        };
        let seed = NodeSeed {
            item_kref: item_kref.clone(),
            item_kind: kind,
            class,
            title,
            space_path,
            source,
            memory_type: md("memory_type"),
            created_at: item
                .as_ref()
                .and_then(|i| i.created_at.clone())
                .or_else(|| rev.created_at.clone())
                .unwrap_or_default(),
            updated_at: rev.created_at.clone().unwrap_or_default(),
            revs: rev.number.max(1) as u32,
        };

        let (node, added) = {
            let mut g = self.store.write().await;
            g.upsert(seed)
        };
        tracing::info!(
            "live: {} '{}' ({})",
            if added { "new memory" } else { "revision" },
            node.title,
            node.kref
        );
        send(
            &self.tx,
            &if added {
                StreamEvent::NodeAdded { node: node.clone() }
            } else {
                StreamEvent::NodeUpdated { node: node.clone() }
            },
        );

        // Edge compensation (no edge.created events exist): diff this
        // revision's edges into the store now, and again a few seconds later —
        // writers typically attach edges right *after* the revision that
        // triggered this event, which the immediate check cannot see.
        self.diff_edges(&rev.kref).await;
        let (client, store, tx, kref) = (
            self.client.clone(),
            self.store.clone(),
            self.tx.clone(),
            rev.kref.clone(),
        );
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(4)).await;
            let me = LiveEdgeCheck { client, store, tx };
            me.diff(&kref).await;
        });
    }

    async fn diff_edges(&self, rev_kref: &kumiho::Kref) {
        LiveEdgeCheck {
            client: self.client.clone(),
            store: self.store.clone(),
            tx: self.tx.clone(),
        }
        .diff(rev_kref)
        .await;
    }

    async fn on_item_deleted(&self, ev: &Event) {
        let item_kref = item_uri(ev.kref.uri()).to_string();
        let removed = {
            let mut g = self.store.write().await;
            g.remove_item(&item_kref)
        };
        if let Some(id) = removed {
            tracing::info!("live: memory removed ({item_kref})");
            send(&self.tx, &StreamEvent::NodeRemoved { id });
        }
    }
}

/// Fetch a revision's edges and broadcast any the store hasn't seen.
struct LiveEdgeCheck {
    client: Client,
    store: Arc<RwLock<GraphStore>>,
    tx: broadcast::Sender<String>,
}

impl LiveEdgeCheck {
    async fn diff(&self, rev_kref: &kumiho::Kref) {
        let edges = match self
            .client
            .get_edges(rev_kref, "", EdgeDirection::Both)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("live get_edges({rev_kref}): {e}");
                return;
            }
        };
        let mut g = self.store.write().await;
        for e in edges {
            let src = item_uri(e.source_kref.uri()).to_string();
            let dst = item_uri(e.target_kref.uri()).to_string();
            if let Some(edge) = g.add_edge(&src, &dst, &e.edge_type) {
                tracing::info!("live: interlink {} —{}→ {}", edge.src, edge.ty, edge.dst);
                send(&self.tx, &StreamEvent::EdgeAdded { edge });
            }
        }
    }
}

/// Keep sockets warm and give the HUD a liveness pulse.
pub async fn heartbeat(tx: broadcast::Sender<String>) {
    let mut tick = tokio::time::interval(Duration::from_secs(20));
    loop {
        tick.tick().await;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        send(&tx, &StreamEvent::Heartbeat { ts });
    }
}
