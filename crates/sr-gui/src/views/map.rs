//! Space Map: sunburst, breadcrumbs, synchronized list, search (SRS §3.2).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};

use egui::{Color32, Key, Pos2, RichText, Sense, Shape, Stroke, Vec2};
use egui_extras::{Column, TableBuilder};
use sr_cli::settings::ColorMode;
use sr_core::{Module, NodeFlags, NodeId, NodeKind, SizeMode, Tree, now_secs};

use crate::app::{Action, App, ScanState};
use crate::commands::Cmd;
use crate::sunburst::{
    ArcSeg, Colorizer, Geometry, Layout, LayoutParams, Palette, Transform, ease_in_out, tessellate,
};
use crate::theme::{self, Typo};
use crate::util::{Job, format_date, group_digits};

/// Drag-and-drop payload: a path dragged from the chart or list onto the
/// Trash Drawer (FR-MAP-12).
#[derive(Debug, Clone)]
pub struct DragPath(pub PathBuf);

#[derive(Debug, Clone, Copy, PartialEq)]
struct LayoutKey {
    center: NodeId,
    params: (u32, u32, SizeMode),
    tree_len: usize,
    root_size: u64,
    drawer_gen: u64,
}

pub struct SearchResult {
    query: String,
    hits: Vec<NodeId>,
    ancestors: HashSet<NodeId>,
    truncated: bool,
}

pub struct MapState {
    pub center: NodeId,
    pub selected: Option<NodeId>,
    hovered: Option<ArcSeg>,
    hovered_center: bool,
    list_hover: Option<NodeId>,
    layout: Layout,
    key: Option<LayoutKey>,
    built_at: f64,
    anim: Option<(Transform, f64)>,
    context_arc: Option<ArcSeg>,
    pub search: String,
    search_job: Option<Job<SearchResult>>,
    search_changed_at: Option<f64>,
    search_result: Option<SearchResult>,
    pub focus_search: bool,
}

impl Default for MapState {
    fn default() -> Self {
        Self {
            center: Tree::ROOT,
            selected: None,
            hovered: None,
            hovered_center: false,
            list_hover: None,
            layout: Layout::default(),
            key: None,
            built_at: f64::NEG_INFINITY,
            anim: None,
            context_arc: None,
            search: String::new(),
            search_job: None,
            search_changed_at: None,
            search_result: None,
            focus_search: false,
        }
    }
}

impl MapState {
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    /// Show `id`: centre on its parent directory and select it.
    pub fn reveal(&mut self, scan: &ScanState, id: NodeId) {
        let Ok(t) = scan.tree().read() else { return };
        let parent = t.node(id).parent.unwrap_or(Tree::ROOT);
        self.center = parent;
        self.selected = Some(id);
        self.anim = None;
        self.key = None;
    }

    fn zoom_to(
        &mut self,
        tree: &Tree,
        new_center: NodeId,
        now: f64,
        animate: bool,
        p: LayoutParams,
    ) {
        if new_center == self.center || tree.node(new_center).flags.contains(NodeFlags::REMOVED) {
            return;
        }
        let old_center = self.center;
        let new_layout = Layout::build(tree, new_center, p);
        let from = if !animate {
            None
        } else if let Some(arc) = self.layout.find_node(new_center) {
            // Zoom in: the new chart grows out of the clicked arc.
            Some(Transform {
                angle_scale: arc.span() / std::f32::consts::TAU,
                angle_offset: arc.start,
                ring_offset: arc.ring as f32,
                alpha: 1.0,
            })
        } else if let Some(arc) = new_layout.find_node(old_center) {
            // Zoom out: the old centre shrinks back into its arc.
            let s = std::f32::consts::TAU / arc.span().max(1e-6);
            Some(Transform {
                angle_scale: s,
                angle_offset: -arc.start * s,
                ring_offset: -(arc.ring as f32),
                alpha: 1.0,
            })
        } else {
            Some(Transform {
                alpha: 0.0,
                ..Transform::IDENTITY
            })
        };
        self.center = new_center;
        self.layout = new_layout;
        self.key = None;
        self.anim = from.map(|f| (f, now));
        if self
            .selected
            .is_some_and(|s| !tree.is_ancestor(new_center, s) || s == new_center)
        {
            self.selected = None;
        }
    }
}

fn params(app: &App) -> LayoutParams {
    LayoutParams {
        rings: app.settings.rings.clamp(2, 10),
        min_angle: app.settings.min_arc_degrees.to_radians(),
        mode: app.settings.size_mode,
    }
}

fn palette(app: &App) -> Palette {
    if app.settings.high_contrast {
        return Palette::HighContrast;
    }
    match app.settings.color_mode {
        ColorMode::Branch => Palette::Branch,
        ColorMode::FileType => Palette::FileType,
        ColorMode::Age => Palette::Age,
    }
}

