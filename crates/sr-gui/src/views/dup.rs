//! DuplicateLens: exact and visually similar duplicates (SRS §3.5).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::RichText;
use sr_cli::settings::Settings;
use sr_core::Module;
use sr_dedup::{AutoSelect, DupGroup, DupOptions, DupResult, GroupKind, Pass, Progress};
use sr_ops::LinkKind;

use crate::app::{Action, App};
use crate::theme::{self, Typo};
use crate::util::{Job, format_date, format_duration};

pub enum DupMsg {
    Progress(Progress),
    Done(DupResult),
}

pub enum PreviewData {
    Image(egui::ColorImage),
    Text(String),
    Unavailable(String),
}

enum Preview {
    Image(egui::TextureHandle),
    Text(String),
    Unavailable(String),
}

pub struct LinkOutcome {
    path: PathBuf,
    result: Result<u64, String>,
}

pub struct DupState {
    pub roots: Vec<PathBuf>,
    min_size_mb: f64,
    include: String,
    exclude: String,
    paranoid: bool,
    similar_images: bool,
    threshold: u32,
    job: Option<Job<DupMsg>>,
    progress: Option<Progress>,
    result: Option<DupResult>,
    /// (similar?, index)
    selected: Option<(bool, usize)>,
    /// Files marked for removal.
    marks: HashSet<PathBuf>,
    rule: usize,
    preferred_folder: String,
    previews: HashMap<PathBuf, Preview>,
    preview_job: Option<Job<(PathBuf, PreviewData)>>,
    link_confirm: Option<LinkKind>,
    link_job: Option<Job<LinkOutcome>>,
    link_results: Vec<LinkOutcome>,
}

impl DupState {
    pub fn new(s: &Settings) -> Self {
        Self {
            roots: sr_platform::home_dir().into_iter().collect(),
            min_size_mb: s.dup_min_size as f64 / 1_048_576.0,
            include: String::new(),
            exclude: String::new(),
            paranoid: s.dup_paranoid,
            similar_images: false,
            threshold: s.dup_similarity_threshold,
            job: None,
            progress: None,
            result: None,
            selected: None,
            marks: HashSet::new(),
            rule: 0,
            preferred_folder: String::new(),
            previews: HashMap::new(),
            preview_job: None,
            link_confirm: None,
            link_job: None,
            link_results: Vec::new(),
        }
    }
}

fn rules(preferred: &str) -> Vec<AutoSelect> {
    vec![
        AutoSelect::KeepOldest,
        AutoSelect::KeepNewest,
        AutoSelect::KeepShortestPath,
        AutoSelect::KeepInFolder(PathBuf::from(preferred)),
        AutoSelect::KeepHighestResolution,
    ]
}

