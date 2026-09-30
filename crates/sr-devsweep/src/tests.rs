use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::*;

fn write(root: &Path, rel: &str, bytes: usize) -> PathBuf {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, vec![b'x'; bytes]).unwrap();
    p
}

fn set_age(p: &Path, days: u64) {
    let t = SystemTime::now() - Duration::from_secs(days * 86_400);
    File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn run(root: &Path, check_git: bool) -> DevReport {
    let mut opts = DevOptions::new(vec![root.to_path_buf()]);
    opts.check_git = check_git;
    analyze(&opts, &CancellationToken::new(), &|_| {})
}

fn project<'a>(r: &'a DevReport, path: &Path) -> &'a Project {
    r.projects
        .iter()
        .find(|p| p.path == path)
        .unwrap_or_else(|| panic!("no project at {path:?}: {:#?}", r.projects))
}

fn all_artifacts(r: &DevReport) -> Vec<&Artifact> {
    r.projects.iter().flat_map(|p| &p.artifacts).collect()
}

fn has_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git_in(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

#[test]
fn builtin_rules_cover_appendix_a() {
    let set = RuleSet::builtin();
    for eco in [
        "Rust",
        "Node.js",
        "Next.js",
        "Nuxt",
        "Vite",
        "Turborepo",
        "Python",
        "Maven",
        "Gradle",
        "Xcode",
        "CocoaPods",
        "Flutter/Dart",
        "Go",
        ".NET",
        "CMake",
        "Zig",
        "Haskell",
        "Elixir",
        "Terraform",
        "Unity",
    ] {
        assert!(
            set.rules.iter().any(|r| r.ecosystem == eco),
            "missing {eco}"
        );
    }
    // Round-trips through TOML.
    let again = RuleSet::from_toml(&set.to_toml()).unwrap();
    assert_eq!(again.rules.len(), set.rules.len());
    assert!(!Risk::Review.explanation().is_empty());
}

#[test]
fn invalid_rules_are_rejected() {
    assert!(RuleSet::from_toml("[[rule]]\necosystem = 1").is_err());
    let bad = "[[rule]]\necosystem='X'\nmarkers=['a[']\nartifacts=[]\nrisk='Safe'\nregenerate=''";
    assert!(matches!(
        RuleSet::from_toml(bad),
        Err(RuleError::Glob { .. })
    ));
}

#[test]
fn ac06_marker_required() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "loose/build/out.o", 5000);
    write(r, "loose/dist/app.js", 5000);
    write(r, "loose/target/x", 5000);
    write(r, "rs/Cargo.toml", 10);
    write(r, "rs/target/debug/bin", 20_000);
    write(r, "rs/build/keep.txt", 10); // Rust does not claim build/
    let rep = run(r, false);
    let arts = all_artifacts(&rep);
    assert_eq!(arts.len(), 1, "{arts:#?}");
    let p = project(&rep, &r.join("rs"));
    assert_eq!(p.types, ["Rust"]);
    assert_eq!(p.artifacts[0].path, r.join("rs/target"));
    assert_eq!(p.artifacts[0].risk, Risk::Safe);
    assert!(p.artifacts[0].allocated >= 20_000);
    assert!(p.artifact_size >= 20_000);
    assert!(p.source_size > 0);
    assert_eq!(p.name, "rs");
    assert_eq!(
        tool_clean_command(p).unwrap()[..2],
        ["cargo".to_string(), "clean".to_string()]
    );
    assert!(!rep.projects.iter().any(|p| p.path.ends_with("loose")));
}

#[test]
fn node_lockfile_decides_risk() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "a/package.json", 2);
    write(r, "a/package-lock.json", 2);
    write(r, "a/node_modules/x/index.js", 100);
    write(r, "b/package.json", 2);
    write(r, "b/node_modules/x/index.js", 100);
    // Contents of node_modules are never discovered as projects.
    write(r, "b/node_modules/x/package.json", 2);
    write(r, "b/node_modules/y/Cargo.toml", 2);
    let rep = run(r, false);
    assert_eq!(rep.projects.len(), 2);
    assert_eq!(project(&rep, &r.join("a")).artifacts[0].risk, Risk::Safe);
    let b = &project(&rep, &r.join("b")).artifacts[0];
    assert_eq!(b.risk, Risk::Caution);
    assert!(b.risk_reason.contains("lockfile"));
}