pub fn view(app: &mut App, ui: &mut egui::Ui) {
    let Some(scan) = &app.scan else { return };
    // The chart appears once the scan completes; until then, show progress.
    if !scan.handle.is_done() {
        scanning_view(app, ui);
        return;
    }
    let tree_arc: Arc<RwLock<Tree>> = scan.tree().clone();
    let tree = tree_arc.read().unwrap_or_else(|e| e.into_inner());
    if tree
        .get(app.map.center)
        .is_none_or(|n| n.flags.contains(NodeFlags::REMOVED))
    {
        app.map.center = Tree::ROOT;
    }

    status_bar(app, ui, &tree);
    breadcrumbs(app, ui, &tree);
    ui.separator();

    egui::Panel::right("map_list")
        .resizable(true)
        .default_size(380.0)
        .min_size(260.0)
        .show(ui, |ui| list_panel(app, ui, &tree));

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| chart(app, ui, &tree));

    poll_search(app, ui.ctx(), &tree_arc);
}

/// Progress screen shown while a scan runs.
fn scanning_view(app: &mut App, ui: &mut egui::Ui) {
    let Some(scan) = &app.scan else { return };
    let h = &scan.handle;
    let p = h.progress.clone();
    let pal = theme::of(ui);
    let bytes = p.bytes.load(Ordering::Relaxed);
    let roots = &h.options.roots;
    // Scanning a whole disk: its used space is a good estimate of the total.
    let total = scan
        .volume
        .as_ref()
        .filter(|v| roots.len() == 1 && roots[0] == v.mount_point)
        .map(|v| v.used())
        .filter(|&u| u > 0);
    let name = if roots.len() == 1 {
        crate::app::path_name(&roots[0])
    } else {
        format!("{} folders", roots.len())
    };
    let (paused, elapsed) = (h.is_paused(), h.elapsed());
    let mut toggle_pause = false;
    let mut cancel = false;

    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(24.0));
        ui.label(
            RichText::new(if paused {
                format!("Paused scanning {name}")
            } else {
                format!("Scanning {name}")
            })
            .semibold()
            .size(15.0)
            .color(pal.slate),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(app.fmt(bytes))
                .display_bold(48.0)
                .color(pal.ink),
        );
        ui.add_space(14.0);
        let width = ui.available_width().min(460.0);
        match total {
            Some(t) => {
                let frac = (bytes as f32 / t as f32).min(0.99);
                theme::meter(ui, frac, width, pal.accent);
                ui.add_space(6.0);
                ui.label(theme::muted(
                    ui,
                    format!(
                        "about {:.0}% of {} used on this disk",
                        frac * 100.0,
                        app.fmt(t)
                    ),
                ));
            }
            None => {
                let animate = !paused && !app.reduce_motion();
                theme::indeterminate(ui, width, pal.accent, animate);
                ui.add_space(6.0);
            }
        }
        ui.label(theme::muted(
            ui,
            format!(
                "{} files in {} folders, {}",
                group_digits(p.files.load(Ordering::Relaxed)),
                group_digits(p.dirs.load(Ordering::Relaxed)),
                crate::util::format_duration(elapsed)
            ),
        ));
        let errors = p.errors.load(Ordering::Relaxed);
        if errors > 0 {
            ui.label(
                RichText::new(format!(
                    "{} locations could not be read",
                    group_digits(errors)
                ))
                .color(pal.warn),
            );
        }
        ui.add_space(18.0);
        // Centre the two buttons as a group.
        let btn_w = 190.0;
        ui.allocate_ui_with_layout(
            egui::vec2(btn_w, 30.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                    toggle_pause = true;
                }
                if ui.button("Cancel scan").on_hover_text("Esc").clicked() {
                    cancel = true;
                }
            },
        );
    });
    if let Some(s) = &app.scan {
        if toggle_pause {
            s.handle.set_paused(!paused);
        }
        if cancel {
            s.handle.cancel();
        }
    }
}

