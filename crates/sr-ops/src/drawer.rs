//! The Trash Drawer (§3.3): a persistent list of staged items.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sr_core::NodeKind;
use sr_platform::ProtectedPaths;

use crate::{IoCtx, OpsError, StagedItem};

/// Staged items. Invariant: no staged item is an ancestor of another.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Drawer {
    items: Vec<StagedItem>,
}

impl Drawer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage an item. Protected paths are rejected (NFR-SAFE-04), as are
    /// exact duplicates and items already covered by a staged ancestor. A
    /// staged item's ancestor absorbs it.
    pub fn stage(&mut self, item: StagedItem, protected: &ProtectedPaths) -> Result<(), OpsError> {
        if protected.is_protected(&item.path) {
            return Err(OpsError::Protected(item.path));
        }
        if self.contains(&item.path) {
            return Err(OpsError::AlreadyStaged(item.path));
        }
        if let Some(by) = self.items.iter().find(|s| item.path.starts_with(&s.path)) {
            return Err(OpsError::Covered {
                by: by.path.clone(),
                path: item.path,
            });
        }
        self.items.retain(|s| !s.path.starts_with(&item.path));
        self.items.push(item);
        Ok(())
    }

    /// Remove an item; no filesystem effect (FR-TRASH-03).
    pub fn unstage(&mut self, path: &Path) -> bool {
        let before = self.items.len();
        self.items.retain(|s| s.path != path);
        self.items.len() != before
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn items(&self) -> &[StagedItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Exactly this path is staged.
    pub fn contains(&self, path: &Path) -> bool {
        self.items.iter().any(|s| s.path == path)
    }

    /// This path or one of its ancestors is staged.
    pub fn covers(&self, path: &Path) -> bool {
        self.items.iter().any(|s| path.starts_with(&s.path))
    }

    /// Total reclaimable bytes (FR-TRASH-02). See [`reclaimable`].
    pub fn reclaimable(&self) -> u64 {
        reclaimable(&self.items)
    }

    /// Persist as JSON, atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), OpsError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).ctx(dir)?;
        }
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(&tmp, json).ctx(&tmp)?;
        fs::rename(&tmp, path).ctx(path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }

    /// Load a drawer saved by [`Drawer::save`] (FR-TRASH-10). Nested entries
    /// in a hand-edited file are collapsed into their ancestors.
    pub fn load(path: &Path) -> Result<Self, OpsError> {
        let bytes = fs::read(path).ctx(path)?;
        let raw: Drawer = serde_json::from_slice(&bytes)?;
        let items = top_level(&raw.items).cloned().collect();
        Ok(Drawer { items })
    }
}

/// For each item: is it top level, i.e. not covered by another
/// (distinct-path) item and the first occurrence of its path?
pub(crate) fn top_level_mask(items: &[StagedItem]) -> Vec<bool> {
    let all: HashSet<&Path> = items.iter().map(|i| i.path.as_path()).collect();
    let mut seen: HashSet<&Path> = HashSet::new();
    items
        .iter()
        .map(|it| !it.path.ancestors().skip(1).any(|a| all.contains(a)) && seen.insert(&it.path))
        .collect()
}

fn top_level(items: &[StagedItem]) -> impl Iterator<Item = &StagedItem> {
    items
        .iter()
        .zip(top_level_mask(items))
        .filter_map(|(it, top)| top.then_some(it))
}

