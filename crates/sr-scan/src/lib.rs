//! Parallel filesystem scanner (SRS §3.1).
//!
//! Directory listings are read on a work-stealing `rayon` pool. Each worker
//! reads one directory, sends its entries to a single builder thread over a
//! `crossbeam` channel, then spawns tasks for its subdirectories. The builder
//! inserts entries into a shared [`Tree`] in batches, so the tree is always
//! consistent and can be rendered while the scan is still running
//! (FR-SCAN-04).
//!
//! A parent's listing is always sent before any of its children are spawned,
//! so the builder never sees a directory before its parent.

pub mod export;

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::Metadata;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sr_core::{
    CancellationToken, EntryInfo, NodeFlags, NodeId, NodeKind, PauseToken, SizeMode, Tree,
    unix_secs,
};
use sr_platform::BulkKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanOptions {
    /// One or more roots. A single root becomes the tree root; several roots
    /// are placed under a virtual root.
    pub roots: Vec<PathBuf>,
    /// Follow symlinks/junctions to directories, with cycle detection
    /// (FR-SCAN-07). Off by default.
    pub follow_symlinks: bool,
    /// Descend into other filesystems (FR-SCAN-08). Off by default.
    pub cross_filesystems: bool,
    /// Paths skipped entirely, including their subtrees (FR-SCAN-11).
    pub exclude_paths: Vec<PathBuf>,
    /// Glob patterns matched against the full path and the entry name.
    pub exclude_globs: Vec<String>,
    /// Worker thread count; `None` uses all cores.
    pub threads: Option<usize>,
}

impl ScanOptions {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            follow_symlinks: false,
            cross_filesystems: false,
            exclude_paths: sr_platform::default_exclusions(),
            exclude_globs: Vec::new(),
            threads: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("no scan roots given")]
    NoRoots,
    #[error("cannot read scan root {path}: {source}")]
    Root {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid exclusion pattern: {0}")]
    Glob(#[from] globset::Error),
}

/// Live counters, readable from any thread.
#[derive(Debug, Default)]
pub struct ScanProgress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub done: AtomicBool,
    pub cancelled: AtomicBool,
    /// Set if a worker panicked (NFR-REL-03).
    pub panic: Mutex<Option<String>>,
    elapsed_ms: AtomicU64,
}

impl ScanProgress {
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(Ordering::Relaxed))
    }
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// A running scan. Dropping the handle cancels the scan.
pub struct ScanHandle {
    pub tree: Arc<RwLock<Tree>>,
    pub progress: Arc<ScanProgress>,
    pub options: ScanOptions,
    cancel: CancellationToken,
    pause: PauseToken,
    thread: Option<JoinHandle<()>>,
    started: Instant,
}

impl ScanHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
        self.pause.set_paused(false);
    }
    pub fn set_paused(&self, paused: bool) {
        self.pause.set_paused(paused);
    }
    pub fn is_paused(&self) -> bool {
        self.pause.is_paused()
    }
    pub fn is_done(&self) -> bool {
        self.progress.is_done()
    }
    pub fn elapsed(&self) -> Duration {
        if self.is_done() {
            self.progress.elapsed()
        } else {
            self.started.elapsed()
        }
    }
    /// Block until the scan finishes.
    pub fn wait(mut self) -> Arc<RwLock<Tree>> {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.tree.clone()
    }
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        if self.thread.is_some() && !self.is_done() {
            self.cancel();
        }
    }
}

/// Start a scan on background threads.
pub fn start_scan(options: ScanOptions) -> Result<ScanHandle, ScanError> {
    let ctx_parts = prepare(&options)?;
    let tree = Arc::new(RwLock::new(ctx_parts.tree));
    let progress = Arc::new(ScanProgress::default());
    let cancel = CancellationToken::new();
    let pause = PauseToken::new();
    let started = Instant::now();

    let thread = {
        let tree = tree.clone();
        let progress = progress.clone();
        let cancel = cancel.clone();
        let pause = pause.clone();
        let options = options.clone();
        let roots = ctx_parts.roots;
        let bounds = Bounds {
            excluder: ctx_parts.excluder,
            sibling_devs: ctx_parts.sibling_devs,
        };
        std::thread::Builder::new()
            .name("sr-scan".into())
            .spawn(move || {
                run(&options, roots, bounds, &tree, &progress, &cancel, &pause);
                progress
                    .elapsed_ms
                    .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                progress
                    .cancelled
                    .store(cancel.is_cancelled(), Ordering::Relaxed);
                progress.done.store(true, Ordering::Release);
            })
            .expect("spawn scan thread")
    };

    Ok(ScanHandle {
        tree,
        progress,
        options,
        cancel,
        pause,
        thread: Some(thread),
        started,
    })
}

