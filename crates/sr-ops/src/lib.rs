//! Destructive operations for SpaceRazer, all behind the Deletion Safety
//! Model (§7): the Trash Drawer, the revalidating executor, the quarantine
//! fallback, the operation journal and duplicate link replacement.
//!
//! No other crate may write, move, link or delete user files
//! (NFR-SAFE-01). Symlinks are never followed (NFR-SAFE-03).

use std::io;
use std::path::PathBuf;

mod drawer;
mod exec;
mod journal;
mod link;
mod quarantine;
mod snapshot;

pub use drawer::Drawer;
pub use exec::{
    ExecEvent, ExecOptions, Method, Outcome, Report, ReportEntry, execute,
    permanent_delete_threshold_exceeded,
};
pub use journal::{Journal, JournalRecord};
pub use link::{LinkKind, replace_with_link};
pub use quarantine::{
    QUARANTINE_DIR_NAME, empty_quarantine, quarantine_dirs, restore_from_quarantine,
};
pub use snapshot::{StagedItem, snapshot};

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{0} is a protected path")]
    Protected(PathBuf),
    #[error("{0} is already staged")]
    AlreadyStaged(PathBuf),
    #[error("{path} is already covered by staged folder {by}")]
    Covered { path: PathBuf, by: PathBuf },
    #[error("{path} changed since it was staged: {reason}")]
    Changed { path: PathBuf, reason: String },
    #[error("{path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error("journal/drawer serialisation: {0}")]
    Json(#[from] serde_json::Error),
}

impl OpsError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        OpsError::Io {
            path: path.into(),
            source,
        }
    }
    pub(crate) fn invalid(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        OpsError::Invalid {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// Extension trait attaching a path to `io::Result`s.
pub(crate) trait IoCtx<T> {
    fn ctx(self, path: &std::path::Path) -> Result<T, OpsError>;
}

impl<T> IoCtx<T> for io::Result<T> {
    fn ctx(self, path: &std::path::Path) -> Result<T, OpsError> {
        self.map_err(|e| OpsError::io(path, e))
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use sr_platform::ProtectedPaths;
    use std::path::Path;

    pub fn no_protection() -> ProtectedPaths {
        ProtectedPaths::custom(vec![], vec![])
    }

    /// Sorted recursive listing (path, len, mtime) without following links.
    pub fn listing(root: &Path) -> Vec<(PathBuf, u64, i64)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            let m = std::fs::symlink_metadata(&p).unwrap();
            out.push((
                p.clone(),
                m.len(),
                sr_core::unix_secs(m.modified().unwrap()),
            ));
            if m.is_dir() {
                for e in std::fs::read_dir(&p).unwrap() {
                    stack.push(e.unwrap().path());
                }
            }
        }
        out.sort();
        out
    }
}