fn split_globs(s: &str) -> Vec<String> {
    s.split([',', ';'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

pub fn start(app: &mut App, ctx: &egui::Context) {
    let d = &mut app.dup;
    let opts = DupOptions {
        roots: d.roots.clone(),
        min_size: ((d.min_size_mb * 1_048_576.0) as u64).max(1),
        include: split_globs(&d.include),
        exclude: split_globs(&d.exclude),
        paranoid: d.paranoid,
        io_threads: app.settings.dup_io_threads,
        hash_cache: app.settings.hash_cache.clone(),
        similar_images: d.similar_images,
        similarity_threshold: d.threshold,
        ..DupOptions::default()
    };
    d.result = None;
    d.selected = None;
    d.marks.clear();
    d.previews.clear();
    d.progress = None;
    d.job = Some(Job::spawn(ctx, "dedup", move |tx, cancel| {
        let tx2 = tx.clone();
        let r = sr_dedup::find_duplicates(&opts, &cancel, &move |p| tx2.send(DupMsg::Progress(p)));
        tx.send(DupMsg::Done(r));
    }));
}

pub fn cancel(app: &mut App) {
    if let Some(j) = &app.dup.job {
        j.cancel.cancel();
    }
}

pub fn poll(app: &mut App) {
    let ctx_now = app.now;
    let d = &mut app.dup;
    if let Some(job) = &d.job {
        let mut done = false;
        for m in job.drain(1000) {
            match m {
                DupMsg::Progress(p) => d.progress = Some(p),
                DupMsg::Done(r) => {
                    let n = r.groups.len();
                    let msg = format!(
                        "DuplicateLens: {n} duplicate groups, {} similar groups, {} reclaimable",
                        r.similar.len(),
                        sr_core::format_size(r.total_wasted(), app.settings.size_units)
                    );
                    app.log.info(msg, ctx_now);
                    d.result = Some(r);
                    done = true;
                }
            }
        }
        if let Some(p) = job.take_panic() {
            app.log
                .error(format!("DuplicateLens crashed: {p}"), ctx_now);
            done = true;
        }
        if done || job.is_finished() {
            d.job = None;
        }
    }
    if let Some(job) = &d.link_job {
        d.link_results.extend(job.drain(10_000));
        if job.is_finished() {
            d.link_job = None;
            let ok: Vec<PathBuf> = d
                .link_results
                .iter()
                .filter(|r| r.result.is_ok())
                .map(|r| r.path.clone())
                .collect();
            let freed: u64 = d
                .link_results
                .iter()
                .filter_map(|r| r.result.as_ref().ok())
                .sum();
            let failed = d.link_results.len() - ok.len();
            app.log.info(
                format!(
                    "Replaced {} files with links, {} freed{}",
                    ok.len(),
                    sr_core::format_size(freed, app.settings.size_units),
                    if failed > 0 {
                        format!(", {failed} failed")
                    } else {
                        String::new()
                    }
                ),
                ctx_now,
            );
            for r in &d.link_results {
                if let Err(e) = &r.result {
                    app.log.warn(format!("{}: {e}", r.path.display()), ctx_now);
                }
            }
            d.link_results.clear();
            remove_files(d, &ok);
        }
    }
}

pub fn after_execution(app: &mut App, done: &[PathBuf]) {
    remove_files(&mut app.dup, done);
}

fn remove_files(d: &mut DupState, done: &[PathBuf]) {
    let set: HashSet<&Path> = done.iter().map(PathBuf::as_path).collect();
    if let Some(r) = &mut d.result {
        for g in r.groups.iter_mut().chain(r.similar.iter_mut()) {
            let keep: Vec<bool> = g
                .files
                .iter()
                .map(|f| !set.contains(f.path.as_path()))
                .collect();
            if g.image_dims.len() == g.files.len() {
                let mut i = 0;
                g.image_dims.retain(|_| {
                    i += 1;
                    keep[i - 1]
                });
            }
            g.files.retain(|f| !set.contains(f.path.as_path()));
        }
        r.groups.retain(|g| g.files.len() > 1);
        r.similar.retain(|g| g.files.len() > 1);
        r.groups.sort_by_key(|g| std::cmp::Reverse(g.wasted()));
    }
    d.marks.retain(|p| !set.contains(p.as_path()));
    d.selected = None;
}

fn group(d: &DupState, sel: (bool, usize)) -> Option<&DupGroup> {
    let r = d.result.as_ref()?;
    if sel.0 {
        r.similar.get(sel.1)
    } else {
        r.groups.get(sel.1)
    }
}

pub fn view(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    poll_previews(app, &ctx);

    ui.horizontal_wrapped(|ui| {
        ui.heading("DuplicateLens");
        ui.label(
            RichText::new(
                "Compares size, then a partial hash, then a full BLAKE3 hash. Optionally finds look-alike images.",
            )
            .weak(),
        );
    });
    options_bar(app, ui, &ctx);
    ui.separator();

    if let Some(p) = &app.dup.progress {
        if app.dup.job.is_some() {
            progress_line(app, ui, p.clone());
        }
    }

    let Some(result) = &app.dup.result else {
        if app.dup.job.is_none() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label("Choose folders and press Find duplicates.");
                ui.label(
                    RichText::new(
                        "Hardlinked copies are recognised as one file and never reported.",
                    )
                    .weak(),
                );
            });
        }
        return;
    };
    let stats = &result.stats;
    ui.label(format!(
        "{} files scanned, {} groups, {} reclaimable, {} hashed, {}{}",
        stats.files_scanned,
        result.groups.len(),
        app.fmt(result.total_wasted()),
        app.fmt(stats.bytes_hashed),
        format_duration(stats.elapsed),
        if stats.cancelled { ", cancelled" } else { "" }
    ));

    egui::Panel::left("dup_groups")
        .resizable(true)
        .default_size(360.0)
        .show(ui, |ui| groups_list(app, ui));
    egui::CentralPanel::default().show(ui, |ui| group_detail(app, ui, &ctx));

    link_dialogs(app, &ctx);
}