/// Scan synchronously and return the finished, sorted tree.
pub fn scan_blocking(options: ScanOptions) -> Result<(Tree, Arc<ScanProgress>), ScanError> {
    let handle = start_scan(options)?;
    let progress = handle.progress.clone();
    let tree = handle.wait();
    let tree = Arc::try_unwrap(tree)
        .map(|l| l.into_inner().unwrap_or_else(|e| e.into_inner()))
        .unwrap_or_else(|arc| arc.read().unwrap_or_else(|e| e.into_inner()).clone());
    Ok((tree, progress))
}

struct Prepared {
    tree: Tree,
    /// (path, node id in tree, device)
    roots: Vec<(PathBuf, NodeId, Option<u64>)>,
    excluder: Excluder,
    /// Devices treated as part of the root's filesystem.
    sibling_devs: Vec<u64>,
}

fn prepare(options: &ScanOptions) -> Result<Prepared, ScanError> {
    if options.roots.is_empty() {
        return Err(ScanError::NoRoots);
    }
    // macOS keeps user data on a separate "Data" volume that is firmlinked
    // into `/`. A scan of `/` treats that volume as the same disk and skips
    // its duplicate mount path so nothing is counted twice.
    let mut exclude_paths = options.exclude_paths.clone();
    let mut sibling_devs = Vec::new();
    if cfg!(target_os = "macos") && options.roots.iter().any(|r| r == Path::new("/")) {
        let data = Path::new("/System/Volumes/Data");
        if let Ok(m) = std::fs::metadata(data) {
            if let Some(dev) = sr_platform::entry_meta(data, &m).device {
                sibling_devs.push(dev);
                exclude_paths.push(data.to_path_buf());
            }
        }
    }
    let excluder = Excluder::new(&exclude_paths, &options.exclude_globs)?;
    let mut roots_meta = Vec::new();
    for r in &options.roots {
        let meta = std::fs::metadata(r).map_err(|source| ScanError::Root {
            path: r.clone(),
            source,
        })?;
        roots_meta.push((r.clone(), meta));
    }
    let device = |p: &Path, m: &Metadata| sr_platform::entry_meta(p, m).device;
    if roots_meta.len() == 1 {
        let (path, meta) = roots_meta.pop().expect("one root");
        let tree = Tree::new(path.clone(), mtime_of(&meta));
        let dev = device(&path, &meta);
        Ok(Prepared {
            tree,
            roots: vec![(path, Tree::ROOT, dev)],
            excluder,
            sibling_devs,
        })
    } else {
        // Virtual root: children carry absolute paths as names, which
        // `Tree::path` joins correctly.
        let mut tree = Tree::new(PathBuf::new(), 0);
        let mut roots = Vec::new();
        for (path, meta) in roots_meta {
            let id = tree.add_child(
                Tree::ROOT,
                path.as_os_str(),
                EntryInfo::dir(mtime_of(&meta)),
            );
            let dev = device(&path, &meta);
            roots.push((path, id, dev));
        }
        Ok(Prepared {
            tree,
            roots,
            excluder,
            sibling_devs,
        })
    }
}

fn mtime_of(meta: &Metadata) -> i64 {
    meta.modified().map(unix_secs).unwrap_or(0)
}

struct Excluder {
    paths: Vec<PathBuf>,
    globs: GlobSet,
}

impl Excluder {
    fn new(paths: &[PathBuf], globs: &[String]) -> Result<Self, globset::Error> {
        let mut b = GlobSetBuilder::new();
        for g in globs {
            b.add(Glob::new(g)?);
        }
        Ok(Self {
            paths: paths.to_vec(),
            globs: b.build()?,
        })
    }

