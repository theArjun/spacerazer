//! Flat arena file tree (SRS §6.3).
//!
//! Nodes are stored in a single `Vec` and linked via parent / first-child /
//! next-sibling indices. Directory sizes are aggregates maintained
//! incrementally as children are inserted, so a partially streamed tree is
//! always internally consistent and can be rendered mid-scan (FR-SCAN-04).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use crate::names::{NameArena, NameId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
    Other,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct NodeFlags: u8 {
        /// File has more than one hard link.
        const HARDLINKED = 1 << 0;
        /// Another link to the same physical file was already counted; this
        /// node contributes nothing to aggregates (FR-SCAN-06).
        const HARDLINK_DUP = 1 << 1;
        /// Cloud placeholder (online-only) file (FR-SCAN-16).
        const CLOUD = 1 << 2;
        /// Reading this entry (or a directory's listing) failed (FR-SCAN-09).
        const ERROR = 1 << 3;
        /// Staged in the Trash Drawer (FR-MAP-13).
        const STAGED = 1 << 4;
        /// Excluded from the scan by a rule.
        const EXCLUDED = 1 << 5;
        /// Removed from the tree (deleted); the slot is dead.
        const REMOVED = 1 << 6;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SizeMode {
    #[default]
    Allocated,
    Apparent,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub name: NameId,
    pub kind: NodeKind,
    pub flags: NodeFlags,
    /// Own size (files) or aggregate of descendants (dirs).
    pub apparent: u64,
    pub allocated: u64,
    /// Descendant count (dirs); 0 for files.
    pub items: u32,
    /// Seconds since the Unix epoch.
    pub mtime: i64,
}

impl Node {
    pub fn size(&self, mode: SizeMode) -> u64 {
        match mode {
            SizeMode::Allocated => self.allocated,
            SizeMode::Apparent => self.apparent,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

/// Attributes of a new entry being inserted into the tree.
#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub kind: NodeKind,
    pub apparent: u64,
    pub allocated: u64,
    pub mtime: i64,
    pub flags: NodeFlags,
    /// (device, inode) or (volume serial, file index), when the entry has
    /// more than one hard link.
    pub file_id: Option<(u64, u64)>,
}

impl EntryInfo {
    pub fn dir(mtime: i64) -> Self {
        Self {
            kind: NodeKind::Dir,
            apparent: 0,
            allocated: 0,
            mtime,
            flags: NodeFlags::empty(),
            file_id: None,
        }
    }

    pub fn file(size: u64, mtime: i64) -> Self {
        Self {
            kind: NodeKind::File,
            apparent: size,
            allocated: size,
            mtime,
            flags: NodeFlags::empty(),
            file_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<Node>,
    names: NameArena,
    file_ids: HashMap<(u64, u64), NodeId>,
    root_path: PathBuf,
    pub issues: Vec<ScanIssue>,
}

impl Tree {
    /// Create a tree whose root node represents `root_path`.
    pub fn new(root_path: impl Into<PathBuf>, root_mtime: i64) -> Self {
        let root_path = root_path.into();
        let mut names = NameArena::new();
        let name = names.intern(root_path.as_os_str());
        let root = Node {
            parent: None,
            first_child: None,
            next_sibling: None,
            name,
            kind: NodeKind::Dir,
            flags: NodeFlags::empty(),
            apparent: 0,
            allocated: 0,
            items: 0,
            mtime: root_mtime,
        };
        Self {
            nodes: vec![root],
            names,
            file_ids: HashMap::new(),
            root_path,
            issues: Vec::new(),
        }
    }

    pub const ROOT: NodeId = NodeId(0);

    pub fn root(&self) -> NodeId {
        Self::ROOT
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    pub fn name(&self, id: NodeId) -> &OsStr {
        self.names.get(self.nodes[id.index()].name)
    }

    pub fn name_lossy(&self, id: NodeId) -> String {
        if id == Self::ROOT {
            return self.root_path.display().to_string();
        }
        self.name(id).to_string_lossy().into_owned()
    }

    /// Insert `name` under `parent`, propagating sizes and counts to every
    /// ancestor. Hard-linked files already seen under the same identity are
    /// inserted with `HARDLINK_DUP` and contribute nothing to aggregates.
    pub fn add_child(&mut self, parent: NodeId, name: &OsStr, info: EntryInfo) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        let mut flags = info.flags;
        if let Some(fid) = info.file_id {
            flags |= NodeFlags::HARDLINKED;
            match self.file_ids.entry(fid) {
                std::collections::hash_map::Entry::Occupied(_) => flags |= NodeFlags::HARDLINK_DUP,
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(id);
                }
            }
        }
        let counted = !flags.contains(NodeFlags::HARDLINK_DUP);
        let (apparent, allocated) = if info.kind == NodeKind::Dir {
            (0, 0)
        } else {
            (info.apparent, info.allocated)
        };
        let name = self.names.intern(name);
        let prev_first = self.nodes[parent.index()].first_child;
        self.nodes.push(Node {
            parent: Some(parent),
            first_child: None,
            next_sibling: prev_first,
            name,
            kind: info.kind,
            flags,
            apparent,
            allocated,
            items: 0,
            mtime: info.mtime,
        });
        self.nodes[parent.index()].first_child = Some(id);
        let (da, dl) = if counted {
            (apparent, allocated)
        } else {
            (0, 0)
        };
        self.propagate(Some(parent), da as i128, dl as i128, 1);
        id
    }

    /// Record a scan issue and flag `node` (if given).
    pub fn add_issue(&mut self, node: Option<NodeId>, path: PathBuf, error: String) {
        if let Some(n) = node {
            self.nodes[n.index()].flags |= NodeFlags::ERROR;
        }
        self.issues.push(ScanIssue { path, error });
    }

    fn propagate(&mut self, mut cur: Option<NodeId>, da: i128, dl: i128, di: i64) {
        while let Some(id) = cur {
            let n = &mut self.nodes[id.index()];
            n.apparent = (n.apparent as i128 + da).max(0) as u64;
            n.allocated = (n.allocated as i128 + dl).max(0) as u64;
            n.items = (n.items as i64 + di).max(0) as u32;
            cur = n.parent;
        }
    }

    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            tree: self,
            next: self.nodes[id.index()].first_child,
        }
    }

    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), move |n| self.nodes[n.index()].parent)
    }

    pub fn depth(&self, id: NodeId) -> usize {
        self.ancestors(id).count() - 1
    }

    pub fn is_ancestor(&self, ancestor: NodeId, of: NodeId) -> bool {
        self.ancestors(of).any(|a| a == ancestor)
    }

    /// Full filesystem path of `id`.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts: Vec<&OsStr> = Vec::new();
        let mut cur = id;
        while cur != Self::ROOT {
            parts.push(self.name(cur));
            cur = self.nodes[cur.index()]
                .parent
                .expect("non-root node has a parent");
        }
        let mut p = self.root_path.clone();
        for part in parts.into_iter().rev() {
            p.push(part);
        }
        p
    }

    /// Resolve a filesystem path to a node, if it lies within this tree.
    pub fn find_path(&self, path: &Path) -> Option<NodeId> {
        let rel = path.strip_prefix(&self.root_path).ok()?;
        let mut cur = Self::ROOT;
        for comp in rel.components() {
            let name = comp.as_os_str();
            cur = self.children(cur).find(|&c| self.name(c) == name)?;
        }
        Some(cur)
    }

    /// Sort the children of `id` by size descending (then name), relinking
    /// the sibling list.
    pub fn sort_children(&mut self, id: NodeId, mode: SizeMode) {
        let mut kids: Vec<NodeId> = self.children(id).collect();
        if kids.len() < 2 {
            return;
        }
        kids.sort_by(|&a, &b| {
            let (na, nb) = (&self.nodes[a.index()], &self.nodes[b.index()]);
            nb.size(mode)
                .cmp(&na.size(mode))
                .then_with(|| self.name(a).cmp(self.name(b)))
        });
        self.nodes[id.index()].first_child = Some(kids[0]);
        for w in kids.windows(2) {
            self.nodes[w[0].index()].next_sibling = Some(w[1]);
        }
        self.nodes[kids[kids.len() - 1].index()].next_sibling = None;
    }

    /// Sort every directory's children by size.
    pub fn sort_all(&mut self, mode: SizeMode) {
        for i in 0..self.nodes.len() {
            if self.nodes[i].kind == NodeKind::Dir && self.nodes[i].first_child.is_some() {
                self.sort_children(NodeId(i as u32), mode);
            }
        }
    }

    /// Detach `id` (and its subtree) from the tree, subtracting its size from
    /// ancestors. Used after a successful deletion.
    pub fn remove(&mut self, id: NodeId) {
        let Some(parent) = self.nodes[id.index()].parent else {
            return;
        };
        if self.nodes[id.index()].flags.contains(NodeFlags::REMOVED) {
            return;
        }
        // Unlink from the parent's sibling list.
        let next = self.nodes[id.index()].next_sibling;
        if self.nodes[parent.index()].first_child == Some(id) {
            self.nodes[parent.index()].first_child = next;
        } else {
            let mut cur = self.nodes[parent.index()].first_child;
            while let Some(c) = cur {
                if self.nodes[c.index()].next_sibling == Some(id) {
                    self.nodes[c.index()].next_sibling = next;
                    break;
                }
                cur = self.nodes[c.index()].next_sibling;
            }
        }
        let n = &self.nodes[id.index()];
        let counted = !n.flags.contains(NodeFlags::HARDLINK_DUP);
        let (a, l) = if counted {
            (n.apparent, n.allocated)
        } else {
            (0, 0)
        };
        let items = n.items as i64 + 1;
        self.propagate(Some(parent), -(a as i128), -(l as i128), -items);
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            self.nodes[x.index()].flags |= NodeFlags::REMOVED;
            stack.extend(self.children(x));
        }
        self.nodes[id.index()].next_sibling = None;
    }

    /// Iterate every live node id in the subtree rooted at `id` (pre-order).
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            tree: self,
            stack: vec![id],
        }
    }

    /// The `n` largest files in the tree (FR-MAP-15).
    pub fn largest_files(&self, n: usize, mode: SizeMode) -> Vec<NodeId> {
        let mut heap = std::collections::BinaryHeap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.kind != NodeKind::File
                || node
                    .flags
                    .intersects(NodeFlags::REMOVED | NodeFlags::HARDLINK_DUP)
            {
                continue;
            }
            heap.push(std::cmp::Reverse((node.size(mode), i as u32)));
            if heap.len() > n {
                heap.pop();
            }
        }
        let mut out: Vec<_> = heap.into_iter().map(|r| r.0).collect();
        out.sort_by(|a, b| b.cmp(a));
        out.into_iter().map(|(_, i)| NodeId(i)).collect()
    }

    /// Approximate heap memory used by the tree.
    pub fn heap_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.names.heap_bytes()
            + self.file_ids.capacity() * 24
    }
}

