//! Application state, action dispatch and the top-level layout (§4.1).

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use egui::{Key, KeyboardShortcut, Modifiers, RichText};
use sr_cli::settings::{Settings, Theme};
use sr_core::{Module, NodeFlags, NodeId, Tree, format_size};
use sr_ops::{Drawer, ExecEvent, Journal, Method, Report, StagedItem};
use sr_platform::{ProtectedPaths, VolumeInfo};
use sr_scan::ScanHandle;

use crate::theme::{self, Typo};
use crate::util::{Job, Level, Log};
use crate::views;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    SpaceMap,
    DevSweep,
    DuplicateLens,
}

/// Requests raised by views and handled centrally once per frame.
#[derive(Debug, Clone)]
pub enum Action {
    StartScan(Vec<PathBuf>),
    Rescan,
    Stage {
        paths: Vec<PathBuf>,
        source: Module,
        reason: String,
    },
    Unstage(PathBuf),
    ClearDrawer,
    Execute(Method),
    Reveal(PathBuf),
    Open(PathBuf),
    CopyPath(PathBuf),
    ExcludeFromScan(PathBuf),
    ShowInDevSweep(PathBuf),
    ShowInMap(PathBuf),
}

pub struct ScanState {
    pub handle: ScanHandle,
    pub volume: Option<VolumeInfo>,
    pub reported_done: bool,
}

impl ScanState {
    pub fn tree(&self) -> &Arc<RwLock<Tree>> {
        &self.handle.tree
    }
}

pub enum StageMsg {
    Item(StagedItem),
    Error(PathBuf, String),
}

pub struct ExecState {
    pub job: Job<ExecEvent>,
    pub method: Method,
    pub total: usize,
    pub done: usize,
}

#[derive(Default)]
pub struct Dialogs {
    pub confirm: Option<Method>,
    pub confirm_text: String,
    pub report: Option<Report>,
    pub journal: bool,
    pub issues: bool,
    pub help: bool,
    pub settings: bool,
    pub log: bool,
    pub largest: Option<Vec<NodeId>>,
    pub quarantine: bool,
    pub journal_records: Option<Vec<sr_ops::JournalRecord>>,
    pub journal_job: Option<Job<Vec<sr_ops::JournalRecord>>>,
    pub settings_section: usize,
    pub quarantine_confirm: Option<PathBuf>,
}

