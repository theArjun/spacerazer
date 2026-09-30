//! The multi-pass duplicate pipeline (README §6.5).

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::PathBuf;
use std::time::Instant;

use rayon::prelude::*;
use sr_core::CancellationToken;
use sr_platform::DeviceKind;

use crate::cache::HashCache;
use crate::collect::{collect_files, group_by_size_counted};
use crate::hash::{files_identical_with, full_hash_with, partial_hash_with};
use crate::progress::Reporter;
use crate::similar::{find_similar_images, is_image_path};
use crate::{
    DupError, DupGroup, DupOptions, DupResult, FileEntry, GroupKind, Pass, Progress, sort_groups,
};

type Errors = Vec<(PathBuf, String)>;
/// Final groups keyed by (size, BLAKE3 digest).
type FullGroups = Vec<((u64, [u8; 32]), Vec<FileEntry>)>;

/// Outcome of hashing one file: the key and whether it came from the cache.
type Hashed<K> = Result<(K, bool), DupError>;

/// Run the full pipeline: collect → size → partial → full → [verify], then
/// optionally perceptual image grouping. Cancellation is honoured between
/// files and between hash chunks; a cancelled run returns promptly with
/// `stats.cancelled = true` and no (or partial) groups.
pub fn find_duplicates(
    opts: &DupOptions,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(Progress) + Sync),
) -> DupResult {
    let start = Instant::now();
    let mut res = DupResult::default();
    run(opts, cancel, on_progress, &mut res);
    res.stats.cancelled = cancel.is_cancelled();
    res.stats.elapsed = start.elapsed();
    res
}