fn status_bar(app: &mut App, ui: &mut egui::Ui, tree: &Tree) {
    let Some(scan) = &app.scan else { return };
    let h = &scan.handle;
    let p = h.progress.clone();
    let elapsed = h.elapsed();
    let pal = theme::of(ui);
    ui.horizontal_wrapped(|ui| {
        let root = tree.node(tree.root());
        ui.label(
            RichText::new(app.fmt(root.size(app.settings.size_mode)))
                .display_bold(22.0)
                .color(pal.ink),
        );
        ui.label(theme::muted(
            ui,
            format!(
                "{} items, scanned in {}",
                group_digits(root.items as u64),
                crate::util::format_duration(elapsed)
            ),
        ));
        if p.cancelled.load(Ordering::Relaxed) {
            theme::tag(ui, "Partial: scan was cancelled", pal.warn);
        }
        ui.add_space(8.0);
        if ui.button("⟳ Rescan").on_hover_text("Ctrl/Cmd+R").clicked() {
            app.actions.push(Action::Rescan);
        }
        if !tree.issues.is_empty()
            && ui
                .button(
                    RichText::new(format!("⚠ {} unreadable", tree.issues.len())).color(pal.warn),
                )
                .clicked()
        {
            app.dialogs.issues = true;
        }
        let ctx = ui.ctx().clone();
        let hint = |c: Cmd| c.shortcut_text(&ctx).unwrap_or_default();
        if ui
            .button("Largest files")
            .on_hover_text(hint(Cmd::LargestFiles))
            .clicked()
        {
            app.actions.push(Action::Command(Cmd::LargestFiles));
        }
        ui.menu_button("Export", |ui| {
            for c in [Cmd::ExportJson, Cmd::ExportCsv, Cmd::ExportSvg] {
                let b = egui::Button::new(c.label()).shortcut_text(hint(c));
                if ui.add(b).clicked() {
                    app.actions.push(Action::Command(c));
                    ui.close();
                }
            }
        });
        if ui
            .button("New scan")
            .on_hover_text(hint(Cmd::NewScan))
            .clicked()
        {
            app.actions.push(Action::Command(Cmd::NewScan));
        }
        ui.separator();
        let mut mode = app.settings.size_mode;
        ui.selectable_value(&mut mode, SizeMode::Allocated, "On disk")
            .on_hover_text("Allocated size: bytes actually used on disk");
        ui.selectable_value(&mut mode, SizeMode::Apparent, "Apparent")
            .on_hover_text("Apparent size: logical file length");
        if mode != app.settings.size_mode {
            app.settings.size_mode = mode;
            app.settings_dirty = true;
        }
        let mut rings = app.settings.rings;
        if ui
            .add(egui::Slider::new(&mut rings, 2..=10).text("rings"))
            .changed()
        {
            app.settings.rings = rings;
            app.settings_dirty = true;
        }
        let mut cm = app.settings.color_mode;
        egui::ComboBox::from_id_salt("color_mode")
            .selected_text(match cm {
                ColorMode::Branch => "Colour: folder",
                ColorMode::FileType => "Colour: file type",
                ColorMode::Age => "Colour: age",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cm, ColorMode::Branch, "By top-level folder");
                ui.selectable_value(&mut cm, ColorMode::FileType, "By file type");
                ui.selectable_value(&mut cm, ColorMode::Age, "By age (green = recent)");
            });
        if cm != app.settings.color_mode {
            app.settings.color_mode = cm;
            app.settings_dirty = true;
        }
    });
}

fn breadcrumbs(app: &mut App, ui: &mut egui::Ui, tree: &Tree) {
    let chain: Vec<NodeId> = {
        let mut v: Vec<NodeId> = tree.ancestors(app.map.center).collect();
        v.reverse();
        v
    };
    let mut go = None;
    let pal = theme::of(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, &id) in chain.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").color(pal.slate));
            }
            let name = if id == Tree::ROOT {
                let p = tree.root_path();
                if p.as_os_str().is_empty() {
                    "All roots".to_string()
                } else {
                    crate::util::display_path(p)
                }
            } else {
                tree.name_lossy(id)
            };
            let current = id == app.map.center;
            let text = if current {
                RichText::new(name).semibold().color(pal.ink)
            } else {
                RichText::new(name).color(pal.accent)
            };
            if ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
                && !current
            {
                go = Some(id);
            }
        }
    });
    if let Some(id) = go {
        let p = params(app);
        let animate = !app.reduce_motion();
        app.map.zoom_to(tree, id, app.now, animate, p);
    }
}

fn ensure_layout(app: &mut App, tree: &Tree) {
    let p = params(app);
    let key = LayoutKey {
        center: app.map.center,
        params: (p.rings, p.min_angle.to_bits(), p.mode),
        tree_len: tree.len(),
        root_size: tree.node(tree.root()).size(p.mode),
        drawer_gen: app.drawer_gen,
    };
    let scanning = app.scan.as_ref().is_some_and(|s| !s.handle.is_done());
    let stale = app.map.key != Some(key);
    let only_growth = app.map.key.is_some_and(|k| {
        k.center == key.center && k.params == key.params && k.drawer_gen == key.drawer_gen
    });
    // While streaming, rebuild at most every 250 ms.
    if stale && (!scanning || !only_growth || app.now - app.map.built_at > 0.25) {
        app.map.layout = Layout::build(tree, app.map.center, p);
        app.map.key = Some(key);
        app.map.built_at = app.now;
    }
}

