//! Quarantine fallback (§7.3) and link-safe permanent removal.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use sr_core::NodeKind;
use sr_platform::entry_meta;

use crate::snapshot::kind_of;
use crate::{IoCtx, JournalRecord, OpsError};

pub const QUARANTINE_DIR_NAME: &str = ".spacerazer-quarantine";

/// Root of the volume holding `path`: the highest ancestor on the same
/// device (falls back to the mount table where device ids are unavailable).
pub(crate) fn volume_root(path: &Path) -> Option<PathBuf> {
    let dev = |p: &Path| {
        fs::symlink_metadata(p)
            .ok()
            .and_then(|m| entry_meta(p, &m).device)
    };
    let parent = path.parent()?;
    match dev(parent) {
        Some(d) => {
            let mut root = parent.to_path_buf();
            for anc in parent.ancestors().skip(1) {
                if dev(anc) != Some(d) {
                    break;
                }
                root = anc.to_path_buf();
            }
            Some(root)
        }
        None => sr_platform::volume_for(path, &sr_platform::volumes()).map(|v| v.mount_point),
    }
}

/// `<root>/.spacerazer-quarantine/<stamp>/<path relative to root>`. Paths
/// outside `root` keep all their normal components (prefix/root dropped).
pub(crate) fn quarantine_destination(root: &Path, stamp: &str, path: &Path) -> PathBuf {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut dest = root.join(QUARANTINE_DIR_NAME).join(stamp);
    for c in rel.components() {
        match c {
            Component::Normal(s) => dest.push(s),
            Component::Prefix(p) => dest.push(
                p.as_os_str()
                    .to_string_lossy()
                    .replace(|c: char| !c.is_alphanumeric(), "_"),
            ),
            _ => {}
        }
    }
    dest
}

/// Move `path` into quarantine under `root` by `rename` only; a cross-device
/// move fails rather than copying.
pub(crate) fn move_to_quarantine(
    path: &Path,
    root: &Path,
    stamp: &str,
) -> Result<PathBuf, OpsError> {
    let mut dest = quarantine_destination(root, stamp, path);
    let mut n = 1;
    while fs::symlink_metadata(&dest).is_ok() {
        let mut name = dest.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{n}"));
        dest.set_file_name(name);
        n += 1;
    }
    let parent = dest.parent().expect("destination has a parent");
    fs::create_dir_all(parent).ctx(parent)?;
    fs::rename(path, &dest).map_err(|e| {
        let _ = remove_empty_dirs_up_to(parent, &root.join(QUARANTINE_DIR_NAME));
        OpsError::io(path, e)
    })?;
    Ok(dest)
}

fn remove_empty_dirs_up_to(from: &Path, stop: &Path) -> io::Result<()> {
    for d in from.ancestors() {
        if !d.starts_with(stop) || d == stop {
            break;
        }
        fs::remove_dir(d)?;
    }
    Ok(())
}

fn remove_link(path: &Path) -> io::Result<()> {
    // Windows directory symlinks and junctions are removed with remove_dir.
    fs::remove_file(path).or_else(|e| {
        if cfg!(windows) {
            fs::remove_dir(path)
        } else {
            Err(e)
        }
    })
}

/// Permanently delete `path`, bottom-up, never following links. Returns
/// allocated bytes actually freed: files with surviving hardlinks free 0.
pub(crate) fn remove_tree(path: &Path) -> Result<u64, OpsError> {
    let meta = fs::symlink_metadata(path).ctx(path)?;
    let em = entry_meta(path, &meta);
    match kind_of(&meta) {
        NodeKind::Dir => {
            let mut freed = 0;
            for entry in fs::read_dir(path).ctx(path)? {
                freed += remove_tree(&entry.ctx(path)?.path())?;
            }
            fs::remove_dir(path).ctx(path)?;
            Ok(freed + em.allocated)
        }
        NodeKind::Symlink => remove_link(path).ctx(path).map(|_| em.allocated),
        _ => {
            fs::remove_file(path).ctx(path)?;
            Ok(if em.nlink > 1 { 0 } else { em.allocated })
        }
    }
}