fn run(
    opts: &DupOptions,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(Progress) + Sync),
    res: &mut DupResult,
) {
    // Collect.
    Reporter::new(on_progress, Pass::Collect, 0, 0);
    let files = collect_files(opts, cancel, &mut res.errors);
    res.stats.files_scanned = files.len() as u64;
    if cancel.is_cancelled() {
        return;
    }
    let seen: HashSet<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    let images: Vec<FileEntry> = if opts.similar_images {
        files
            .iter()
            .filter(|f| is_image_path(&f.path))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    // Pass 1: size + hardlink twins.
    let total = files.len() as u64;
    let (buckets, twins) = group_by_size_counted(files);
    let candidates: Vec<FileEntry> = buckets.into_iter().flatten().collect();
    res.stats.hardlink_twins_skipped = twins;
    res.stats.candidates_after_size = candidates.len() as u64;
    Reporter::new(on_progress, Pass::Size, total, 0);

    let kinds: Vec<DeviceKind> = opts
        .roots
        .iter()
        .map(|r| sr_platform::device_kind(r))
        .collect();
    let threads = opts
        .io_threads
        .unwrap_or_else(|| kinds.iter().map(|k| k.io_concurrency()).min().unwrap_or(4))
        .max(1);
    // Prefer buffered reads on network file systems (SIGBUS risk, §6.5).
    let mmap_threshold = if kinds.contains(&DeviceKind::Network) {
        u64::MAX
    } else {
        opts.mmap_threshold
    };
    let pool = match rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("sr-dedup-hash-{i}"))
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            res.errors
                .push((PathBuf::new(), format!("cannot start hashing threads: {e}")));
            return;
        }
    };
    let mut cache = opts
        .hash_cache
        .as_deref()
        .map(HashCache::load)
        .unwrap_or_default();
    let pb = opts.partial_bytes;

    // Pass 2: partial hash.
    let partial_groups = {
        let rep = Reporter::new(
            on_progress,
            Pass::Partial,
            candidates.len() as u64,
            candidates
                .iter()
                .map(|f| f.size.min(pb.saturating_mul(2)))
                .sum(),
        );
        let out = hash_pass(
            &pool,
            &rep,
            cancel,
            candidates,
            |f| f.size.min(pb.saturating_mul(2)),
            |f| match cache.partial(f, pb) {
                Some(h) => Ok((h, true)),
                None => partial_hash_with(&f.path, f.size, pb, cancel, &|n| rep.add_hashed(n))
                    .map(|h| (h, false)),
            },
        );
        rep.emit();
        res.stats.bytes_hashed += rep.hashed.load(std::sync::atomic::Ordering::Relaxed);
        absorb(res, &out, &mut cache, |c, f, h| c.record_partial(f, pb, *h));
        out.groups
    };
    res.stats.candidates_after_partial = count(&partial_groups);
    if cancel.is_cancelled() {
        save_cache(opts, &mut cache, None, res);
        return;
    }

    // Pass 3: full hash.
    let candidates: Vec<FileEntry> = partial_groups.into_iter().flat_map(|(_, g)| g).collect();
    let full_groups = {
        let rep = Reporter::new(
            on_progress,
            Pass::Full,
            candidates.len() as u64,
            candidates.iter().map(|f| f.size).sum(),
        );
        let out = hash_pass(
            &pool,
            &rep,
            cancel,
            candidates,
            |f| f.size,
            |f| match cache.full(f) {
                Some(h) => Ok((h, true)),
                None => full_hash_with(&f.path, f.size, mmap_threshold, cancel, &|n| {
                    rep.add_hashed(n)
                })
                .map(|h| (h, false)),
            },
        );
        rep.emit();
        res.stats.bytes_hashed += rep.hashed.load(std::sync::atomic::Ordering::Relaxed);
        absorb(res, &out, &mut cache, |c, f, h| c.record_full(f, h));
        out.groups
    };
    res.stats.candidates_after_full = count(&full_groups);
    save_cache(
        opts,
        &mut cache,
        (!cancel.is_cancelled()).then_some(&seen),
        res,
    );
    if cancel.is_cancelled() {
        return;
    }

    // Optional pass 4: byte-by-byte verification.
    let final_groups = if opts.paranoid {
        verify(&pool, full_groups, cancel, on_progress, &mut res.errors)
    } else {
        full_groups
    };
    res.stats.candidates_after_verify = count(&final_groups);
    if cancel.is_cancelled() {
        return;
    }
    res.groups = final_groups
        .into_iter()
        .map(|((size, hash), files)| DupGroup {
            kind: GroupKind::Identical,
            hash: Some(blake3::Hash::from_bytes(hash).to_hex().to_string()),
            size,
            files,
            image_dims: Vec::new(),
        })
        .collect();
    sort_groups(&mut res.groups);

    // Perceptual pass: one representative per physical file / identical group.
    if opts.similar_images && !images.is_empty() {
        let redundant: HashSet<&PathBuf> = res
            .groups
            .iter()
            .flat_map(|g| g.files.iter().skip(1).map(|f| &f.path))
            .collect();
        let mut ids = HashSet::new();
        let reps: Vec<FileEntry> = images
            .into_iter()
            .filter(|f| !redundant.contains(&f.path) && f.file_id.is_none_or(|id| ids.insert(id)))
            .collect();
        let (similar, errors) =
            find_similar_images(&reps, opts.similarity_threshold, cancel, on_progress);
        res.stats.images_hashed = (reps.len() as u64).saturating_sub(errors.len() as u64);
        res.similar = similar;
        res.errors.extend(errors);
    }
}

struct PassOutput<K> {
    /// Groups of ≥ 2 files keyed by (size, hash), deterministic order.
    groups: Vec<((u64, K), Vec<FileEntry>)>,
    /// Freshly computed hashes to store in the cache.
    fresh: Vec<(FileEntry, K)>,
    errors: Errors,
    cache_hits: u64,
}