pub struct Children<'a> {
    tree: &'a Tree,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = self.tree.nodes[cur.index()].next_sibling;
        Some(cur)
    }
}

pub struct Descendants<'a> {
    tree: &'a Tree,
    stack: Vec<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.stack.pop()?;
        self.stack.extend(self.tree.children(cur));
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Tree, NodeId, NodeId, NodeId) {
        let mut t = Tree::new("/r", 0);
        let a = t.add_child(Tree::ROOT, OsStr::new("a"), EntryInfo::dir(0));
        let f1 = t.add_child(a, OsStr::new("f1"), EntryInfo::file(100, 0));
        let f2 = t.add_child(Tree::ROOT, OsStr::new("f2"), EntryInfo::file(50, 0));
        (t, a, f1, f2)
    }

    #[test]
    fn aggregates_propagate() {
        let (t, a, _, _) = sample();
        assert_eq!(t.node(Tree::ROOT).apparent, 150);
        assert_eq!(t.node(Tree::ROOT).items, 3);
        assert_eq!(t.node(a).apparent, 100);
        assert_eq!(t.node(a).items, 1);
    }

    #[test]
    fn paths_and_lookup() {
        let (t, a, f1, _) = sample();
        assert_eq!(t.path(f1), PathBuf::from("/r/a/f1"));
        assert_eq!(t.find_path(Path::new("/r/a/f1")), Some(f1));
        assert_eq!(t.find_path(Path::new("/r/a")), Some(a));
        assert_eq!(t.find_path(Path::new("/r")), Some(Tree::ROOT));
        assert_eq!(t.find_path(Path::new("/x")), None);
    }

    #[test]
    fn sort_desc() {
        let (mut t, a, _, f2) = sample();
        t.sort_all(SizeMode::Apparent);
        let kids: Vec<_> = t.children(Tree::ROOT).collect();
        assert_eq!(kids, vec![a, f2]);
    }

    #[test]
    fn hardlinks_counted_once() {
        let mut t = Tree::new("/r", 0);
        let mut info = EntryInfo::file(1000, 0);
        info.file_id = Some((1, 42));
        let x = t.add_child(Tree::ROOT, OsStr::new("x"), info.clone());
        let y = t.add_child(Tree::ROOT, OsStr::new("y"), info);
        assert_eq!(t.node(Tree::ROOT).apparent, 1000);
        assert!(t.node(x).flags.contains(NodeFlags::HARDLINKED));
        assert!(t.node(y).flags.contains(NodeFlags::HARDLINK_DUP));
    }

    #[test]
    fn remove_subtracts() {
        let (mut t, a, _, f2) = sample();
        t.remove(a);
        assert_eq!(t.node(Tree::ROOT).apparent, 50);
        assert_eq!(t.node(Tree::ROOT).items, 1);
        assert_eq!(t.children(Tree::ROOT).collect::<Vec<_>>(), vec![f2]);
        t.remove(a); // idempotent
        assert_eq!(t.node(Tree::ROOT).apparent, 50);
    }

    #[test]
    fn largest() {
        let (t, _, f1, f2) = sample();
        assert_eq!(t.largest_files(10, SizeMode::Apparent), vec![f1, f2]);
        assert_eq!(t.largest_files(1, SizeMode::Apparent), vec![f1]);
    }

    #[test]
    fn node_is_compact() {
        // NFR-PERF-05: ~100 bytes per node budget.
        assert!(std::mem::size_of::<Node>() <= 64);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn root_equals_sum_of_files(sizes in proptest::collection::vec((0u64..1_000_000, 0usize..8), 1..200)) {
                let mut t = Tree::new("/r", 0);
                let mut dirs = vec![Tree::ROOT];
                let mut total = 0u64;
                for (i, (size, pick)) in sizes.iter().enumerate() {
                    let parent = dirs[pick % dirs.len()];
                    if i % 3 == 0 {
                        let d = t.add_child(parent, OsStr::new(&format!("d{i}")), EntryInfo::dir(0));
                        dirs.push(d);
                    } else {
                        t.add_child(parent, OsStr::new(&format!("f{i}")), EntryInfo::file(*size, 0));
                        total += size;
                    }
                }
                prop_assert_eq!(t.node(Tree::ROOT).apparent, total);
                // Every directory equals the sum of its children.
                for d in dirs {
                    let sum: u64 = t.children(d).map(|c| t.node(c).apparent).sum();
                    prop_assert_eq!(t.node(d).apparent, sum);
                }
            }
        }
    }
}
