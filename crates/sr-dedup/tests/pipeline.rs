use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sr_core::CancellationToken;
use sr_dedup::*;

fn opts(root: &Path) -> DupOptions {
    DupOptions {
        roots: vec![root.to_path_buf()],
        min_size: 1,
        io_threads: Some(2),
        ..Default::default()
    }
}

fn run(o: &DupOptions) -> DupResult {
    find_duplicates(o, &CancellationToken::new(), &|_| {})
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn names(g: &DupGroup) -> Vec<String> {
    g.files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn finds_groups_sorted_by_wasted() {
    let d = tempfile::tempdir().unwrap();
    let small = pattern(5_000, 1);
    let big = pattern(200_000, 2);
    for n in ["s1", "s2", "s3"] {
        fs::write(d.path().join(n), &small).unwrap();
    }
    fs::create_dir(d.path().join("sub")).unwrap();
    fs::write(d.path().join("b1"), &big).unwrap();
    fs::write(d.path().join("sub/b2"), &big).unwrap();
    fs::write(d.path().join("unique"), pattern(5_000, 9)).unwrap();

    let progress = Mutex::new(Vec::new());
    let r = find_duplicates(&opts(d.path()), &CancellationToken::new(), &|p| {
        progress.lock().unwrap().push(p.pass)
    });
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.groups.len(), 2);
    assert_eq!(names(&r.groups[0]), ["b1", "b2"]);
    assert_eq!(r.groups[0].wasted(), 200_000);
    assert_eq!(names(&r.groups[1]), ["s1", "s2", "s3"]);
    assert_eq!(r.groups[1].wasted(), 10_000);
    assert_eq!(r.total_wasted(), 210_000);
    assert_eq!(
        r.groups[0].hash.as_deref(),
        Some(blake3::hash(&big).to_hex().as_str())
    );
    assert_eq!(r.stats.files_scanned, 6);
    assert_eq!(r.stats.candidates_after_full, 5);
    let passes = progress.into_inner().unwrap();
    for p in [Pass::Collect, Pass::Size, Pass::Partial, Pass::Full] {
        assert!(passes.contains(&p), "missing {p:?}");
    }
}

/// AC-08: one differing byte in the middle survives pass 2, not pass 3.
#[test]
fn middle_byte_split_by_full_hash_ac08() {
    let d = tempfile::tempdir().unwrap();
    let a = pattern(100_000, 3);
    let mut b = a.clone();
    b[50_000] ^= 0xff;
    fs::write(d.path().join("a"), &a).unwrap();
    fs::write(d.path().join("b"), &b).unwrap();
    assert_eq!(
        partial_hash(&d.path().join("a"), 100_000, 16 * 1024).unwrap(),
        partial_hash(&d.path().join("b"), 100_000, 16 * 1024).unwrap()
    );
    let r = run(&opts(d.path()));
    assert_eq!(r.stats.candidates_after_size, 2);
    assert_eq!(r.stats.candidates_after_partial, 2);
    assert_eq!(r.stats.candidates_after_full, 0);
    assert!(r.groups.is_empty());
}

/// AC-02: hardlinks to one physical file are not duplicates.
#[cfg(unix)]
#[test]
fn hardlink_twins_not_reported_ac02() {
    let d = tempfile::tempdir().unwrap();
    let data = pattern(10_000, 4);
    fs::write(d.path().join("orig"), &data).unwrap();
    fs::hard_link(d.path().join("orig"), d.path().join("link")).unwrap();
    let r = run(&opts(d.path()));
    assert!(r.groups.is_empty());
    assert_eq!(r.stats.hardlink_twins_skipped, 1);

    // A real copy is a duplicate of the physical file, reported once.
    fs::write(d.path().join("copy"), &data).unwrap();
    let r = run(&opts(d.path()));
    assert_eq!(r.groups.len(), 1);
    assert_eq!(r.groups[0].files.len(), 2);
    assert_eq!(r.groups[0].wasted(), 10_000);
}

#[test]
fn collect_filters() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a.jpg"), pattern(2_000, 1)).unwrap();
    fs::write(d.path().join("b.txt"), pattern(2_000, 1)).unwrap();
    fs::write(d.path().join("tiny.jpg"), b"x").unwrap();
    fs::write(d.path().join("empty.jpg"), b"").unwrap();
    fs::create_dir(d.path().join("node_modules")).unwrap();
    fs::write(d.path().join("node_modules/c.jpg"), pattern(2_000, 1)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.path().join("a.jpg"), d.path().join("link.jpg")).unwrap();

    let get = |o: &DupOptions| {
        let mut errs = Vec::new();
        let mut v: Vec<String> = collect_files(o, &CancellationToken::new(), &mut errs)
            .into_iter()
            .map(|f| {
                f.path
                    .strip_prefix(d.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        v.sort();
        assert!(errs.is_empty(), "{errs:?}");
        v
    };
    let mut o = DupOptions {
        min_size: 0,
        ..opts(d.path())
    };
    assert_eq!(
        get(&o),
        ["a.jpg", "b.txt", "node_modules/c.jpg", "tiny.jpg"],
        "min 1 byte, no symlinks"
    );
    o.min_size = 100;
    o.include = vec!["*.jpg".into()];
    assert_eq!(get(&o), ["a.jpg", "node_modules/c.jpg"]);
    o.exclude = vec!["node_modules".into()];
    assert_eq!(get(&o), ["a.jpg"]);

    let mut errs = Vec::new();
    o.exclude = vec!["[".into()];
    collect_files(&o, &CancellationToken::new(), &mut errs);
    assert_eq!(errs.len(), 1, "invalid glob reported");
}

#[test]
fn group_by_size_drops_singletons_and_twins() {
    let f = |p: &str, size, id| FileEntry {
        path: p.into(),
        size,
        mtime: 0,
        file_id: Some((1, id)),
        device: Some(1),
    };
    let groups = group_by_size(vec![
        f("/a", 10, 1),
        f("/b", 10, 1), // twin of /a
        f("/c", 20, 2),
        f("/d", 20, 3),
        f("/e", 30, 4),
        f("/g", 40, 5),
        f("/h", 40, 5), // twins only -> dropped
    ]);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
        [PathBuf::from("/c"), "/d".into()]
    );
}

#[test]
fn full_hash_mmap_and_buffered_agree() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f");
    let data = pattern(40 * 1024 * 1024 + 123, 5);
    fs::write(&p, &data).unwrap();
    let want = *blake3::hash(&data).as_bytes();
    let size = data.len() as u64;
    assert_eq!(full_hash(&p, size, 0).unwrap(), want, "mmap");
    assert_eq!(full_hash(&p, size, u64::MAX).unwrap(), want, "buffered");
    // Recorded size differs from the file -> error, not a bogus digest.
    assert!(full_hash(&p, size + 1, 0).is_err());
    assert!(full_hash(&p, size - 1, u64::MAX).is_err());
    assert!(partial_hash(&p, size - 1, 16).is_err());
    // Empty file.
    let e = d.path().join("e");
    fs::write(&e, b"").unwrap();
    assert_eq!(full_hash(&e, 0, 0).unwrap(), *blake3::hash(b"").as_bytes());
}

#[test]
fn paranoid_verification() {
    let d = tempfile::tempdir().unwrap();
    let data = pattern(70_000, 6);
    for n in ["x", "y", "z"] {
        fs::write(d.path().join(n), &data).unwrap();
    }
    let mut other = data.clone();
    other[10] ^= 1;
    fs::write(d.path().join("w"), &other).unwrap();
    assert!(files_identical(&d.path().join("x"), &d.path().join("y")).unwrap());
    assert!(!files_identical(&d.path().join("x"), &d.path().join("w")).unwrap());
    let r = run(&DupOptions {
        paranoid: true,
        ..opts(d.path())
    });
    assert_eq!(r.groups.len(), 1);
    assert_eq!(names(&r.groups[0]), ["x", "y", "z"]);
    assert_eq!(r.stats.candidates_after_verify, 3);
}

#[test]
fn hash_cache_avoids_rehashing() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("root");
    fs::create_dir(&root).unwrap();
    let data = pattern(300_000, 7);
    fs::write(root.join("a"), &data).unwrap();
    fs::write(root.join("b"), &data).unwrap();
    let cache = d.path().join("cache/hashes.bin");
    let o = DupOptions {
        hash_cache: Some(cache.clone()),
        ..opts(&root)
    };

    let first = run(&o);
    assert!(first.stats.bytes_hashed > 0);
    assert!(cache.exists());
    let second = run(&o);
    assert_eq!(second.stats.bytes_hashed, 0, "served from cache");
    assert_eq!(second.stats.cache_hits, 4);
    assert_eq!(second.groups[0].hash, first.groups[0].hash);

    // A changed file is rehashed.
    fs::write(root.join("b"), pattern(300_001, 7)).unwrap();
    fs::write(root.join("c"), &data).unwrap();
    let third = run(&o);
    assert!(third.stats.bytes_hashed > 0);
    assert_eq!(names(&third.groups[0]), ["a", "c"]);

    // A corrupt cache is ignored, not trusted.
    fs::write(&cache, b"SRHC 1 deadbeef\n{not json").unwrap();
    let fourth = run(&o);
    assert!(fourth.errors.is_empty());
    assert!(fourth.stats.bytes_hashed > 0);
    assert_eq!(fourth.groups[0].hash, first.groups[0].hash);
}

