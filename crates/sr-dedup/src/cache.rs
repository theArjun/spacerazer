//! Persistent hash cache (FR-DUP-08, NFR-REL-04).
//!
//! File format: a header line `SRHC <version> <blake3-hex of payload>\n`
//! followed by the JSON payload. A missing, corrupt, tampered or
//! foreign-version file is discarded and the cache starts empty.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::FileEntry;

const MAGIC: &str = "SRHC";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: i64,
    file_id: Option<(u64, u64)>,
    /// (partial_bytes used, xxh3-128 hex).
    partial: Option<(u64, String)>,
    /// BLAKE3 hex.
    full: Option<String>,
}

impl CacheEntry {
    fn fresh(f: &FileEntry) -> Self {
        Self {
            size: f.size,
            mtime: f.mtime,
            file_id: f.file_id,
            partial: None,
            full: None,
        }
    }
    fn matches(&self, f: &FileEntry) -> bool {
        self.size == f.size && self.mtime == f.mtime && self.file_id == f.file_id
    }
}

#[derive(Debug, Default)]
pub(crate) struct HashCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

impl HashCache {
    /// Load `path`, returning an empty cache on any problem.
    pub(crate) fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| Self::parse(&b))
            .unwrap_or_default()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        let nl = bytes.iter().position(|&b| b == b'\n')?;
        let header = std::str::from_utf8(&bytes[..nl]).ok()?;
        let payload = &bytes[nl + 1..];
        let mut parts = header.split(' ');
        if parts.next()? != MAGIC || parts.next()?.parse::<u32>().ok()? != VERSION {
            return None;
        }
        if parts.next()? != blake3::hash(payload).to_hex().as_str() {
            return None;
        }
        let entries = serde_json::from_slice(payload).ok()?;
        Some(Self { entries })
    }

    /// Write atomically (temp file + rename).
    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        let payload = serde_json::to_vec(&self.entries)?;
        let mut out =
            format!("{MAGIC} {VERSION} {}\n", blake3::hash(&payload).to_hex()).into_bytes();
        out.extend_from_slice(&payload);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, out)?;
        fs::rename(&tmp, path)
    }

    fn valid(&self, f: &FileEntry) -> Option<&CacheEntry> {
        self.entries.get(&f.path).filter(|e| e.matches(f))
    }

    fn slot(&mut self, f: &FileEntry) -> &mut CacheEntry {
        let e = self
            .entries
            .entry(f.path.clone())
            .or_insert_with(|| CacheEntry::fresh(f));
        if !e.matches(f) {
            *e = CacheEntry::fresh(f);
        }
        e
    }

    pub(crate) fn partial(&self, f: &FileEntry, partial_bytes: u64) -> Option<u128> {
        let (pb, hex) = self.valid(f)?.partial.as_ref()?;
        (*pb == partial_bytes).then(|| u128::from_str_radix(hex, 16).ok())?
    }

    pub(crate) fn full(&self, f: &FileEntry) -> Option<[u8; 32]> {
        let hex = self.valid(f)?.full.as_ref()?;
        blake3::Hash::from_hex(hex).ok().map(|h| *h.as_bytes())
    }

    pub(crate) fn record_partial(&mut self, f: &FileEntry, partial_bytes: u64, h: u128) {
        self.slot(f).partial = Some((partial_bytes, format!("{h:032x}")));
    }

    pub(crate) fn record_full(&mut self, f: &FileEntry, h: &[u8; 32]) {
        self.slot(f).full = Some(blake3::Hash::from_bytes(*h).to_hex().to_string());
    }

    /// Drop entries under `roots` that were not seen in the latest walk
    /// (deleted or no longer eligible files).
    pub(crate) fn prune(&mut self, roots: &[PathBuf], seen: &HashSet<PathBuf>) {
        self.entries
            .retain(|p, _| seen.contains(p) || !roots.iter().any(|r| p.starts_with(r)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(p: &str) -> FileEntry {
        FileEntry {
            path: p.into(),
            size: 10,
            mtime: 5,
            file_id: Some((1, 2)),
            device: Some(1),
        }
    }

    #[test]
    fn round_trip_and_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.cache");
        let mut c = HashCache::default();
        let f = entry("/x/a");
        c.record_partial(&f, 16, 0xdead_beef);
        c.record_full(&f, &[7; 32]);
        c.save(&path).unwrap();

        let c = HashCache::load(&path);
        assert_eq!(c.partial(&f, 16), Some(0xdead_beef));
        assert_eq!(c.partial(&f, 32), None, "different partial size");
        assert_eq!(c.full(&f), Some([7; 32]));
        let changed = FileEntry {
            mtime: 6,
            ..f.clone()
        };
        assert_eq!(c.full(&changed), None);

        // Tampered payload -> checksum mismatch -> discarded.
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 2;
        bytes[last] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(HashCache::load(&path).entries.is_empty());

        // Wrong version -> discarded.
        let text = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        fs::write(&path, text.replacen("SRHC 1", "SRHC 99", 1)).unwrap();
        assert!(HashCache::load(&path).entries.is_empty());
        fs::write(&path, b"garbage").unwrap();
        assert!(HashCache::load(&path).entries.is_empty());
    }
}
