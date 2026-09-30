//! Sunburst layout, tessellation and hit-testing (SRS §6.4).
//!
//! Layout is a breadth-first cumulative-angle pass from the centre node,
//! merging arcs below the minimum angle into one "smaller items" arc per
//! parent (FR-MAP-03). Arcs in each ring are stored in increasing angle
//! order, so hit-testing is a binary search per ring (NFR-PERF-04).

use std::f32::consts::TAU;

use egui::{Color32, Mesh, Pos2, Vec2, epaint::Hsva};
use sr_core::{NodeFlags, NodeId, NodeKind, SizeMode, Tree};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArcSeg {
    /// The node, or `None` for an aggregated "smaller items" arc.
    pub node: Option<NodeId>,
    pub parent: NodeId,
    /// 1-based ring index (ring 1 is closest to the centre).
    pub ring: u32,
    pub start: f32,
    pub end: f32,
    /// Index of the ring-1 ancestor, used for hue.
    pub branch: u32,
    pub size: u64,
    /// Number of nodes merged into an aggregate arc (1 for real arcs).
    pub count: u32,
    pub staged: bool,
    pub kind: NodeKind,
    pub mtime: i64,
}

impl ArcSeg {
    pub fn span(&self) -> f32 {
        self.end - self.start
    }
    pub fn mid(&self) -> f32 {
        (self.start + self.end) * 0.5
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    pub rings: u32,
    pub min_angle: f32,
    pub mode: SizeMode,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// `rings[k]` holds ring k+1, sorted by angle.
    pub rings: Vec<Vec<ArcSeg>>,
}

impl Layout {
    pub fn build(tree: &Tree, center: NodeId, p: LayoutParams) -> Self {
        let mut rings: Vec<Vec<ArcSeg>> = Vec::with_capacity(p.rings as usize);
        let center_staged = tree
            .ancestors(center)
            .any(|a| tree.node(a).flags.contains(NodeFlags::STAGED));

        // Frontier of parents to expand in the next ring: (node, start, end, branch, staged).
        let mut frontier: Vec<(NodeId, f32, f32, u32, bool)> =
            vec![(center, 0.0, TAU, 0, center_staged)];
        for ring in 1..=p.rings {
            let mut arcs = Vec::new();
            let mut next = Vec::new();
            for &(parent, start, end, branch, staged) in &frontier {
                let psize = tree.node(parent).size(p.mode);
                if psize == 0 {
                    continue;
                }
                let span = end - start;
                let per_byte = span / psize as f32;
                // Collect children big enough to draw; the rest are merged.
                let mut big: Vec<NodeId> = Vec::new();
                let mut small_size = 0u64;
                let mut small_count = 0u32;
                for c in tree.children(parent) {
                    let n = tree.node(c);
                    if n.flags
                        .intersects(NodeFlags::REMOVED | NodeFlags::HARDLINK_DUP)
                    {
                        continue;
                    }
                    let s = n.size(p.mode);
                    if s == 0 {
                        continue;
                    }
                    if s as f32 * per_byte >= p.min_angle {
                        big.push(c);
                    } else {
                        small_size += s;
                        small_count += 1;
                    }
                }
                big.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).size(p.mode)));
                let mut a = start;
                for (i, c) in big.into_iter().enumerate() {
                    let n = tree.node(c);
                    let s = n.size(p.mode);
                    let e = (a + s as f32 * per_byte).min(end);
                    let br = if ring == 1 { i as u32 } else { branch };
                    let st = staged || n.flags.contains(NodeFlags::STAGED);
                    arcs.push(ArcSeg {
                        node: Some(c),
                        parent,
                        ring,
                        start: a,
                        end: e,
                        branch: br,
                        size: s,
                        count: 1,
                        staged: st,
                        kind: n.kind,
                        mtime: n.mtime,
                    });
                    if n.kind == NodeKind::Dir && n.first_child.is_some() {
                        next.push((c, a, e, br, st));
                    }
                    a = e;
                }
                if small_count > 0 {
                    let e = (a + small_size as f32 * per_byte).min(end);
                    if e > a {
                        arcs.push(ArcSeg {
                            node: None,
                            parent,
                            ring,
                            start: a,
                            end: e,
                            branch: if ring == 1 { u32::MAX } else { branch },
                            size: small_size,
                            count: small_count,
                            staged,
                            kind: NodeKind::Other,
                            mtime: 0,
                        });
                    }
                }
            }
            if arcs.is_empty() {
                break;
            }
            rings.push(arcs);
            frontier = next;
        }
        Self { rings }
    }

    /// Find the arc at polar angle `theta` in 1-based `ring`.
    pub fn arc_at(&self, ring: u32, theta: f32) -> Option<&ArcSeg> {
        let arcs = self.rings.get(ring.checked_sub(1)? as usize)?;
        let i = arcs.partition_point(|a| a.end <= theta);
        arcs.get(i).filter(|a| a.start <= theta && theta < a.end)
    }

    pub fn find_node(&self, node: NodeId) -> Option<&ArcSeg> {
        self.rings.iter().flatten().find(|a| a.node == Some(node))
    }

    #[cfg(test)]
    pub fn arc_count(&self) -> usize {
        self.rings.iter().map(Vec::len).sum()
    }
}

