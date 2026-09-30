//! Detection rules: plain data loaded from TOML (NFR-MAIN-03) and their
//! compiled glob form used by the analyzer.

use std::ffi::OsString;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// Risk tag shown next to every artifact (FR-DEV-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord, Hash)]
pub enum Risk {
    Safe,
    Caution,
    Review,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "Safe",
            Risk::Caution => "Caution",
            Risk::Review => "Review",
        }
    }

    /// Inline explanation for tooltips (NFR-USE-02).
    pub fn explanation(self) -> &'static str {
        match self {
            Risk::Safe => {
                "Fully regenerable: the tool recreates it automatically or with one command."
            }
            Risk::Caution => {
                "Regenerable, but slow or network-dependent (e.g. dependencies re-downloaded without a lockfile)."
            }
            Risk::Review => {
                "May contain user state or files tracked by Git; check its contents before deleting."
            }
        }
    }
}

/// How a rule locates its artifacts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleKind {
    /// `markers` must exist in the project dir; `artifacts` name its subdirs.
    #[default]
    Marker,
    /// Any subdir matching `artifacts` that itself contains one of `markers`
    /// (literal file names), e.g. a venv holding `pyvenv.cfg`.
    DirContains,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub ecosystem: String,
    /// Globs matched against entry names directly in the project dir.
    /// Entries containing '/' are literal relative paths (e.g. Unity's
    /// `ProjectSettings/ProjectVersion.txt`).
    pub markers: Vec<String>,
    /// Globs matched against the names of direct subdirectories.
    pub artifacts: Vec<String>,
    pub risk: Risk,
    pub regenerate: String,
    /// If none of these globs is present in the project dir, Safe → Caution.
    #[serde(default)]
    pub caution_without: Vec<String>,
    #[serde(default)]
    pub kind: RuleKind,
    /// `false`: the rule only flags artifacts, which are attributed to the
    /// nearest enclosing project (or to an implied project at the dir).
    #[serde(default = "yes")]
    pub defines_project: bool,
    /// Official clean command (argv; run with cwd = project dir; `{dir}` is
    /// replaced with the project path). Empty = none.
    #[serde(default)]
    pub clean: Vec<String>,
    /// Risk explanation shown instead of the generic one.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("invalid rules TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("rule '{ecosystem}': invalid glob '{glob}': {message}")]
    Glob {
        ecosystem: String,
        glob: String,
        message: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleSet {
    #[serde(rename = "rule", default)]
    pub rules: Vec<Rule>,
}

const BUILTIN: &str = include_str!("rules.toml");

impl RuleSet {
    /// The embedded Appendix A rule table.
    pub fn builtin() -> Self {
        Self::from_toml(BUILTIN).expect("embedded rules.toml is valid")
    }

    /// Parse and validate (all globs must compile) a `[[rule]]` TOML table.
    pub fn from_toml(s: &str) -> Result<Self, RuleError> {
        let set: RuleSet = toml::from_str(s)?;
        for r in &set.rules {
            CompiledRule::new(r.clone())?;
        }
        Ok(set)
    }

    /// Append user-defined rules (FR-DEV-09).
    pub fn with_custom(mut self, extra: Vec<Rule>) -> Self {
        self.rules.extend(extra);
        self
    }

    /// Serialize back to TOML (for settings).
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::builtin()
    }
}

/// A rule with its globs compiled.
pub(crate) struct CompiledRule {
    pub rule: Rule,
    name_markers: GlobSet,
    path_markers: Vec<String>,
    pub artifacts: GlobSet,
    caution_without: GlobSet,
}

fn globset(rule: &Rule, globs: &[String]) -> Result<GlobSet, RuleError> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        let glob = Glob::new(g).map_err(|e| RuleError::Glob {
            ecosystem: rule.ecosystem.clone(),
            glob: g.clone(),
            message: e.to_string(),
        })?;
        b.add(glob);
    }
    b.build().map_err(|e| RuleError::Glob {
        ecosystem: rule.ecosystem.clone(),
        glob: globs.join(", "),
        message: e.to_string(),
    })
}

impl CompiledRule {
    pub fn new(rule: Rule) -> Result<Self, RuleError> {
        let (paths, names): (Vec<String>, Vec<String>) =
            rule.markers.iter().cloned().partition(|m| m.contains('/'));
        Ok(Self {
            name_markers: globset(&rule, &names)?,
            artifacts: globset(&rule, &rule.artifacts)?,
            caution_without: globset(&rule, &rule.caution_without)?,
            path_markers: paths,
            rule,
        })
    }

    /// Whether a `Marker` rule's markers are present in `dir` (whose entry
    /// names are `names`).
    pub fn matches_dir(&self, dir: &Path, names: &[OsString]) -> bool {
        self.rule.kind == RuleKind::Marker
            && (names
                .iter()
                .any(|n| self.name_markers.is_match(Path::new(n)))
                || self
                    .path_markers
                    .iter()
                    .any(|m| dir.join(m).symlink_metadata().is_ok()))
    }

    /// For `DirContains` rules: whether `candidate` holds one of the markers.
    pub fn contained_in(&self, candidate: &Path) -> bool {
        self.rule.kind == RuleKind::DirContains
            && self
                .rule
                .markers
                .iter()
                .any(|m| candidate.join(m).symlink_metadata().is_ok())
    }

    /// Effective risk and reason for an artifact in a dir with `names`.
    pub fn assess(&self, names: &[OsString]) -> (Risk, String) {
        let base = if self.rule.note.is_empty() {
            self.rule.risk.explanation().to_string()
        } else {
            self.rule.note.clone()
        };
        if !self.rule.caution_without.is_empty()
            && !names
                .iter()
                .any(|n| self.caution_without.is_match(Path::new(n)))
            && self.rule.risk < Risk::Caution
        {
            return (
                Risk::Caution,
                "No lockfile found: reinstalling may resolve different versions and needs the network."
                    .into(),
            );
        }
        (self.rule.risk, base)
    }

    pub fn clean_command(&self, dir: &Path) -> Option<Vec<String>> {
        if self.rule.clean.is_empty() {
            return None;
        }
        let d = dir.to_string_lossy();
        Some(
            self.rule
                .clean
                .iter()
                .map(|a| a.replace("{dir}", &d))
                .collect(),
        )
    }
}

/// Compile a rule set, skipping (and reporting) invalid rules.
pub(crate) fn compile(set: &RuleSet) -> (Vec<CompiledRule>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for r in &set.rules {
        match CompiledRule::new(r.clone()) {
            Ok(c) => ok.push(c),
            Err(e) => errs.push(e.to_string()),
        }
    }
    (ok, errs)
}