#[test]
fn cancellation_returns_promptly() {
    let d = tempfile::tempdir().unwrap();
    let data = pattern(50_000, 8);
    fs::write(d.path().join("a"), &data).unwrap();
    fs::write(d.path().join("b"), &data).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = find_duplicates(&opts(d.path()), &cancel, &|_| {});
    assert!(r.stats.cancelled);
    assert!(r.groups.is_empty());

    // Cancel from the progress callback once hashing starts.
    let cancel = CancellationToken::new();
    let r = find_duplicates(&opts(d.path()), &cancel, &|p| {
        if p.pass == Pass::Partial {
            cancel.cancel()
        }
    });
    assert!(r.stats.cancelled);
    assert!(r.groups.is_empty());
    assert!(r.errors.is_empty(), "cancellation is not an error");
}

#[test]
fn exports() {
    let d = tempfile::tempdir().unwrap();
    let data = pattern(1_000, 9);
    fs::write(d.path().join("a,1"), &data).unwrap();
    fs::write(d.path().join("b"), &data).unwrap();
    let r = run(&opts(d.path()));
    let csv = export_csv(&r);
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("group,kind,hash"));
    assert!(
        lines[1].contains("\"") && lines[1].contains("a,1\""),
        "{}",
        lines[1]
    );
    assert!(lines[2].starts_with("1,identical,"));
    let json: serde_json::Value = serde_json::from_str(&export_json(&r)).unwrap();
    assert_eq!(json["total_wasted"], 1_000);
    assert_eq!(json["groups"][0]["files"].as_array().unwrap().len(), 2);
}
