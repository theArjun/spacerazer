//! Parallel project discovery and artifact sizing.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use sr_core::{CancellationToken, unix_secs};
use sr_platform::{FileId, entry_meta};

use crate::rules::{CompiledRule, RuleKind, compile};
use crate::{Artifact, DevEvent, DevOptions, DevReport, Project, Risk, git};

const VCS_DIRS: [&str; 3] = [".git", ".hg", ".svn"];
const TICK: u64 = 256;

/// Recursive size of a directory (allocated bytes, hardlinks counted once).
#[derive(Default)]
pub(crate) struct DirSize {
    pub allocated: u64,
    pub apparent: u64,
    pub entries: u64,
    /// A `.git` entry exists somewhere inside (nested repository).
    pub has_repo: bool,
    links: Vec<(FileId, u64, u64)>,
}

impl DirSize {
    fn merge(mut self, o: DirSize) -> DirSize {
        self.allocated += o.allocated;
        self.apparent += o.apparent;
        self.entries += o.entries;
        self.has_repo |= o.has_repo;
        self.links.extend(o.links);
        self
    }
}

pub(crate) fn size_dir(path: &Path, cancel: &CancellationToken) -> DirSize {
    let mut s = size_raw(path, cancel);
    s.links.sort_unstable_by_key(|l| l.0);
    s.links.dedup_by_key(|l| l.0);
    for (_, alloc, app) in std::mem::take(&mut s.links) {
        s.allocated += alloc;
        s.apparent += app;
    }
    s
}

fn size_raw(path: &Path, cancel: &CancellationToken) -> DirSize {
    let mut s = DirSize::default();
    if cancel.is_cancelled() {
        return s;
    }
    let Ok(rd) = fs::read_dir(path) else {
        return s;
    };
    let mut subdirs = Vec::new();
    for ent in rd.flatten() {
        let Ok(md) = ent.metadata() else { continue };
        let p = ent.path();
        if ent.file_name() == ".git" {
            s.has_repo = true;
        }
        let m = entry_meta(&p, &md);
        s.entries += 1;
        if md.is_dir() {
            subdirs.push(p);
        } else if let Some(id) = m.file_id.filter(|_| m.nlink > 1) {
            s.links.push((id, m.allocated, m.apparent));
            continue;
        }
        s.allocated += m.allocated;
        s.apparent += m.apparent;
    }
    subdirs
        .par_iter()
        .map(|p| size_raw(p, cancel))
        .reduce(DirSize::default, DirSize::merge)
        .merge(s)
}

/// Source statistics accumulated for the nearest enclosing project.
#[derive(Clone, Copy)]
struct Acc {
    size: u64,
    mtime: i64,
}

impl Acc {
    const EMPTY: Acc = Acc {
        size: 0,
        mtime: i64::MIN,
    };
    fn merge(&mut self, o: Acc) {
        self.size += o.size;
        self.mtime = self.mtime.max(o.mtime);
    }
    fn last_mtime(self) -> i64 {
        if self.mtime == i64::MIN {
            0
        } else {
            self.mtime
        }
    }
}

/// Artifacts found in a directory that is not itself a project; they are
/// attached to the nearest enclosing project, or become an implied project.
struct Loose {
    dir: PathBuf,
    acc: Acc,
    in_repo: bool,
    artifacts: Vec<Artifact>,
}

struct Out {
    acc: Acc,
    projects: Vec<Project>,
    loose: Vec<Loose>,
}

impl Default for Out {
    fn default() -> Self {
        Out {
            acc: Acc::EMPTY,
            projects: Vec::new(),
            loose: Vec::new(),
        }
    }
}

enum Job {
    Artifact(PathBuf, Vec<usize>),
    Walk(PathBuf),
}

enum Res {
    Artifact(Artifact),
    Sub(Out),
}

struct Ctx<'a> {
    rules: Vec<CompiledRule>,
    opts: &'a DevOptions,
    cancel: &'a CancellationToken,
    on_event: &'a (dyn Fn(DevEvent) + Sync),
    visited: AtomicU64,
    errors: Mutex<Vec<(PathBuf, String)>>,
}

