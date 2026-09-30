//! Tree summary export as JSON or CSV (FR-MAP-16, FR-SET-04).

use serde::Serialize;
use sr_core::{NodeFlags, NodeId, NodeKind, SizeMode, Tree};

#[derive(Debug, Serialize)]
pub struct NodeSummary {
    pub path: String,
    pub kind: NodeKind,
    pub allocated: u64,
    pub apparent: u64,
    pub items: u32,
    pub mtime: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<NodeSummary>,
}

/// Nested summary of `id` down to `max_depth` levels, children sorted by
/// size and truncated to `max_children` per directory.
pub fn summarize(
    tree: &Tree,
    id: NodeId,
    max_depth: usize,
    max_children: usize,
    mode: SizeMode,
) -> NodeSummary {
    let n = tree.node(id);
    let mut children = Vec::new();
    if max_depth > 0 && n.is_dir() {
        let mut kids: Vec<NodeId> = live_children(tree, id).collect();
        kids.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).size(mode)));
        kids.truncate(max_children);
        children = kids
            .into_iter()
            .map(|c| summarize(tree, c, max_depth - 1, max_children, mode))
            .collect();
    }
    NodeSummary {
        path: tree.path(id).display().to_string(),
        kind: n.kind,
        allocated: n.allocated,
        apparent: n.apparent,
        items: n.items,
        mtime: n.mtime,
        children,
    }
}

fn live_children(tree: &Tree, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    tree.children(id)
        .filter(move |&c| !tree.node(c).flags.contains(NodeFlags::REMOVED))
}

pub fn to_json(tree: &Tree, max_depth: usize, max_children: usize, mode: SizeMode) -> String {
    #[derive(Serialize)]
    struct Doc<'a> {
        root: NodeSummary,
        issues: &'a [sr_core::ScanIssue],
    }
    let doc = Doc {
        root: summarize(tree, tree.root(), max_depth, max_children, mode),
        issues: &tree.issues,
    };
    serde_json::to_string_pretty(&doc).expect("summary serializes")
}

/// Flat CSV: one row per node down to `max_depth`.
pub fn to_csv(tree: &Tree, max_depth: usize, mode: SizeMode) -> String {
    let mut out = String::from("path,kind,allocated,apparent,items,mtime,depth\n");
    let mut stack = vec![(tree.root(), 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let n = tree.node(id);
        out.push_str(&format!(
            "{},{:?},{},{},{},{},{}\n",
            csv_field(&tree.path(id).display().to_string()),
            n.kind,
            n.allocated,
            n.apparent,
            n.items,
            n.mtime,
            depth
        ));
        if depth < max_depth {
            let mut kids: Vec<NodeId> = live_children(tree, id).collect();
            kids.sort_by_key(|&c| tree.node(c).size(mode));
            stack.extend(kids.into_iter().map(|c| (c, depth + 1)));
        }
    }
    out
}

pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