fn chart(app: &mut App, ui: &mut egui::Ui, tree: &Tree) {
    ensure_layout(app, tree);
    let p = params(app);
    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let radius = (rect.width().min(rect.height()) * 0.5 - 12.0).max(40.0);
    let geo = Geometry::new(rect.center(), radius, p.rings);
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter_at(rect);

    // Animation (FR-MAP-05).
    let xf = match app.map.anim {
        Some((from, start)) => {
            let dur = (app.settings.animation_ms.max(1) as f64) / 1000.0;
            let t = ((app.now - start) / dur) as f32;
            if t >= 1.0 || app.reduce_motion() {
                app.map.anim = None;
                Transform::IDENTITY
            } else {
                ui.ctx().request_repaint();
                Transform::lerp(from, Transform::IDENTITY, ease_in_out(t))
            }
        }
        None => Transform::IDENTITY,
    };
    let animating = app.map.anim.is_some();

    let colors = Colorizer {
        palette: palette(app),
        dark,
        tree,
        now: now_secs(),
    };
    let mesh = tessellate(&app.map.layout, &geo, &colors, xf, p.rings);
    painter.add(Shape::mesh(mesh));

    // Hover / hit-testing (NFR-PERF-04).
    app.map.hovered = None;
    app.map.hovered_center = false;
    if !animating {
        if let Some(pos) = response.hover_pos() {
            match geo.polar(pos) {
                (Some(0), _) => app.map.hovered_center = true,
                (Some(ring), theta) => {
                    app.map.hovered = app.map.layout.arc_at(ring, theta).copied()
                }
                _ => {}
            }
        }
    }
    // Mirror list hover into the chart (FR-MAP-08).
    let list_hover_arc = app
        .map
        .list_hover
        .and_then(|id| app.map.layout.find_node(id).copied());

    let accent = ui.visuals().selection.stroke.color;
    let outline = theme::of(ui).ink;
    if !animating {
        // Search highlights (FR-MAP-14).
        if let Some(sr) = &app.map.search_result {
            for arc in app.map.layout.rings.iter().flatten() {
                if let Some(n) = arc.node {
                    if sr.ancestors.contains(&n) || sr.hits.binary_search(&n).is_ok() {
                        outline_arc(&painter, &geo, arc, Stroke::new(2.0, accent));
                    }
                }
            }
        }
        if let Some(sel) = app
            .map
            .selected
            .and_then(|s| app.map.layout.find_node(s).copied())
        {
            outline_arc(&painter, &geo, &sel, Stroke::new(3.0, accent));
        }
        for arc in app.map.hovered.iter().chain(list_hover_arc.iter()) {
            outline_arc(&painter, &geo, arc, Stroke::new(2.0, outline));
        }
        labels(&painter, &geo, &app.map.layout, tree, &colors);
    }

    // Centre disk: the current size is the headline figure of the screen.
    let pal = theme::of(ui);
    painter.circle_filled(geo.center, geo.hole - 2.0, pal.surface);
    painter.circle_stroke(geo.center, geo.hole - 2.0, Stroke::new(1.0, pal.line));
    if app.map.hovered_center && app.map.center != Tree::ROOT {
        painter.circle_stroke(geo.center, geo.hole - 3.0, Stroke::new(2.0, pal.accent));
    }
    let center_name = |id: NodeId| {
        if id == Tree::ROOT {
            let p = tree.root_path();
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        } else {
            tree.name_lossy(id)
        }
    };
    let center_size = tree.node(app.map.center).size(p.mode).max(1);
    let (title, size, note) = match &app.map.hovered {
        Some(arc) => (
            arc_name(tree, arc),
            arc.size,
            format!(
                "{:.1}% of this folder",
                arc.size as f64 * 100.0 / center_size as f64
            ),
        ),
        None => (
            center_name(app.map.center),
            tree.node(app.map.center).size(p.mode),
            if app.map.center != Tree::ROOT {
                "Click to go up".to_string()
            } else {
                String::new()
            },
        ),
    };
    let figure = (geo.hole * 0.36).clamp(22.0, 52.0);
    let max_chars = ((geo.hole * 2.0 - 24.0) / 7.5).max(4.0) as usize;
    painter.text(
        geo.center - Vec2::new(0.0, figure * 0.62 + 4.0),
        egui::Align2::CENTER_CENTER,
        truncate(&title, max_chars),
        theme::semibold_font(13.0),
        pal.slate,
    );
    painter.text(
        geo.center,
        egui::Align2::CENTER_CENTER,
        app.fmt(size),
        theme::display_font(figure),
        pal.ink,
    );
    if !note.is_empty() {
        painter.text(
            geo.center + Vec2::new(0.0, figure * 0.62 + 4.0),
            egui::Align2::CENTER_CENTER,
            note,
            egui::FontId::proportional(12.0),
            pal.slate,
        );
    }

    // Tooltip (FR-MAP-04).
    let hovered = app.map.hovered;
    let response = if let Some(arc) = hovered {
        response.on_hover_ui_at_pointer(|ui| tooltip(app, ui, tree, &arc))
    } else {
        response
    };

    // Clicks (FR-MAP-05/06).
    if response.clicked() {
        if app.map.hovered_center {
            if let Some(parent) = tree.node(app.map.center).parent {
                let animate = !app.reduce_motion();
                app.map.zoom_to(tree, parent, app.now, animate, p);
            }
        } else if let Some(arc) = hovered {
            if let Some(n) = arc.node {
                app.map.selected = Some(n);
                if tree.node(n).kind == NodeKind::Dir {
                    let animate = !app.reduce_motion();
                    app.map.zoom_to(tree, n, app.now, animate, p);
                }
            }
        }
    }

    // Drag onto the Trash Drawer (FR-MAP-12).
    if response.drag_started() {
        if let Some(n) = hovered.and_then(|a| a.node) {
            response.dnd_set_drag_payload(DragPath(tree.path(n)));
        }
    }

    // Context menu (FR-MAP-11).
    if response.secondary_clicked() {
        app.map.context_arc = hovered;
    }
    let ctx_node = app.map.context_arc.and_then(|a| a.node);
    if let Some(n) = ctx_node {
        response.context_menu(|ui| context_menu(app, ui, tree, n));
    }

    keyboard(app, ui.ctx(), tree);

    if tree.node(app.map.center).size(p.mode) == 0 {
        let msg = if app.scan.as_ref().is_some_and(|s| !s.handle.is_done()) {
            "Scanning…"
        } else {
            "This folder is empty"
        };
        painter.text(
            geo.center + Vec2::new(0.0, radius * 0.6),
            egui::Align2::CENTER_CENTER,
            msg,
            egui::FontId::proportional(14.0),
            ui.visuals().weak_text_color(),
        );
    }
}