    fn excluded(&self, path: &Path, name: &std::ffi::OsStr) -> bool {
        self.paths.iter().any(|p| path == p)
            || (!self.globs.is_empty()
                && (self.globs.is_match(path) || self.globs.is_match(Path::new(name))))
    }
}

/// One directory entry as read by a worker.
struct RawEntry {
    name: OsString,
    info: EntryInfo,
    /// Token assigned to a subdirectory that will be listed.
    token: Option<u64>,
}

enum Msg {
    Listing {
        dir: u64,
        entries: Vec<RawEntry>,
    },
    Error {
        dir: u64,
        path: PathBuf,
        error: String,
    },
}

struct Walker<'a> {
    options: &'a ScanOptions,
    excluder: &'a Excluder,
    tx: Sender<Msg>,
    next_token: AtomicU64,
    visited: Mutex<HashSet<(u64, u64)>>,
    progress: &'a ScanProgress,
    cancel: &'a CancellationToken,
    pause: &'a PauseToken,
    sibling_devs: Vec<u64>,
}

/// What the walk may enter: exclusions and filesystem boundaries.
struct Bounds {
    excluder: Excluder,
    sibling_devs: Vec<u64>,
}

fn run(
    options: &ScanOptions,
    roots: Vec<(PathBuf, NodeId, Option<u64>)>,
    bounds: Bounds,
    tree: &Arc<RwLock<Tree>>,
    progress: &ScanProgress,
    cancel: &CancellationToken,
    pause: &PauseToken,
) {
    let (tx, rx) = crossbeam_channel::bounded::<Msg>(4096);
    let mut tokens: HashMap<u64, NodeId> = HashMap::new();
    for (i, (_, id, _)) in roots.iter().enumerate() {
        tokens.insert(i as u64, *id);
    }
    let walker = Walker {
        options,
        excluder: &bounds.excluder,
        tx,
        next_token: AtomicU64::new(roots.len() as u64),
        visited: Mutex::new(HashSet::new()),
        progress,
        cancel,
        pause,
        sibling_devs: bounds.sibling_devs,
    };

    let mut builder = std::thread::Builder::new().name("sr-scan-pool".into());
    builder = builder.stack_size(8 << 20);
    std::thread::scope(|s| {
        let walker_ref = &walker;
        let roots_ref = &roots;
        let pool_thread = builder
            .spawn_scoped(s, move || {
                let mut pb =
                    rayon::ThreadPoolBuilder::new().thread_name(|i| format!("sr-scan-{i}"));
                if let Some(n) = walker_ref.options.threads {
                    pb = pb.num_threads(n.max(1));
                }
                let pool = match pb.build() {
                    Ok(p) => p,
                    Err(e) => {
                        record_panic(walker_ref.progress, format!("thread pool: {e}"));
                        return;
                    }
                };
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    pool.scope(|scope| {
                        for (i, (path, _, dev)) in roots_ref.iter().enumerate() {
                            let path = path.clone();
                            let dev = *dev;
                            scope.spawn(move |scope| walker_ref.walk(scope, path, i as u64, dev));
                        }
                    });
                }));
                if let Err(p) = result {
                    record_panic(walker_ref.progress, panic_message(&p));
                }
            })
            .expect("spawn scan pool thread");

        build(&rx, tree, &mut tokens, pool_thread, cancel);
    });

    if let Ok(mut t) = tree.write() {
        t.sort_all(SizeMode::Allocated);
    }
}

/// Builder loop: drain listings into the tree in batches.
fn build(
    rx: &Receiver<Msg>,
    tree: &Arc<RwLock<Tree>>,
    tokens: &mut HashMap<u64, NodeId>,
    pool_thread: std::thread::ScopedJoinHandle<'_, ()>,
    cancel: &CancellationToken,
) {
    let mut pool_thread = Some(pool_thread);
    loop {
        let first = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(m) => Some(m),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if pool_thread.as_ref().is_some_and(|t| t.is_finished()) {
                    let _ = pool_thread.take().map(|t| t.join());
                    // Drain whatever is left, then stop.
                    let rest: Vec<Msg> = rx.try_iter().collect();
                    if !rest.is_empty() {
                        apply(tree, tokens, rest, cancel);
                    }
                    return;
                }
                None
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        let Some(first) = first else { continue };
        let deadline = Instant::now() + Duration::from_millis(15);
        let mut batch = vec![first];
        while Instant::now() < deadline && batch.len() < 2048 {
            match rx.try_recv() {
                Ok(m) => batch.push(m),
                Err(_) => break,
            }
        }
        apply(tree, tokens, batch, cancel);
    }
}

