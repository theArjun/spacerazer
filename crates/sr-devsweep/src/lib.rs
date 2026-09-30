//! DevSweep (§3.4): detects developer projects by marker files, sizes their
//! regenerable build artifacts, reports global toolchain caches and Docker
//! usage, and computes staleness from source files only.
//!
//! Rules are data (`rules.toml`, NFR-MAIN-03). External tools are run with
//! argument vectors, never through a shell (NFR-SEC-04).

mod caches;
mod rules;
mod scan;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};
use sr_core::CancellationToken;

pub use caches::{
    DockerRow, DockerUsage, GlobalCache, docker_prune_command, docker_usage,
    global_cache_locations, parse_docker_df, scan_global_caches,
};
pub use rules::{Risk, Rule, RuleError, RuleKind, RuleSet};

const DAY: i64 = 86_400;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub ecosystem: String,
    pub allocated: u64,
    pub apparent: u64,
    pub entries: u64,
    pub risk: Risk,
    pub risk_reason: String,
    pub regenerate: String,
    pub tracked_by_git: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub path: PathBuf,
    pub name: String,
    pub types: Vec<String>,
    pub artifacts: Vec<Artifact>,
    pub artifact_size: u64,
    pub source_size: u64,
    /// Newest mtime of source files (excluding artifacts, VCS metadata and
    /// nested projects); 0 if none.
    pub last_source_mtime: i64,
    pub last_commit: Option<i64>,
    /// Official clean command (FR-DEV-11); run with cwd = `path`.
    #[serde(default)]
    pub clean_command: Option<Vec<String>>,
}

impl Project {
    /// Last activity: newest source mtime or last commit, whichever is later.
    pub fn last_activity(&self) -> i64 {
        self.last_source_mtime
            .max(self.last_commit.unwrap_or(i64::MIN))
    }

    pub fn inactive_days(&self, now: i64) -> u64 {
        (now.saturating_sub(self.last_activity()).max(0) / DAY) as u64
    }

    pub fn is_stale(&self, threshold_days: u64, now: i64) -> bool {
        self.inactive_days(now) >= threshold_days
    }
}

pub struct DevOptions {
    pub roots: Vec<PathBuf>,
    pub rules: RuleSet,
    pub check_git: bool,
    /// Maximum directory depth below each root (root = 0).
    pub max_depth: Option<usize>,
    pub exclude: Vec<PathBuf>,
}

impl DevOptions {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            rules: RuleSet::builtin(),
            check_git: true,
            max_depth: None,
            exclude: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum DevEvent {
    /// Cumulative number of directories visited so far.
    DirsVisited(u64),
    ProjectFound(Project),
}

#[derive(Clone, Debug, Default)]
pub struct DevReport {
    /// Sorted by artifact size, largest first.
    pub projects: Vec<Project>,
    /// Unreadable directories and invalid custom rules (empty path).
    pub errors: Vec<(PathBuf, String)>,
}

/// Discover projects under `opts.roots` and size their artifacts in parallel.
pub fn analyze(
    opts: &DevOptions,
    cancel: &CancellationToken,
    on_event: &(dyn Fn(DevEvent) + Sync),
) -> DevReport {
    scan::analyze(opts, cancel, on_event)
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Empty = all types; otherwise any match (case-insensitive).
    pub types: Vec<String>,
    pub min_artifact_size: u64,
    pub inactive_days: Option<u64>,
}

/// FR-DEV-07 filtering.
pub fn filter_projects<'a>(projects: &'a [Project], f: &Filter, now: i64) -> Vec<&'a Project> {
    projects
        .iter()
        .filter(|p| {
            (f.types.is_empty()
                || p.types
                    .iter()
                    .any(|t| f.types.iter().any(|w| w.eq_ignore_ascii_case(t))))
                && p.artifact_size >= f.min_artifact_size
                && f.inactive_days.is_none_or(|d| p.is_stale(d, now))
        })
        .collect()
}

/// FR-DEV-08 "select all stale": artifacts of stale projects, skipping pinned
/// projects (and anything below a pinned path) and `Review` artifacts, which
/// always need an explicit decision.
pub fn select_stale<'a>(
    projects: &'a [Project],
    threshold_days: u64,
    pinned: &[PathBuf],
    now: i64,
) -> Vec<&'a Artifact> {
    projects
        .iter()
        .filter(|p| p.is_stale(threshold_days, now))
        .filter(|p| !pinned.iter().any(|pin| p.path.starts_with(pin)))
        .flat_map(|p| &p.artifacts)
        .filter(|a| a.risk != Risk::Review)
        .collect()
}

/// The tool's own clean command (FR-DEV-11), to be run via [`run_command`]
/// with `cwd = project.path`.
pub fn tool_clean_command(project: &Project) -> Option<Vec<String>> {
    project.clean_command.clone()
}

/// Run an argument vector (never via a shell, NFR-SEC-04).
pub fn run_command(argv: &[String], cwd: &Path) -> std::io::Result<Output> {
    let (prog, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
}

/// Run `git -C dir <args> [extra]`; stdout on success, `None` if git is
/// missing or fails.
pub(crate) fn git(dir: &Path, args: &[&str], extra: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    if let Some(e) = extra {
        cmd.arg(e);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}
