//! Pass 0/1: file collection and size grouping.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use sr_core::{CancellationToken, unix_secs};

use crate::{DupOptions, FileEntry};

fn build_globset(patterns: &[String], errors: &mut Vec<(PathBuf, String)>) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        match Glob::new(p) {
            Ok(g) => {
                b.add(g);
            }
            Err(e) => errors.push((PathBuf::from(p), format!("invalid pattern: {e}"))),
        }
    }
    match b.build() {
        Ok(set) => Some(set),
        Err(e) => {
            errors.push((PathBuf::new(), format!("invalid patterns: {e}")));
            None
        }
    }
}

fn matches(set: &GlobSet, path: &Path) -> bool {
    set.is_match(path) || path.file_name().is_some_and(|n| set.is_match(n))
}

/// Walk `opts.roots` without following symlinks and return every regular
/// file of at least `opts.min_size` bytes that passes the include/exclude
/// globs. Directories matching an exclude glob are pruned (roots excepted).
/// Cloud placeholders (dataless files) are skipped so hashing never triggers
/// a download. Paths reached twice through overlapping roots are reported
/// once. Unreadable entries are appended to `errors`.
pub fn collect_files(
    opts: &DupOptions,
    cancel: &CancellationToken,
    errors: &mut Vec<(PathBuf, String)>,
) -> Vec<FileEntry> {
    let min_size = opts.min_size.max(1);
    let include = build_globset(&opts.include, errors);
    let exclude = build_globset(&opts.exclude, errors);
    let excluded = |p: &Path| exclude.as_ref().is_some_and(|s| matches(s, p));
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for root in &opts.roots {
        let mut stack = vec![root.clone()];
        while let Some(path) = stack.pop() {
            if cancel.is_cancelled() {
                return out;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    errors.push((path, e.to_string()));
                    continue;
                }
            };
            let ft = meta.file_type();
            if ft.is_dir() {
                if &path != root && excluded(&path) {
                    continue;
                }
                match fs::read_dir(&path) {
                    Ok(rd) => {
                        for ent in rd {
                            match ent {
                                Ok(e) => stack.push(e.path()),
                                Err(e) => errors.push((path.clone(), e.to_string())),
                            }
                        }
                    }
                    Err(e) => errors.push((path, e.to_string())),
                }
            } else if ft.is_file() {
                if meta.len() < min_size
                    || excluded(&path)
                    || include.as_ref().is_some_and(|s| !matches(s, &path))
                {
                    continue;
                }
                let em = sr_platform::entry_meta(&path, &meta);
                if em.cloud_placeholder || !seen.insert(path.clone()) {
                    continue;
                }
                out.push(FileEntry {
                    size: meta.len(),
                    mtime: meta.modified().map(unix_secs).unwrap_or(0),
                    file_id: em.file_id,
                    device: em.device,
                    path,
                });
            }
        }
    }
    out
}

/// Pass 1: bucket files by exact size, collapse hardlink twins (same file
/// id) into their first path, and drop buckets with fewer than two physical
/// files (FR-DUP-02/03). Buckets are sorted by size descending, files by path.
pub fn group_by_size(files: Vec<FileEntry>) -> Vec<Vec<FileEntry>> {
    group_by_size_counted(files).0
}

/// [`group_by_size`] plus the number of hardlink twins collapsed.
pub(crate) fn group_by_size_counted(files: Vec<FileEntry>) -> (Vec<Vec<FileEntry>>, u64) {
    let mut by_size: HashMap<u64, Vec<FileEntry>> = HashMap::new();
    for f in files {
        by_size.entry(f.size).or_default().push(f);
    }
    let mut twins = 0;
    let mut buckets: Vec<Vec<FileEntry>> = by_size
        .into_values()
        .filter(|b| b.len() >= 2)
        .filter_map(|mut b| {
            b.sort_by(|x, y| x.path.cmp(&y.path));
            let before = b.len();
            let mut ids = HashSet::new();
            b.retain(|f| f.file_id.is_none_or(|id| ids.insert(id)));
            twins += (before - b.len()) as u64;
            (b.len() >= 2).then_some(b)
        })
        .collect();
    buckets.sort_by(|a, b| b[0].size.cmp(&a[0].size));
    (buckets, twins)
}
