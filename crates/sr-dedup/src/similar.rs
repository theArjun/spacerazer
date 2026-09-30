//! Perceptual image similarity (FR-DUP-10..12, FR-DUP-14).
//!
//! Each image is decoded, rotated according to its EXIF orientation,
//! downscaled to at most 512 px and hashed twice with `image_hasher`:
//! - dHash: `HashAlg::Gradient`, 8×8 → 64 bits;
//! - pHash: `HashAlg::Mean` with DCT preprocessing (`preproc_dct`), 64 bits.
//!
//! Two images match when **both** Hamming distances are ≤ the threshold
//! (scale 0–64; 0 = visually identical, default 8). Candidates are found
//! with a [`BkTree`] over the dHash, so grouping avoids O(n²) comparisons.
//! Matches are joined transitively (union-find); `max_distance` of a group
//! is the largest matching edge inside it.
//!
//! The `image` crate has no scaled (DCT-domain) JPEG decode, so the full
//! image is decoded once and then thumbnailed before hashing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader};
use image_hasher::{HashAlg, Hasher, HasherConfig};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sr_core::CancellationToken;

use crate::progress::Reporter;
use crate::{DupError, DupGroup, FileEntry, GroupKind, Pass, Progress, sort_groups};

const MAX_HASH_EDGE: u32 = 512;

/// 64-bit perceptual hashes of one image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageHashes {
    pub dhash: u64,
    pub phash: u64,
}

impl ImageHashes {
    /// The larger of the two Hamming distances.
    pub fn distance(&self, other: &Self) -> u32 {
        hamming(self.dhash, other.dhash).max(hamming(self.phash, other.phash))
    }
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Extensions decodable with the enabled `image` features.
pub fn is_image_path(p: &Path) -> bool {
    const EXTS: [&str; 9] = [
        "jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "jfif",
    ];
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// (dHash, pHash) hashers, built once.
type Hashers = (Hasher<[u8; 8]>, Hasher<[u8; 8]>);

fn hashers() -> &'static Hashers {
    static H: OnceLock<Hashers> = OnceLock::new();
    H.get_or_init(|| {
        let d = HasherConfig::with_bytes_type::<[u8; 8]>()
            .hash_alg(HashAlg::Gradient)
            .to_hasher();
        let p = HasherConfig::with_bytes_type::<[u8; 8]>()
            .hash_alg(HashAlg::Mean)
            .preproc_dct()
            .to_hasher();
        (d, p)
    })
}

/// Decode `path` (EXIF orientation applied), downscale and hash it.
/// Returns the hashes and the displayed (oriented) dimensions.
pub fn image_hash(path: &Path) -> Result<(ImageHashes, (u32, u32)), DupError> {
    let mut decoder = ImageReader::open(path)?
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let img = DynamicImage::from_decoder(decoder)?;
    let (w, h) = (img.width(), img.height());
    let mut img = if img.width() > MAX_HASH_EDGE || img.height() > MAX_HASH_EDGE {
        img.thumbnail(MAX_HASH_EDGE, MAX_HASH_EDGE)
    } else {
        img
    };
    img.apply_orientation(orientation);
    let dims = match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (h, w),
        _ => (w, h),
    };
    let (d, p) = hashers();
    let hashes = ImageHashes {
        dhash: u64::from_le_bytes(d.hash_image(&img).into_inner()),
        phash: u64::from_le_bytes(p.hash_image(&img).into_inner()),
    };
    Ok((hashes, dims))
}

/// A BK-tree over 64-bit keys with the Hamming metric (FR-DUP-11).
#[derive(Debug)]
pub struct BkTree<T> {
    nodes: Vec<BkNode<T>>,
}

#[derive(Debug)]
struct BkNode<T> {
    key: u64,
    value: T,
    children: Vec<(u32, usize)>,
}

impl<T> Default for BkTree<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

