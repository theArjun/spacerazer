//! Revalidating deletion executor (§7.2).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sr_core::{CancellationToken, NodeKind};
use sr_platform::ProtectedPaths;

use crate::drawer::{reclaimable, top_level_mask};
use crate::quarantine::{move_to_quarantine, remove_tree, volume_root};
use crate::snapshot::revalidate;
use crate::{Journal, JournalRecord, StagedItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Method {
    /// OS trash, falling back to quarantine (default, recoverable).
    #[default]
    Trash,
    Permanent,
    /// Report only; no filesystem changes (FR-TRASH-05).
    DryRun,
}

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub method: Method,
    /// Entries a staged directory may gain before it is skipped as changed.
    pub dir_growth_tolerance: u64,
    /// Where quarantine folders go; default is the root of each item's volume.
    pub quarantine_root: Option<PathBuf>,
    /// Skip the OS trash and quarantine directly (tests, unsupported volumes).
    pub force_quarantine: bool,
    /// For `DryRun`: which real method the operation descriptions refer to.
    pub dry_run_as: Method,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// `destination` is set when the item was quarantined.
    Done {
        bytes_freed: u64,
        destination: Option<PathBuf>,
    },
    WouldDo {
        operation: String,
        bytes: u64,
    },
    Skipped {
        reason: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEntry {
    pub path: PathBuf,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub method: Method,
    /// One entry per processed item; items after a cancellation are absent.
    pub entries: Vec<ReportEntry>,
    /// `Done` (or, for a dry run, `WouldDo`) entries.
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Measured bytes freed; for a dry run, the expected total.
    pub bytes_freed: u64,
    pub cancelled: bool,
}

impl Report {
    fn new(method: Method) -> Self {
        Self {
            method,
            entries: Vec::new(),
            succeeded: 0,
            skipped: 0,
            failed: 0,
            bytes_freed: 0,
            cancelled: false,
        }
    }

    fn push(&mut self, entry: ReportEntry) {
        match &entry.outcome {
            Outcome::Done { bytes_freed, .. }
            | Outcome::WouldDo {
                bytes: bytes_freed, ..
            } => {
                self.succeeded += 1;
                self.bytes_freed += bytes_freed;
            }
            Outcome::Skipped { .. } => self.skipped += 1,
            Outcome::Failed { .. } => self.failed += 1,
        }
        self.entries.push(entry);
    }
}

#[derive(Debug, Clone)]
pub enum ExecEvent {
    Started { total: usize },
    Item { index: usize, entry: ReportEntry },
    Finished(Report),
}

/// FR-TRASH-06: does this batch need a deliberate confirmation?
pub fn permanent_delete_threshold_exceeded(
    items: &[StagedItem],
    bytes_threshold: u64,
    count_threshold: usize,
) -> bool {
    items.len() > count_threshold || reclaimable(items) > bytes_threshold
}

/// Expected bytes per item for a dry run: nested items count 0, and a
/// hardlink group is credited to its last member only if every link is
/// in the batch.
fn expected_bytes(items: &[StagedItem]) -> Vec<u64> {
    let top = top_level_mask(items);
    let mut groups: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    let mut out: Vec<u64> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            if !top[i] {
                return 0;
            }
            match it.file_id {
                Some(id) if it.kind != NodeKind::Dir && it.nlink > 1 => {
                    groups.entry(id).or_default().push(i);
                    0
                }
                _ => it.allocated,
            }
        })
        .collect();
    for idx in groups.values() {
        let first = &items[idx[0]];
        if idx.len() as u64 >= first.nlink {
            out[*idx.last().unwrap()] = first.allocated;
        }
    }
    out
}

/// Human-readable description of what a dry run would do to `item`.
fn describe(item: &StagedItem, method: Method) -> String {
    let what = match item.kind {
        NodeKind::Dir => format!("directory ({} entries)", item.entry_count),
        NodeKind::Symlink => "symlink (target untouched)".into(),
        NodeKind::File => "file".into(),
        NodeKind::Other => "special file".into(),
    };
    match method {
        Method::Permanent => format!("delete permanently: {what}"),
        _ => format!("move to trash: {what}"),
    }
}