fn outline_arc(painter: &egui::Painter, g: &Geometry, arc: &ArcSeg, stroke: Stroke) {
    let (r0, r1) = g.radii(arc.ring as f32);
    let r1 = r1 - 1.0;
    let n = ((arc.span() * r1 / 6.0).ceil() as usize).clamp(2, 256);
    let mut pts: Vec<Pos2> = Vec::with_capacity(2 * n + 2);
    for i in 0..=n {
        pts.push(g.point(r1, arc.start + arc.span() * i as f32 / n as f32));
    }
    for i in (0..=n).rev() {
        pts.push(g.point(r0, arc.start + arc.span() * i as f32 / n as f32));
    }
    painter.add(Shape::closed_line(pts, stroke));
}

fn labels(
    painter: &egui::Painter,
    g: &Geometry,
    layout: &Layout,
    tree: &Tree,
    colors: &Colorizer<'_>,
) {
    if g.ring_width < 16.0 {
        return;
    }
    let font = egui::FontId::proportional(11.0);
    for arc in layout.rings.iter().flatten() {
        let Some(n) = arc.node else { continue };
        let (r0, r1) = g.radii(arc.ring as f32);
        let mid_r = (r0 + r1) * 0.5;
        let arc_len = arc.span() * mid_r;
        // Horizontal labels need room both along and across the ring.
        let width = arc_len.min(g.ring_width * 1.6);
        if arc_len < 48.0 || width < 36.0 {
            continue;
        }
        let max_chars = (width / 6.5) as usize;
        let name = truncate(&tree.name_lossy(n), max_chars);
        if name.chars().count() < 3 {
            continue;
        }
        let pos = g.point(mid_r, arc.mid());
        painter.text(
            pos,
            egui::Align2::CENTER_CENTER,
            name,
            font.clone(),
            label_color(colors.color(arc)),
        );
    }
}

/// Black or white text, whichever contrasts more with the arc fill.
fn label_color(fill: Color32) -> Color32 {
    let lum = 0.2126 * fill.r() as f32 + 0.7152 * fill.g() as f32 + 0.0722 * fill.b() as f32;
    if lum > 140.0 {
        Color32::from_black_alpha(230)
    } else {
        Color32::from_white_alpha(240)
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else if max <= 1 {
        "…".into()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn arc_name(tree: &Tree, arc: &ArcSeg) -> String {
    match arc.node {
        Some(n) => tree.name_lossy(n),
        None => format!("{} smaller items", arc.count),
    }
}

fn tooltip(app: &App, ui: &mut egui::Ui, tree: &Tree, arc: &ArcSeg) {
    let p = theme::of(ui);
    let mode = app.settings.size_mode;
    let parent_size = tree.node(arc.parent).size(mode).max(1);
    ui.set_max_width(300.0);
    ui.label(RichText::new(arc_name(tree, arc)).semibold().color(p.ink));
    ui.label(
        RichText::new(app.fmt(arc.size))
            .display_bold(22.0)
            .color(p.ink),
    );
    ui.add_space(2.0);
    let row = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(text).size(12.0).color(p.slate));
    };
    row(
        ui,
        format!(
            "{:.1}% of {}",
            arc.size as f64 * 100.0 / parent_size as f64,
            tree.name_lossy(arc.parent)
        ),
    );
    if let Some(v) = app.scan.as_ref().and_then(|s| s.volume.as_ref()) {
        if v.total > 0 {
            row(
                ui,
                format!(
                    "{:.2}% of the disk",
                    arc.size as f64 * 100.0 / v.total as f64
                ),
            );
        }
    }
    let Some(n) = arc.node else {
        row(ui, "Too small to draw at this zoom level".into());
        return;
    };
    let node = tree.node(n);
    if node.is_dir() {
        row(ui, format!("{} items", group_digits(node.items as u64)));
    }
    row(ui, format!("Modified {}", format_date(node.mtime)));
    let f = node.flags;
    let mut notes: Vec<(&str, egui::Color32)> = Vec::new();
    if arc.staged {
        notes.push(("In Trash Drawer", p.accent));
    }
    if f.contains(NodeFlags::CLOUD) {
        notes.push(("Cloud file, local size only", p.accent));
    }
    if f.contains(NodeFlags::HARDLINKED) {
        notes.push(("Hard link, counted once", p.slate));
    }
    if f.contains(NodeFlags::ERROR) {
        notes.push(("Partly unreadable", p.warn));
    }
    if f.contains(NodeFlags::EXCLUDED) {
        notes.push(("Not scanned: another disk", p.slate));
    }
    if node.kind == NodeKind::Symlink {
        notes.push(("Symbolic link, not followed", p.slate));
    }
    if !notes.is_empty() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (t, c) in notes {
                theme::tag(ui, t, c);
            }
        });
    }
}

