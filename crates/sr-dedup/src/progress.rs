//! Throttled progress reporting shared by the hashing workers.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use crate::{Pass, Progress};

const EMIT_INTERVAL_MS: u64 = 100;

pub(crate) struct Reporter<'a> {
    cb: &'a (dyn Fn(Progress) + Sync),
    pass: Pass,
    files_total: u64,
    bytes_total: u64,
    files_done: AtomicU64,
    /// Bytes actually read from disk.
    pub(crate) hashed: AtomicU64,
    /// Bytes credited from the cache.
    cached: AtomicU64,
    start: Instant,
    last_emit_ms: AtomicU64,
}

impl<'a> Reporter<'a> {
    pub(crate) fn new(
        cb: &'a (dyn Fn(Progress) + Sync),
        pass: Pass,
        files_total: u64,
        bytes_total: u64,
    ) -> Self {
        let r = Self {
            cb,
            pass,
            files_total,
            bytes_total,
            files_done: AtomicU64::new(0),
            hashed: AtomicU64::new(0),
            cached: AtomicU64::new(0),
            start: Instant::now(),
            last_emit_ms: AtomicU64::new(0),
        };
        r.emit();
        r
    }

    pub(crate) fn add_hashed(&self, n: u64) {
        self.hashed.fetch_add(n, Relaxed);
        self.maybe_emit();
    }

    pub(crate) fn file_done(&self, cached_bytes: u64) {
        self.cached.fetch_add(cached_bytes, Relaxed);
        self.files_done.fetch_add(1, Relaxed);
        self.maybe_emit();
    }

    fn maybe_emit(&self) {
        let now = self.start.elapsed().as_millis() as u64;
        let last = self.last_emit_ms.load(Relaxed);
        if now >= last + EMIT_INTERVAL_MS
            && self
                .last_emit_ms
                .compare_exchange(last, now, Relaxed, Relaxed)
                .is_ok()
        {
            self.emit();
        }
    }

    pub(crate) fn emit(&self) {
        let secs = self.start.elapsed().as_secs_f64();
        let hashed = self.hashed.load(Relaxed);
        let done = (hashed + self.cached.load(Relaxed)).min(self.bytes_total.max(hashed));
        let throughput = if secs > 0.0 {
            hashed as f64 / secs
        } else {
            0.0
        };
        let eta_secs =
            (throughput > 0.0).then(|| self.bytes_total.saturating_sub(done) as f64 / throughput);
        (self.cb)(Progress {
            pass: self.pass,
            files_done: self.files_done.load(Relaxed),
            files_total: self.files_total,
            bytes_hashed: done,
            bytes_total: self.bytes_total,
            throughput,
            eta_secs,
        });
    }
}
