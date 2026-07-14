//! The backend↔frontend contract (JSON over REST + WebSocket) and the
//! in-memory [`GraphStore`] the server maintains from the live graph.
//!
//! Identity model: one dashboard **node = one memory item** (its revisions are
//! versions of the same memory). Cross-item revision edges become item-level
//! interlinks; same-item `SUPERSEDES` chains are revision lineage, surfaced in
//! the detail card rather than drawn as 3D edges.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Which of the two memory families a node belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryClass {
    /// Conversation-derived memory (`conversation`, `fact`, `entity`, …).
    Conversation,
    /// Code decision memory (`code_decision`, `code_anchor`, `code_evidence`, …).
    Code,
}

/// A memory space (rendered as its own sphere in per-space mode).
#[derive(Debug, Clone, Serialize)]
pub struct SpaceOut {
    pub id: u32,
    /// Full path, e.g. `/CognitiveMemory/work/kumiho-memory`.
    pub path: String,
}

/// One memory point.
#[derive(Debug, Clone, Serialize)]
pub struct NodeOut {
    pub id: u32,
    /// Item kref URI (revision-agnostic identity).
    pub kref: String,
    /// `conversation` or `code` — drives color + filters.
    pub kind: MemoryClass,
    /// The raw item kind (`conversation`, `fact`, `code_decision`, …).
    pub item_kind: String,
    pub title: String,
    /// Index into the snapshot `spaces` array.
    pub space: u32,
    /// Source identity: originating client when recorded, else the author.
    pub source: String,
    /// Memory-layer type from revision metadata (`fact`, `synthesis`, …).
    pub memory_type: String,
    /// ISO-8601 creation time of the item.
    pub created_at: String,
    /// ISO-8601 creation time of the newest revision.
    pub updated_at: String,
    /// Revision count (belief-revision depth).
    pub revs: u32,
    /// Deterministic layout seed (FNV-1a of the item kref).
    pub seed: u32,
    #[serde(skip)]
    pub dead: bool,
}

/// A typed interlink between two memories.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeOut {
    pub src: u32,
    pub dst: u32,
    #[serde(rename = "type")]
    pub ty: String,
}

/// Tenant usage for the HUD (real numbers from `GetTenantUsage`).
#[derive(Debug, Clone, Serialize)]
pub struct TenantOut {
    pub node_count: i64,
    pub node_limit: i64,
    pub tenant_id: String,
}

/// One typed link in a node's detail card.
#[derive(Debug, Clone, Serialize)]
pub struct DetailLink {
    #[serde(rename = "type")]
    pub ty: String,
    /// `out` (this node → other) or `in`.
    pub dir: String,
    /// Node id when the other endpoint is a known memory node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    pub title: String,
    pub kref: String,
}

/// Full detail for one node (fetched on demand).
#[derive(Debug, Clone, Serialize)]
pub struct DetailOut {
    #[serde(flatten)]
    pub node: NodeOut,
    pub space_path: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub links: Vec<DetailLink>,
    /// Revision numbers, newest first (the SUPERSEDES lineage within the item).
    pub revisions: Vec<i32>,
}

/// One ranked hit from the server-side semantic search (Tier 2).
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: u32,
    pub title: String,
    pub kind: MemoryClass,
    pub score: f32,
}

/// Everything the server pushes over the WebSocket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum StreamEvent {
    /// First frame on every socket.
    Hello { proto: u32, server: String },
    /// Upstream/server state: `core` = snapshot loaded, `live` = event stream
    /// connected. `info` is a short human-readable status line.
    Status { core: bool, live: bool, info: String },
    /// The full graph (sent once per socket after `core` is ready).
    Snapshot {
        /// Unix milliseconds.
        generated_at: u64,
        endpoint: String,
        spaces: Vec<SpaceOut>,
        nodes: Vec<NodeOut>,
        edges: Vec<EdgeOut>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tenant: Option<TenantOut>,
    },
    NodeAdded { node: NodeOut },
    NodeUpdated { node: NodeOut },
    EdgeAdded { edge: EdgeOut },
    NodeRemoved { id: u32 },
    Heartbeat { ts: u64 },
}

/// Fields extracted from an item + its latest revision when (up)serting a node.
#[derive(Debug, Clone)]
pub struct NodeSeed {
    pub item_kref: String,
    pub item_kind: String,
    pub class: MemoryClass,
    pub title: String,
    pub space_path: String,
    pub source: String,
    pub memory_type: String,
    pub created_at: String,
    pub updated_at: String,
    pub revs: u32,
}

/// 32-bit FNV-1a — a stable, implementation-independent layout seed.
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// Strip the `?r=…` query so a revision kref keys its item.
pub fn item_uri(kref_uri: &str) -> &str {
    kref_uri.split('?').next().unwrap_or(kref_uri)
}

/// The server's in-memory picture of the memory graph.
#[derive(Default)]
pub struct GraphStore {
    pub nodes: Vec<NodeOut>,
    pub edges: Vec<EdgeOut>,
    pub spaces: Vec<SpaceOut>,
    pub tenant: Option<TenantOut>,
    by_item: HashMap<String, u32>,
    by_space: HashMap<String, u32>,
    edge_set: HashSet<(u32, u32, String)>,
}

impl GraphStore {
    pub fn node_id(&self, item_kref: &str) -> Option<u32> {
        self.by_item.get(item_kref).copied()
    }

    pub fn intern_space(&mut self, path: &str) -> u32 {
        if let Some(id) = self.by_space.get(path) {
            return *id;
        }
        let id = self.spaces.len() as u32;
        self.spaces.push(SpaceOut {
            id,
            path: path.to_string(),
        });
        self.by_space.insert(path.to_string(), id);
        id
    }