pub fn context_menu(app: &mut App, ui: &mut egui::Ui, tree: &Tree, n: NodeId) {
    let path = tree.path(n);
    ui.label(RichText::new(tree.name_lossy(n)).semibold());
    ui.separator();
    if ui.button("Reveal in file manager").clicked() {
        app.actions.push(Action::Reveal(path.clone()));
        ui.close();
    }
    if ui.button("Open").clicked() {
        app.actions.push(Action::Open(path.clone()));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        app.actions.push(Action::CopyPath(path.clone()));
        ui.close();
    }
    ui.separator();
    let staged = app.drawer.covers(&path);
    if staged {
        if app.drawer.contains(&path) && ui.button("Remove from Trash Drawer").clicked() {
            app.actions.push(Action::Unstage(path.clone()));
            ui.close();
        }
    } else if ui
        .add_enabled(
            !app.protected.is_protected(&path),
            egui::Button::new("Add to Trash Drawer"),
        )
        .on_disabled_hover_text("Protected path")
        .clicked()
    {
        app.actions.push(Action::Stage {
            paths: vec![path.clone()],
            source: Module::SpaceMap,
            reason: "Selected in Space Map".into(),
        });
        ui.close();
    }
    if ui.button("Exclude from scan").clicked() {
        app.actions.push(Action::ExcludeFromScan(path.clone()));
        ui.close();
    }
    if tree.node(n).is_dir() && ui.button("Show in DevSweep").clicked() {
        app.actions.push(Action::ShowInDevSweep(path));
        ui.close();
    }
}

/// Keyboard navigation (FR-MAP-17).
fn keyboard(app: &mut App, ctx: &egui::Context, tree: &Tree) {
    if ctx.egui_wants_keyboard_input() || app.dialogs.confirm.is_some() {
        return;
    }
    let (left, right, up, down, enter, back, del) = ctx.input(|i| {
        (
            i.key_pressed(Key::ArrowLeft),
            i.key_pressed(Key::ArrowRight),
            i.key_pressed(Key::ArrowUp),
            i.key_pressed(Key::ArrowDown),
            i.key_pressed(Key::Enter),
            i.key_pressed(Key::Backspace),
            i.key_pressed(Key::Delete),
        )
    });
    let p = params(app);
    let layout = &app.map.layout;
    let sel_arc = app.map.selected.and_then(|s| layout.find_node(s).copied());
    let mut new_sel = app.map.selected;
    if (left || right || up || down) && sel_arc.is_none() {
        new_sel = layout
            .rings
            .first()
            .and_then(|r| r.first())
            .and_then(|a| a.node);
    } else if let Some(a) = sel_arc {
        if left || right {
            let ring = &layout.rings[a.ring as usize - 1];
            let sibs: Vec<NodeId> = ring
                .iter()
                .filter(|x| x.parent == a.parent)
                .filter_map(|x| x.node)
                .collect();
            if let Some(i) = sibs.iter().position(|&x| Some(x) == a.node) {
                let len = sibs.len();
                let j = if right {
                    (i + 1) % len
                } else {
                    (i + len - 1) % len
                };
                new_sel = Some(sibs[j]);
            }
        } else if up {
            if let Some(child) = layout.rings.get(a.ring as usize).and_then(|r| {
                r.iter()
                    .find(|x| Some(x.parent) == a.node && x.node.is_some())
            }) {
                new_sel = child.node;
            }
        } else if down && a.ring > 1 {
            new_sel = Some(a.parent);
        }
    }
    app.map.selected = new_sel;
    let animate = !app.reduce_motion();
    if enter {
        if let Some(s) = app.map.selected.filter(|&s| tree.node(s).is_dir()) {
            app.map.zoom_to(tree, s, app.now, animate, p);
        }
    }
    if back {
        if let Some(parent) = tree.node(app.map.center).parent {
            let old = app.map.center;
            app.map.zoom_to(tree, parent, app.now, animate, p);
            app.map.selected = Some(old);
        }
    }
    if del && app.map.selected.is_some() {
        app.actions.push(Action::Command(Cmd::StageSelection));
    }
}

/// Stage the chart's selected item in the Trash Drawer.
pub fn stage_selection(app: &mut App) {
    let Some(sel) = app.map.selected else { return };
    let path = app
        .scan
        .as_ref()
        .and_then(|s| s.tree().read().ok().map(|t| t.path(sel)));
    if let Some(path) = path {
        app.actions.push(Action::Stage {
            paths: vec![path],
            source: Module::SpaceMap,
            reason: "Selected in Space Map".into(),
        });
    }
}

/// Export the current scan (FR-MAP-16).
pub fn export_current(app: &mut App, kind: &str) {
    let Some(tree) = app.scan.as_ref().map(|s| s.tree().clone()) else {
        return;
    };
    let t = tree.read().unwrap_or_else(|e| e.into_inner());
    export(app, &t, kind);
}

