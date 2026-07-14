//! Graph-native traversal for the Why/Impact explorer (kumiho-SDKs#65).
//!
//! Pure, bounded walks over the in-memory store graph — the same nodes and
//! edges the orb draws, so every result can be spotlighted 1:1. Deterministic,
//! LLM-free, sub-millisecond. (Deeper SDK-side traversal that could discover
//! edges outside the loaded window can slot in behind the same endpoints
//! later; the store already converges via the live edge re-checks.)

use crate::model::EdgeOut;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

pub const MAX_DEPTH: u32 = 3;
pub const NODE_CAP: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Out,
    In,
    Both,
}

impl Dir {
    pub fn parse(s: Option<&str>) -> Dir {
        match s {
            Some("out") => Dir::Out,
            Some("in") => Dir::In,
            _ => Dir::Both,
        }
    }
}

/// One traversed node with its BFS distance from the origin.
#[derive(Debug, Clone, Serialize)]
pub struct HopNode {
    pub id: u32,
    pub hop: u32,
}

/// A traversed subgraph: nodes with hop distance + the edges walked.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Subgraph {
    pub nodes: Vec<HopNode>,
    pub edges: Vec<EdgeOut>,
    pub truncated: bool,
}

/// Bounded BFS from `origin` along `types` edges in direction `dir`.
///
/// `dir` is relative to the current frontier node: `Out` follows edges where
/// the frontier is the source (e.g. this `DERIVED_FROM` → upstream), `In`
/// follows edges pointing at it (e.g. who `DEPENDS_ON` this). The origin
/// itself is not included in `nodes`.
pub fn bfs(
    edges: &[EdgeOut],
    dead: impl Fn(u32) -> bool,
    origin: u32,
    types: &HashSet<String>,
    dir: Dir,
    max_depth: u32,
    cap: usize,
) -> Subgraph {
    let max_depth = max_depth.clamp(1, MAX_DEPTH);
    let mut out = Subgraph::default();
    let mut seen: HashSet<u32> = HashSet::from([origin]);
    let mut edge_seen: HashSet<usize> = HashSet::new();
    let mut frontier: VecDeque<(u32, u32)> = VecDeque::from([(origin, 0)]);

    while let Some((at, hop)) = frontier.pop_front() {
        if hop >= max_depth {
            continue;
        }
        for (i, e) in edges.iter().enumerate() {
            if !types.is_empty() && !types.contains(&e.ty) {
                continue;
            }
            let next = if e.src == at && matches!(dir, Dir::Out | Dir::Both) {
                Some(e.dst)
            } else if e.dst == at && matches!(dir, Dir::In | Dir::Both) {
                Some(e.src)
            } else {
                None
            };
            let Some(next) = next else { continue };
            if dead(next) {
                continue;
            }
            if edge_seen.insert(i) {
                out.edges.push(e.clone());
            }
            if seen.insert(next) {
                if out.nodes.len() >= cap {
                    out.truncated = true;
                    return out;
                }
                out.nodes.push(HopNode {
                    id: next,
                    hop: hop + 1,
                });
                frontier.push_back((next, hop + 1));
            }
        }
    }
    out
}

/// Undirected shortest path between two nodes over all edge types.
/// Returns the ordered node chain (inclusive) and the edges along it.
pub fn shortest_path(
    edges: &[EdgeOut],
    dead: impl Fn(u32) -> bool,
    from: u32,
    to: u32,
    max_depth: u32,
) -> Option<(Vec<u32>, Vec<EdgeOut>)> {
    if from == to {
        return Some((vec![from], vec![]));
    }
    // adjacency with the edge index that connects each pair
    let mut adj: HashMap<u32, Vec<(u32, usize)>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        adj.entry(e.src).or_default().push((e.dst, i));
        adj.entry(e.dst).or_default().push((e.src, i));
    }
    let mut parent: HashMap<u32, (u32, usize)> = HashMap::new();
    let mut q: VecDeque<(u32, u32)> = VecDeque::from([(from, 0)]);
    let mut seen: HashSet<u32> = HashSet::from([from]);
    while let Some((at, d)) = q.pop_front() {
        if d >= max_depth {
            continue;
        }
        for &(next, ei) in adj.get(&at).into_iter().flatten() {
            if dead(next) || !seen.insert(next) {
                continue;
            }
            parent.insert(next, (at, ei));
            if next == to {
                // rebuild chain
                let mut chain = vec![to];
                let mut path_edges = Vec::new();
                let mut cur = to;
                while cur != from {
                    let (prev, ei) = parent[&cur];
                    path_edges.push(edges[ei].clone());
                    chain.push(prev);
                    cur = prev;
                }
                chain.reverse();
                path_edges.reverse();
                return Some((chain, path_edges));
            }
            q.push_back((next, d + 1));
        }
    }
    None
}