    /// Insert or refresh a node. Returns the node and whether it was new.
    pub fn upsert(&mut self, seed: NodeSeed) -> (NodeOut, bool) {
        let space = self.intern_space(&seed.space_path);
        if let Some(&id) = self.by_item.get(&seed.item_kref) {
            let n = &mut self.nodes[id as usize];
            n.title = seed.title;
            n.source = seed.source;
            n.memory_type = seed.memory_type;
            n.updated_at = seed.updated_at;
            n.revs = n.revs.max(seed.revs);
            n.dead = false;
            return (n.clone(), false);
        }
        let id = self.nodes.len() as u32;
        let node = NodeOut {
            id,
            seed: fnv1a(&seed.item_kref),
            kref: seed.item_kref.clone(),
            kind: seed.class,
            item_kind: seed.item_kind,
            title: seed.title,
            space,
            source: seed.source,
            memory_type: seed.memory_type,
            created_at: seed.created_at,
            updated_at: seed.updated_at,
            revs: seed.revs,
            dead: false,
        };
        self.nodes.push(node.clone());
        self.by_item.insert(seed.item_kref, id);
        (node, true)
    }

    /// Add a cross-item interlink. `None` when an endpoint is unknown, the
    /// edge is a self-loop (same item), or it already exists.
    pub fn add_edge(&mut self, src_item: &str, dst_item: &str, ty: &str) -> Option<EdgeOut> {
        let (src, dst) = (self.node_id(src_item)?, self.node_id(dst_item)?);
        if src == dst || self.nodes[src as usize].dead || self.nodes[dst as usize].dead {
            return None;
        }
        if !self.edge_set.insert((src, dst, ty.to_string())) {
            return None;
        }
        let edge = EdgeOut {
            src,
            dst,
            ty: ty.to_string(),
        };
        self.edges.push(edge.clone());
        Some(edge)
    }

    /// Tombstone a node (ids stay stable) and prune its edges.
    pub fn remove_item(&mut self, item_kref: &str) -> Option<u32> {
        let id = self.by_item.get(item_kref).copied()?;
        let n = &mut self.nodes[id as usize];
        if n.dead {
            return None;
        }
        n.dead = true;
        self.edges.retain(|e| e.src != id && e.dst != id);
        self.edge_set.retain(|(s, d, _)| *s != id && *d != id);
        Some(id)
    }

    /// Live (non-tombstoned) nodes, for snapshot serialization.
    pub fn live_nodes(&self) -> Vec<NodeOut> {
        self.nodes.iter().filter(|n| !n.dead).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(kref: &str, title: &str) -> NodeSeed {
        NodeSeed {
            item_kref: kref.into(),
            item_kind: "conversation".into(),
            class: MemoryClass::Conversation,
            title: title.into(),
            space_path: "/P/s".into(),
            source: "tester".into(),
            memory_type: "fact".into(),
            created_at: "2026-07-13T00:00:00+00:00".into(),
            updated_at: "2026-07-13T00:00:00+00:00".into(),
            revs: 1,
        }
    }

    #[test]
    fn upsert_is_idempotent_by_item() {
        let mut g = GraphStore::default();
        let (a, added) = g.upsert(seed("kref://P/s/a.conversation", "a"));
        assert!(added);
        let (b, added2) = g.upsert(seed("kref://P/s/a.conversation", "a2"));
        assert!(!added2);
        assert_eq!(a.id, b.id);
        assert_eq!(g.nodes[a.id as usize].title, "a2");
        assert_eq!(g.spaces.len(), 1);
    }

    #[test]
    fn edges_dedupe_and_skip_unknown_or_self() {
        let mut g = GraphStore::default();
        g.upsert(seed("kref://P/s/a.conversation", "a"));
        g.upsert(seed("kref://P/s/b.conversation", "b"));
        assert!(g
            .add_edge(
                "kref://P/s/a.conversation",
                "kref://P/s/b.conversation",
                "REFERENCED"
            )
            .is_some());
        // duplicate
        assert!(g
            .add_edge(
                "kref://P/s/a.conversation",
                "kref://P/s/b.conversation",
                "REFERENCED"
            )
            .is_none());
        // self-loop (same item, different revision krefs collapse upstream)
        assert!(g
            .add_edge(
                "kref://P/s/a.conversation",
                "kref://P/s/a.conversation",
                "SUPERSEDES"
            )
            .is_none());
        // unknown endpoint
        assert!(g
            .add_edge(
                "kref://P/s/a.conversation",
                "kref://P/s/zz.conversation",
                "ABOUT"
            )
            .is_none());
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn remove_prunes_edges_and_keeps_ids_stable() {
        let mut g = GraphStore::default();
        g.upsert(seed("kref://P/s/a.conversation", "a"));
        g.upsert(seed("kref://P/s/b.conversation", "b"));
        g.add_edge(
            "kref://P/s/a.conversation",
            "kref://P/s/b.conversation",
            "REFERENCED",
        );
        let removed = g.remove_item("kref://P/s/a.conversation");
        assert_eq!(removed, Some(0));
        assert!(g.edges.is_empty());
        assert_eq!(g.live_nodes().len(), 1);
        assert_eq!(g.live_nodes()[0].id, 1);
    }

    #[test]
    fn item_uri_strips_revision_query() {
        assert_eq!(
            item_uri("kref://P/s/a.conversation?r=15"),
            "kref://P/s/a.conversation"
        );
        assert_eq!(item_uri("kref://P/s/a.conversation"), "kref://P/s/a.conversation");
    }
}