#[test]
fn python_venv_and_caches() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "py/pyproject.toml", 2);
    write(r, "py/myenv/pyvenv.cfg", 2);
    write(r, "py/myenv/lib/site.py", 50);
    write(r, "py/pkg/mod.py", 5);
    write(r, "py/pkg/__pycache__/mod.cpython.pyc", 5);
    write(r, "py/.pytest_cache/v", 5);
    // __pycache__ next to .py files in a dir with no project marker.
    write(r, "scripts/tool.py", 5);
    write(r, "scripts/__pycache__/tool.pyc", 5);
    // No .py → not flagged.
    write(r, "other/__pycache__/x.pyc", 5);
    let rep = run(r, false);
    let py = project(&rep, &r.join("py"));
    assert_eq!(py.types, ["Python"]);
    let venv = py
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("myenv"))
        .unwrap();
    assert_eq!(venv.risk, Risk::Review);
    assert!(
        py.artifacts
            .iter()
            .any(|a| a.path.ends_with("pkg/__pycache__"))
    );
    assert!(
        py.artifacts
            .iter()
            .any(|a| a.path.ends_with(".pytest_cache"))
    );
    assert_eq!(py.artifacts.len(), 3);
    let scripts = project(&rep, &r.join("scripts"));
    assert_eq!(scripts.artifacts[0].risk, Risk::Safe);
    assert!(
        !all_artifacts(&rep)
            .iter()
            .any(|a| a.path.starts_with(r.join("other")))
    );
}

#[test]
fn ac07_stale_uses_sources_only() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("proj");
    for f in ["Cargo.toml", "src/main.rs"] {
        let p = write(&r, f, 10);
        set_age(&p, 200);
    }
    let t = write(&r, "target/debug/app", 1000);
    set_age(&t, 1);
    let g = write(&r, ".git/index", 10); // VCS metadata ignored (no real repo)
    set_age(&g, 0);
    let rep = run(d.path(), false);
    let p = project(&rep, &r);
    let now = sr_core::now_secs();
    assert!(p.inactive_days(now) >= 199, "{}", p.inactive_days(now));
    assert!(p.is_stale(180, now));
    assert!(!p.is_stale(365, now));
    assert_eq!(select_stale(&rep.projects, 180, &[], now).len(), 1);
    assert!(select_stale(&rep.projects, 180, std::slice::from_ref(&r), now).is_empty());
    let f = Filter {
        inactive_days: Some(180),
        ..Default::default()
    };
    assert_eq!(filter_projects(&rep.projects, &f, now).len(), 1);
    let f = Filter {
        types: vec!["node.js".into()],
        ..Default::default()
    };
    assert!(filter_projects(&rep.projects, &f, now).is_empty());
    let f = Filter {
        types: vec!["rust".into()],
        min_artifact_size: 1 << 40,
        ..Default::default()
    };
    assert!(filter_projects(&rep.projects, &f, now).is_empty());
}

#[test]
fn nested_repo_inside_artifact_is_not_flagged() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "app/CMakeLists.txt", 2);
    write(r, "app/build/vendored/.git/HEAD", 2);
    write(r, "app/build/vendored/Cargo.toml", 2);
    // Nested project in a normal source dir is its own project.
    write(r, "app/tools/gen/package.json", 2);
    write(r, "app/tools/gen/yarn.lock", 2);
    write(r, "app/tools/gen/node_modules/m.js", 2);
    let rep = run(r, false);
    let app = project(&rep, &r.join("app"));
    assert!(app.artifacts.is_empty(), "{:#?}", app.artifacts);
    assert_eq!(project(&rep, &r.join("app/build/vendored")).types, ["Rust"]);
    assert_eq!(project(&rep, &r.join("app/tools/gen")).artifacts.len(), 1);
}

#[test]
fn shared_artifact_is_deduped_and_multi_type() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "Cargo.toml", 2);
    write(r, "pom.xml", 2);
    write(r, "CMakeLists.txt", 2);
    write(r, "build.gradle.kts", 2);
    write(r, "target/a", 10);
    write(r, "build/b", 10);
    write(r, "cmake-build-debug/c", 10);
    let rep = run(r, false);
    assert_eq!(rep.projects.len(), 1);
    let p = &rep.projects[0];
    assert_eq!(p.artifacts.len(), 3, "{:#?}", p.artifacts);
    for t in ["Rust", "Maven", "Gradle", "CMake"] {
        assert!(p.types.iter().any(|x| x == t));
    }
    // build/ is claimed by Gradle (Safe) and CMake (Caution): conservative wins.
    let b = p
        .artifacts
        .iter()
        .find(|a| a.path.ends_with("build"))
        .unwrap();
    assert_eq!((b.risk, b.ecosystem.as_str()), (Risk::Caution, "CMake"));
}

#[test]
fn unity_nested_marker_go_and_terraform() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "game/ProjectSettings/ProjectVersion.txt", 2);
    write(r, "game/Library/cache", 10);
    write(r, "game/Temp/t", 10);
    write(r, "gosvc/go.mod", 2);
    write(r, "infra/main.tf", 2);
    write(r, "infra/.terraform/providers/p", 10);
    let rep = run(r, false);
    let g = project(&rep, &r.join("game"));
    assert_eq!(g.artifacts.len(), 2);
    assert!(g.artifacts.iter().all(|a| a.risk == Risk::Caution));
    let go = project(&rep, &r.join("gosvc"));
    assert_eq!(
        (go.types.as_slice(), go.artifacts.len()),
        (&["Go".to_string()][..], 0)
    );
    assert_eq!(
        project(&rep, &r.join("infra")).artifacts[0].risk,
        Risk::Caution
    );
}