/// Bytes freed by deleting `items`: nested items are not double counted and
/// a hardlinked file frees nothing unless all of its links are included.
pub(crate) fn reclaimable(items: &[StagedItem]) -> u64 {
    let mut total = 0u64;
    let mut links: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new(); // id -> (count, nlink, bytes)
    for it in top_level(items) {
        match it.file_id {
            Some(id) if it.kind != NodeKind::Dir && it.nlink > 1 => {
                let e = links.entry(id).or_insert((0, it.nlink, it.allocated));
                e.0 += 1;
            }
            _ => total += it.allocated,
        }
    }
    total
        + links
            .values()
            .filter(|(count, nlink, _)| count >= nlink)
            .map(|(_, _, bytes)| bytes)
            .sum::<u64>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::no_protection;
    use proptest::prelude::*;
    use sr_core::Module;
    use std::path::PathBuf;

    fn item(path: &str, bytes: u64) -> StagedItem {
        StagedItem {
            path: PathBuf::from(path),
            kind: NodeKind::Dir,
            allocated: bytes,
            apparent: bytes,
            mtime: 0,
            mtime_nanos: 0,
            file_id: None,
            nlink: 1,
            entry_count: 0,
            source: Module::SpaceMap,
            reason: String::new(),
            staged_at: 0,
        }
    }

    fn linked(path: &str, bytes: u64, id: u64, nlink: u64) -> StagedItem {
        StagedItem {
            kind: NodeKind::File,
            file_id: Some((1, id)),
            nlink,
            ..item(path, bytes)
        }
    }

    #[test]
    fn nested_selection_is_absorbed() {
        let p = no_protection();
        let mut d = Drawer::new();
        d.stage(item("/a/b/c", 10), &p).unwrap();
        d.stage(item("/a/b/d", 20), &p).unwrap();
        d.stage(item("/a/x", 5), &p).unwrap();
        assert_eq!(d.reclaimable(), 35);
        d.stage(item("/a/b", 100), &p).unwrap();
        assert_eq!(d.len(), 2);
        assert!(!d.contains(Path::new("/a/b/c")));
        assert!(d.covers(Path::new("/a/b/c/deep")));
        assert!(!d.covers(Path::new("/a/bc")));
        assert_eq!(d.reclaimable(), 105);
        assert!(matches!(
            d.stage(item("/a/b/c", 10), &p),
            Err(OpsError::Covered { .. })
        ));
        assert!(matches!(
            d.stage(item("/a/b", 100), &p),
            Err(OpsError::AlreadyStaged(_))
        ));
        assert!(d.unstage(Path::new("/a/b")));
        assert!(!d.unstage(Path::new("/a/b")));
        d.clear();
        assert!(d.is_empty());
    }

    #[test]
    fn protected_rejected() {
        let p = ProtectedPaths::custom(vec!["/home/u".into()], vec!["/usr".into()]);
        let mut d = Drawer::new();
        assert!(matches!(
            d.stage(item("/home/u", 1), &p),
            Err(OpsError::Protected(_))
        ));
        assert!(d.stage(item("/usr/lib/x", 1), &p).is_err());
        assert!(d.stage(item("/home/u/proj", 1), &p).is_ok());
    }

    #[test]
    fn hardlink_accounting() {
        let p = no_protection();
        let mut d = Drawer::new();
        d.stage(linked("/a", 100, 7, 2), &p).unwrap();
        assert_eq!(d.reclaimable(), 0, "other link survives");
        d.stage(linked("/b", 100, 7, 2), &p).unwrap();
        assert_eq!(d.reclaimable(), 100, "all links staged, counted once");
        d.stage(linked("/c", 50, 8, 3), &p).unwrap();
        assert_eq!(d.reclaimable(), 100);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("state/drawer.json");
        let mut d = Drawer::new();
        d.stage(item("/x/y", 3), &no_protection()).unwrap();
        d.save(&file).unwrap();
        let l = Drawer::load(&file).unwrap();
        assert_eq!(l.items(), d.items());
        assert_eq!(fs::read_dir(t.path().join("state")).unwrap().count(), 1);
    }

    fn arb_path() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(0u8..3, 1..5)
    }

    fn to_path(v: &[u8]) -> PathBuf {
        let mut p = PathBuf::from("/r");
        for c in v {
            p.push(format!("n{c}"));
        }
        p
    }

    proptest! {
        /// Random nested selections: reclaimable equals the sum over the
        /// disjoint top-level selections, whatever the staging order.
        #[test]
        fn reclaimable_never_double_counts(
            sel in prop::collection::vec((arb_path(), 1u64..1000), 1..20)
        ) {
            let p = no_protection();
            let mut d = Drawer::new();
            let mut first_size: HashMap<PathBuf, u64> = HashMap::new();
            for (v, size) in &sel {
                let path = to_path(v);
                let res = d.stage(item(path.to_str().unwrap(), *size), &p);
                let covered = first_size.keys().any(|k| path.starts_with(k));
                prop_assert_eq!(res.is_ok(), !covered);
                if res.is_ok() {
                    first_size.retain(|k, _| !k.starts_with(&path));
                    first_size.insert(path, *size);
                }
            }
            let expected: u64 = first_size.values().sum();
            prop_assert_eq!(d.reclaimable(), expected);
            prop_assert_eq!(d.len(), first_size.len());
            for a in d.items() {
                for b in d.items() {
                    prop_assert!(a.path == b.path || !a.path.starts_with(&b.path));
                }
            }
            // An unnormalised list (e.g. a hand-edited drawer file) counts
            // each top-level path once, ignoring nested entries.
            let raw: Vec<StagedItem> = sel
                .iter()
                .map(|(v, s)| item(to_path(v).to_str().unwrap(), *s))
                .collect();
            let mut want = 0;
            for (i, it) in raw.iter().enumerate() {
                let dup = raw[..i].iter().any(|o| o.path == it.path);
                let nested = raw.iter().any(|o| o.path != it.path && it.path.starts_with(&o.path));
                if !dup && !nested {
                    want += it.allocated;
                }
            }
            prop_assert_eq!(reclaimable(&raw), want);
        }
    }
}
