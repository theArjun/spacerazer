//! Protected paths that may never be staged or deleted (FR-SET-03,
//! NFR-SAFE-04).

use std::path::{Path, PathBuf};

/// Built-in protected paths: OS directories, the user's home root, the
/// app's own install directory and volume roots. Matching is exact for these
/// entries (their *contents* may be staged) except for the system trees in
/// [`builtin_protected_trees`], which protect everything beneath them.
pub fn builtin_protected_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![PathBuf::from("/")];
    if let Some(h) = crate::home_dir() {
        v.push(h.clone());
        for sub in [
            "Desktop",
            "Documents",
            "Downloads",
            "Pictures",
            "Music",
            "Movies",
            "Videos",
            "Library",
        ] {
            v.push(h.join(sub));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.to_path_buf());
        }
    }
    for vol in crate::volumes() {
        v.push(vol.mount_point);
    }
    if cfg!(unix) {
        v.extend(
            [
                "/home",
                "/Users",
                "/Volumes",
                "/mnt",
                "/media",
                "/tmp",
                "/var",
                "/opt",
                "/Applications",
                "/private",
            ]
            .map(PathBuf::from),
        );
    }
    if cfg!(windows) {
        v.extend(
            [
                r"C:\",
                r"C:\Users",
                r"C:\Program Files",
                r"C:\Program Files (x86)",
                r"C:\ProgramData",
            ]
            .map(PathBuf::from),
        );
    }
    v
}

/// System trees whose every descendant is protected.
fn builtin_protected_trees() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.extend(
            [
                "/System",
                "/usr",
                "/bin",
                "/sbin",
                "/private/etc",
                "/Library/Apple",
            ]
            .map(PathBuf::from),
        );
    } else if cfg!(unix) {
        v.extend(
            [
                "/bin", "/sbin", "/usr", "/lib", "/lib64", "/etc", "/boot", "/proc", "/sys",
                "/dev", "/run",
            ]
            .map(PathBuf::from),
        );
    }
    if cfg!(windows) {
        v.extend([r"C:\Windows", r"C:\Program Files\WindowsApps"].map(PathBuf::from));
    }
    v
}

#[derive(Debug, Clone)]
pub struct ProtectedPaths {
    exact: Vec<PathBuf>,
    trees: Vec<PathBuf>,
}

impl ProtectedPaths {
    /// Built-in rules plus user additions. User entries protect their whole
    /// subtree. Built-in entries cannot be removed.
    pub fn new(user: &[PathBuf]) -> Self {
        let mut trees = builtin_protected_trees();
        trees.extend(user.iter().cloned());
        Self {
            exact: builtin_protected_paths(),
            trees,
        }
    }

    /// Only the given rules; for tests.
    pub fn custom(exact: Vec<PathBuf>, trees: Vec<PathBuf>) -> Self {
        Self { exact, trees }
    }

    pub fn is_protected(&self, path: &Path) -> bool {
        let path = normalize(path);
        self.exact.iter().any(|p| normalize(p) == path)
            || self.trees.iter().any(|t| path.starts_with(normalize(t)))
    }
}

fn normalize(p: &Path) -> PathBuf {
    // Lexical normalisation only; resolving symlinks would follow links.
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_tree() {
        let p = ProtectedPaths::custom(vec!["/home/u".into()], vec!["/usr".into()]);
        assert!(p.is_protected(Path::new("/home/u")));
        assert!(p.is_protected(Path::new("/home/u/")));
        assert!(p.is_protected(Path::new("/home/u/x/..")));
        assert!(!p.is_protected(Path::new("/home/u/project")));
        assert!(p.is_protected(Path::new("/usr/lib/x")));
    }

    #[test]
    fn builtin_home_protected() {
        let p = ProtectedPaths::new(&[]);
        if let Some(h) = crate::home_dir() {
            assert!(p.is_protected(&h));
            assert!(!p.is_protected(&h.join("some-project/target")));
        }
    }
}