fn options_bar(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    let mut go = false;
    {
        let d = &mut app.dup;
        ui.horizontal_wrapped(|ui| {
            ui.label("Roots:");
            let mut remove = None;
            for (i, r) in d.roots.iter().enumerate() {
                ui.label(RichText::new(r.display().to_string()).monospace());
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                d.roots.remove(i);
            }
            if ui.button("Add folder…").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    d.roots.push(p);
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Min size");
            ui.add(egui::DragValue::new(&mut d.min_size_mb).range(0.000001..=100_000.0).speed(0.1).suffix(" MiB"));
            ui.add(egui::TextEdit::singleline(&mut d.include).hint_text("include globs, e.g. *.jpg").desired_width(140.0));
            ui.add(egui::TextEdit::singleline(&mut d.exclude).hint_text("exclude globs").desired_width(140.0));
            ui.checkbox(&mut d.paranoid, "Paranoid").on_hover_text(
                "Compare final groups byte-by-byte before reporting them (slower, eliminates any hash-collision risk).",
            );
            ui.checkbox(&mut d.similar_images, "Similar images");
            if d.similar_images {
                ui.add(egui::Slider::new(&mut d.threshold, 0..=20).text("max distance"))
                    .on_hover_text("Hamming distance between perceptual hashes (0 = visually identical, higher = looser).");
            }
            ui.separator();
            if let Some(j) = &d.job {
                if ui.button("Cancel").clicked() {
                    j.cancel.cancel();
                }
            } else if ui
                .add_enabled(!d.roots.is_empty(), theme::primary(ui, "Find duplicates"))
                .clicked()
            {
                go = true;
            }
        });
    }
    if go {
        start(app, ctx);
    }
}

fn progress_line(app: &App, ui: &mut egui::Ui, p: Progress) {
    let pass = match p.pass {
        Pass::Collect => "Collecting files",
        Pass::Size => "Grouping by size",
        Pass::Partial => "Pass 2: partial hash",
        Pass::Full => "Pass 3: full hash",
        Pass::Verify => "Verifying byte-by-byte",
        Pass::Perceptual => "Perceptual hashing",
    };
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new(pass).semibold());
        let frac = if p.bytes_total > 0 {
            p.bytes_hashed as f32 / p.bytes_total as f32
        } else if p.files_total > 0 {
            p.files_done as f32 / p.files_total as f32
        } else {
            0.0
        };
        ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).desired_width(220.0));
        ui.label(format!(
            "{}/{} files, {} / {}, {}/s{}",
            p.files_done,
            p.files_total,
            app.fmt(p.bytes_hashed),
            app.fmt(p.bytes_total),
            app.fmt(p.throughput as u64),
            p.eta_secs
                .map(|e| format!(
                    ", ETA {}",
                    format_duration(std::time::Duration::from_secs_f64(e))
                ))
                .unwrap_or_default()
        ));
    });
}

fn groups_list(app: &mut App, ui: &mut egui::Ui) {
    let Some(r) = app.dup.result.take() else {
        return;
    };
    groups_list_inner(app, ui, &r);
    app.dup.result = Some(r);
}

