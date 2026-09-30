use super::*;
use std::fs;

fn opts(root: &Path) -> ScanOptions {
    ScanOptions::new(vec![root.to_path_buf()])
}

#[test]
fn scans_sizes_and_structure() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("a/b")).unwrap();
    fs::write(d.path().join("a/b/f1"), vec![1u8; 10_000]).unwrap();
    fs::write(d.path().join("a/f2"), vec![1u8; 5_000]).unwrap();
    fs::write(d.path().join("f3"), b"x").unwrap();

    let (t, p) = scan_blocking(opts(d.path())).unwrap();
    assert!(p.is_done());
    let root = t.node(t.root());
    assert_eq!(root.apparent, 15_001);
    assert_eq!(root.items, 5);
    let a = t.find_path(&d.path().join("a")).unwrap();
    assert_eq!(t.node(a).apparent, 15_000);
    // Sorted by allocated size: `a` first.
    assert_eq!(t.children(t.root()).next(), Some(a));
    assert_eq!(p.files.load(Ordering::Relaxed), 3);
    assert_eq!(p.dirs.load(Ordering::Relaxed), 2);
}

#[test]
fn many_files_parallel() {
    let d = tempfile::tempdir().unwrap();
    let mut expected = 0u64;
    for i in 0..20 {
        let sub = d.path().join(format!("d{i}"));
        fs::create_dir(&sub).unwrap();
        for j in 0..25 {
            fs::write(sub.join(format!("f{j}")), vec![0u8; i * 10 + j]).unwrap();
            expected += (i * 10 + j) as u64;
        }
    }
    let (t, _) = scan_blocking(opts(d.path())).unwrap();
    assert_eq!(t.node(t.root()).apparent, expected);
    assert_eq!(t.node(t.root()).items, 20 + 500);
}

#[cfg(unix)]
#[test]
fn hardlinks_counted_once() {
    // AC-02 (scan half)
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), vec![0u8; 100_000]).unwrap();
    fs::hard_link(d.path().join("a"), d.path().join("b")).unwrap();
    let (t, _) = scan_blocking(opts(d.path())).unwrap();
    assert_eq!(t.node(t.root()).apparent, 100_000);
}

#[cfg(unix)]
#[test]
fn symlink_loop_does_not_hang() {
    // AC-03: following disabled by default; loop is harmless either way.
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("x")).unwrap();
    std::os::unix::fs::symlink(d.path(), d.path().join("x/loop")).unwrap();
    fs::write(d.path().join("x/f"), vec![0u8; 10]).unwrap();

    let (t, _) = scan_blocking(opts(d.path())).unwrap();
    let l = t.find_path(&d.path().join("x/loop")).unwrap();
    assert_eq!(t.node(l).kind, NodeKind::Symlink);

    let mut o = opts(d.path());
    o.follow_symlinks = true;
    let (t, _) = scan_blocking(o).unwrap();
    // The loop points back at an already-visited directory; not descended.
    assert!(t.node(t.root()).items < 10);
}

#[cfg(unix)]
#[test]
fn follows_symlink_when_enabled() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("big"), vec![0u8; 50_000]).unwrap();
    let d = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();

    let (t, _) = scan_blocking(opts(d.path())).unwrap();
    assert!(t.node(t.root()).apparent < 50_000);

    let mut o = opts(d.path());
    o.follow_symlinks = true;
    let (t, _) = scan_blocking(o).unwrap();
    assert!(t.node(t.root()).apparent >= 50_000);
}

#[test]
fn exclusions() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(d.path().join("skip")).unwrap();
    fs::write(d.path().join("skip/f"), vec![0u8; 1000]).unwrap();
    fs::write(d.path().join("keep.txt"), vec![0u8; 10]).unwrap();
    fs::write(d.path().join("drop.log"), vec![0u8; 10]).unwrap();
    let mut o = opts(d.path());
    o.exclude_paths.push(d.path().join("skip"));
    o.exclude_globs.push("*.log".into());
    let (t, _) = scan_blocking(o).unwrap();
    assert_eq!(t.node(t.root()).apparent, 10);
    assert_eq!(t.node(t.root()).items, 1);
}

#[cfg(unix)]
#[test]
fn unreadable_dir_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let locked = d.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("f"), b"x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (t, p) = scan_blocking(opts(d.path())).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    // Running as root can read anything; only assert when access was denied.
    if p.errors.load(Ordering::Relaxed) > 0 {
        assert_eq!(t.issues.len(), 1);
        let n = t.find_path(&locked).unwrap();
        assert!(t.node(n).flags.contains(NodeFlags::ERROR));
    }
}

#[test]
fn multiple_roots() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("x"), vec![1u8; 10_000]).unwrap();
    fs::write(b.path().join("y"), vec![1u8; 50_000]).unwrap();
    let (t, _) = scan_blocking(ScanOptions::new(vec![a.path().into(), b.path().into()])).unwrap();
    assert_eq!(t.node(t.root()).apparent, 60_000);
    let first = t.children(t.root()).next().unwrap();
    assert_eq!(t.path(first), b.path());
    let y = t.children(first).next().unwrap();
    assert_eq!(t.path(y), b.path().join("y"));
}

#[test]
fn cancel_is_fast() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..50 {
        let sub = d.path().join(format!("d{i}"));
        fs::create_dir(&sub).unwrap();
        for j in 0..50 {
            fs::write(sub.join(format!("f{j}")), b"").unwrap();
        }
    }
    let h = start_scan(opts(d.path())).unwrap();
    h.set_paused(true);
    let t0 = Instant::now();
    h.cancel();
    let progress = h.progress.clone();
    let _ = h.wait();
    assert!(t0.elapsed() < Duration::from_millis(500));
    assert!(progress.is_done());
}

#[test]
fn missing_root_errors() {
    let r = start_scan(opts(Path::new("/definitely/not/here")));
    assert!(matches!(r, Err(ScanError::Root { .. })));
}

#[test]
fn export_formats() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a,b"), vec![0u8; 10]).unwrap();
    let (t, _) = scan_blocking(opts(d.path())).unwrap();
    let json = export::to_json(&t, 3, 100, SizeMode::Apparent);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["root"]["apparent"], 10);
    let csv = export::to_csv(&t, 3, SizeMode::Apparent);
    assert!(csv.contains("a,b\""));
    assert_eq!(csv.lines().count(), 3);
}