/// Parse a comma-separated list of edge types; uppercase A-Z_ only.
pub fn parse_types(raw: Option<&str>) -> HashSet<String> {
    raw.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(src: u32, dst: u32, ty: &str) -> EdgeOut {
        EdgeOut {
            src,
            dst,
            ty: ty.into(),
        }
    }

    // 0 --DERIVED_FROM--> 1 --DERIVED_FROM--> 2
    // 3 --DEPENDS_ON--> 0        4 --ABOUT--> 2
    fn graph() -> Vec<EdgeOut> {
        vec![
            e(0, 1, "DERIVED_FROM"),
            e(1, 2, "DERIVED_FROM"),
            e(3, 0, "DEPENDS_ON"),
            e(4, 2, "ABOUT"),
        ]
    }

    #[test]
    fn why_walk_is_multi_hop_upstream() {
        let g = graph();
        let types = parse_types(Some("DERIVED_FROM,MOTIVATED_BY"));
        let sub = bfs(&g, |_| false, 0, &types, Dir::Out, 3, 80);
        let ids: Vec<u32> = sub.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(sub.nodes[0].hop, 1);
        assert_eq!(sub.nodes[1].hop, 2);
        assert_eq!(sub.edges.len(), 2);
    }

    #[test]
    fn impact_walk_follows_incoming() {
        let g = graph();
        let types = parse_types(Some("DEPENDS_ON,IMPLEMENTED_IN"));
        let sub = bfs(&g, |_| false, 0, &types, Dir::In, 3, 80);
        assert_eq!(sub.nodes.len(), 1);
        assert_eq!(sub.nodes[0].id, 3);
    }

    #[test]
    fn depth_and_cap_bound_the_walk() {
        let g = graph();
        let types = parse_types(Some("DERIVED_FROM"));
        let d1 = bfs(&g, |_| false, 0, &types, Dir::Out, 1, 80);
        assert_eq!(d1.nodes.len(), 1);
        let capped = bfs(&g, |_| false, 0, &types, Dir::Out, 3, 1);
        assert!(capped.truncated);
        assert_eq!(capped.nodes.len(), 1);
    }

    #[test]
    fn dead_nodes_are_skipped() {
        let g = graph();
        let types = parse_types(Some("DERIVED_FROM"));
        let sub = bfs(&g, |id| id == 1, 0, &types, Dir::Out, 3, 80);
        assert!(sub.nodes.is_empty());
    }

    #[test]
    fn path_connects_across_types_undirected() {
        let g = graph();
        // 3 → 0 → 1 → 2 ← 4 : path from 3 to 4 spans three edge types
        let (chain, path_edges) = shortest_path(&g, |_| false, 3, 4, 6).unwrap();
        assert_eq!(chain, vec![3, 0, 1, 2, 4]);
        assert_eq!(path_edges.len(), 4);
        assert!(shortest_path(&g, |_| false, 3, 4, 2).is_none()); // depth bound
        assert_eq!(shortest_path(&g, |_| false, 2, 2, 6).unwrap().0, vec![2]);
    }

    #[test]
    fn type_parsing_rejects_junk() {
        let t = parse_types(Some("DERIVED_FROM, drop table, ABOUT,"));
        assert_eq!(t.len(), 2);
        assert!(t.contains("DERIVED_FROM") && t.contains("ABOUT"));
    }
}
