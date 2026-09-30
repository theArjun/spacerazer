//! Snapshots of staged items and link-safe tree walking.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sr_core::{Module, NodeKind, now_secs, unix_secs};
use sr_platform::entry_meta;

use crate::{IoCtx, OpsError};

/// State of an item at staging time; execution revalidates against it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedItem {
    pub path: PathBuf,
    pub kind: NodeKind,
    /// Total allocated bytes (recursive for directories, links not followed,
    /// hardlinks inside a directory counted once).
    pub allocated: u64,
    pub apparent: u64,
    pub mtime: i64,
    /// Sub-second part of the mtime, compared on revalidation so a same-size
    /// rewrite within the same second is still detected.
    #[serde(default)]
    pub mtime_nanos: u32,
    pub file_id: Option<(u64, u64)>,
    pub nlink: u64,
    /// Recursive entry count for directories, 0 otherwise.
    pub entry_count: u64,
    pub source: Module,
    pub reason: String,
    pub staged_at: i64,
}

pub(crate) fn kind_of(meta: &Metadata) -> NodeKind {
    let ft = meta.file_type();
    if ft.is_symlink() {
        NodeKind::Symlink
    } else if ft.is_dir() {
        NodeKind::Dir
    } else if ft.is_file() {
        NodeKind::File
    } else {
        NodeKind::Other
    }
}

pub(crate) fn mtime_of(meta: &Metadata) -> (i64, u32) {
    meta.modified().map_or((0, 0), |t| {
        let nanos = t
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        (unix_secs(t), nanos)
    })
}

/// Totals of a directory walk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalkTotals {
    pub allocated: u64,
    pub apparent: u64,
    pub entries: u64,
}

/// Walk the contents of directory `dir` without following links. The
/// directory itself is not counted.
pub(crate) fn walk_dir(dir: &Path) -> io::Result<WalkTotals> {
    let mut totals = WalkTotals::default();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            let em = entry_meta(&path, &meta);
            totals.entries += 1;
            let first = match em.file_id {
                Some(id) if em.nlink > 1 => seen.insert(id),
                _ => true,
            };
            if first {
                totals.allocated += em.allocated;
                totals.apparent += em.apparent;
            }
            if kind_of(&meta) == NodeKind::Dir {
                stack.push(path);
            }
        }
    }
    Ok(totals)
}

/// Snapshot `path` for staging. Uses `symlink_metadata`: a symlink is
/// recorded as the link itself.
pub fn snapshot(
    path: &Path,
    source: Module,
    reason: impl Into<String>,
) -> Result<StagedItem, OpsError> {
    let meta = fs::symlink_metadata(path).ctx(path)?;
    let em = entry_meta(path, &meta);
    let kind = kind_of(&meta);
    let (mut allocated, mut apparent, mut entry_count) = (em.allocated, em.apparent, 0);
    if kind == NodeKind::Dir {
        let t = walk_dir(path).ctx(path)?;
        allocated += t.allocated;
        apparent += t.apparent;
        entry_count = t.entries;
    }
    let (mtime, mtime_nanos) = mtime_of(&meta);
    Ok(StagedItem {
        path: path.to_path_buf(),
        kind,
        allocated,
        apparent,
        mtime,
        mtime_nanos,
        file_id: em.file_id,
        nlink: em.nlink,
        entry_count,
        source,
        reason: reason.into(),
        staged_at: now_secs(),
    })
}

