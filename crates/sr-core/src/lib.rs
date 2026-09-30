//! Shared core types for SpaceRazer: the arena file tree, interned names,
//! size formatting, cancellation and module identifiers.

pub mod format;
pub mod names;
pub mod tree;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

pub use format::{SizeUnits, format_size};
pub use names::{NameArena, NameId};
pub use tree::{EntryInfo, Node, NodeFlags, NodeId, NodeKind, ScanIssue, SizeMode, Tree};

/// Which part of the app produced a staged item or request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Module {
    SpaceMap,
    DevSweep,
    DuplicateLens,
}

impl Module {
    pub fn label(self) -> &'static str {
        match self {
            Module::SpaceMap => "Map",
            Module::DevSweep => "DevSweep",
            Module::DuplicateLens => "DuplicateLens",
        }
    }
}

/// Shared cancellation flag checked by background jobs between units of
/// work (§6.2).
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Shared pause flag. Workers call [`PauseToken::wait_while_paused`] between
/// units of work; it returns early if the job is cancelled.
#[derive(Debug, Clone, Default)]
pub struct PauseToken(Arc<AtomicBool>);

impl PauseToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_paused(&self, paused: bool) {
        self.0.store(paused, Ordering::Relaxed);
    }
    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub fn wait_while_paused(&self, cancel: &CancellationToken) {
        while self.is_paused() && !cancel.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// Seconds since the Unix epoch for a `SystemTime`, negative before 1970.
pub fn unix_secs(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub fn now_secs() -> i64 {
    unix_secs(std::time::SystemTime::now())
}
