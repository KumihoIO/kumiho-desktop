//! Reading the real memory graph through the `kumiho` SDK.
//!
//! Snapshot strategy: one `item_search` sweep per configured memory kind
//! (server-side kind filter — no walking hundreds of spaces), then per item
//! one `get_revisions` call (latest metadata + belief-revision depth) and
//! `get_edges` over the newest revisions, all under bounded concurrency.

use crate::config::Config;
use crate::model::{
    item_uri, DetailLink, DetailOut, GraphStore, NodeSeed, RevisionMeta, RevisionOut, TenantOut,
};
use futures::stream::{self, StreamExt};
use kumiho::{Client, EdgeDirection, Item, Revision};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Metadata keys that may carry the originating client, most specific first.
/// Today's writers record none of them (SDK gap, see README) — the author
/// identity is the honest fallback.
const SOURCE_KEYS: &[&str] = &["source_client", "client", "agent", "created_by"];

pub struct SnapshotStats {
    pub items: usize,
    pub edges: usize,
    pub skipped: usize,
    pub elapsed_ms: u128,
}

/// `(src_item, dst_item, type)` collected during the crawl; applied to the
/// store once all nodes are interned so endpoint lookup can't race insertion.
type RawEdge = (String, String, String);

fn meta<'a>(rev: &'a Revision, key: &str) -> &'a str {
    rev.metadata.get(key).map(String::as_str).unwrap_or("")
}

fn source_of(rev: &Revision, item: &Item) -> String {
    for k in SOURCE_KEYS {
        let v = meta(rev, k);
        if !v.is_empty() {
            return v.to_string();
        }
    }
    if !item.username.is_empty() {
        item.username.clone()
    } else {
        item.author.clone()
    }
}

fn space_path_of(item: &Item) -> String {
    let space = item.kref.space();
    if space.is_empty() {
        format!("/{}", item.kref.project())
    } else {
        format!("/{}/{}", item.kref.project(), space)
    }
}

/// Latest revision = the one flagged `latest`, else the highest number
/// (mirrors `Item::get_latest_revision`).
fn latest_of(revs: &[Revision]) -> Option<&Revision> {
    revs.iter()
        .find(|r| r.latest)
        .or_else(|| revs.iter().max_by_key(|r| r.number))
}

fn node_seed(cfg: &Config, item: &Item, revs: &[Revision]) -> Option<NodeSeed> {
    let class = cfg.classify(&item.kind)?;
    let latest = latest_of(revs)?;
    let title = {
        let t = meta(latest, "title");
        if t.is_empty() {
            item.item_name.clone()
        } else {
            t.to_string()
        }
    };
    Some(NodeSeed {
        item_kref: item_uri(item.kref.uri()).to_string(),
        item_kind: item.kind.clone(),
        class,
        title,
        space_path: space_path_of(item),
        source: source_of(latest, item),
        memory_type: meta(latest, "memory_type").to_string(),
        created_at: item.created_at.clone().unwrap_or_default(),
        updated_at: latest
            .created_at
            .clone()
            .or_else(|| item.created_at.clone())
            .unwrap_or_default(),
        revs: revs.len() as u32,
    })
}

/// Fetch cross-item edges seen from the newest `edge_revs` revisions (0 = all).
async fn edges_of(
    client: &Client,
    item_kref: &str,
    revs: &[Revision],
    edge_revs: usize,
) -> Vec<RawEdge> {
    let mut ordered: Vec<&Revision> = revs.iter().collect();
    ordered.sort_by_key(|r| -r.number);
    if edge_revs > 0 {
        ordered.truncate(edge_revs);
    }
    let mut out = Vec::new();
    for rev in ordered {
        match client.get_edges(&rev.kref, "", EdgeDirection::Both).await {
            Ok(edges) => {
                for e in edges {
                    let src = item_uri(e.source_kref.uri()).to_string();
                    let dst = item_uri(e.target_kref.uri()).to_string();
                    if src != dst {
                        out.push((src, dst, e.edge_type));
                    }
                }
            }
            Err(e) => tracing::debug!("get_edges({item_kref}): {e}"),
        }
    }
    out
}