pub(crate) fn analyze(
    opts: &DevOptions,
    cancel: &CancellationToken,
    on_event: &(dyn Fn(DevEvent) + Sync),
) -> DevReport {
    let (rules, rule_errors) = compile(&opts.rules);
    let ctx = Ctx {
        rules,
        opts,
        cancel,
        on_event,
        visited: AtomicU64::new(0),
        errors: Mutex::new(
            rule_errors
                .into_iter()
                .map(|e| (PathBuf::new(), e))
                .collect(),
        ),
    };
    let outs: Vec<Out> = opts
        .roots
        .par_iter()
        .filter(|r| !ctx.excluded(r))
        .map(|r| {
            let in_repo = r.ancestors().skip(1).any(|a| a.join(".git").exists());
            ctx.walk(r, 0, in_repo)
        })
        .collect();
    let mut projects = Vec::new();
    for out in outs {
        projects.extend(out.projects);
        for l in out.loose {
            projects.push(ctx.project(&l.dir, l.acc, l.in_repo, &[], l.artifacts));
        }
    }
    projects.sort_by(|a, b| a.path.cmp(&b.path));
    projects.dedup_by(|a, b| a.path == b.path);
    projects.sort_by(|a, b| {
        b.artifact_size
            .cmp(&a.artifact_size)
            .then(a.path.cmp(&b.path))
    });
    on_event(DevEvent::DirsVisited(ctx.visited.load(Ordering::Relaxed)));
    DevReport {
        projects,
        errors: ctx.errors.into_inner().unwrap_or_else(|e| e.into_inner()),
    }
}