fn hash_pass<K, F>(
    pool: &rayon::ThreadPool,
    rep: &Reporter<'_>,
    cancel: &CancellationToken,
    files: Vec<FileEntry>,
    weight: impl Fn(&FileEntry) -> u64 + Sync,
    compute: F,
) -> PassOutput<K>
where
    K: Hash + Eq + Ord + Copy + Send,
    F: Fn(&FileEntry) -> Hashed<K> + Sync,
{
    let results: Vec<(FileEntry, Hashed<K>)> = pool.install(|| {
        files
            .into_par_iter()
            .map(|f| {
                if cancel.is_cancelled() {
                    return (f, Err(DupError::Cancelled));
                }
                let r = compute(&f);
                let cached = matches!(r, Ok((_, true)));
                rep.file_done(if cached { weight(&f) } else { 0 });
                (f, r)
            })
            .collect()
    });
    let mut out = PassOutput {
        groups: Vec::new(),
        fresh: Vec::new(),
        errors: Vec::new(),
        cache_hits: 0,
    };
    let mut map: HashMap<(u64, K), Vec<FileEntry>> = HashMap::new();
    for (f, r) in results {
        match r {
            Ok((k, cached)) => {
                if cached {
                    out.cache_hits += 1;
                } else {
                    out.fresh.push((f.clone(), k));
                }
                map.entry((f.size, k)).or_default().push(f);
            }
            Err(DupError::Cancelled) => {}
            Err(e) => out.errors.push((f.path, e.to_string())),
        }
    }
    out.groups = map.into_iter().filter(|(_, g)| g.len() >= 2).collect();
    out.groups
        .sort_by(|a, b| b.0.0.cmp(&a.0.0).then(a.0.1.cmp(&b.0.1)));
    out
}

fn absorb<K>(
    res: &mut DupResult,
    out: &PassOutput<K>,
    cache: &mut HashCache,
    record: impl Fn(&mut HashCache, &FileEntry, &K),
) {
    for (f, k) in &out.fresh {
        record(cache, f, k);
    }
    res.stats.cache_hits += out.cache_hits;
    res.errors.extend(out.errors.iter().cloned());
}

fn count<K>(groups: &[(K, Vec<FileEntry>)]) -> u64 {
    groups.iter().map(|(_, g)| g.len() as u64).sum()
}

fn save_cache(
    opts: &DupOptions,
    cache: &mut HashCache,
    seen: Option<&HashSet<PathBuf>>,
    res: &mut DupResult,
) {
    let Some(path) = &opts.hash_cache else { return };
    if let Some(seen) = seen {
        cache.prune(&opts.roots, seen);
    }
    if let Err(e) = cache.save(path) {
        res.errors
            .push((path.clone(), format!("cannot write hash cache: {e}")));
    }
}

/// Split each group into subgroups of byte-identical files.
fn verify(
    pool: &rayon::ThreadPool,
    groups: FullGroups,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(Progress) + Sync),
    errors: &mut Errors,
) -> FullGroups {
    let files_total = count(&groups);
    let rep = Reporter::new(
        on_progress,
        Pass::Verify,
        files_total,
        groups.iter().map(|((s, _), g)| s * g.len() as u64).sum(),
    );
    let results: Vec<(FullGroups, Errors)> = pool.install(|| {
        groups
            .into_par_iter()
            .map(|(key, files)| {
                let mut subgroups: Vec<Vec<FileEntry>> = Vec::new();
                let mut errs = Vec::new();
                'file: for f in files {
                    for sg in &mut subgroups {
                        match files_identical_with(&sg[0].path, &f.path, cancel, &|n| {
                            rep.add_hashed(2 * n)
                        }) {
                            Ok(true) => {
                                sg.push(f);
                                rep.file_done(0);
                                continue 'file;
                            }
                            Ok(false) => {}
                            Err(DupError::Cancelled) => return (Vec::new(), errs),
                            Err(e) => {
                                errs.push((f.path, e.to_string()));
                                continue 'file;
                            }
                        }
                    }
                    rep.file_done(f.size);
                    subgroups.push(vec![f]);
                }
                let kept = subgroups
                    .into_iter()
                    .filter(|g| g.len() >= 2)
                    .map(|g| (key, g))
                    .collect();
                (kept, errs)
            })
            .collect()
    });
    rep.emit();
    let mut out = Vec::new();
    for (g, e) in results {
        out.extend(g);
        errors.extend(e);
    }
    out
}