fn groups_list_inner(app: &mut App, ui: &mut egui::Ui, r: &DupResult) {
    let mut clicked = None;
    // Auto-select across all identical groups (FR-DUP-17).
    ui.horizontal_wrapped(|ui| {
        let rs = rules(&app.dup.preferred_folder);
        egui::ComboBox::from_id_salt("rule")
            .selected_text(rs[app.dup.rule].label().to_string())
            .show_ui(ui, |ui| {
                for (i, rule) in rs.iter().enumerate() {
                    ui.selectable_value(&mut app.dup.rule, i, rule.label().to_string())
                        .on_hover_text(rule.explanation());
                }
            })
            .response
            .on_hover_text(rs[app.dup.rule].explanation());
        if app.dup.rule == 3 {
            ui.add(
                egui::TextEdit::singleline(&mut app.dup.preferred_folder)
                    .hint_text("preferred folder")
                    .desired_width(140.0),
            );
        }
    });
    ui.horizontal(|ui| {
        if ui
            .button("Auto-select all")
            .on_hover_text("Marks copies to remove in every identical group, always keeping one file. Similar groups are never auto-selected.")
            .clicked()
        {
            let rule = rules(&app.dup.preferred_folder)[app.dup.rule].clone();
            let mut marks = HashSet::new();
            for g in &r.groups {
                for (f, m) in g.files.iter().zip(sr_dedup::auto_select(g, &rule)) {
                    if m {
                        marks.insert(f.path.clone());
                    }
                }
            }
            app.dup.marks = marks;
        }
        if ui.button("Clear marks").clicked() {
            app.dup.marks.clear();
        }
    });
    let marked_bytes: u64 = r
        .groups
        .iter()
        .chain(&r.similar)
        .flat_map(|g| &g.files)
        .filter(|f| app.dup.marks.contains(&f.path))
        .map(|f| f.size)
        .sum();
    ui.horizontal(|ui| {
        ui.label(format!(
            "{} marked, {}",
            app.dup.marks.len(),
            app.fmt(marked_bytes)
        ));
        if ui
            .add_enabled(
                !app.dup.marks.is_empty(),
                theme::primary(ui, "Send marked to Trash Drawer"),
            )
            .clicked()
        {
            app.actions.push(Action::Stage {
                paths: app.dup.marks.iter().cloned().collect(),
                source: Module::DuplicateLens,
                reason: "Duplicate copy".into(),
            });
            app.drawer_open = true;
        }
    });
    ui.separator();
    let sections = [
        (false, "Identical", &r.groups),
        (true, "Similar (review only)", &r.similar),
    ];
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (similar, title, groups) in sections {
                if groups.is_empty() {
                    continue;
                }
                ui.label(RichText::new(format!("{title} — {} groups", groups.len())).semibold());
                for (i, g) in groups.iter().enumerate().take(5000) {
                    let sel = app.dup.selected == Some((similar, i));
                    let name = g
                        .files
                        .first()
                        .map(|f| crate::app::path_name(&f.path))
                        .unwrap_or_default();
                    let marked = g
                        .files
                        .iter()
                        .filter(|f| app.dup.marks.contains(&f.path))
                        .count();
                    let label = format!(
                        "{}{} × {}, {} wasted{}",
                        if similar { "≈ " } else { "" },
                        g.files.len(),
                        name,
                        app.fmt(g.wasted()),
                        if marked > 0 {
                            format!(", {marked} marked")
                        } else {
                            String::new()
                        }
                    );
                    if ui.selectable_label(sel, label).clicked() {
                        clicked = Some((similar, i));
                    }
                }
            }
        });
    if let Some(s) = clicked {
        app.dup.selected = Some(s);
    }
}