impl<T> BkTree<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn insert(&mut self, key: u64, value: T) {
        let idx = self.nodes.len();
        if idx > 0 {
            let mut cur = 0;
            loop {
                let d = hamming(key, self.nodes[cur].key);
                match self.nodes[cur].children.iter().find(|(cd, _)| *cd == d) {
                    Some(&(_, next)) => cur = next,
                    None => {
                        self.nodes[cur].children.push((d, idx));
                        break;
                    }
                }
            }
        }
        self.nodes.push(BkNode {
            key,
            value,
            children: Vec::new(),
        });
    }

    /// All values whose key is within `max_dist` of `query`, with distances.
    pub fn find(&self, query: u64, max_dist: u32) -> Vec<(&T, u32)> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let mut stack = vec![0];
        while let Some(i) = stack.pop() {
            let node = &self.nodes[i];
            let d = hamming(query, node.key);
            if d <= max_dist {
                out.push((&node.value, d));
            }
            let (lo, hi) = (d.saturating_sub(max_dist), d + max_dist);
            stack.extend(
                node.children
                    .iter()
                    .filter(|(cd, _)| (lo..=hi).contains(cd))
                    .map(|(_, c)| *c),
            );
        }
        out
    }
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Hash every image in `files` (others are ignored) and group visually
/// similar ones. Returns groups sorted by wasted space and per-file errors.
/// A cancelled run returns no groups.
pub fn find_similar_images(
    files: &[FileEntry],
    threshold: u32,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(Progress) + Sync),
) -> (Vec<DupGroup>, Vec<(PathBuf, String)>) {
    let images: Vec<&FileEntry> = files.iter().filter(|f| is_image_path(&f.path)).collect();
    let rep = Reporter::new(
        on_progress,
        Pass::Perceptual,
        images.len() as u64,
        images.iter().map(|f| f.size).sum(),
    );
    let results: Vec<Option<_>> = images
        .par_iter()
        .map(|f| {
            if cancel.is_cancelled() {
                return None;
            }
            let r = image_hash(&f.path);
            rep.add_hashed(f.size);
            rep.file_done(0);
            Some(r)
        })
        .collect();
    rep.emit();
    let mut errors = Vec::new();
    if cancel.is_cancelled() {
        return (Vec::new(), errors);
    }
    let mut hashed = Vec::new();
    for (f, r) in images.into_iter().zip(results.into_iter().flatten()) {
        match r {
            Ok((h, dims)) => hashed.push((f, h, dims)),
            Err(e) => errors.push((f.path.clone(), e.to_string())),
        }
    }

    let mut tree = BkTree::new();
    for (i, (_, h, _)) in hashed.iter().enumerate() {
        tree.insert(h.dhash, i);
    }
    let mut parent: Vec<usize> = (0..hashed.len()).collect();
    let mut edges = Vec::new();
    for (i, (_, h, _)) in hashed.iter().enumerate() {
        for (&j, dd) in tree.find(h.dhash, threshold) {
            if j <= i {
                continue;
            }
            let pd = hamming(h.phash, hashed[j].1.phash);
            if pd <= threshold {
                edges.push((i, dd.max(pd)));
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..hashed.len() {
        let r = find(&mut parent, i);
        comps.entry(r).or_default().push(i);
    }
    let mut max_edge: HashMap<usize, u32> = HashMap::new();
    for (i, d) in edges {
        let r = find(&mut parent, i);
        let e = max_edge.entry(r).or_default();
        *e = (*e).max(d);
    }
    let mut groups: Vec<DupGroup> = comps
        .into_iter()
        .filter(|(_, m)| m.len() >= 2)
        .map(|(root, mut members)| {
            members.sort_by(|&a, &b| hashed[a].0.path.cmp(&hashed[b].0.path));
            DupGroup {
                kind: GroupKind::Similar {
                    max_distance: max_edge.get(&root).copied().unwrap_or(0),
                },
                hash: None,
                size: members.iter().map(|&m| hashed[m].0.size).max().unwrap_or(0),
                files: members.iter().map(|&m| hashed[m].0.clone()).collect(),
                image_dims: members.iter().map(|&m| Some(hashed[m].2)).collect(),
            }
        })
        .collect();
    sort_groups(&mut groups);
    (groups, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bktree_matches_brute_force() {
        let mut x: u64 = 0x1234_5678_9abc_def1;
        let mut keys = Vec::new();
        for _ in 0..400 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Cluster keys so small distances occur.
            keys.push(if keys.len() % 3 == 0 {
                x
            } else {
                keys[keys.len() - 1] ^ (1 << (x % 64))
            });
        }
        let mut tree = BkTree::new();
        for (i, &k) in keys.iter().enumerate() {
            tree.insert(k, i);
        }
        assert_eq!(tree.len(), keys.len());
        for &q in keys.iter().step_by(7) {
            for max in [0, 1, 3, 10] {
                let mut got: Vec<usize> = tree.find(q, max).into_iter().map(|(v, _)| *v).collect();
                got.sort();
                let want: Vec<usize> = (0..keys.len())
                    .filter(|&i| hamming(keys[i], q) <= max)
                    .collect();
                assert_eq!(got, want);
            }
        }
    }

    #[test]
    fn image_extensions() {
        assert!(is_image_path(Path::new("/a/B.JPG")));
        assert!(is_image_path(Path::new("x.webp")));
        assert!(!is_image_path(Path::new("x.mp4")));
        assert!(!is_image_path(Path::new("jpg")));
    }
}
