//! User settings stored as human-readable TOML in the platform config
//! directory (FR-SET-01, FR-SET-02).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sr_core::{SizeMode, SizeUnits};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ColorMode {
    #[default]
    Branch,
    FileType,
    Age,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Scanning
    pub exclude_paths: Vec<PathBuf>,
    pub exclude_globs: Vec<String>,
    pub follow_symlinks: bool,
    pub cross_filesystems: bool,
    pub scan_threads: Option<usize>,

    // Display
    pub size_mode: SizeMode,
    pub size_units: SizeUnits,
    pub rings: u32,
    pub min_arc_degrees: f32,
    pub color_mode: ColorMode,
    pub high_contrast: bool,
    pub theme: Theme,
    pub reduce_motion: bool,
    pub animation_ms: u32,

    // Safety
    /// User additions to the built-in protected paths (FR-SET-03).
    pub protected_paths: Vec<PathBuf>,
    pub confirm_bytes_threshold: u64,
    pub confirm_count_threshold: usize,

    // DevSweep
    pub dev_stale_days: u64,
    pub dev_pinned: Vec<PathBuf>,
    pub dev_custom_rules: Vec<sr_devsweep::Rule>,
    pub dev_check_git: bool,
    pub dev_express_mode: bool,

    // DuplicateLens
    pub dup_min_size: u64,
    pub dup_paranoid: bool,
    pub dup_io_threads: Option<usize>,
    pub dup_similarity_threshold: u32,
    pub hash_cache: Option<PathBuf>,
    pub ffmpeg_path: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            exclude_paths: sr_platform::default_exclusions(),
            exclude_globs: Vec::new(),
            follow_symlinks: false,
            cross_filesystems: false,
            scan_threads: None,
            size_mode: SizeMode::Allocated,
            size_units: SizeUnits::default(),
            rings: 5,
            min_arc_degrees: 0.5,
            color_mode: ColorMode::Branch,
            high_contrast: false,
            theme: Theme::System,
            reduce_motion: false,
            animation_ms: 250,
            protected_paths: Vec::new(),
            confirm_bytes_threshold: 10_000_000_000,
            confirm_count_threshold: 1000,
            dev_stale_days: 90,
            dev_pinned: Vec::new(),
            dev_custom_rules: Vec::new(),
            dev_check_git: true,
            dev_express_mode: false,
            dup_min_size: 1 << 20,
            dup_paranoid: false,
            dup_io_threads: None,
            dup_similarity_threshold: 8,
            hash_cache: sr_platform::app_dirs().map(|d| d.cache.join("hashes.json")),
            ffmpeg_path: None,
        }
    }
}

impl Settings {
    pub fn default_path() -> Option<PathBuf> {
        sr_platform::app_dirs().map(|d| d.config.join("settings.toml"))
    }

    /// Load settings; a missing file yields defaults, an unreadable or
    /// malformed one yields defaults plus an error message.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<Settings>(&s) {
                Ok(mut v) => {
                    v.clamp();
                    (v, None)
                }
                Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    pub fn load_default() -> (Self, Option<String>) {
        match Self::default_path() {
            Some(p) => Self::load(&p),
            None => (Self::default(), None),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let s = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(tmp, path)
    }

    pub fn save_default(&self) -> std::io::Result<()> {
        match Self::default_path() {
            Some(p) => self.save(&p),
            None => Ok(()),
        }
    }

    pub fn clamp(&mut self) {
        self.rings = self.rings.clamp(2, 10);
        self.min_arc_degrees = self.min_arc_degrees.clamp(0.05, 10.0);
        self.animation_ms = self.animation_ms.min(2000);
        self.dup_min_size = self.dup_min_size.max(1);
    }

    pub fn protected(&self) -> sr_platform::ProtectedPaths {
        sr_platform::ProtectedPaths::new(&self.protected_paths)
    }

    pub fn scan_options(&self, roots: Vec<PathBuf>) -> sr_scan::ScanOptions {
        let mut o = sr_scan::ScanOptions::new(roots);
        for p in &self.exclude_paths {
            if !o.exclude_paths.contains(p) {
                o.exclude_paths.push(p.clone());
            }
        }
        o.exclude_globs = self.exclude_globs.clone();
        o.follow_symlinks = self.follow_symlinks;
        o.cross_filesystems = self.cross_filesystems;
        o.threads = self.scan_threads;
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_toml() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.toml");
        let s = Settings {
            rings: 7,
            exclude_globs: vec!["*.iso".into()],
            ..Default::default()
        };
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("rings = 7"));
        let (l, err) = Settings::load(&p);
        assert!(err.is_none());
        assert_eq!(l.rings, 7);
        assert_eq!(l.exclude_globs, vec!["*.iso".to_string()]);
    }

    #[test]
    fn partial_and_bad_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.toml");
        std::fs::write(&p, "rings = 99\n").unwrap();
        let (l, err) = Settings::load(&p);
        assert!(err.is_none());
        assert_eq!(l.rings, 10);
        std::fs::write(&p, "rings = \"x\"").unwrap();
        let (_, err) = Settings::load(&p);
        assert!(err.is_some());
        let (_, err) = Settings::load(&d.path().join("missing.toml"));
        assert!(err.is_none());
    }
}