/// Execute staged items in order. Each item is checked against the
/// protected list and revalidated immediately before its operation; a
/// failure never aborts other items (NFR-SAFE-06); cancellation is checked
/// between items. Every executed (non-dry-run) operation is journalled.
pub fn execute(
    items: &[StagedItem],
    opts: &ExecOptions,
    protected: &ProtectedPaths,
    journal: Option<&Journal>,
    cancel: &CancellationToken,
    on_event: &mut dyn FnMut(ExecEvent),
) -> Report {
    on_event(ExecEvent::Started { total: items.len() });
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let expected = expected_bytes(items);
    let mut report = Report::new(opts.method);
    for (index, item) in items.iter().enumerate() {
        if cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let outcome = run_item(item, expected[index], opts, protected, journal, &stamp);
        let entry = ReportEntry {
            path: item.path.clone(),
            outcome,
        };
        report.push(entry.clone());
        on_event(ExecEvent::Item { index, entry });
    }
    on_event(ExecEvent::Finished(report.clone()));
    report
}

fn run_item(
    item: &StagedItem,
    expected: u64,
    opts: &ExecOptions,
    protected: &ProtectedPaths,
    journal: Option<&Journal>,
    stamp: &str,
) -> Outcome {
    let path = &item.path;
    if protected.is_protected(path) {
        return Outcome::Skipped {
            reason: "protected path".into(),
        };
    }
    let current_bytes = match revalidate(item, opts.dir_growth_tolerance) {
        Ok(b) => b,
        Err(reason) => return Outcome::Skipped { reason },
    };
    let (operation, result, destination) = match opts.method {
        Method::DryRun => {
            return Outcome::WouldDo {
                operation: describe(item, opts.dry_run_as),
                bytes: expected,
            };
        }
        Method::Permanent => ("delete", remove_tree(path).map_err(|e| e.to_string()), None),
        Method::Trash => {
            let trash_err = if opts.force_quarantine {
                "quarantine forced".to_string()
            } else {
                match trash::delete(path) {
                    Ok(()) => return finish(journal, "trash", path, None, Ok(current_bytes)),
                    Err(e) => e.to_string(),
                }
            };
            let root = opts.quarantine_root.clone().or_else(|| volume_root(path));
            match root.map(|r| move_to_quarantine(path, &r, stamp)) {
                Some(Ok(dest)) => ("quarantine", Ok(current_bytes), Some(dest)),
                Some(Err(e)) => (
                    "quarantine",
                    Err(format!(
                        "trash unavailable ({trash_err}); quarantine failed: {e}"
                    )),
                    None,
                ),
                None => (
                    "quarantine",
                    Err(format!("trash unavailable ({trash_err}); no volume root")),
                    None,
                ),
            }
        }
    };
    finish(journal, operation, path, destination, result)
}