/// Existing quarantine folders at the roots of mounted volumes.
pub fn quarantine_dirs() -> Vec<PathBuf> {
    sr_platform::volumes()
        .into_iter()
        .map(|v| v.mount_point.join(QUARANTINE_DIR_NAME))
        .filter(|d| fs::symlink_metadata(d).is_ok_and(|m| m.is_dir()))
        .collect()
}

/// Permanently delete a quarantine folder's contents; returns bytes freed.
/// Refuses any directory not named [`QUARANTINE_DIR_NAME`].
pub fn empty_quarantine(dir: &Path) -> Result<u64, OpsError> {
    if dir.file_name().and_then(|n| n.to_str()) != Some(QUARANTINE_DIR_NAME) {
        return Err(OpsError::invalid(dir, "not a quarantine directory"));
    }
    let meta = fs::symlink_metadata(dir).ctx(dir)?;
    if kind_of(&meta) != NodeKind::Dir {
        return Err(OpsError::invalid(dir, "not a directory"));
    }
    let mut freed = 0;
    for entry in fs::read_dir(dir).ctx(dir)? {
        freed += remove_tree(&entry.ctx(dir)?.path())?;
    }
    Ok(freed)
}

/// Move a quarantined item back to its original path, if it is still in
/// quarantine and the original path is free.
pub fn restore_from_quarantine(rec: &JournalRecord) -> Result<(), OpsError> {
    let dest = rec
        .destination
        .as_deref()
        .filter(|d| {
            rec.operation == "quarantine"
                && rec.succeeded()
                && d.components().any(|c| c.as_os_str() == QUARANTINE_DIR_NAME)
        })
        .ok_or_else(|| {
            OpsError::invalid(&rec.path, "record is not a successful quarantine move")
        })?;
    fs::symlink_metadata(dest).ctx(dest)?;
    if fs::symlink_metadata(&rec.path).is_ok() {
        return Err(OpsError::invalid(&rec.path, "original path is occupied"));
    }
    if let Some(parent) = rec.path.parent() {
        fs::create_dir_all(parent).ctx(parent)?;
    }
    fs::rename(dest, &rec.path).ctx(&rec.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_layout() {
        let d = quarantine_destination(Path::new("/vol"), "S", Path::new("/vol/a/b"));
        assert_eq!(d, Path::new("/vol/.spacerazer-quarantine/S/a/b"));
        let d = quarantine_destination(Path::new("/q"), "S", Path::new("/x/../y"));
        assert_eq!(d, Path::new("/q/.spacerazer-quarantine/S/x/y"));
    }

    #[test]
    fn volume_root_is_ancestor() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        fs::write(&f, b"x").unwrap();
        let r = volume_root(&f).unwrap();
        assert!(f.starts_with(&r));
    }

    #[test]
    fn empty_refuses_other_dirs_and_clears() {
        let t = tempfile::tempdir().unwrap();
        assert!(empty_quarantine(t.path()).is_err());
        let q = t.path().join(QUARANTINE_DIR_NAME);
        fs::create_dir_all(q.join("s/a")).unwrap();
        fs::write(q.join("s/a/f"), vec![0u8; 8192]).unwrap();
        assert!(empty_quarantine(&q).unwrap() >= 8192);
        assert_eq!(fs::read_dir(&q).unwrap().count(), 0);
    }

    #[test]
    fn remove_tree_counts_hardlinks_and_measures() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir(&d).unwrap();
        fs::write(d.join("a"), vec![1u8; 10_000]).unwrap();
        fs::write(t.path().join("outside"), vec![1u8; 10_000]).unwrap();
        fs::hard_link(t.path().join("outside"), d.join("link")).unwrap();
        let expected_a = entry_meta(&d, &fs::symlink_metadata(d.join("a")).unwrap()).allocated;
        let freed = remove_tree(&d).unwrap();
        assert!(freed >= expected_a && freed < expected_a + 10_000);
        assert!(t.path().join("outside").exists());
    }
}