fn group_detail(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    let Some(sel) = app.dup.selected else {
        ui.centered_and_justified(|ui| ui.label("Select a group to compare its files."));
        return;
    };
    let Some(g) = group(&app.dup, sel).cloned() else {
        return;
    };
    request_previews(app, ctx, &g);
    let similar = matches!(g.kind, GroupKind::Similar { .. });

    ui.horizontal_wrapped(|ui| {
        match &g.kind {
            GroupKind::Identical => {
                ui.label(
                    RichText::new("IDENTICAL")
                        .semibold()
                        .color(theme::of(ui).ok),
                );
                if let Some(h) = &g.hash {
                    ui.label(
                        RichText::new(format!("BLAKE3 {}", &h[..16.min(h.len())]))
                            .monospace()
                            .weak(),
                    );
                }
            }
            GroupKind::Similar { max_distance } => {
                ui.label(
                    RichText::new("SIMILAR")
                        .semibold()
                        .color(theme::of(ui).warn),
                )
                .on_hover_text(
                    "Visually similar, not byte-identical. Review carefully; never auto-selected.",
                );
                ui.label(format!("max distance {max_distance}"));
            }
        }
        ui.label(format!(
            "{} files, {} each, {} wasted",
            g.files.len(),
            app.fmt(g.size),
            app.fmt(g.wasted())
        ));
    });
    ui.horizontal(|ui| {
        if !similar {
            let rule = rules(&app.dup.preferred_folder)[app.dup.rule].clone();
            if ui.button(format!("Apply “{}”", rule.label())).on_hover_text(rule.explanation()).clicked() {
                for (f, m) in g.files.iter().zip(sr_dedup::auto_select(&g, &rule)) {
                    if m {
                        app.dup.marks.insert(f.path.clone());
                    } else {
                        app.dup.marks.remove(&f.path);
                    }
                }
            }
        }
        let any_marked = g.files.iter().any(|f| app.dup.marks.contains(&f.path));
        if !similar && any_marked {
            let same_volume = g.files.iter().all(|f| f.device.is_some() && f.device == g.files[0].device);
            if ui
                .button("Replace with clone")
                .on_hover_text("Reflink / copy-on-write clone: frees space while keeping each copy independent. Preferred where supported.")
                .clicked()
            {
                app.dup.link_confirm = Some(LinkKind::Reflink);
            }
            if ui
                .add_enabled(same_volume, egui::Button::new("Replace with hardlink"))
                .on_hover_text("All paths will share one file: editing one changes all of them.")
                .on_disabled_hover_text("Hardlinks require all files on the same volume.")
                .clicked()
            {
                app.dup.link_confirm = Some(LinkKind::Hardlink);
            }
        }
    });
    ui.separator();

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let marks: Vec<bool> = g
                    .files
                    .iter()
                    .map(|f| app.dup.marks.contains(&f.path))
                    .collect();
                for (i, f) in g.files.iter().enumerate() {
                    ui.group(|ui| {
                        ui.set_width(240.0);
                        ui.vertical(|ui| {
                            let mut marked = marks[i];
                            let unmarked_others = marks
                                .iter()
                                .enumerate()
                                .filter(|&(j, m)| j != i && !m)
                                .count();
                            // Never allow every member of a group to be marked (FR-DUP-17).
                            let can_mark = marked || unmarked_others > 0;
                            let label = if marked { "Remove" } else { "Keep" };
                            if ui
                                .add_enabled(can_mark, egui::Checkbox::new(&mut marked, label))
                                .on_disabled_hover_text("At least one copy must be kept")
                                .changed()
                            {
                                if marked {
                                    app.dup.marks.insert(f.path.clone());
                                } else {
                                    app.dup.marks.remove(&f.path);
                                }
                            }
                            preview(app, ui, &f.path);
                            ui.label(RichText::new(crate::app::path_name(&f.path)).semibold());
                            ui.label(RichText::new(f.path.display().to_string()).small().weak())
                                .context_menu(|ui| {
                                    if ui.button("Reveal in file manager").clicked() {
                                        app.actions.push(Action::Reveal(f.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Open").clicked() {
                                        app.actions.push(Action::Open(f.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Copy path").clicked() {
                                        app.actions.push(Action::CopyPath(f.path.clone()));
                                        ui.close();
                                    }
                                });
                            ui.label(app.fmt(f.size));
                            ui.label(format!("Modified {}", format_date(f.mtime)));
                            if let Some(Some((w, h))) = g.image_dims.get(i) {
                                ui.label(format!("{w} × {h} px"));
                            }
                            if ui.small_button("Reveal").clicked() {
                                app.actions.push(Action::Reveal(f.path.clone()));
                            }
                        });
                    });
                }
            });
        });
}

fn preview(app: &App, ui: &mut egui::Ui, path: &Path) {
    match app.dup.previews.get(path) {
        Some(Preview::Image(tex)) => {
            let size = tex.size_vec2();
            let scale = (220.0 / size.x).min(180.0 / size.y).min(1.0);
            ui.image((tex.id(), size * scale));
        }
        Some(Preview::Text(t)) => {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_max_height(180.0);
                egui::ScrollArea::vertical()
                    .id_salt(path)
                    .max_height(170.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(t).monospace().small());
                    });
            });
        }
        Some(Preview::Unavailable(why)) => {
            ui.label(RichText::new(why).weak().italics());
        }
        None => {
            ui.spinner();
        }
    }
}

/// Load thumbnails and text previews off the UI thread.
fn request_previews(app: &mut App, ctx: &egui::Context, g: &DupGroup) {
    let missing: Vec<PathBuf> = g
        .files
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| !app.dup.previews.contains_key(p))
        .collect();
    if missing.is_empty() || app.dup.preview_job.is_some() {
        return;
    }
    app.dup.preview_job = Some(Job::spawn(ctx, "preview", move |tx, cancel| {
        for p in missing {
            if cancel.is_cancelled() {
                return;
            }
            let data = load_preview(&p);
            tx.send((p, data));
        }
    }));
}