fn apply(
    tree: &Arc<RwLock<Tree>>,
    tokens: &mut HashMap<u64, NodeId>,
    batch: Vec<Msg>,
    cancel: &CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }
    let mut t = tree.write().unwrap_or_else(|e| e.into_inner());
    for msg in batch {
        match msg {
            Msg::Listing { dir, entries } => {
                let Some(&parent) = tokens.get(&dir) else {
                    continue;
                };
                tokens.remove(&dir);
                for e in entries {
                    let id = t.add_child(parent, &e.name, e.info);
                    if let Some(tok) = e.token {
                        tokens.insert(tok, id);
                    }
                }
            }
            Msg::Error { dir, path, error } => {
                let node = tokens.remove(&dir);
                t.add_issue(node, path, error);
            }
        }
    }
}

/// Platform-neutral metadata for one directory entry (never follows links).
struct Stat {
    kind: BulkKind,
    meta: sr_platform::EntryMeta,
    mtime: i64,
}

impl Stat {
    fn from_metadata(path: &Path, m: &Metadata) -> Self {
        let ft = m.file_type();
        let kind = if ft.is_dir() {
            BulkKind::Dir
        } else if ft.is_symlink() {
            BulkKind::Symlink
        } else if ft.is_file() {
            BulkKind::File
        } else {
            BulkKind::Other
        };
        Self {
            kind,
            meta: sr_platform::entry_meta(path, m),
            mtime: mtime_of(m),
        }
    }
}

type Listing = Vec<(OsString, std::io::Result<Stat>)>;

/// List a directory with metadata: one bulk call per batch where the
/// platform supports it, otherwise `read_dir` plus per-entry metadata.
fn list_dir(path: &Path) -> std::io::Result<Listing> {
    if let Ok(bulk) = sr_platform::read_dir_bulk(path) {
        return Ok(bulk
            .into_iter()
            .map(|e| {
                let stat = match e.error {
                    Some(err) => Err(err),
                    None => Ok(Stat {
                        kind: e.kind,
                        meta: e.meta,
                        mtime: e.mtime,
                    }),
                };
                (e.name, stat)
            })
            .collect());
    }
    let mut out = Vec::new();
    for de in std::fs::read_dir(path)? {
        let de = de?;
        let name = de.file_name();
        // `DirEntry::metadata` does not follow symlinks (fstatat on Linux).
        let stat = de
            .metadata()
            .map(|m| Stat::from_metadata(&path.join(&name), &m));
        out.push((name, stat));
    }
    Ok(out)
}