/// Screen geometry of the chart.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub center: Pos2,
    pub hole: f32,
    pub ring_width: f32,
}

impl Geometry {
    pub fn new(center: Pos2, radius: f32, rings: u32) -> Self {
        let hole = radius * 0.22;
        let ring_width = (radius - hole) / rings.max(1) as f32;
        Self {
            center,
            hole,
            ring_width,
        }
    }

    /// Screen position → (ring, angle). Ring 0 means the centre disk.
    pub fn polar(&self, p: Pos2) -> (Option<u32>, f32) {
        let d = p - self.center;
        let r = d.length();
        let mut theta = d.y.atan2(d.x) + TAU / 4.0; // 0 at 12 o'clock, clockwise
        if theta < 0.0 {
            theta += TAU;
        }
        if theta >= TAU {
            theta -= TAU;
        }
        if r < self.hole {
            return (Some(0), theta);
        }
        let ring = ((r - self.hole) / self.ring_width).floor() as u32 + 1;
        (Some(ring), theta)
    }

    pub fn point(&self, radius: f32, theta: f32) -> Pos2 {
        let a = theta - TAU / 4.0;
        self.center + Vec2::new(a.cos(), a.sin()) * radius
    }

    pub fn radii(&self, ring: f32) -> (f32, f32) {
        let r0 = self.hole + (ring - 1.0) * self.ring_width;
        (r0, r0 + self.ring_width)
    }
}

/// Transform applied to a layout while animating a zoom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub angle_scale: f32,
    pub angle_offset: f32,
    pub ring_offset: f32,
    /// Opacity multiplier.
    pub alpha: f32,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        angle_scale: 1.0,
        angle_offset: 0.0,
        ring_offset: 0.0,
        alpha: 1.0,
    };

    pub fn lerp(a: Transform, b: Transform, t: f32) -> Transform {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Transform {
            angle_scale: l(a.angle_scale, b.angle_scale),
            angle_offset: l(a.angle_offset, b.angle_offset),
            ring_offset: l(a.ring_offset, b.ring_offset),
            alpha: l(a.alpha, b.alpha),
        }
    }

    fn angle(&self, a: f32) -> f32 {
        (a * self.angle_scale + self.angle_offset).clamp(0.0, TAU)
    }
}

pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Branch,
    FileType,
    Age,
    HighContrast,
}

pub struct Colorizer<'a> {
    pub palette: Palette,
    pub dark: bool,
    pub tree: &'a Tree,
    pub now: i64,
}