fn finish(
    journal: Option<&Journal>,
    operation: &str,
    path: &Path,
    destination: Option<PathBuf>,
    result: Result<u64, String>,
) -> Outcome {
    let mut result = result;
    if let Some(j) = journal {
        let rec = JournalRecord::new(operation, path, destination.clone(), result.clone());
        if let Err(e) = j.append(&rec) {
            // A completed operation stays `Done` (the item is gone either
            // way); a journal error is only attached to failures.
            result = result.map_err(|err| format!("{err}; journal write failed: {e}"));
        }
    }
    match result {
        Ok(bytes_freed) => Outcome::Done {
            bytes_freed,
            destination,
        },
        Err(error) => Outcome::Failed { error },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{listing, no_protection};
    use crate::{QUARANTINE_DIR_NAME, restore_from_quarantine, snapshot};
    use proptest::prelude::*;
    use sr_core::Module;
    use std::fs;

    fn opts(method: Method) -> ExecOptions {
        ExecOptions {
            method,
            ..Default::default()
        }
    }

    fn run(items: &[StagedItem], o: &ExecOptions, j: Option<&Journal>) -> Report {
        execute(
            items,
            o,
            &no_protection(),
            j,
            &CancellationToken::new(),
            &mut |_| {},
        )
    }

    fn snap(p: &Path) -> StagedItem {
        snapshot(p, Module::SpaceMap, "test").unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn ac04_symlink_to_dir_removes_link_only() {
        let t = tempfile::tempdir().unwrap();
        let big = t.path().join("big");
        fs::create_dir_all(big.join("inner")).unwrap();
        fs::write(big.join("inner/f"), vec![1u8; 50_000]).unwrap();
        for (i, method) in [Method::Permanent, Method::Trash].into_iter().enumerate() {
            let link = t.path().join(format!("link{i}"));
            std::os::unix::fs::symlink(&big, &link).unwrap();
            let o = ExecOptions {
                force_quarantine: true,
                quarantine_root: Some(t.path().join("q")),
                ..opts(method)
            };
            let r = run(&[snap(&link)], &o, None);
            assert_eq!(r.succeeded, 1, "{r:?}");
            assert!(fs::symlink_metadata(&link).is_err());
            assert_eq!(fs::read(big.join("inner/f")).unwrap().len(), 50_000);
            assert!(r.bytes_freed < 50_000);
        }
    }

    #[test]
    fn ac05_modified_after_staging_is_skipped() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        let g = t.path().join("g");
        fs::write(&f, b"one").unwrap();
        fs::write(&g, b"two").unwrap();
        let items = [snap(&f), snap(&g)];
        fs::write(&f, b"changed!").unwrap();
        let r = run(&items, &opts(Method::Permanent), None);
        assert!(
            matches!(&r.entries[0].outcome, Outcome::Skipped { reason } if reason.contains("size"))
        );
        assert!(matches!(r.entries[1].outcome, Outcome::Done { .. }));
        assert_eq!((r.succeeded, r.skipped, r.failed), (1, 1, 0));
        assert_eq!(fs::read(&f).unwrap(), b"changed!");
        assert!(!g.exists());
        // Missing on a second run.
        let r = run(&items[1..], &opts(Method::Permanent), None);
        assert!(
            matches!(&r.entries[0].outcome, Outcome::Skipped { reason } if reason == "no longer exists")
        );
    }

    #[test]
    fn protected_at_execution_time() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        fs::write(&f, b"x").unwrap();
        let p = ProtectedPaths::custom(vec![], vec![t.path().to_path_buf()]);
        let r = execute(
            &[snap(&f)],
            &opts(Method::Permanent),
            &p,
            None,
            &CancellationToken::new(),
            &mut |_| {},
        );
        assert_eq!(r.skipped, 1);
        assert!(f.exists());
    }

    #[test]
    fn dry_run_changes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir_all(d.join("s")).unwrap();
        fs::write(d.join("s/x"), vec![0u8; 9000]).unwrap();
        let f = t.path().join("f");
        fs::write(&f, b"hello").unwrap();
        let gone = t.path().join("gone");
        fs::write(&gone, b"bye").unwrap();
        let items = [snap(&d), snap(&f), snap(&gone)];
        fs::remove_file(&gone).unwrap();
        let before = listing(t.path());
        let j = Journal::open(t.path().join("journal.jsonl"));
        let mut events = 0;
        let r = execute(
            &items,
            &opts(Method::DryRun),
            &no_protection(),
            Some(&j),
            &CancellationToken::new(),
            &mut |_| events += 1,
        );
        assert_eq!(listing(t.path()), before);
        assert!(!j.path().exists(), "dry run is not journalled");
        assert_eq!(events, 5);
        assert_eq!((r.succeeded, r.skipped), (2, 1));
        assert_eq!(r.bytes_freed, items[0].allocated + items[1].allocated);
        assert!(
            matches!(&r.entries[0].outcome, Outcome::WouldDo { operation, .. } if operation.contains("directory"))
        );
    }

    #[test]
    fn permanent_dir_delete_measures_and_journals() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/b/f"), vec![0u8; 12_345]).unwrap();
        let item = snap(&d);
        let j = Journal::open(t.path().join("journal.jsonl"));
        let r = run(
            std::slice::from_ref(&item),
            &opts(Method::Permanent),
            Some(&j),
        );
        assert!(!d.exists());
        assert_eq!(r.bytes_freed, item.allocated);
        let recs = j.read_all().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(
            (recs[0].operation.as_str(), recs[0].size),
            ("delete", item.allocated)
        );
    }

    #[test]
    fn hardlinks_free_bytes_only_with_last_link() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a");
        let b = t.path().join("b");
        fs::write(&a, vec![3u8; 40_000]).unwrap();
        fs::hard_link(&a, &b).unwrap();
        let items = [snap(&a), snap(&b)];
        assert_eq!(run(&items[..1], &opts(Method::DryRun), None).bytes_freed, 0);
        let dry = run(&items, &opts(Method::DryRun), None);
        assert_eq!(dry.bytes_freed, items[0].allocated);
        // Deleting `a` bumps b's ctime but not mtime, so b still revalidates.
        let r = run(&items, &opts(Method::Permanent), None);
        let freed: Vec<u64> = r
            .entries
            .iter()
            .map(|e| match e.outcome {
                Outcome::Done { bytes_freed, .. } => bytes_freed,
                _ => panic!("{e:?}"),
            })
            .collect();
        assert_eq!(freed, vec![0, items[0].allocated]);
    }

    #[test]
    fn quarantine_fallback_and_restore() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("data/f");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, b"precious").unwrap();
        let q = t.path().join("qroot");
        let o = ExecOptions {
            force_quarantine: true,
            quarantine_root: Some(q.clone()),
            ..opts(Method::Trash)
        };
        let j = Journal::open(t.path().join("journal.jsonl"));
        let r = run(&[snap(&f)], &o, Some(&j));
        let Outcome::Done {
            destination: Some(dest),
            ..
        } = &r.entries[0].outcome
        else {
            panic!("{r:?}")
        };
        assert!(dest.starts_with(q.join(QUARANTINE_DIR_NAME)));
        assert!(dest.ends_with("data/f"));
        assert!(!f.exists());
        assert_eq!(fs::read(dest).unwrap(), b"precious");
        let rec = &j.read_all().unwrap()[0];
        assert_eq!(rec.operation, "quarantine");
        restore_from_quarantine(rec).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"precious");
        assert!(
            restore_from_quarantine(rec).is_err(),
            "no longer in quarantine"
        );
    }

    #[test]
    fn failure_does_not_abort_others() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        let g = t.path().join("g");
        fs::write(&f, b"1").unwrap();
        fs::write(&g, b"2").unwrap();
        let blocker = t.path().join("not-a-dir");
        fs::write(&blocker, b"").unwrap();
        let items = [snap(&f), snap(&g)];
        let o = ExecOptions {
            force_quarantine: true,
            quarantine_root: Some(blocker),
            ..opts(Method::Trash)
        };
        let r = run(&items[..1], &o, None);
        assert_eq!(r.failed, 1);
        assert!(f.exists());
        let r = run(&items, &opts(Method::Permanent), None);
        assert_eq!(r.succeeded, 2);
    }

    #[test]
    fn cancellation_between_items() {
        let t = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..3).map(|i| t.path().join(format!("f{i}"))).collect();
        for p in &paths {
            fs::write(p, b"x").unwrap();
        }
        let items: Vec<StagedItem> = paths.iter().map(|p| snap(p)).collect();
        let cancel = CancellationToken::new();
        let mut finished = None;
        let r = execute(
            &items,
            &opts(Method::Permanent),
            &no_protection(),
            None,
            &cancel,
            &mut |e| match e {
                ExecEvent::Item { .. } => cancel.cancel(),
                ExecEvent::Finished(r) => finished = Some(r),
                ExecEvent::Started { total } => assert_eq!(total, 3),
            },
        );
        assert!(r.cancelled);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(finished.as_ref(), Some(&r));
        assert!(!paths[0].exists() && paths[1].exists() && paths[2].exists());
    }

    #[test]
    fn threshold_helper() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f");
        fs::write(&f, vec![0u8; 10_000]).unwrap();
        let items = [snap(&f)];
        assert!(!permanent_delete_threshold_exceeded(&items, 1 << 40, 1000));
        assert!(permanent_delete_threshold_exceeded(&items, 100, 1000));
        assert!(permanent_delete_threshold_exceeded(&items, 1 << 40, 0));
    }

    #[derive(Debug, Clone, Copy)]
    enum Change {
        None,
        /// Files: rewritten with a new size; dirs: a new entry added.
        Grow,
        Delete,
        ReplaceSameSize,
        ReplaceWithDir,
    }

    fn arb_change() -> impl Strategy<Value = Change> {
        prop_oneof![
            3 => Just(Change::None),
            1 => Just(Change::Grow),
            1 => Just(Change::Delete),
            1 => Just(Change::ReplaceSameSize),
            1 => Just(Change::ReplaceWithDir),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        /// Items modified between staging and execution are never deleted;
        /// unmodified ones are.
        #[test]
        fn modified_items_never_deleted(
            spec in prop::collection::vec((any::<bool>(), arb_change()), 1..8)
        ) {
            let t = tempfile::tempdir().unwrap();
            let mut items = Vec::new();
            for (i, (is_dir, _)) in spec.iter().enumerate() {
                let p = t.path().join(format!("item{i}"));
                if *is_dir {
                    fs::create_dir_all(p.join("sub")).unwrap();
                    fs::write(p.join("sub/x"), b"content").unwrap();
                } else {
                    fs::write(&p, b"content").unwrap();
                }
                items.push(snap(&p));
            }
            for (item, (is_dir, change)) in items.iter().zip(&spec) {
                let p = &item.path;
                let sentinel = |p: &Path| fs::write(p, b"new").unwrap();
                match (change, is_dir) {
                    (Change::None, _) => {}
                    (Change::Grow, false) => sentinel(p),
                    (Change::Grow, true) => sentinel(&p.join("new")),
                    (Change::Delete, _) => { let _ = fs::remove_dir_all(p).or_else(|_| fs::remove_file(p)); }
                    (Change::ReplaceSameSize, false) => {
                        let tmp = p.with_extension("tmp");
                        fs::write(&tmp, b"CONTENT").unwrap();
                        fs::rename(&tmp, p).unwrap();
                    }
                    (Change::ReplaceSameSize, true) => {
                        fs::remove_dir_all(p).unwrap();
                        fs::create_dir(p).unwrap();
                        sentinel(&p.join("a"));
                        sentinel(&p.join("b"));
                        sentinel(&p.join("c"));
                    }
                    (Change::ReplaceWithDir, false) => { fs::remove_file(p).unwrap(); fs::create_dir(p).unwrap(); }
                    (Change::ReplaceWithDir, true) => { fs::remove_dir_all(p).unwrap(); sentinel(p); }
                }
            }
            let r = run(&items, &opts(Method::Permanent), None);
            for ((item, (_, change)), entry) in items.iter().zip(&spec).zip(&r.entries) {
                let exists = fs::symlink_metadata(&item.path).is_ok();
                match change {
                    Change::None => {
                        prop_assert!(matches!(entry.outcome, Outcome::Done { .. }), "{:?}", entry);
                        prop_assert!(!exists);
                    }
                    Change::Delete => {
                        prop_assert!(matches!(entry.outcome, Outcome::Skipped { .. }), "{:?}", entry)
                    }
                    _ => {
                        prop_assert!(matches!(entry.outcome, Outcome::Skipped { .. }), "{:?}", entry);
                        prop_assert!(exists, "modified item deleted: {:?}", change);
                    }
                }
            }
        }
    }
}