/// Crawl the graph and populate `store`. `progress(done, total)` is called as
/// items complete (throttled by the caller).
pub async fn load_snapshot(
    client: &Client,
    cfg: &Config,
    store: &Arc<RwLock<GraphStore>>,
    progress: impl Fn(usize, usize) + Send + Sync,
) -> Result<SnapshotStats, kumiho::Error> {
    let t0 = std::time::Instant::now();

    // 1. Enumerate memory items by kind (server-side filter, paginated).
    let mut items: Vec<Item> = Vec::new();
    for kind in cfg.kinds() {
        let mut cursor: Option<String> = None;
        loop {
            let page = client
                .item_search("", "", kind, Some(cfg.page_size), cursor.clone(), false)
                .await?;
            cursor = page.next_cursor.clone();
            items.extend(page);
            if cursor.is_none() {
                break;
            }
        }
    }
    let total = items.len();
    tracing::info!("snapshot: {total} memory items to load");

    // 2. Per item: revisions (metadata) + edges, under bounded concurrency.
    let done = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let results: Vec<(NodeSeed, Vec<RawEdge>)> = stream::iter(items)
        .map(|item| {
            let done = &done;
            let skipped = &skipped;
            let progress = &progress;
            async move {
                let revs = match client.get_revisions(&item.kref).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::debug!("get_revisions({}): {e}", item.kref);
                        skipped.fetch_add(1, Ordering::Relaxed);
                        progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                        return None;
                    }
                };
                let seed = node_seed(cfg, &item, &revs);
                if seed.is_none() {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
                let edges = if seed.is_some() {
                    edges_of(client, item.kref.uri(), &revs, cfg.edge_revs).await
                } else {
                    Vec::new()
                };
                progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                seed.map(|s| (s, edges))
            }
        })
        .buffer_unordered(cfg.concurrency)
        .filter_map(|r| async move { r })
        .collect()
        .await;

    // 3. Tenant usage for the HUD (best-effort).
    let tenant = client.get_tenant_usage().await.ok().map(|t| TenantOut {
        node_count: t.node_count,
        node_limit: t.node_limit,
        tenant_id: t.tenant_id,
    });

    // 4. Apply atomically: nodes first, then edges (endpoints must exist).
    let mut g = store.write().await;
    for (seed, _) in &results {
        g.upsert(seed.clone());
    }
    let mut edge_count = 0;
    for (_, edges) in &results {
        for (src, dst, ty) in edges {
            if g.add_edge(src, dst, ty).is_some() {
                edge_count += 1;
            }
        }
    }
    g.tenant = tenant;
    let stats = SnapshotStats {
        items: g.nodes.len(),
        edges: edge_count,
        skipped: skipped.load(Ordering::Relaxed),
        elapsed_ms: t0.elapsed().as_millis(),
    };
    Ok(stats)
}

