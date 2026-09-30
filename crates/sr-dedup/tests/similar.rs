use std::fs;
use std::path::Path;

use image::codecs::png::PngEncoder;
use image::{DynamicImage, ImageEncoder, Rgb, RgbImage};
use sr_core::CancellationToken;
use sr_dedup::*;

fn scene(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let blob = if (fx - 0.3).powi(2) + (fy - 0.4).powi(2) < 0.04 {
            120.0
        } else {
            0.0
        };
        let v = (fx * 200.0 + blob).min(255.0) as u8;
        Rgb([v, (fy * 255.0) as u8, 255 - v])
    })
}

fn other_scene(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        let c = if ((x / 16) + (y / 16)) % 2 == 0 {
            230
        } else {
            20
        };
        Rgb([c, 255 - c, ((x * y) % 256) as u8])
    })
}

fn entry(p: &Path) -> FileEntry {
    FileEntry {
        path: p.to_path_buf(),
        size: fs::metadata(p).unwrap().len(),
        mtime: 0,
        file_id: None,
        device: None,
    }
}

#[test]
fn similar_images_grouped_different_not() {
    let d = tempfile::tempdir().unwrap();
    let orig = scene(400, 300);
    orig.save(d.path().join("orig.png")).unwrap();
    // Resized and brightened copy.
    let mut small = image::imageops::resize(&orig, 200, 150, image::imageops::FilterType::Triangle);
    for p in small.pixels_mut() {
        for c in &mut p.0 {
            *c = c.saturating_add(12);
        }
    }
    small.save(d.path().join("small_bright.png")).unwrap();
    other_scene(400, 300)
        .save(d.path().join("different.png"))
        .unwrap();
    fs::write(d.path().join("broken.png"), b"not a png").unwrap();
    fs::write(d.path().join("notes.txt"), b"ignored").unwrap();

    let files: Vec<FileEntry> = [
        "orig.png",
        "small_bright.png",
        "different.png",
        "broken.png",
        "notes.txt",
    ]
    .iter()
    .map(|n| entry(&d.path().join(n)))
    .collect();
    let (groups, errors) = find_similar_images(&files, 8, &CancellationToken::new(), &|_| {});
    assert_eq!(errors.len(), 1, "broken image reported: {errors:?}");
    assert_eq!(groups.len(), 1, "{groups:?}");
    let g = &groups[0];
    let names: Vec<_> = g
        .files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["orig.png", "small_bright.png"]);
    assert!(matches!(g.kind, GroupKind::Similar { max_distance } if max_distance <= 8));
    assert_eq!(g.image_dims, [Some((400, 300)), Some((200, 150))]);
    // FR-DUP-14: never auto-selected.
    assert_eq!(
        auto_select(g, &AutoSelect::KeepHighestResolution),
        [false, false]
    );

    let (a, _) = image_hash(&d.path().join("orig.png")).unwrap();
    let (c, _) = image_hash(&d.path().join("different.png")).unwrap();
    assert!(
        a.distance(&c) > 8,
        "different image distance {}",
        a.distance(&c)
    );
}

/// Minimal little-endian TIFF/EXIF block with a single Orientation tag.
fn exif_orientation(value: u16) -> Vec<u8> {
    let mut v = b"II*\0".to_vec();
    v.extend_from_slice(&8u32.to_le_bytes()); // IFD offset
    v.extend_from_slice(&1u16.to_le_bytes()); // one entry
    v.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    v.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    v.extend_from_slice(&1u32.to_le_bytes()); // count
    v.extend_from_slice(&value.to_le_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&0u32.to_le_bytes()); // next IFD
    v
}

/// FR-DUP-12: a copy stored rotated with an EXIF orientation tag matches.
#[test]
fn exif_rotated_copy_matches() {
    let d = tempfile::tempdir().unwrap();
    let orig = scene(320, 200);
    orig.save(d.path().join("orig.png")).unwrap();
    // Stored pixels rotated 90° counter-clockwise; EXIF 6 says "rotate 90° CW to display".
    let stored = DynamicImage::ImageRgb8(orig).rotate270().to_rgb8();
    let mut buf = Vec::new();
    let mut enc = PngEncoder::new(&mut buf);
    enc.set_exif_metadata(exif_orientation(6)).unwrap();
    enc.write_image(
        &stored,
        stored.width(),
        stored.height(),
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    fs::write(d.path().join("rotated.png"), buf).unwrap();

    let (a, dims_a) = image_hash(&d.path().join("orig.png")).unwrap();
    let (b, dims_b) = image_hash(&d.path().join("rotated.png")).unwrap();
    assert_eq!(dims_a, (320, 200));
    assert_eq!(dims_b, (320, 200), "dimensions reported as displayed");
    assert!(a.distance(&b) <= 2, "distance {}", a.distance(&b));
}

#[test]
fn pipeline_reports_similar_but_not_identical_twice() {
    let d = tempfile::tempdir().unwrap();
    let orig = scene(300, 300);
    orig.save(d.path().join("a.png")).unwrap();
    fs::copy(d.path().join("a.png"), d.path().join("a_copy.png")).unwrap();
    image::imageops::resize(&orig, 150, 150, image::imageops::FilterType::Triangle)
        .save(d.path().join("a_small.png"))
        .unwrap();
    let o = DupOptions {
        roots: vec![d.path().to_path_buf()],
        min_size: 1,
        similar_images: true,
        ..Default::default()
    };
    let r = find_duplicates(&o, &CancellationToken::new(), &|_| {});
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.groups.len(), 1);
    assert_eq!(r.similar.len(), 1);
    assert_eq!(
        r.similar[0].files.len(),
        2,
        "identical copy collapsed to one representative"
    );
    assert_eq!(r.stats.images_hashed, 2);
}