impl Walker<'_> {
    fn walk<'s>(
        &'s self,
        scope: &rayon::Scope<'s>,
        path: PathBuf,
        token: u64,
        root_dev: Option<u64>,
    ) {
        self.pause.wait_while_paused(self.cancel);
        if self.cancel.is_cancelled() {
            return;
        }
        let listing = match list_dir(&path) {
            Ok(l) => l,
            Err(e) => {
                self.progress.errors.fetch_add(1, Ordering::Relaxed);
                let _ = self.tx.send(Msg::Error {
                    dir: token,
                    path,
                    error: e.to_string(),
                });
                return;
            }
        };
        if self.cancel.is_cancelled() {
            return;
        }
        let mut entries = Vec::with_capacity(listing.len());
        let mut subdirs = Vec::new();
        for (name, stat) in listing {
            let child = path.join(&name);
            if self.excluder.excluded(&child, &name) {
                continue;
            }
            let stat = match stat {
                Ok(s) => s,
                Err(e) => {
                    self.progress.errors.fetch_add(1, Ordering::Relaxed);
                    let mut info = EntryInfo::file(0, 0);
                    info.kind = NodeKind::Other;
                    info.flags |= NodeFlags::ERROR;
                    entries.push(RawEntry {
                        name,
                        info,
                        token: None,
                    });
                    let _ = self.tx.send(Msg::Error {
                        dir: u64::MAX,
                        path: child,
                        error: e.to_string(),
                    });
                    continue;
                }
            };
            let (info, descend) = self.classify(&child, &stat, root_dev);
            let mut entry = RawEntry {
                name,
                info,
                token: None,
            };
            if let Some(dev) = descend {
                let tok = self.next_token.fetch_add(1, Ordering::Relaxed);
                entry.token = Some(tok);
                subdirs.push((child, tok, dev));
            }
            entries.push(entry);
        }
        if self
            .tx
            .send(Msg::Listing {
                dir: token,
                entries,
            })
            .is_err()
        {
            return;
        }
        for (child, tok, dev) in subdirs {
            scope.spawn(move |s| self.walk(s, child, tok, dev));
        }
    }

    /// Build the entry and decide whether to descend. Returns the device to
    /// use for the child's boundary checks when descending.
    fn classify(
        &self,
        path: &Path,
        stat: &Stat,
        root_dev: Option<u64>,
    ) -> (EntryInfo, Option<Option<u64>>) {
        let pm = &stat.meta;
        let mtime = stat.mtime;
        let mut flags = NodeFlags::empty();
        if pm.cloud_placeholder {
            flags |= NodeFlags::CLOUD;
        }

        match stat.kind {
            BulkKind::Dir => {
                self.progress.dirs.fetch_add(1, Ordering::Relaxed);
                let mut info = EntryInfo::dir(mtime);
                info.flags = flags;
                let other_fs = !self.options.cross_filesystems
                    && root_dev.is_some()
                    && pm
                        .device
                        .is_some_and(|d| Some(d) != root_dev && !self.sibling_devs.contains(&d));
                // Never list the contents of a cloud placeholder directory:
                // doing so can trigger hydration (FR-SCAN-16).
                if other_fs || pm.cloud_placeholder {
                    info.flags |= NodeFlags::EXCLUDED;
                    return (info, None);
                }
                if self.options.follow_symlinks {
                    if let Some(id) = pm.file_id {
                        if !self.visited.lock().expect("visited lock").insert(id) {
                            return (info, None);
                        }
                    }
                }
                (info, Some(pm.device.or(root_dev)))
            }
            BulkKind::Symlink => {
                if self.options.follow_symlinks {
                    if let Ok(target) = std::fs::metadata(path) {
                        if target.is_dir() {
                            let tm = sr_platform::entry_meta(path, &target);
                            if let Some(id) = tm.file_id {
                                // Cycle detection: each physical directory is
                                // listed at most once.
                                if self.visited.lock().expect("visited lock").insert(id) {
                                    self.progress.dirs.fetch_add(1, Ordering::Relaxed);
                                    let mut info = EntryInfo::dir(mtime_of(&target));
                                    info.flags = flags;
                                    return (info, Some(tm.device.or(root_dev)));
                                }
                            }
                        }
                    }
                }
                let mut info = EntryInfo::file(pm.apparent, mtime);
                info.kind = NodeKind::Symlink;
                info.allocated = pm.allocated;
                info.flags = flags;
                self.progress.files.fetch_add(1, Ordering::Relaxed);
                (info, None)
            }
            BulkKind::File | BulkKind::Other => {
                let kind = if stat.kind == BulkKind::File {
                    NodeKind::File
                } else {
                    NodeKind::Other
                };
                self.progress.files.fetch_add(1, Ordering::Relaxed);
                self.progress
                    .bytes
                    .fetch_add(pm.allocated, Ordering::Relaxed);
                let file_id = if pm.nlink > 1 { pm.file_id } else { None };
                (
                    EntryInfo {
                        kind,
                        apparent: pm.apparent,
                        allocated: pm.allocated,
                        mtime,
                        flags,
                        file_id,
                    },
                    None,
                )
            }
        }
    }
}

fn record_panic(progress: &ScanProgress, msg: String) {
    if let Ok(mut p) = progress.panic.lock() {
        *p = Some(msg);
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "worker thread panicked".into()
    }
}

#[cfg(test)]
mod tests;