impl Ctx<'_> {
    fn excluded(&self, p: &Path) -> bool {
        self.opts.exclude.iter().any(|e| p.starts_with(e))
    }

    fn error(&self, p: &Path, e: impl ToString) {
        if let Ok(mut v) = self.errors.lock() {
            v.push((p.to_path_buf(), e.to_string()));
        }
    }

    fn walk(&self, dir: &Path, depth: usize, in_repo_parent: bool) -> Out {
        if self.cancel.is_cancelled() {
            return Out::default();
        }
        let n = self.visited.fetch_add(1, Ordering::Relaxed) + 1;
        if n % TICK == 0 {
            (self.on_event)(DevEvent::DirsVisited(n));
        }
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                self.error(dir, e);
                return Out::default();
            }
        };

        let mut acc = Acc::EMPTY;
        let mut names = Vec::new();
        let mut subdirs: Vec<OsString> = Vec::new();
        let mut has_git = false;
        for ent in rd.flatten() {
            let name = ent.file_name();
            if VCS_DIRS.iter().any(|v| name == *v) {
                has_git |= name == ".git";
            } else if let Ok(ft) = ent.file_type() {
                if ft.is_dir() {
                    subdirs.push(name.clone());
                } else if let Ok(md) = ent.metadata() {
                    acc.size += entry_meta(&ent.path(), &md).allocated;
                    if let Ok(t) = md.modified() {
                        acc.mtime = acc.mtime.max(unix_secs(t));
                    }
                }
            }
            names.push(name);
        }
        let in_repo = in_repo_parent || has_git;

        let matched: Vec<usize> = (0..self.rules.len())
            .filter(|&i| self.rules[i].matches_dir(dir, &names))
            .collect();
        let may_descend = self.opts.max_depth.is_none_or(|m| depth < m);

        let jobs: Vec<Job> = subdirs
            .into_iter()
            .filter_map(|name| {
                let path = dir.join(&name);
                if self.excluded(&path) {
                    return None;
                }
                let n = Path::new(&name);
                let claims: Vec<usize> = (0..self.rules.len())
                    .filter(|&i| {
                        let r = &self.rules[i];
                        r.artifacts.is_match(n)
                            && match r.rule.kind {
                                RuleKind::Marker => matched.contains(&i),
                                RuleKind::DirContains => r.contained_in(&path),
                            }
                    })
                    .collect();
                if !claims.is_empty() {
                    Some(Job::Artifact(path, claims))
                } else {
                    may_descend.then_some(Job::Walk(path))
                }
            })
            .collect();

        let results: Vec<Res> = jobs
            .into_par_iter()
            .filter_map(|job| match job {
                Job::Walk(p) => Some(Res::Sub(self.walk(&p, depth + 1, in_repo))),
                Job::Artifact(p, claims) => {
                    match self.artifact(&p, dir, &names, &claims, in_repo) {
                        Some(a) => Some(Res::Artifact(a)),
                        // Contains a nested repository (FR-DEV-13): treat as source.
                        None => may_descend.then(|| Res::Sub(self.walk(&p, depth + 1, in_repo))),
                    }
                }
            })
            .collect();

        let mut own = Vec::new();
        let mut child_loose = Vec::new();
        let mut projects = Vec::new();
        for r in results {
            match r {
                Res::Artifact(a) => own.push(a),
                Res::Sub(o) => {
                    acc.merge(o.acc);
                    projects.extend(o.projects);
                    child_loose.extend(o.loose);
                }
            }
        }
        if matched.iter().any(|&i| self.rules[i].rule.defines_project) {
            own.extend(child_loose.into_iter().flat_map(|l| l.artifacts));
            projects.push(self.project(dir, acc, in_repo, &matched, own));
            Out {
                acc: Acc::EMPTY,
                projects,
                loose: Vec::new(),
            }
        } else if !own.is_empty() {
            own.extend(child_loose.into_iter().flat_map(|l| l.artifacts));
            Out {
                acc,
                projects,
                loose: vec![Loose {
                    dir: dir.to_path_buf(),
                    acc,
                    in_repo,
                    artifacts: own,
                }],
            }
        } else {
            Out {
                acc,
                projects,
                loose: child_loose,
            }
        }
    }

    /// Size a claimed artifact dir; `None` if it contains a nested repository.
    fn artifact(
        &self,
        path: &Path,
        dir: &Path,
        names: &[OsString],
        claims: &[usize],
        in_repo: bool,
    ) -> Option<Artifact> {
        let size = size_dir(path, self.cancel);
        if size.has_repo {
            return None;
        }
        // The most conservative claim wins; ties keep rule order.
        let (rule, (mut risk, mut risk_reason)) = claims
            .iter()
            .map(|&i| (&self.rules[i], self.rules[i].assess(names)))
            .reduce(|a, b| if b.1.0 > a.1.0 { b } else { a })?;
        let mut tracked = false;
        if in_repo && self.opts.check_git {
            let rel = path.strip_prefix(dir).unwrap_or(path);
            tracked =
                git(dir, &["ls-files", "--"], Some(rel)).is_some_and(|o| !o.trim().is_empty());
            if tracked {
                risk = Risk::Review;
                risk_reason =
                    "Tracked by Git (not ignored): deleting it would change the repository.".into();
            }
        }
        Some(Artifact {
            path: path.to_path_buf(),
            ecosystem: rule.rule.ecosystem.clone(),
            allocated: size.allocated,
            apparent: size.apparent,
            entries: size.entries,
            risk,
            risk_reason,
            regenerate: rule.rule.regenerate.clone(),
            tracked_by_git: tracked,
        })
    }

    fn project(
        &self,
        dir: &Path,
        acc: Acc,
        in_repo: bool,
        matched: &[usize],
        mut artifacts: Vec<Artifact>,
    ) -> Project {
        let mut types: Vec<String> = Vec::new();
        let defining = matched
            .iter()
            .map(|&i| &self.rules[i])
            .filter(|r| r.rule.defines_project);
        for t in defining
            .clone()
            .map(|r| &r.rule.ecosystem)
            .chain(artifacts.iter().map(|a| &a.ecosystem))
        {
            if !types.contains(t) {
                types.push(t.clone());
            }
        }
        artifacts.sort_by(|a, b| b.allocated.cmp(&a.allocated).then(a.path.cmp(&b.path)));
        let last_commit = (in_repo && self.opts.check_git)
            .then(|| git(dir, &["log", "-1", "--format=%ct"], None))
            .flatten()
            .and_then(|s| s.trim().parse().ok());
        let p = Project {
            path: dir.to_path_buf(),
            name: dir
                .file_name()
                .map_or_else(|| dir.to_string_lossy(), |n| n.to_string_lossy())
                .into_owned(),
            types,
            artifact_size: artifacts.iter().map(|a| a.allocated).sum(),
            artifacts,
            source_size: acc.size,
            last_source_mtime: acc.last_mtime(),
            last_commit,
            clean_command: defining.clone().find_map(|r| r.clean_command(dir)),
        };
        (self.on_event)(DevEvent::ProjectFound(p.clone()));
        p
    }
}