fn list_panel(app: &mut App, ui: &mut egui::Ui, tree: &Tree) {
    let mode = app.settings.size_mode;
    let searching = app
        .map
        .search_result
        .as_ref()
        .filter(|r| !r.query.is_empty() && r.query == app.map.search.trim());
    let (rows, title): (Vec<NodeId>, String) = match searching {
        Some(sr) => {
            let mut hits: Vec<NodeId> = sr
                .hits
                .iter()
                .copied()
                .filter(|&h| !tree.node(h).flags.contains(NodeFlags::REMOVED))
                .collect();
            hits.sort_by_key(|&h| std::cmp::Reverse(tree.node(h).size(mode)));
            let t = format!(
                "{} matches for “{}”{}",
                hits.len(),
                sr.query,
                if sr.truncated { " (first 5,000)" } else { "" }
            );
            (hits, t)
        }
        None => {
            let mut kids: Vec<NodeId> = tree
                .children(app.map.center)
                .filter(|&c| !tree.node(c).flags.contains(NodeFlags::REMOVED))
                .collect();
            kids.sort_by_key(|&c| std::cmp::Reverse(tree.node(c).size(mode)));
            (
                kids,
                format!(
                    "{} items in {}",
                    group_digits(tree.node(app.map.center).items as u64),
                    if app.map.center == Tree::ROOT {
                        crate::app::path_name(tree.root_path())
                    } else {
                        tree.name_lossy(app.map.center)
                    }
                ),
            )
        }
    };
    ui.label(RichText::new(title).semibold());
    let total = tree.node(app.map.center).size(mode).max(1);
    // The chart's hovered arc, mapped to its ring-1 ancestor for mirroring.
    let chart_hover = app.map.hovered.and_then(|a| a.node).and_then(|n| {
        tree.ancestors(n)
            .find(|&x| tree.node(x).parent == Some(app.map.center))
    });
    let mut new_hover = None;
    let mut zoom = None;
    let search_mode = searching.is_some();
    let colors = Colorizer {
        palette: palette(app),
        dark: ui.visuals().dark_mode,
        tree,
        now: now_secs(),
    };
    let row_h = 22.0;
    TableBuilder::new(ui)
        .striped(true)
        .sense(Sense::click_and_drag())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::remainder().at_least(120.0).clip(true))
        .column(Column::auto().at_least(70.0))
        .column(Column::auto().at_least(40.0))
        .column(Column::auto().at_least(80.0))
        .header(20.0, |mut h| {
            h.col(|ui| {
                theme::column_header(ui, "Name");
            });
            h.col(|ui| {
                theme::column_header(ui, "Size");
            });
            h.col(|ui| {
                theme::column_header(ui, "%");
            });
            h.col(|ui| {
                theme::column_header(ui, "Modified");
            });
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let id = rows[row.index()];
                let n = tree.node(id);
                let selected = app.map.selected == Some(id) || chart_hover == Some(id);
                row.set_selected(selected);
                let staged = app.drawer.items().iter().any(|i| {
                    tree.find_path(&i.path)
                        .is_some_and(|s| tree.is_ancestor(s, id))
                });
                row.col(|ui| {
                    let arc = app.map.layout.find_node(id).copied();
                    let swatch = arc.map(|a| colors.color(&a)).unwrap_or(Color32::GRAY);
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, swatch);
                    let icon = match n.kind {
                        NodeKind::Dir => "",
                        NodeKind::Symlink => "↪ ",
                        _ => "",
                    };
                    let mut name = format!("{icon}{}", tree.name_lossy(id));
                    if search_mode {
                        name = tree.path(id).display().to_string();
                    }
                    if n.flags.contains(NodeFlags::CLOUD) {
                        name.push_str(" ☁");
                    }
                    let mut text = RichText::new(name);
                    if staged {
                        text = text.strikethrough().weak();
                    }
                    ui.label(text);
                });
                row.col(|ui| {
                    ui.label(app.fmt(n.size(mode)));
                });
                row.col(|ui| {
                    if !search_mode {
                        ui.label(format!("{:.0}", n.size(mode) as f64 * 100.0 / total as f64));
                    }
                });
                row.col(|ui| {
                    ui.label(format_date(n.mtime));
                });
                let resp = row.response();
                if resp.hovered() {
                    new_hover = Some(id);
                }
                if resp.clicked() {
                    app.map.selected = Some(id);
                }
                if resp.double_clicked() {
                    zoom = Some(id);
                }
                if resp.drag_started() {
                    resp.dnd_set_drag_payload(DragPath(tree.path(id)));
                }
                resp.context_menu(|ui| context_menu(app, ui, tree, id));
            });
        });
    app.map.list_hover = new_hover;
    if let Some(id) = zoom {
        let p = params(app);
        let animate = !app.reduce_motion();
        if tree.node(id).is_dir() {
            app.map.zoom_to(tree, id, app.now, animate, p);
        } else if let Some(parent) = tree.node(id).parent {
            app.map.zoom_to(tree, parent, app.now, animate, p);
            app.map.selected = Some(id);
        }
    }
}

// ----------------------------------------------------------------- search

pub fn search_box(app: &mut App, ui: &mut egui::Ui) {
    let edit = egui::TextEdit::singleline(&mut app.map.search)
        .hint_text("🔍 search (substring or glob)")
        .desired_width(220.0);
    let r = ui.add(edit);
    if app.map.focus_search {
        r.request_focus();
        app.map.focus_search = false;
    }
    if r.changed() {
        app.map.search_changed_at = Some(app.now);
    }
    if !app.map.search.is_empty() && ui.small_button("×").clicked() {
        app.map.search.clear();
        app.map.search_result = None;
        app.map.search_changed_at = None;
    }
}