const HIGH_CONTRAST: [Color32; 8] = [
    Color32::from_rgb(0, 114, 178),
    Color32::from_rgb(230, 159, 0),
    Color32::from_rgb(0, 158, 115),
    Color32::from_rgb(204, 121, 167),
    Color32::from_rgb(86, 180, 233),
    Color32::from_rgb(213, 94, 0),
    Color32::from_rgb(240, 228, 66),
    Color32::from_rgb(120, 120, 120),
];

impl Colorizer<'_> {
    pub fn color(&self, arc: &ArcSeg) -> Color32 {
        if arc.node.is_none() {
            return if self.dark {
                Color32::from_gray(90)
            } else {
                Color32::from_gray(185)
            };
        }
        let depth = arc.ring as f32;
        let c = match self.palette {
            Palette::Branch => {
                let hue = (arc.branch as f32 * 0.618_034).fract();
                let v = if self.dark {
                    0.92 - depth * 0.07
                } else {
                    0.95 - depth * 0.035
                };
                let s = if self.dark { 0.55 } else { 0.62 - depth * 0.06 };
                Hsva::new(hue, s.clamp(0.2, 1.0), v.clamp(0.35, 1.0), 1.0).into()
            }
            Palette::HighContrast => {
                let base = HIGH_CONTRAST[arc.branch as usize % HIGH_CONTRAST.len()];
                if arc.ring > 1 {
                    base.gamma_multiply(1.0 - (depth - 1.0) * 0.1)
                } else {
                    base
                }
            }
            Palette::FileType => {
                if arc.kind == NodeKind::Dir {
                    let g = if self.dark {
                        70 + (depth * 12.0) as u8
                    } else {
                        200 - (depth * 12.0) as u8
                    };
                    Color32::from_gray(g)
                } else {
                    let name = arc
                        .node
                        .map(|n| self.tree.name(n).to_string_lossy().to_lowercase());
                    category_color(name.as_deref().unwrap_or(""))
                }
            }
            Palette::Age => {
                let days = ((self.now - arc.mtime).max(0) / 86_400) as f32;
                // Green (recent) → red (over two years).
                let t = (days / 730.0).clamp(0.0, 1.0);
                Hsva::new(
                    0.33 * (1.0 - t),
                    0.6,
                    if self.dark { 0.8 } else { 0.85 },
                    1.0,
                )
                .into()
            }
        };
        if arc.staged {
            c.gamma_multiply(0.35)
        } else {
            c
        }
    }
}

/// File-type category colour (FR-MAP-09).
pub fn category_color(name: &str) -> Color32 {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    category(ext).1
}

pub fn category(ext: &str) -> (&'static str, Color32) {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "tif" | "tiff" | "bmp" | "raw"
        | "cr2" | "nef" | "arw" | "dng" | "svg" | "psd" => {
            ("Images", Color32::from_rgb(230, 159, 0))
        }
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "wmv" => {
            ("Video", Color32::from_rgb(213, 94, 0))
        }
        "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "aiff" => {
            ("Audio", Color32::from_rgb(204, 121, 167))
        }
        "zip" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "rar" | "tar" | "zst" | "dmg" | "iso"
        | "pkg" | "msi" => ("Archives", Color32::from_rgb(120, 94, 240)),
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "pages"
        | "key" | "numbers" | "odt" | "rtf" | "csv" => {
            ("Documents", Color32::from_rgb(0, 158, 115))
        }
        "rs" | "js" | "ts" | "tsx" | "jsx" | "py" | "go" | "java" | "kt" | "c" | "h" | "cpp"
        | "hpp" | "swift" | "rb" | "php" | "cs" | "json" | "toml" | "yaml" | "yml" | "html"
        | "css" => ("Code", Color32::from_rgb(86, 180, 233)),
        "o" | "a" | "so" | "dylib" | "dll" | "exe" | "rlib" | "rmeta" | "class" | "jar" | "pyc"
        | "wasm" => ("Binaries", Color32::from_rgb(0, 114, 178)),
        _ => ("Other", Color32::from_rgb(150, 150, 150)),
    }
}

