//! DuplicateLens engine (README §3.5, §6.5): exact duplicate detection via a
//! multi-pass pipeline and perceptual similarity for images.
//!
//! Pipeline for exact duplicates:
//!
//! 1. [`collect_files`] walks the roots without following symlinks, keeping
//!    regular files that pass the size and glob filters.
//! 2. [`group_by_size`] drops unique sizes and collapses hardlink twins
//!    (same device + inode / file index) into one physical file (FR-DUP-03).
//! 3. [`partial_hash`]: xxh3-128 over `size ‖ head ‖ tail` (FR-DUP-04).
//! 4. [`full_hash`]: BLAKE3, memory-mapped with parallel hashing for large
//!    files and buffered reads otherwise (FR-DUP-05).
//! 5. Optional paranoid byte-by-byte verification ([`files_identical`],
//!    FR-DUP-06).
//!
//! Hashing runs on a dedicated rayon pool sized by device type (FR-DUP-07)
//! and results can be cached across runs (FR-DUP-08, see [`DupOptions::hash_cache`]).
//!
//! Similar images (FR-DUP-10..12) are hashed with dHash + pHash on an
//! EXIF-oriented, downscaled decode and grouped through a [`BkTree`].
//!
//! Not implemented: video similarity (FR-DUP-13, optional "C" priority).
//! Video files are only ever considered by the exact-duplicate pipeline.
//!
//! `unsafe` is confined to one function mapping a file read-only (spec C-1).

#![deny(unsafe_code)]

mod cache;
mod collect;
mod export;
mod hash;
mod pipeline;
mod progress;
mod select;
mod similar;

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use collect::{collect_files, group_by_size};
pub use export::{export_csv, export_json};
pub use hash::{files_identical, full_hash, partial_hash};
pub use pipeline::find_duplicates;
pub use select::{AutoSelect, auto_select, selection_is_valid};
pub use similar::{BkTree, ImageHashes, find_similar_images, hamming, image_hash, is_image_path};

/// Options for a duplicate search (FR-DUP-01).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DupOptions {
    pub roots: Vec<PathBuf>,
    /// Minimum file size in bytes; values below 1 are treated as 1.
    pub min_size: u64,
    /// Globs matched against the full path or the file name; empty = all.
    pub include: Vec<String>,
    /// Globs matched against the full path or the name; matching
    /// directories are pruned.
    pub exclude: Vec<String>,
    /// Bytes hashed from the head and from the tail in pass 2.
    pub partial_bytes: u64,
    /// Files at least this large are hashed via mmap + parallel BLAKE3.
    pub mmap_threshold: u64,
    /// Byte-by-byte verification of final groups (FR-DUP-06).
    pub paranoid: bool,
    /// Hashing threads; default from the root's device type (FR-DUP-07).
    pub io_threads: Option<usize>,
    /// Hash cache file (FR-DUP-08). Entries are keyed by path, size, mtime
    /// (seconds) and file id; a corrupt or foreign-version file is discarded.
    pub hash_cache: Option<PathBuf>,
    /// Also search for visually similar images (FR-DUP-10).
    pub similar_images: bool,
    /// Maximum Hamming distance on 64-bit hashes (0 = visually identical,
    /// 64 = anything). Both dHash and pHash must be within it. Default 8.
    pub similarity_threshold: u32,
}

impl Default for DupOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            min_size: 1_048_576,
            include: Vec::new(),
            exclude: Vec::new(),
            partial_bytes: 16 * 1024,
            mmap_threshold: 64 * 1024 * 1024,
            paranoid: false,
            io_threads: None,
            hash_cache: None,
            similar_images: false,
            similarity_threshold: 8,
        }
    }
}

/// A candidate file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
    /// (device, inode) or (volume serial, file index).
    pub file_id: Option<(u64, u64)>,
    pub device: Option<u64>,
}

/// Pipeline stage reported in [`Progress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pass {
    Collect,
    Size,
    Partial,
    Full,
    Verify,
    Perceptual,
}

/// Per-pass progress (FR-DUP-09).
#[derive(Clone, Debug)]
pub struct Progress {
    pub pass: Pass,
    pub files_done: u64,
    pub files_total: u64,
    /// Bytes processed in this pass (read from disk or served from cache).
    pub bytes_hashed: u64,
    pub bytes_total: u64,
    /// Bytes actually read per second in this pass.
    pub throughput: f64,
    pub eta_secs: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKind {
    Identical,
    /// Perceptual match; never auto-selected (FR-DUP-14).
    Similar {
        max_distance: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DupGroup {
    pub kind: GroupKind,
    /// BLAKE3 hex digest for identical groups.
    pub hash: Option<String>,
    /// Per-file size for identical groups; largest member for similar ones.
    pub size: u64,
    pub files: Vec<FileEntry>,
    /// Parallel to `files` for similar groups; empty for identical groups.
    pub image_dims: Vec<Option<(u32, u32)>>,
}

impl DupGroup {
    /// Reclaimable bytes: `size × (n − 1)` for identical groups, total minus
    /// the largest member for similar groups.
    pub fn wasted(&self) -> u64 {
        match self.kind {
            GroupKind::Identical => self.size * (self.files.len() as u64).saturating_sub(1),
            GroupKind::Similar { .. } => {
                let total: u64 = self.files.iter().map(|f| f.size).sum();
                let max = self.files.iter().map(|f| f.size).max().unwrap_or(0);
                total - max
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub files_scanned: u64,
    pub candidates_after_size: u64,
    pub candidates_after_partial: u64,
    pub candidates_after_full: u64,
    /// Equal to `candidates_after_full` unless paranoid mode split groups.
    pub candidates_after_verify: u64,
    /// Bytes actually read for partial + full hashing (cache hits excluded).
    pub bytes_hashed: u64,
    pub cache_hits: u64,
    pub hardlink_twins_skipped: u64,
    pub images_hashed: u64,
    pub cancelled: bool,
    pub elapsed: Duration,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DupResult {
    /// Identical groups sorted by wasted space, descending (FR-DUP-15).
    pub groups: Vec<DupGroup>,
    /// Similar-image groups sorted by wasted space, descending.
    pub similar: Vec<DupGroup>,
    pub errors: Vec<(PathBuf, String)>,
    pub stats: Stats,
}

impl DupResult {
    /// Total reclaimable bytes over the identical groups.
    pub fn total_wasted(&self) -> u64 {
        self.groups.iter().map(DupGroup::wasted).sum()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DupError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("image decode failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("file changed size during hashing (expected {expected} bytes, now {actual})")]
    SizeChanged { expected: u64, actual: u64 },
    #[error("cancelled")]
    Cancelled,
}

impl From<DupError> for std::io::Error {
    fn from(e: DupError) -> Self {
        match e {
            DupError::Io(e) => e,
            DupError::Cancelled => std::io::Error::new(std::io::ErrorKind::Interrupted, e),
            other => std::io::Error::other(other),
        }
    }
}

/// Sort groups by wasted space (desc), then size (desc), then first path.
pub(crate) fn sort_groups(groups: &mut [DupGroup]) {
    groups.sort_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
            .then(b.size.cmp(&a.size))
            .then_with(|| {
                a.files
                    .first()
                    .map(|f| &f.path)
                    .cmp(&b.files.first().map(|f| &f.path))
            })
    });
}