fn poll_search(app: &mut App, ctx: &egui::Context, tree: &Arc<RwLock<Tree>>) {
    if let Some(job) = &app.map.search_job {
        if let Some(r) = job.drain(8).pop() {
            app.map.search_result = Some(r);
            app.map.search_job = None;
        } else if job.is_finished() {
            app.map.search_job = None;
        }
    }
    let Some(t0) = app.map.search_changed_at else {
        return;
    };
    if app.now - t0 < 0.25 {
        ctx.request_repaint_after(std::time::Duration::from_millis(260));
        return;
    }
    app.map.search_changed_at = None;
    let query = app.map.search.trim().to_string();
    if query.is_empty() {
        app.map.search_result = None;
        return;
    }
    let tree = tree.clone();
    app.map.search_job = Some(Job::spawn(ctx, "search", move |tx, cancel| {
        let t = tree.read().unwrap_or_else(|e| e.into_inner());
        let matcher = Matcher::new(&query);
        let mut hits = Vec::new();
        let mut truncated = false;
        for i in 1..t.len() {
            if i % 65_536 == 0 && cancel.is_cancelled() {
                return;
            }
            let id = NodeId(i as u32);
            if t.node(id).flags.contains(NodeFlags::REMOVED) {
                continue;
            }
            if matcher.matches(&t.name(id).to_string_lossy()) {
                if hits.len() >= 5000 {
                    truncated = true;
                    break;
                }
                hits.push(id);
            }
        }
        let mut ancestors = HashSet::new();
        for &h in &hits {
            for a in t.ancestors(h).skip(1) {
                if !ancestors.insert(a) {
                    break;
                }
            }
        }
        tx.send(SearchResult {
            query,
            hits,
            ancestors,
            truncated,
        });
    }));
}

/// Case-insensitive substring, or glob when the query contains `*` or `?`.
pub struct Matcher {
    pattern: Vec<char>,
    glob: bool,
    needle: String,
}

impl Matcher {
    pub fn new(q: &str) -> Self {
        let lower = q.to_lowercase();
        Self {
            glob: lower.contains(['*', '?']),
            pattern: lower.chars().collect(),
            needle: lower,
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        if !self.glob {
            return name.contains(&self.needle);
        }
        let text: Vec<char> = name.chars().collect();
        wildcard(&self.pattern, &text)
    }
}

fn wildcard(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ----------------------------------------------------------------- export

fn export(app: &mut App, tree: &Tree, kind: &str) {
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(format!("spacerazer.{kind}"))
        .add_filter(kind.to_uppercase(), &[kind])
        .save_file()
    else {
        return;
    };
    let mode = app.settings.size_mode;
    let content = match kind {
        "json" => sr_scan::export::to_json(tree, 6, 500, mode),
        "csv" => sr_scan::export::to_csv(tree, 6, mode),
        _ => svg(app, tree),
    };
    match std::fs::write(&path, content) {
        Ok(()) => app
            .log
            .info(format!("Exported {}", path.display()), app.now),
        Err(e) => app.log.error(format!("Export failed: {e}"), app.now),
    }
}

/// Render the current chart as SVG (FR-MAP-16).
fn svg(app: &App, tree: &Tree) -> String {
    let size = 800.0;
    let geo = Geometry::new(
        Pos2::new(size / 2.0, size / 2.0),
        size / 2.0 - 10.0,
        app.settings.rings,
    );
    let colors = Colorizer {
        palette: palette(app),
        dark: false,
        tree,
        now: now_secs(),
    };
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" viewBox=\"0 0 {size} {size}\">\n"
    );
    for arc in app.map.layout.rings.iter().flatten() {
        let (r0, r1) = geo.radii(arc.ring as f32);
        let c = colors.color(arc);
        let large = if arc.span() > std::f32::consts::PI {
            1
        } else {
            0
        };
        let (a0, a1) = (
            arc.start,
            arc.end.min(arc.start + std::f32::consts::TAU - 1e-4),
        );
        let p0 = geo.point(r1, a0);
        let p1 = geo.point(r1, a1);
        let p2 = geo.point(r0, a1);
        let p3 = geo.point(r0, a0);
        let title = xml_escape(&arc_name(tree, arc));
        out.push_str(&format!(
            "<path d=\"M{:.2},{:.2} A{r1:.2},{r1:.2} 0 {large} 1 {:.2},{:.2} L{:.2},{:.2} A{r0:.2},{r0:.2} 0 {large} 0 {:.2},{:.2} Z\" fill=\"#{:02x}{:02x}{:02x}\" stroke=\"white\" stroke-width=\"0.5\"><title>{title} — {}</title></path>\n",
            p0.x, p0.y, p1.x, p1.y, p2.x, p2.y, p3.x, p3.y, c.r(), c.g(), c.b(), app.fmt(arc.size)
        ));
    }
    out.push_str("</svg>\n");
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher() {
        assert!(Matcher::new("MOD").matches("node_modules"));
        assert!(Matcher::new("*.iso").matches("Ubuntu.ISO"));
        assert!(!Matcher::new("*.iso").matches("iso.txt"));
        assert!(Matcher::new("f?o*").matches("foobar"));
        assert!(!Matcher::new("f?o").matches("foobar"));
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
