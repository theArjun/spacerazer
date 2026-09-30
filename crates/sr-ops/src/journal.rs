//! Append-only operation journal (§7.4).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{IoCtx, OpsError};

/// One operation. `operation` is one of `trash`, `quarantine`, `delete`,
/// `hardlink`, `reflink`, `restore`, `empty-quarantine`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct JournalRecord {
    /// Unix seconds.
    pub timestamp: i64,
    pub operation: String,
    pub path: PathBuf,
    /// Where the item went (quarantine) or what it now links to.
    pub destination: Option<PathBuf>,
    /// Bytes freed (measured where possible).
    pub size: u64,
    /// `ok` or `failed`.
    pub result: String,
    pub error: Option<String>,
}

impl JournalRecord {
    pub fn new(
        operation: &str,
        path: &Path,
        destination: Option<PathBuf>,
        outcome: Result<u64, String>,
    ) -> Self {
        let (size, result, error) = match outcome {
            Ok(n) => (n, "ok", None),
            Err(e) => (0, "failed", Some(e)),
        };
        Self {
            timestamp: sr_core::now_secs(),
            operation: operation.into(),
            path: path.to_path_buf(),
            destination,
            size,
            result: result.into(),
            error,
        }
    }

    pub fn succeeded(&self) -> bool {
        self.result == "ok"
    }
}

/// A JSON Lines journal file.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<app data dir>/journal.jsonl`.
    pub fn default_location() -> Option<PathBuf> {
        sr_platform::app_dirs().map(|d| d.data.join("journal.jsonl"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record as a single line (one `write` call).
    pub fn append(&self, rec: &JournalRecord) -> Result<(), OpsError> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).ctx(dir)?;
        }
        let mut line = serde_json::to_vec(rec)?;
        line.push(b'\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ctx(&self.path)?;
        f.write_all(&line).ctx(&self.path)
    }

    /// All records, oldest first. A missing file is an empty journal; lines
    /// that fail to parse (e.g. torn by a crash) are skipped.
    pub fn read_all(&self) -> Result<Vec<JournalRecord>, OpsError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(OpsError::io(&self.path, e)),
        };
        Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_read() {
        let t = tempfile::tempdir().unwrap();
        let j = Journal::open(t.path().join("sub/j.jsonl"));
        assert!(j.read_all().unwrap().is_empty());
        let a = JournalRecord::new("delete", Path::new("/x"), None, Ok(5));
        let b = JournalRecord::new("trash", Path::new("/y"), None, Err("boom".into()));
        j.append(&a).unwrap();
        j.append(&b).unwrap();
        // Simulate a torn final line.
        OpenOptions::new()
            .append(true)
            .open(j.path())
            .unwrap()
            .write_all(b"{\"timest")
            .unwrap();
        let all = j.read_all().unwrap();
        assert_eq!(all, vec![a, b]);
        assert!(all[0].succeeded() && !all[1].succeeded());
    }
}