pub struct App {
    pub settings: Settings,
    pub settings_dirty: bool,
    pub tab: Tab,
    pub volumes: Vec<VolumeInfo>,
    pub volumes_job: Option<Job<Vec<VolumeInfo>>>,
    pub scan: Option<ScanState>,
    pub map: views::map::MapState,
    pub drawer: Drawer,
    pub drawer_path: Option<PathBuf>,
    pub drawer_open: bool,
    pub drawer_gen: u64,
    staged_nodes: Vec<NodeId>,
    pub protected: ProtectedPaths,
    pub journal: Option<Journal>,
    pub staging: Vec<Job<StageMsg>>,
    pub exec: Option<ExecState>,
    pub dev: views::dev::DevState,
    pub dup: views::dup::DupState,
    pub dialogs: Dialogs,
    pub log: Log,
    pub actions: Vec<Action>,
    pub now: f64,
    /// Dev aid: `SPACERAZER_SCREENSHOT=<png>` captures the window after
    /// `SPACERAZER_SCREENSHOT_DELAY` seconds (default 5) and exits.
    screenshot: Option<(PathBuf, f64, bool)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_roots: Vec<PathBuf>) -> Self {
        let (settings, settings_err) = Settings::load_default();
        let drawer_path = sr_platform::app_dirs().map(|d| d.data.join("drawer.json"));
        let mut log = Log::default();
        if let Some(e) = settings_err {
            log.warn(
                format!("Settings could not be loaded, using defaults: {e}"),
                0.0,
            );
        }
        let drawer = match &drawer_path {
            Some(p) if p.exists() => match Drawer::load(p) {
                Ok(d) => d,
                Err(e) => {
                    log.warn(format!("Trash Drawer could not be restored: {e}"), 0.0);
                    Drawer::new()
                }
            },
            _ => Drawer::new(),
        };
        let protected = settings.protected();
        let journal = Journal::default_location().map(Journal::open);
        let volumes_job = Some(Job::spawn(&cc.egui_ctx, "volumes", |tx, _| {
            tx.send(sr_platform::volumes())
        }));
        let dev = views::dev::DevState::new(&settings);
        let dup = views::dup::DupState::new(&settings);
        let mut app = Self {
            settings,
            settings_dirty: false,
            tab: Tab::SpaceMap,
            volumes: Vec::new(),
            volumes_job,
            scan: None,
            map: views::map::MapState::default(),
            drawer,
            drawer_path,
            drawer_open: false,
            drawer_gen: 0,
            staged_nodes: Vec::new(),
            protected,
            journal,
            staging: Vec::new(),
            exec: None,
            dev,
            dup,
            dialogs: Dialogs::default(),
            log,
            actions: Vec::new(),
            now: 0.0,
            screenshot: std::env::var_os("SPACERAZER_SCREENSHOT").map(|p| {
                let delay = std::env::var("SPACERAZER_SCREENSHOT_DELAY")
                    .ok()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(5.0);
                (PathBuf::from(p), delay, false)
            }),
        };
        crate::theme::install(&cc.egui_ctx);
        app.apply_theme(&cc.egui_ctx);
        // Dev aid: `SPACERAZER_TAB=dev|dup` opens that module and runs it on
        // the given folders instead of scanning them.
        match std::env::var("SPACERAZER_TAB").as_deref() {
            Ok("dev") if !initial_roots.is_empty() => {
                app.tab = Tab::DevSweep;
                app.dev.roots = initial_roots;
                views::dev::start(&mut app, &cc.egui_ctx);
            }
            Ok("dup") if !initial_roots.is_empty() => {
                app.tab = Tab::DuplicateLens;
                app.dup.roots = initial_roots;
                views::dup::start(&mut app, &cc.egui_ctx);
            }
            _ if !initial_roots.is_empty() => app.actions.push(Action::StartScan(initial_roots)),
            _ => {}
        }
        app
    }

    pub fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.settings.theme {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        });
        ctx.all_styles_mut(|s| {
            s.animation_time = if self.settings.reduce_motion {
                0.0
            } else {
                0.083
            };
        });
    }

    pub fn fmt(&self, bytes: u64) -> String {
        format_size(bytes, self.settings.size_units)
    }

    pub fn reduce_motion(&self) -> bool {
        self.settings.reduce_motion
    }

    // ----------------------------------------------------------------- jobs

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.volumes_job {
            if let Some(v) = job.drain(1).pop() {
                self.volumes = v;
                self.volumes_job = None;
            } else if job.is_finished() {
                self.volumes_job = None;
            }
        }

        if let Some(scan) = &mut self.scan {
            if scan.handle.is_done() && !scan.reported_done {
                scan.reported_done = true;
                let p = &scan.handle.progress;
                if let Some(msg) = p.panic.lock().ok().and_then(|g| g.clone()) {
                    self.log
                        .error(format!("Scan worker crashed: {msg}"), self.now);
                } else {
                    let files = p.files.load(std::sync::atomic::Ordering::Relaxed);
                    let errors = p.errors.load(std::sync::atomic::Ordering::Relaxed);
                    let what = if p.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        "Scan cancelled"
                    } else {
                        "Scan finished"
                    };
                    self.log.info(
                        format!(
                            "{what}: {files} files in {}{}",
                            crate::util::format_duration(p.elapsed()),
                            if errors > 0 {
                                format!(", {errors} issues")
                            } else {
                                String::new()
                            }
                        ),
                        self.now,
                    );
                }
                self.refresh_staged_flags();
                self.map.invalidate();
            } else if !scan.handle.is_done() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        // Staging snapshots.
        let mut changed = false;
        let mut items = Vec::new();
        for job in &self.staging {
            for m in job.drain(10_000) {
                items.push(m);
            }
            if let Some(p) = job.take_panic() {
                self.log.error(format!("Staging crashed: {p}"), self.now);
            }
        }
        for m in items {
            match m {
                StageMsg::Item(item) => match self.drawer.stage(item, &self.protected) {
                    Ok(()) => changed = true,
                    Err(e) => self.log.warn(format!("Not staged: {e}"), self.now),
                },
                StageMsg::Error(p, e) => self
                    .log
                    .warn(format!("Cannot stage {}: {e}", p.display()), self.now),
            }
        }
        self.staging.retain(|j| !j.is_finished());
        if changed {
            self.drawer_changed();
        }

        // Execution.
        let mut finished: Option<Report> = None;
        if let Some(exec) = &mut self.exec {
            for ev in exec.job.drain(10_000) {
                match ev {
                    ExecEvent::Started { total } => exec.total = total,
                    ExecEvent::Item { .. } => exec.done += 1,
                    ExecEvent::Finished(r) => finished = Some(r),
                }
            }
            if let Some(p) = exec.job.take_panic() {
                self.log.error(format!("Execution crashed: {p}"), self.now);
            }
            if finished.is_none() && exec.job.is_finished() {
                self.exec = None;
            }
        }
        if let Some(report) = finished {
            self.exec = None;
            self.finish_execution(report);
        }

        views::dev::poll(self);
        views::dup::poll(self);
    }

    fn finish_execution(&mut self, report: Report) {
        let executed = report.method != Method::DryRun;
        if executed {
            let done: Vec<PathBuf> = report
                .entries
                .iter()
                .filter(|e| matches!(e.outcome, sr_ops::Outcome::Done { .. }))
                .map(|e| e.path.clone())
                .collect();
            for p in &done {
                self.drawer.unstage(p);
            }
            if let Some(scan) = &self.scan {
                if let Ok(mut t) = scan.tree().write() {
                    for p in &done {
                        if let Some(id) = t.find_path(p) {
                            t.remove(id);
                        }
                    }
                }
            }
            views::dev::after_execution(self, &done);
            views::dup::after_execution(self, &done);
            self.drawer_changed();
            self.map.invalidate();
            self.log.info(
                format!(
                    "{}: {} succeeded, {} skipped, {} failed, {} freed",
                    method_label(report.method),
                    report.succeeded,
                    report.skipped,
                    report.failed,
                    self.fmt(report.bytes_freed)
                ),
                self.now,
            );
            if let Some(v) = self.scan.as_ref().and_then(|s| s.volume.clone()) {
                if is_cow_fs(&v.file_system) {
                    self.log.info(
                        "Note: on copy-on-write filesystems (APFS, Btrfs) cloned blocks may be shared; actual free-space change can be smaller than reported.",
                        self.now,
                    );
                }
            }
        }
        self.dialogs.report = Some(report);
    }

    /// Keep tree STAGED flags in sync with the drawer (FR-MAP-13) and
    /// persist the drawer (FR-TRASH-10).
    pub fn drawer_changed(&mut self) {
        self.drawer_gen += 1;
        self.refresh_staged_flags();
        if let Some(p) = &self.drawer_path {
            if let Err(e) = self.drawer.save(p) {
                self.log
                    .warn(format!("Could not save Trash Drawer: {e}"), self.now);
            }
        }
        self.map.invalidate();
    }

    fn refresh_staged_flags(&mut self) {
        let Some(scan) = &self.scan else { return };
        let Ok(mut t) = scan.tree().write() else {
            return;
        };
        for id in self.staged_nodes.drain(..) {
            if t.get(id).is_some() {
                t.node_mut(id).flags.remove(NodeFlags::STAGED);
            }
        }
        for item in self.drawer.items() {
            if let Some(id) = t.find_path(&item.path) {
                t.node_mut(id).flags.insert(NodeFlags::STAGED);
                self.staged_nodes.push(id);
            }
        }
    }

    // -------------------------------------------------------------- actions

    fn handle_actions(&mut self, ctx: &egui::Context) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            match a {
                Action::StartScan(roots) => self.start_scan(roots),
                Action::Rescan => {
                    if let Some(s) = &self.scan {
                        let roots = s.handle.options.roots.clone();
                        self.start_scan(roots);
                    }
                }
                Action::Stage {
                    paths,
                    source,
                    reason,
                } => self.stage(ctx, paths, source, reason),
                Action::Unstage(p) => {
                    if self.drawer.unstage(&p) {
                        self.drawer_changed();
                    }
                }
                Action::ClearDrawer => {
                    self.drawer.clear();
                    self.drawer_changed();
                }
                Action::Execute(method) => self.execute(ctx, method),
                Action::Reveal(p) => {
                    if let Err(e) = sr_platform::reveal(&p) {
                        self.log.error(format!("Reveal failed: {e}"), self.now);
                    }
                }
                Action::Open(p) => {
                    if let Err(e) = sr_platform::open(&p) {
                        self.log.error(format!("Open failed: {e}"), self.now);
                    }
                }
                Action::CopyPath(p) => ctx.copy_text(p.display().to_string()),
                Action::ExcludeFromScan(p) => {
                    if !self.settings.exclude_paths.contains(&p) {
                        self.settings.exclude_paths.push(p.clone());
                        self.settings_dirty = true;
                    }
                    if let Some(scan) = &self.scan {
                        if let Ok(mut t) = scan.tree().write() {
                            if let Some(id) = t.find_path(&p) {
                                t.remove(id);
                            }
                        }
                    }
                    self.map.invalidate();
                    self.log.info(
                        format!("Excluded {} from future scans", p.display()),
                        self.now,
                    );
                }
                Action::ShowInDevSweep(p) => {
                    self.tab = Tab::DevSweep;
                    views::dev::focus_path(self, &p);
                }
                Action::ShowInMap(p) => {
                    self.tab = Tab::SpaceMap;
                    let found = self
                        .scan
                        .as_ref()
                        .and_then(|s| s.tree().read().ok().and_then(|t| t.find_path(&p)));
                    match found {
                        Some(id) => self.map.reveal(self.scan.as_ref().unwrap(), id),
                        None => self
                            .log
                            .info("That path is not part of the current scan.", self.now),
                    }
                }
            }
        }
        if self.settings_dirty {
            self.settings_dirty = false;
            self.settings.clamp();
            self.protected = self.settings.protected();
            if let Err(e) = self.settings.save_default() {
                self.log
                    .error(format!("Could not save settings: {e}"), self.now);
            }
            self.apply_theme(ctx);
            self.map.invalidate();
        }
    }

    pub fn start_scan(&mut self, roots: Vec<PathBuf>) {
        if let Some(old) = self.scan.take() {
            old.handle.cancel();
        }
        let opts = self.settings.scan_options(roots.clone());
        match sr_scan::start_scan(opts) {
            Ok(handle) => {
                let volume = sr_platform::volume_for(&roots[0], &self.volumes);
                self.map = views::map::MapState::default();
                self.staged_nodes.clear();
                self.scan = Some(ScanState {
                    handle,
                    volume,
                    reported_done: false,
                });
                self.tab = Tab::SpaceMap;
            }
            Err(e) => self.log.error(format!("Cannot start scan: {e}"), self.now),
        }
    }

    fn stage(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, source: Module, reason: String) {
        let mut ok = Vec::new();
        for p in paths {
            if self.protected.is_protected(&p) {
                self.log.warn(
                    format!("{} is a protected path and cannot be staged", p.display()),
                    self.now,
                );
            } else if self.drawer.covers(&p) {
                self.log.info(
                    format!("{} is already in the Trash Drawer", p.display()),
                    self.now,
                );
            } else {
                ok.push(p);
            }
        }
        if ok.is_empty() {
            return;
        }
        // Snapshots walk directories; never on the UI thread.
        self.staging
            .push(Job::spawn(ctx, "stage", move |tx, cancel| {
                for p in ok {
                    if cancel.is_cancelled() {
                        return;
                    }
                    match sr_ops::snapshot(&p, source, reason.clone()) {
                        Ok(item) => tx.send(StageMsg::Item(item)),
                        Err(e) => tx.send(StageMsg::Error(p, e.to_string())),
                    }
                }
            }));
    }

    fn execute(&mut self, ctx: &egui::Context, method: Method) {
        if self.exec.is_some() || self.drawer.is_empty() {
            return;
        }
        let items: Vec<StagedItem> = self.drawer.items().to_vec();
        let protected = self.protected.clone();
        let journal = self.journal.as_ref().map(|j| j.path().to_path_buf());
        let total = items.len();
        let job = Job::spawn(ctx, "exec", {
            move |tx, cancel| {
                let journal = journal.map(Journal::open);
                let opts = sr_ops::ExecOptions {
                    method,
                    ..Default::default()
                };
                let tx2 = tx.clone();
                let report = sr_ops::execute(
                    &items,
                    &opts,
                    &protected,
                    journal.as_ref(),
                    &cancel,
                    &mut |ev| {
                        if !matches!(ev, ExecEvent::Finished(_)) {
                            tx2.send(ev)
                        }
                    },
                );
                tx.send(ExecEvent::Finished(report));
            }
        });
        self.exec = Some(ExecState {
            job,
            method,
            total,
            done: 0,
        });
    }

    // ------------------------------------------------------------ shortcuts

    fn global_shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let (m1, m2, m3, find, rescan, help, esc) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::Num1)),
                i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::Num2)),
                i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::Num3)),
                i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::F)),
                i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::R)),
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(t) if t == "?")),
                i.key_pressed(Key::Escape),
            )
        });
        if m1 {
            self.tab = Tab::SpaceMap;
        }
        if m2 {
            self.tab = Tab::DevSweep;
        }
        if m3 {
            self.tab = Tab::DuplicateLens;
        }
        if find {
            self.tab = Tab::SpaceMap;
            self.map.focus_search = true;
        }
        if rescan {
            self.actions.push(Action::Rescan);
        }
        if help && !ctx.egui_wants_keyboard_input() {
            self.dialogs.help = !self.dialogs.help;
        }
        if esc {
            // Esc closes a dialog first, otherwise cancels the running job.
            let d = &mut self.dialogs;
            if d.confirm.is_some() {
                d.confirm = None;
            } else if d.help
                || d.settings
                || d.journal
                || d.issues
                || d.log
                || d.report.is_some()
                || d.largest.is_some()
            {
                d.help = false;
                d.settings = false;
                d.journal = false;
                d.issues = false;
                d.log = false;
                d.report = None;
                d.largest = None;
            } else if let Some(e) = &self.exec {
                e.job.cancel.cancel();
            } else if let Some(s) = self.scan.as_ref().filter(|s| !s.handle.is_done()) {
                s.handle.cancel();
            } else {
                views::dev::cancel(self);
                views::dup::cancel(self);
            }
        }
    }

    // ------------------------------------------------------------------- ui

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::of(ui);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("SpaceRazer").display_bold(19.0).color(p.ink));
            ui.add_space(18.0);
            // Segmented module switcher.
            egui::Frame::new()
                .fill(p.mist)
                .corner_radius(8)
                .inner_margin(egui::Margin::same(3))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (tab, label, key) in [
                        (Tab::SpaceMap, "Space Map", "1"),
                        (Tab::DevSweep, "DevSweep", "2"),
                        (Tab::DuplicateLens, "DuplicateLens", "3"),
                    ] {
                        let r = theme::tab(ui, self.tab == tab, label);
                        if r.on_hover_text(format!("Ctrl/Cmd+{key}")).clicked() {
                            self.tab = tab;
                        }
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(theme::ghost(ui, "⚙"))
                    .on_hover_text("Settings")
                    .clicked()
                {
                    self.dialogs.settings = !self.dialogs.settings;
                }
                if ui
                    .add(theme::ghost(ui, "?"))
                    .on_hover_text("Keyboard shortcuts (?)")
                    .clicked()
                {
                    self.dialogs.help = !self.dialogs.help;
                }
                ui.menu_button("☰", |ui| {
                    ui.set_min_width(180.0);
                    if ui.button("Operation journal").clicked() {
                        self.dialogs.journal = true;
                        ui.close();
                    }
                    if ui.button("Quarantine").clicked() {
                        self.dialogs.quarantine = true;
                        ui.close();
                    }
                    if ui.button("Scan issues").clicked() {
                        self.dialogs.issues = true;
                        ui.close();
                    }
                    if ui.button("Log").clicked() {
                        self.dialogs.log = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Copy diagnostics").clicked() {
                        ui.ctx().copy_text(views::dialogs::diagnostics(self));
                        self.log.info("Diagnostics copied to clipboard", self.now);
                        ui.close();
                    }
                });
                if self.tab == Tab::SpaceMap && self.scan.is_some() {
                    ui.add_space(6.0);
                    views::map::search_box(self, ui);
                }
            });
        });
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let now = self.now;
        self.log.toasts.retain(|t| t.2 > now);
        if self.log.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -64.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                let p = theme::of(ui);
                for (msg, level, _) in self.log.toasts.iter().rev().take(4) {
                    let bar = match level {
                        Level::Info => p.accent,
                        Level::Warn => p.warn,
                        Level::Error => p.danger,
                    };
                    let r = egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin {
                            left: 14,
                            right: 14,
                            top: 10,
                            bottom: 10,
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(400.0);
                            ui.label(RichText::new(msg).color(p.ink));
                        })
                        .response;
                    // Level shown by a coloured edge, not by recolouring the text.
                    let edge =
                        egui::Rect::from_min_size(r.rect.min, egui::vec2(3.0, r.rect.height()));
                    ui.painter()
                        .rect_filled(edge.shrink2(egui::vec2(0.0, 6.0)), 2.0, bar);
                    ui.add_space(4.0);
                }
            });
    }

    fn dev_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, delay, requested)) = &mut self.screenshot else {
            return;
        };
        if !*requested {
            if self.now >= *delay {
                *requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            return;
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                if let Err(e) = buf.save(&*path) {
                    eprintln!("screenshot failed: {e}");
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let dirs: Vec<PathBuf> = dropped.into_iter().filter(|p| p.is_dir()).collect();
        if !dirs.is_empty() {
            self.actions.push(Action::StartScan(dirs));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.now = ctx.input(|i| i.time);
        self.poll_jobs(&ctx);
        self.global_shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        let pal = theme::of(ui);
        egui::Panel::top("tabs")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .fill(pal.paper)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("drawer")
            .resizable(self.drawer_open)
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .fill(pal.surface)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ui, |ui| views::drawer::panel(self, ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(18, 12)),
            )
            .show(ui, |ui| match self.tab {
                Tab::SpaceMap => {
                    if self.scan.is_some() {
                        views::map::view(self, ui);
                    } else {
                        views::home::view(self, ui);
                    }
                }
                Tab::DevSweep => views::dev::view(self, ui),
                Tab::DuplicateLens => views::dup::view(self, ui),
            });

        views::dialogs::show(self, &ctx);
        self.toasts(&ctx);
        self.handle_actions(&ctx);
        self.dev_screenshot(&ctx);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if let Some(p) = &self.drawer_path {
            let _ = self.drawer.save(p);
        }
    }
}

pub fn method_label(m: Method) -> &'static str {
    match m {
        Method::Trash => "Move to Trash",
        Method::Permanent => "Delete permanently",
        Method::DryRun => "Dry run",
    }
}

pub fn is_cow_fs(fs: &str) -> bool {
    matches!(
        fs.to_ascii_lowercase().as_str(),
        "apfs" | "btrfs" | "bcachefs" | "xfs" | "refs"
    )
}

pub fn path_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