/// On-demand detail for one node: fresh summary/tags/lineage + merged links
/// (fresh edges from the newest revisions ∪ edges already in the store).
pub async fn fetch_detail(
    client: &Client,
    cfg: &Config,
    store: &Arc<RwLock<GraphStore>>,
    id: u32,
) -> Option<DetailOut> {
    let (node, space_path) = {
        let g = store.read().await;
        let n = g.nodes.get(id as usize)?.clone();
        if n.dead {
            return None;
        }
        let sp = g.spaces.get(n.space as usize)?.path.clone();
        (n, sp)
    };

    let kref = kumiho::Kref::unchecked(node.kref.clone());
    let revs = client.get_revisions(&kref).await.ok().unwrap_or_default();
    let latest = latest_of(&revs);
    let summary = latest
        .map(|r| {
            let s = meta(r, "summary");
            if s.is_empty() {
                meta(r, "embedding_text")
            } else {
                s
            }
            .to_string()
        })
        .unwrap_or_default();
    let tags = latest.map(|r| r.tags.clone()).unwrap_or_default();
    let mut revisions: Vec<i32> = revs.iter().map(|r| r.number).collect();
    revisions.sort_unstable_by(|a, b| b.cmp(a));

    // Fresh edges from the newest revisions of *this* item…
    let raw = edges_of(client, &node.kref, &revs, cfg.edge_revs.max(1)).await;
    let g = store.read().await;
    let mut links: Vec<DetailLink> = Vec::new();
    let push = |ty: &str, dir: &str, other: &str, links: &mut Vec<DetailLink>| {
        if links
            .iter()
            .any(|l| l.ty == ty && l.dir == dir && l.kref == other)
        {
            return;
        }
        let known = g.node_id(other).map(|oid| &g.nodes[oid as usize]);
        links.push(DetailLink {
            ty: ty.to_string(),
            dir: dir.to_string(),
            id: known.map(|n| n.id),
            title: known.map(|n| n.title.clone()).unwrap_or_else(|| {
                // derive a readable name from the kref path for foreign kinds
                other.rsplit('/').next().unwrap_or(other).to_string()
            }),
            kref: other.to_string(),
        });
    };
    for (src, dst, ty) in &raw {
        if src == &node.kref {
            push(ty, "out", dst, &mut links);
        } else {
            push(ty, "in", src, &mut links);
        }
    }
    // …plus interlinks the store already knows (e.g. discovered live).
    for e in g.edges.iter() {
        if e.src == node.id {
            let other = g.nodes[e.dst as usize].kref.clone();
            push(&e.ty, "out", &other, &mut links);
        } else if e.dst == node.id {
            let other = g.nodes[e.src as usize].kref.clone();
            push(&e.ty, "in", &other, &mut links);
        }
    }
    drop(g);

    Some(DetailOut {
        node,
        space_path,
        summary,
        tags,
        links,
        revisions,
    })
}

/// The node's item kref, if it exists and is live.
async fn node_kref(store: &Arc<RwLock<GraphStore>>, id: u32) -> Option<String> {
    let g = store.read().await;
    let n = g.nodes.get(id as usize)?;
    (!n.dead).then(|| n.kref.clone())
}

/// Revision lineage, newest first — the time-travel index (#65).
pub async fn fetch_revisions(
    client: &Client,
    store: &Arc<RwLock<GraphStore>>,
    id: u32,
) -> Option<Vec<RevisionMeta>> {
    let uri = node_kref(store, id).await?;
    let kref = kumiho::Kref::unchecked(uri);
    let mut revs = client.get_revisions(&kref).await.ok()?;
    revs.sort_unstable_by_key(|r| std::cmp::Reverse(r.number));
    Some(
        revs.iter()
            .map(|r| RevisionMeta {
                n: r.number,
                title: meta(r, "title").to_string(),
                created_at: r.created_at.clone().unwrap_or_default(),
                latest: r.latest,
            })
            .collect(),
    )
}

/// One historical revision's content ("what did I used to think?").
pub async fn fetch_revision(
    client: &Client,
    store: &Arc<RwLock<GraphStore>>,
    id: u32,
    number: i32,
) -> Option<RevisionOut> {
    let uri = node_kref(store, id).await?;
    let rev = client
        .get_revision(&format!("{uri}?r={number}"))
        .await
        .ok()?;
    let summary = {
        let s = meta(&rev, "summary");
        if s.is_empty() {
            meta(&rev, "embedding_text")
        } else {
            s
        }
        .to_string()
    };
    Some(RevisionOut {
        n: rev.number,
        title: meta(&rev, "title").to_string(),
        summary,
        memory_type: meta(&rev, "memory_type").to_string(),
        tags: rev.tags.clone(),
        created_at: rev.created_at.clone().unwrap_or_default(),
    })
}