/// Compare the current on-disk state with the snapshot (§7.2 step 2).
/// Files and links: kind, file identity, apparent size and mtime must match.
/// Directories: kind and identity must match and the recursive entry count
/// may not grow by more than `dir_growth_tolerance` (a directory's own mtime
/// changes on benign activity, so it is not compared).
///
/// On success returns the bytes deleting the item would free right now
/// (0 for a file whose other hardlinks survive).
pub(crate) fn revalidate(item: &StagedItem, dir_growth_tolerance: u64) -> Result<u64, String> {
    let meta = match fs::symlink_metadata(&item.path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err("no longer exists".into()),
        Err(e) => return Err(format!("cannot read metadata: {e}")),
    };
    let kind = kind_of(&meta);
    if kind != item.kind {
        return Err(format!("kind changed from {:?} to {kind:?}", item.kind));
    }
    let em = entry_meta(&item.path, &meta);
    if let (Some(a), Some(b)) = (item.file_id, em.file_id)
        && a != b
    {
        return Err("replaced by a different file".into());
    }
    if kind == NodeKind::Dir {
        let t = walk_dir(&item.path).map_err(|e| format!("cannot re-walk: {e}"))?;
        let limit = item.entry_count.saturating_add(dir_growth_tolerance);
        if t.entries > limit {
            return Err(format!(
                "directory grew from {} to {} entries",
                item.entry_count, t.entries
            ));
        }
        return Ok(em.allocated + t.allocated);
    }
    if em.apparent != item.apparent {
        return Err(format!(
            "size changed from {} to {}",
            item.apparent, em.apparent
        ));
    }
    let (mtime, nanos) = mtime_of(&meta);
    if (mtime, nanos) != (item.mtime, item.mtime_nanos) {
        return Err(format!("modified (mtime {} -> {mtime})", item.mtime));
    }
    Ok(if em.nlink > 1 { 0 } else { em.allocated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_dir_counts_recursively_and_dedups_hardlinks() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a"), vec![1u8; 5000]).unwrap();
        fs::write(d.join("sub/b"), vec![2u8; 3000]).unwrap();
        fs::hard_link(d.join("a"), d.join("sub/a2")).unwrap();
        let s = snapshot(&d, Module::SpaceMap, "test").unwrap();
        assert_eq!(s.kind, NodeKind::Dir);
        assert_eq!(s.entry_count, 4);
        let dir_self = entry_meta(&d, &fs::symlink_metadata(&d).unwrap()).apparent;
        let sub = entry_meta(&d, &fs::symlink_metadata(d.join("sub")).unwrap()).apparent;
        assert_eq!(s.apparent, dir_self + sub + 8000);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_symlink_is_the_link() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("big")).unwrap();
        fs::write(t.path().join("big/f"), vec![0u8; 100_000]).unwrap();
        let l = t.path().join("link");
        std::os::unix::fs::symlink(t.path().join("big"), &l).unwrap();
        let s = snapshot(&l, Module::SpaceMap, "").unwrap();
        assert_eq!(s.kind, NodeKind::Symlink);
        assert_eq!(s.entry_count, 0);
        assert!(s.allocated < 100_000);
    }

    #[test]
    fn revalidate_detects_changes() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        fs::write(&f, b"abc").unwrap();
        let s = snapshot(&f, Module::SpaceMap, "").unwrap();
        assert_eq!(revalidate(&s, 0), Ok(s.allocated));
        fs::write(&f, b"abcd").unwrap();
        assert!(revalidate(&s, 0).unwrap_err().contains("size"));
        fs::remove_file(&f).unwrap();
        fs::create_dir(&f).unwrap();
        assert!(revalidate(&s, 0).is_err());
        fs::remove_dir(&f).unwrap();
        assert_eq!(revalidate(&s, 0).unwrap_err(), "no longer exists");
    }

    #[test]
    fn revalidate_dir_growth_tolerance() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir(&d).unwrap();
        fs::write(d.join("a"), b"1").unwrap();
        let s = snapshot(&d, Module::DevSweep, "").unwrap();
        fs::write(d.join("b"), b"2").unwrap();
        assert!(revalidate(&s, 0).unwrap_err().contains("grew"));
        assert!(revalidate(&s, 1).is_ok());
        fs::remove_file(d.join("a")).unwrap();
        fs::remove_file(d.join("b")).unwrap();
        assert!(revalidate(&s, 0).is_ok(), "shrinking is allowed");
    }
}
