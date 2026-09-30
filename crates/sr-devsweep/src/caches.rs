//! Global toolchain caches (FR-DEV-03) and Docker usage (FR-DEV-04).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sr_core::CancellationToken;

use crate::Risk;
use crate::scan::size_dir;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalCache {
    pub name: String,
    pub path: PathBuf,
    pub allocated: u64,
    pub risk: Risk,
    pub hint: String,
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Per-platform default cache locations: (name, path, risk, hint).
pub fn global_cache_locations() -> Vec<(String, PathBuf, Risk, String)> {
    use Risk::*;
    let Some(home) = sr_platform::home_dir() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    let mut add = |name: &str, path: PathBuf, risk: Risk, hint: &str| {
        v.push((name.to_string(), path, risk, hint.to_string()));
    };

    let cargo = env_path("CARGO_HOME").unwrap_or_else(|| home.join(".cargo"));
    add(
        "Cargo registry",
        cargo.join("registry"),
        Caution,
        "Re-downloaded by cargo on the next build",
    );
    add(
        "Cargo git",
        cargo.join("git"),
        Caution,
        "Re-cloned by cargo on the next build",
    );
    let gradle = env_path("GRADLE_USER_HOME").unwrap_or_else(|| home.join(".gradle"));
    add(
        "Gradle caches",
        gradle.join("caches"),
        Caution,
        "Re-downloaded by Gradle on the next build",
    );
    add(
        "Maven repository",
        home.join(".m2").join("repository"),
        Caution,
        "Re-downloaded by Maven on the next build",
    );
    let gomod = env_path("GOMODCACHE").unwrap_or_else(|| {
        let gopath = std::env::var_os("GOPATH")
            .and_then(|g| std::env::split_paths(&g).next())
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| home.join("go"));
        gopath.join("pkg").join("mod")
    });
    add(
        "Go modules",
        gomod,
        Caution,
        "Read-only files; clean with `go clean -modcache`",
    );

    if cfg!(windows) {
        let local = env_path("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"));
        add(
            "npm cache",
            local.join("npm-cache"),
            Safe,
            "Refilled by npm on demand",
        );
        add(
            "pnpm store",
            local.join("pnpm").join("store"),
            Caution,
            "Breaks pnpm links until reinstall",
        );
        add(
            "Yarn cache",
            local.join("Yarn").join("Cache"),
            Safe,
            "Refilled by yarn on demand",
        );
        add(
            "pip cache",
            local.join("pip").join("Cache"),
            Safe,
            "Refilled by pip on demand",
        );
    } else if cfg!(target_os = "macos") {
        let lib = home.join("Library");
        let caches = lib.join("Caches");
        let dev = lib.join("Developer");
        add(
            "npm cache",
            home.join(".npm"),
            Safe,
            "Refilled by npm on demand",
        );
        add(
            "pnpm store",
            lib.join("pnpm").join("store"),
            Caution,
            "Breaks pnpm links until reinstall",
        );
        add(
            "Yarn cache",
            caches.join("Yarn"),
            Safe,
            "Refilled by yarn on demand",
        );
        add(
            "pip cache",
            caches.join("pip"),
            Safe,
            "Refilled by pip on demand",
        );
        add(
            "Xcode DerivedData",
            dev.join("Xcode").join("DerivedData"),
            Safe,
            "Rebuilt by Xcode on the next build",
        );
        add(
            "iOS DeviceSupport",
            dev.join("Xcode").join("iOS DeviceSupport"),
            Caution,
            "Re-copied from the device when it is next connected (slow)",
        );
        add(
            "CoreSimulator caches",
            dev.join("CoreSimulator").join("Caches"),
            Safe,
            "Recreated by the simulator",
        );
        add(
            "CocoaPods cache",
            caches.join("CocoaPods"),
            Safe,
            "Refilled by `pod install`",
        );
    } else {
        let cache = env_path("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
        let data = env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
        add(
            "npm cache",
            home.join(".npm"),
            Safe,
            "Refilled by npm on demand",
        );
        add(
            "pnpm store",
            data.join("pnpm").join("store"),
            Caution,
            "Breaks pnpm links until reinstall",
        );
        add(
            "Yarn cache",
            cache.join("yarn"),
            Safe,
            "Refilled by yarn on demand",
        );
        add(
            "pip cache",
            cache.join("pip"),
            Safe,
            "Refilled by pip on demand",
        );
    }
    v
}

/// Existing global caches with their allocated sizes.
///
/// Sizing large caches can take a long time, so it runs on its own small
/// thread pool: sharing rayon's global pool would starve project analysis
/// running at the same time.
pub fn scan_global_caches(cancel: &CancellationToken) -> Vec<GlobalCache> {
    let run = || {
        global_cache_locations()
            .into_par_iter()
            .filter(|(_, p, _, _)| !cancel.is_cancelled() && p.is_dir())
            .map(|(name, path, risk, hint)| GlobalCache {
                allocated: size_dir(&path, cancel).allocated,
                name,
                path,
                risk,
                hint,
            })
            .collect()
    };
    match rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .thread_name(|i| format!("sr-caches-{i}"))
        .build()
    {
        Ok(pool) => pool.install(run),
        Err(_) => run(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DockerRow {
    /// "Images", "Containers", "Local Volumes", "Build Cache".
    pub kind: String,
    pub total: String,
    pub active: String,
    pub size: String,
    pub reclaimable: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DockerUsage {
    pub rows: Vec<DockerRow>,
}

/// Parse the output of `docker system df --format '{{json .}}'`.
pub fn parse_docker_df(out: &str) -> DockerUsage {
    let field = |v: &serde_json::Value, k: &str| match &v[k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    let rows = out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(|v| v.is_object())
        .map(|v| DockerRow {
            kind: field(&v, "Type"),
            total: field(&v, "TotalCount"),
            active: field(&v, "Active"),
            size: field(&v, "Size"),
            reclaimable: field(&v, "Reclaimable"),
        })
        .collect();
    DockerUsage { rows }
}

/// Docker disk usage via `docker system df` (argv, no shell); `None` if
/// Docker is not installed or the daemon is not reachable.
pub fn docker_usage() -> Option<DockerUsage> {
    let out = Command::new("docker")
        .args(["system", "df", "--format", "{{json .}}"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(parse_docker_df(&String::from_utf8_lossy(&out.stdout)))
}

/// Docker's own prune command for a `DockerRow::kind`. Docker data is never
/// deleted directly (FR-DEV-04).
pub fn docker_prune_command(kind: &str) -> Option<Vec<String>> {
    let k = kind.to_ascii_lowercase();
    let sub = if k.contains("build") {
        "builder"
    } else if k.contains("image") {
        "image"
    } else if k.contains("container") {
        "container"
    } else if k.contains("volume") {
        "volume"
    } else {
        return None;
    };
    Some(["docker", sub, "prune", "-f"].map(String::from).to_vec())
}