fn load_preview(p: &Path) -> PreviewData {
    if sr_dedup::is_image_path(p) {
        return match image::ImageReader::open(p).and_then(|r| r.with_guessed_format()) {
            Ok(reader) => match reader.decode() {
                Ok(img) => {
                    let thumb = img.thumbnail(256, 256).to_rgba8();
                    let size = [thumb.width() as usize, thumb.height() as usize];
                    PreviewData::Image(egui::ColorImage::from_rgba_unmultiplied(
                        size,
                        thumb.as_raw(),
                    ))
                }
                Err(e) => PreviewData::Unavailable(format!("Cannot decode: {e}")),
            },
            Err(e) => PreviewData::Unavailable(e.to_string()),
        };
    }
    use std::io::Read;
    let mut buf = vec![0u8; 4096];
    match std::fs::File::open(p).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => {
            buf.truncate(n);
            match std::str::from_utf8(&buf) {
                Ok(s) if !s.contains('\0') => PreviewData::Text(s.to_string()),
                Err(e) if e.valid_up_to() > n.saturating_sub(4) && n > 0 => {
                    PreviewData::Text(String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned())
                }
                _ => PreviewData::Unavailable("Binary file — no preview".into()),
            }
        }
        Err(e) => PreviewData::Unavailable(e.to_string()),
    }
}

fn poll_previews(app: &mut App, ctx: &egui::Context) {
    let Some(job) = &app.dup.preview_job else {
        return;
    };
    for (p, data) in job.drain(64) {
        let preview = match data {
            PreviewData::Image(img) => Preview::Image(ctx.load_texture(
                p.display().to_string(),
                img,
                egui::TextureOptions::LINEAR,
            )),
            PreviewData::Text(t) => Preview::Text(t),
            PreviewData::Unavailable(s) => Preview::Unavailable(s),
        };
        app.dup.previews.insert(p, preview);
    }
    if job.is_finished() {
        app.dup.preview_job = None;
    }
    if app.dup.previews.len() > 400 {
        app.dup.previews.clear();
    }
}

fn link_dialogs(app: &mut App, ctx: &egui::Context) {
    let Some(kind) = app.dup.link_confirm else {
        return;
    };
    let Some(g) = app.dup.selected.and_then(|s| group(&app.dup, s)).cloned() else {
        app.dup.link_confirm = None;
        return;
    };
    let marked: Vec<PathBuf> = g
        .files
        .iter()
        .filter(|f| app.dup.marks.contains(&f.path))
        .map(|f| f.path.clone())
        .collect();
    let keep = g
        .files
        .iter()
        .find(|f| !app.dup.marks.contains(&f.path))
        .map(|f| f.path.clone());
    let mut close = false;
    egui::Modal::new(egui::Id::new("link_confirm")).show(ctx, |ui| {
        let what = match kind {
            LinkKind::Hardlink => "hardlinks",
            LinkKind::Reflink => "copy-on-write clones",
        };
        ui.heading(format!("Replace {} files with {what}?", marked.len()));
        if let Some(k) = &keep {
            ui.label(format!("Kept original: {}", k.display()));
        }
        if kind == LinkKind::Hardlink {
            ui.label(
                RichText::new("⚠ After this, all paths refer to the same file. Editing any one of them changes all of them.")
                    .color(theme::of(ui).warn),
            );
        }
        ui.label("Each file is replaced atomically: the link is created under a temporary name, its content is verified, then it is renamed over the copy. On any failure the copy is left untouched.");
        ui.horizontal(|ui| {
            let cancel = ui.button("Cancel");
            cancel.request_focus();
            if cancel.clicked() {
                close = true;
            }
            if ui.add(theme::danger(ui, "Replace")).clicked() && keep.is_some() {
                let keep = keep.clone().unwrap_or_default();
                let protected = app.protected.clone();
                let journal = app.journal.as_ref().map(|j| j.path().to_path_buf());
                let marked = marked.clone();
                app.dup.link_job = Some(Job::spawn(ctx, "link", move |tx, cancel| {
                    let journal = journal.map(sr_ops::Journal::open);
                    for p in marked {
                        if cancel.is_cancelled() {
                            return;
                        }
                        let result = sr_ops::snapshot(&p, Module::DuplicateLens, "Duplicate copy")
                            .and_then(|snap| sr_ops::replace_with_link(&keep, &snap, kind, &protected, journal.as_ref()))
                            .map_err(|e| e.to_string());
                        tx.send(LinkOutcome { path: p, result });
                    }
                }));
                close = true;
            }
        });
    });
    if close {
        app.dup.link_confirm = None;
    }
}