/// Append an annular sector to `mesh`, with segment count scaled by on-screen
/// arc length (LOD).
/// `radii` is (inner, outer); `angles` is (start, end). Staged arcs are
/// `hatch`ed with alternating darker stripes (FR-MAP-13).
pub fn add_sector(
    mesh: &mut Mesh,
    g: &Geometry,
    (r0, r1): (f32, f32),
    (a0, a1): (f32, f32),
    color: Color32,
    hatch: bool,
) {
    let span = a1 - a0;
    if span <= 0.0 || r1 <= r0 {
        return;
    }
    let arc_px = span * r1;
    let segs = ((arc_px / 4.0).ceil() as usize).clamp(1, 256);
    let dark = color.gamma_multiply(0.6);
    let step = span / segs as f32;
    for i in 0..segs {
        let c = if hatch && (i / 2) % 2 == 1 {
            dark
        } else {
            color
        };
        let t0 = a0 + step * i as f32;
        let t1 = t0 + step;
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(g.point(r0, t0), c);
        mesh.colored_vertex(g.point(r1, t0), c);
        mesh.colored_vertex(g.point(r0, t1), c);
        mesh.colored_vertex(g.point(r1, t1), c);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 3, base + 2);
    }
}

/// Tessellate a whole layout into one mesh.
pub fn tessellate(
    layout: &Layout,
    g: &Geometry,
    colors: &Colorizer<'_>,
    xf: Transform,
    max_ring: u32,
) -> Mesh {
    let mut mesh = Mesh::default();
    let gap_r = 1.0_f32;
    let gap_a_px = 0.8_f32;
    for ring in &layout.rings {
        for arc in ring {
            let ring_f = arc.ring as f32 + xf.ring_offset;
            if ring_f < 1.0 - 1e-3 || ring_f > max_ring as f32 + 0.999 {
                continue;
            }
            let a0 = xf.angle(arc.start);
            let a1 = xf.angle(arc.end);
            if a1 - a0 <= 0.0 {
                continue;
            }
            let (r0, r1) = g.radii(ring_f);
            let r1 = r1 - gap_r;
            // Angular gap of ~1px at the outer radius, when the arc is wide enough.
            let ga = (gap_a_px / r1).min((a1 - a0) * 0.2);
            let mut c = colors.color(arc);
            if xf.alpha < 1.0 {
                c = c.gamma_multiply(xf.alpha);
            }
            add_sector(
                &mut mesh,
                g,
                (r0, r1),
                (a0 + ga * 0.5, a1 - ga * 0.5),
                c,
                arc.staged,
            );
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use sr_core::EntryInfo;
    use std::ffi::OsStr;

    fn tree() -> Tree {
        let mut t = Tree::new("/r", 0);
        let a = t.add_child(Tree::ROOT, OsStr::new("a"), EntryInfo::dir(0));
        t.add_child(a, OsStr::new("a1"), EntryInfo::file(600, 0));
        t.add_child(a, OsStr::new("a2"), EntryInfo::file(200, 0));
        t.add_child(Tree::ROOT, OsStr::new("b"), EntryInfo::file(200, 0));
        for i in 0..50 {
            t.add_child(
                Tree::ROOT,
                OsStr::new(&format!("tiny{i}")),
                EntryInfo::file(1, 0),
            );
        }
        t
    }

    fn params() -> LayoutParams {
        LayoutParams {
            rings: 5,
            min_angle: 0.5f32.to_radians(),
            mode: SizeMode::Apparent,
        }
    }

    #[test]
    fn layout_angles_and_aggregation() {
        let t = tree();
        let l = Layout::build(&t, Tree::ROOT, params());
        assert_eq!(l.rings.len(), 2);
        let r1 = &l.rings[0];
        // a, b, then one "smaller items" arc for 50 tiny files.
        assert_eq!(r1.len(), 3);
        assert_eq!(t.name(r1[0].node.unwrap()), "a");
        assert!(r1[2].node.is_none());
        assert_eq!(r1[2].count, 50);
        let total: f32 = r1.iter().map(ArcSeg::span).sum();
        assert!((total - TAU).abs() < 1e-3);
        // Monotonic angles for binary search.
        for w in r1.windows(2) {
            assert!(w[0].end <= w[1].start + 1e-6);
        }
        // Ring 2 children of `a` stay within a's span.
        for arc in &l.rings[1] {
            assert!(arc.start >= r1[0].start - 1e-6 && arc.end <= r1[0].end + 1e-6);
        }
    }

    #[test]
    fn hit_testing() {
        let t = tree();
        let l = Layout::build(&t, Tree::ROOT, params());
        let a = &l.rings[0][0];
        let hit = l.arc_at(1, a.mid()).unwrap();
        assert_eq!(hit.node, a.node);
        assert!(l.arc_at(3, 0.1).is_none());
        let g = Geometry::new(Pos2::new(100.0, 100.0), 100.0, 5);
        // Straight up is angle 0 and ring 1 just outside the hole.
        let (ring, theta) = g.polar(Pos2::new(100.0, 100.0 - g.hole - 1.0));
        assert_eq!(ring, Some(1));
        assert!(theta.abs() < 1e-3 || (theta - TAU).abs() < 1e-3);
        let (ring, _) = g.polar(Pos2::new(100.0, 100.0));
        assert_eq!(ring, Some(0));
    }

    #[test]
    fn zoom_into_subdir() {
        let t = tree();
        let a = t.children(Tree::ROOT).find(|&c| t.name(c) == "a").unwrap();
        let l = Layout::build(&t, a, params());
        assert_eq!(l.rings.len(), 1);
        assert_eq!(l.rings[0].len(), 2);
        assert!((l.rings[0][0].span() - TAU * 0.75).abs() < 1e-3);
    }

    #[test]
    fn staged_propagates() {
        let mut t = tree();
        let a = t.children(Tree::ROOT).find(|&c| t.name(c) == "a").unwrap();
        t.node_mut(a).flags |= NodeFlags::STAGED;
        let l = Layout::build(&t, Tree::ROOT, params());
        assert!(l.rings[1].iter().all(|arc| arc.staged));
    }

    #[test]
    fn mesh_is_bounded() {
        let t = tree();
        let l = Layout::build(&t, Tree::ROOT, params());
        let g = Geometry::new(Pos2::new(300.0, 300.0), 300.0, 5);
        let c = Colorizer {
            palette: Palette::Branch,
            dark: true,
            tree: &t,
            now: 0,
        };
        let m = tessellate(&l, &g, &c, Transform::IDENTITY, 5);
        assert!(!m.vertices.is_empty());
        assert!(m.vertices.len() < 20_000);
    }

    #[test]
    fn large_tree_layout_is_fast() {
        // NFR-PERF-03 proxy: layout over a wide tree stays bounded by visible arcs.
        let mut t = Tree::new("/r", 0);
        for d in 0..200 {
            let dir = t.add_child(Tree::ROOT, OsStr::new(&format!("d{d}")), EntryInfo::dir(0));
            for f in 0..2000 {
                t.add_child(
                    dir,
                    OsStr::new(&format!("f{f}")),
                    EntryInfo::file((f % 97 + 1) as u64 * 1000, 0),
                );
            }
        }
        let t0 = std::time::Instant::now();
        let l = Layout::build(&t, Tree::ROOT, params());
        let elapsed = t0.elapsed();
        assert!(l.arc_count() < 4000, "{}", l.arc_count());
        assert!(elapsed.as_millis() < 200, "{elapsed:?}");
    }
}