#[test]
fn custom_rules_max_depth_exclude_and_events() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "a/b/c/Cargo.toml", 2);
    write(r, "x/Custom.marker", 2);
    write(r, "x/out-bin/o", 10);
    write(r, "skip/Cargo.toml", 2);
    let custom = RuleSet::from_toml(
        "[[rule]]\necosystem='Custom'\nmarkers=['*.marker']\nartifacts=['out-*']\nrisk='Safe'\nregenerate='make'",
    )
    .unwrap();
    let mut opts = DevOptions::new(vec![r.to_path_buf()]);
    opts.rules = RuleSet::builtin().with_custom(custom.rules);
    opts.exclude = vec![r.join("skip")];
    let found = Mutex::new(Vec::new());
    let rep = analyze(&opts, &CancellationToken::new(), &|e| {
        if let DevEvent::ProjectFound(p) = e {
            found.lock().unwrap().push(p.path);
        }
    });
    assert_eq!(rep.projects.len(), 2);
    assert_eq!(found.lock().unwrap().len(), 2);
    assert_eq!(project(&rep, &r.join("x")).artifacts[0].regenerate, "make");

    opts.max_depth = Some(2);
    let rep = analyze(&opts, &CancellationToken::new(), &|_| {});
    assert_eq!(rep.projects.len(), 1);

    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(analyze(&opts, &cancel, &|_| {}).projects.is_empty());
}

#[cfg(unix)]
#[test]
fn hardlinks_once_and_symlinks_not_followed() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path().join("root");
    write(&r, "p/Cargo.toml", 2);
    let a = write(&r, "p/target/a", 64 * 1024);
    fs::hard_link(&a, r.join("p/target/b")).unwrap();
    let outside = d.path().join("outside");
    write(&outside, "Cargo.toml", 2);
    write(&outside, "target/big", 10);
    std::os::unix::fs::symlink(&outside, r.join("link")).unwrap();
    let rep = run(&r, false);
    assert_eq!(rep.projects.len(), 1);
    let art = &rep.projects[0].artifacts[0];
    assert!(
        art.allocated >= 64 * 1024 && art.allocated < 2 * 64 * 1024,
        "{}",
        art.allocated
    );
    assert_eq!(art.entries, 2);
}

#[test]
fn git_tracked_artifact_is_review() {
    if !has_git() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "Cargo.toml", 2);
    write(r, "target/committed", 10);
    write(r, "sub/package.json", 2);
    write(r, "sub/package-lock.json", 2);
    write(r, "sub/node_modules/ignored.js", 2);
    fs::write(r.join(".gitignore"), "sub/node_modules/\n").unwrap();
    git_in(r, &["init", "-q"]);
    git_in(r, &["add", "-A"]);
    git_in(r, &["add", "-f", "target/committed"]);
    git_in(r, &["commit", "-q", "-m", "init"]);
    let rep = run(r, true);
    let p = project(&rep, r);
    assert!(p.last_commit.is_some());
    let t = &p.artifacts[0];
    assert!(t.tracked_by_git);
    assert_eq!(t.risk, Risk::Review);
    let n = &project(&rep, &r.join("sub")).artifacts[0];
    assert!(!n.tracked_by_git);
    assert_eq!(n.risk, Risk::Safe);
    // Without git checks nothing is tagged.
    assert_eq!(project(&run(r, false), r).artifacts[0].risk, Risk::Safe);
}

#[test]
fn docker_parsing_and_prune() {
    let out = r#"{"Active":"2","Reclaimable":"1.2GB (50%)","Size":"2.4GB","TotalCount":"5","Type":"Images"}
{"Active":"0","Reclaimable":"300MB","Size":"300MB","TotalCount":"12","Type":"Build Cache"}
garbage"#;
    let u = parse_docker_df(out);
    assert_eq!(u.rows.len(), 2);
    assert_eq!(u.rows[0].reclaimable, "1.2GB (50%)");
    assert_eq!(
        docker_prune_command(&u.rows[1].kind).unwrap(),
        ["docker", "builder", "prune", "-f"]
    );
    assert_eq!(docker_prune_command("Local Volumes").unwrap()[1], "volume");
    assert!(docker_prune_command("Other").is_none());
}

#[test]
fn global_cache_defaults_and_run_command() {
    let locs = global_cache_locations();
    assert!(locs.iter().any(|(n, ..)| n == "Go modules"));
    if cfg!(target_os = "macos") {
        assert!(locs.iter().any(|(n, ..)| n == "Xcode DerivedData"));
    }
    // Sizing real caches is slow; a cancelled scan must return promptly.
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(scan_global_caches(&cancel).is_empty());
    assert!(run_command(&[], Path::new(".")).is_err());
    if has_git() {
        let o = run_command(&["git".into(), "--version".into()], Path::new(".")).unwrap();
        assert!(o.status.success());
    }
}
