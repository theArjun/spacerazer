//! Duplicate resolution by hardlink or reflink (FR-DUP-18..20).

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sr_core::NodeKind;
use sr_platform::{ProtectedPaths, entry_meta};

use crate::snapshot::{kind_of, revalidate};
use crate::{IoCtx, Journal, JournalRecord, OpsError, StagedItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkKind {
    Hardlink,
    Reflink,
}

impl LinkKind {
    fn operation(self) -> &'static str {
        match self {
            LinkKind::Hardlink => "hardlink",
            LinkKind::Reflink => "reflink",
        }
    }
}

fn hash_file(path: &Path) -> Result<blake3::Hash, OpsError> {
    let mut h = blake3::Hasher::new();
    h.update_reader(File::open(path).ctx(path)?).ctx(path)?;
    Ok(h.finalize())
}

/// A fresh name next to `path`: `.<name>.srtmp-<pid>-<nanos>-<seq>`.
fn temp_name(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        ".{name}.srtmp-{}-{nanos}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Replace `dup` with a link to `keep`, atomically (FR-DUP-20): the link is
/// created at a temporary name in `dup`'s directory, its content hash is
/// verified against `dup`'s current content, and it is renamed over `dup`.
/// On any failure the temporary is removed and `dup` is untouched. Returns
/// the bytes freed (0 when `dup` has other hardlinks). Reflink savings are
/// "up to": blocks may already be shared.
pub fn replace_with_link(
    keep: &Path,
    dup: &StagedItem,
    kind: LinkKind,
    protected: &ProtectedPaths,
    journal: Option<&Journal>,
) -> Result<u64, OpsError> {
    let result = link_inner(keep, dup, kind, protected);
    if let Some(j) = journal {
        let rec = JournalRecord::new(
            kind.operation(),
            &dup.path,
            Some(keep.to_path_buf()),
            result.as_ref().map(|b| *b).map_err(|e| e.to_string()),
        );
        j.append(&rec)?;
    }
    result
}

fn link_inner(
    keep: &Path,
    dup: &StagedItem,
    kind: LinkKind,
    protected: &ProtectedPaths,
) -> Result<u64, OpsError> {
    let path = &dup.path;
    if protected.is_protected(path) {
        return Err(OpsError::Protected(path.clone()));
    }
    if dup.kind != NodeKind::File {
        return Err(OpsError::invalid(
            path,
            "only regular files can be replaced by links",
        ));
    }
    let changed = |reason| OpsError::Changed {
        path: path.clone(),
        reason,
    };
    let freed = revalidate(dup, 0).map_err(changed)?;
    let keep_meta = fs::symlink_metadata(keep).ctx(keep)?;
    if kind_of(&keep_meta) != NodeKind::File {
        return Err(OpsError::invalid(keep, "link source is not a regular file"));
    }
    let keep_em = entry_meta(keep, &keep_meta);
    let dup_em = entry_meta(path, &fs::symlink_metadata(path).ctx(path)?);
    if keep_em.file_id.is_some() && keep_em.file_id == dup_em.file_id {
        return Err(OpsError::invalid(path, "already the same physical file"));
    }
    if keep_em.apparent != dup_em.apparent {
        return Err(OpsError::invalid(path, "size differs from the kept file"));
    }
    if kind == LinkKind::Hardlink && (keep_em.device.is_none() || keep_em.device != dup_em.device) {
        return Err(OpsError::invalid(
            path,
            "hardlinks require both files on the same volume",
        ));
    }

    let tmp = temp_name(path);
    let created = match kind {
        LinkKind::Hardlink => fs::hard_link(keep, &tmp),
        LinkKind::Reflink => sr_platform::reflink(keep, &tmp),
    };
    created.ctx(&tmp)?;
    let verify_and_swap = || -> Result<(), OpsError> {
        if hash_file(&tmp)? != hash_file(path)? {
            return Err(OpsError::invalid(
                path,
                "content differs from the kept file",
            ));
        }
        // Re-check immediately before the swap: `dup` must not have changed
        // while we hashed it.
        revalidate(dup, 0).map_err(changed)?;
        fs::rename(&tmp, path).ctx(path)
    };
    verify_and_swap().inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot;
    use crate::testutil::{listing, no_protection};
    use sr_core::Module;

    fn setup(content_b: &[u8]) -> (tempfile::TempDir, PathBuf, StagedItem) {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        let b = t.path().join("b");
        fs::write(&a, vec![7u8; 20_000]).unwrap();
        fs::write(&b, content_b).unwrap();
        let s = snapshot(&b, Module::DuplicateLens, "dup").unwrap();
        (t, a, s)
    }

    #[test]
    fn hardlink_replacement() {
        let (t, a, s) = setup(&[7u8; 20_000]);
        let j = Journal::open(t.path().join("j/journal.jsonl"));
        let freed =
            replace_with_link(&a, &s, LinkKind::Hardlink, &no_protection(), Some(&j)).unwrap();
        assert_eq!(freed, s.allocated);
        let ea = entry_meta(&a, &fs::symlink_metadata(&a).unwrap());
        let eb = entry_meta(&s.path, &fs::symlink_metadata(&s.path).unwrap());
        assert_eq!(ea.file_id, eb.file_id);
        assert_eq!(fs::read(&s.path).unwrap(), vec![7u8; 20_000]);
        let recs = j.read_all().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].operation, "hardlink");
        // Already linked now.
        let s2 = snapshot(&s.path, Module::DuplicateLens, "").unwrap();
        assert!(replace_with_link(&a, &s2, LinkKind::Hardlink, &no_protection(), None).is_err());
    }

    #[test]
    fn reflink_replacement_or_clean_failure() {
        let (t, a, s) = setup(&[7u8; 20_000]);
        match replace_with_link(&a, &s, LinkKind::Reflink, &no_protection(), None) {
            Ok(_) => {
                assert_eq!(fs::read(&s.path).unwrap(), vec![7u8; 20_000]);
                assert_ne!(
                    entry_meta(&a, &fs::symlink_metadata(&a).unwrap()).file_id,
                    entry_meta(&a, &fs::symlink_metadata(&s.path).unwrap()).file_id
                );
            }
            Err(_) => assert_eq!(fs::read_dir(t.path()).unwrap().count(), 2),
        }
    }

    #[test]
    fn mismatch_leaves_original_untouched() {
        let mut other = vec![7u8; 20_000];
        other[19_999] = 8;
        let (t, a, s) = setup(&other);
        let before = listing(t.path());
        let err = replace_with_link(&a, &s, LinkKind::Hardlink, &no_protection(), None);
        assert!(err.unwrap_err().to_string().contains("content differs"));
        assert_eq!(
            listing(t.path()),
            before,
            "no temp file left, dup unchanged"
        );
        assert_eq!(fs::read(&s.path).unwrap(), other);
    }

    #[test]
    fn changed_or_protected_dup_rejected() {
        let (t, a, s) = setup(&[7u8; 20_000]);
        let p = ProtectedPaths::custom(vec![], vec![t.path().to_path_buf()]);
        assert!(matches!(
            replace_with_link(&a, &s, LinkKind::Hardlink, &p, None),
            Err(OpsError::Protected(_))
        ));
        fs::write(&s.path, [7u8; 20_001]).unwrap();
        assert!(matches!(
            replace_with_link(&a, &s, LinkKind::Hardlink, &no_protection(), None),
            Err(OpsError::Changed { .. })
        ));
        assert_eq!(fs::read(&s.path).unwrap().len(), 20_001);
    }
}
