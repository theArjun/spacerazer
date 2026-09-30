//! Dialogs and auxiliary windows: execution confirmation and report,
//! journal, quarantine, scan issues, top files, log, help, settings.

use std::path::PathBuf;

use egui::RichText;
use sr_cli::settings::Theme;
use sr_core::{Module, SizeMode, SizeUnits};
use sr_ops::{JournalRecord, Method, Outcome};

use crate::app::{Action, App, method_label};
use crate::theme::{self, Typo};
use crate::util::{Job, Level, format_date};

pub fn show(app: &mut App, ctx: &egui::Context) {
    confirm(app, ctx);
    report(app, ctx);
    journal(app, ctx);
    quarantine(app, ctx);
    issues(app, ctx);
    largest(app, ctx);
    log(app, ctx);
    help(app, ctx);
    settings(app, ctx);
}

/// Confirmation before executing the drawer (FR-TRASH-06, UI-6).
fn confirm(app: &mut App, ctx: &egui::Context) {
    let Some(method) = app.dialogs.confirm else {
        return;
    };
    let items = app.drawer.items().to_vec();
    let n = items.len();
    let bytes = app.drawer.reclaimable();
    let heavy = sr_ops::permanent_delete_threshold_exceeded(
        &items,
        app.settings.confirm_bytes_threshold,
        app.settings.confirm_count_threshold,
    );
    let mut close = false;
    let mut go = false;
    egui::Modal::new(egui::Id::new("confirm_exec")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        match method {
            Method::Permanent => {
                ui.label(RichText::new("Delete permanently?").display_bold(22.0).color(theme::of(ui).danger));
                ui.label(format!("{n} items, {} will be deleted. This cannot be undone.", app.fmt(bytes)));
            }
            _ => {
                ui.label(RichText::new("Move to Trash?").display_bold(22.0));
                ui.label(format!("{n} items, {} will be moved to the trash.", app.fmt(bytes)));
                ui.label(RichText::new("You can restore them from the system trash (or SpaceRazer's quarantine).").weak());
            }
        }
        let mut by_source = std::collections::BTreeMap::<&str, (usize, u64)>::new();
        for it in &items {
            let e = by_source.entry(it.source.label()).or_default();
            e.0 += 1;
            e.1 += it.allocated;
        }
        for (src, (c, b)) in by_source {
            ui.label(RichText::new(format!("  {src}: {c} items, {}", app.fmt(b))).weak());
        }
        ui.label(RichText::new("Each item is re-checked just before it is removed; anything that changed since staging is skipped.").small().weak());
        let need_typed = method == Method::Permanent && heavy;
        if need_typed {
            ui.add_space(6.0);
            ui.label(format!(
                "This exceeds your safety threshold ({} or {} items). Type DELETE to confirm:",
                app.fmt(app.settings.confirm_bytes_threshold),
                app.settings.confirm_count_threshold
            ));
            ui.text_edit_singleline(&mut app.dialogs.confirm_text);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // The safe choice holds focus; destructive is never the default.
            let cancel = ui.button("Cancel");
            if !need_typed {
                cancel.request_focus();
            }
            if cancel.clicked() {
                close = true;
            }
            let ok = !need_typed || app.dialogs.confirm_text.trim() == "DELETE";
            let button = match method {
                Method::Permanent => theme::danger(ui, "Delete permanently"),
                _ => theme::primary(ui, "Move to Trash"),
            };
            if ui.add_enabled(ok, button).clicked() {
                go = true;
            }
        });
    });
    if go {
        app.actions.push(Action::Execute(method));
        close = true;
    }
    if close {
        app.dialogs.confirm = None;
        app.dialogs.confirm_text.clear();
    }
}

/// Final report (FR-TRASH-05/08).
fn report(app: &mut App, ctx: &egui::Context) {
    let Some(r) = &app.dialogs.report else { return };
    let mut open = true;
    let title = if r.method == Method::DryRun {
        "Dry run report"
    } else {
        "Execution report"
    };
    egui::Window::new(title)
        .open(&mut open)
        .default_width(640.0)
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(method_label(r.method)).semibold());
                if r.method == Method::DryRun {
                    ui.label(format!(
                        "{} operations, {} would be freed, nothing was changed",
                        r.succeeded,
                        app.fmt(r.bytes_freed)
                    ));
                } else {
                    ui.label(format!(
                        "{} succeeded, {} skipped, {} failed, {} freed",
                        r.succeeded,
                        r.skipped,
                        r.failed,
                        app.fmt(r.bytes_freed)
                    ));
                }
                if r.cancelled {
                    ui.label(RichText::new("cancelled").italics());
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    egui::Grid::new("report")
                        .striped(true)
                        .num_columns(2)
                        .show(ui, |ui| {
                            for e in &r.entries {
                                let (txt, color) = match &e.outcome {
                                    Outcome::Done {
                                        bytes_freed,
                                        destination,
                                    } => (
                                        match destination {
                                            Some(d) => format!(
                                                "✔ {} → {}",
                                                app.fmt(*bytes_freed),
                                                d.display()
                                            ),
                                            None => format!("✔ {}", app.fmt(*bytes_freed)),
                                        },
                                        theme::of(ui).ok,
                                    ),
                                    Outcome::WouldDo { operation, bytes } => (
                                        format!("{operation} ({})", app.fmt(*bytes)),
                                        ui.visuals().text_color(),
                                    ),
                                    Outcome::Skipped { reason } => {
                                        (format!("skipped: {reason}"), theme::of(ui).warn)
                                    }
                                    Outcome::Failed { error } => {
                                        (format!("failed: {error}"), ui.visuals().error_fg_color)
                                    }
                                };
                                ui.label(e.path.display().to_string());
                                ui.label(RichText::new(txt).color(color));
                                ui.end_row();
                            }
                        });
                });
            if r.method != Method::DryRun && r.skipped + r.failed > 0 {
                ui.label(
                    RichText::new("Skipped and failed items stay in the Trash Drawer.").weak(),
                );
            }
        });
    if !open {
        app.dialogs.report = None;
    }
}

fn journal(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.journal {
        return;
    }
    // Read the journal on a background thread once per opening.
    if app.dialogs.journal_records.is_none() {
        match &app.dialogs.journal_job {
            None => {
                let path = app.journal.as_ref().map(|j| j.path().to_path_buf());
                app.dialogs.journal_job = Some(Job::spawn(ctx, "journal", move |tx, _| {
                    let recs = path
                        .map(|p| sr_ops::Journal::open(p).read_all().unwrap_or_default())
                        .unwrap_or_default();
                    tx.send(recs);
                }));
            }
            Some(job) => {
                if let Some(recs) = job.drain(1).pop() {
                    app.dialogs.journal_records = Some(recs);
                    app.dialogs.journal_job = None;
                }
            }
        }
    }
    let loaded = app.dialogs.journal_records.as_ref();
    let mut open = true;
    let mut restore: Option<JournalRecord> = None;
    egui::Window::new("Operation journal")
        .open(&mut open)
        .default_width(720.0)
        .show(ctx, |ui| {
            if let Some(j) = &app.journal {
                ui.label(
                    RichText::new(j.path().display().to_string())
                        .weak()
                        .monospace(),
                );
            }
            match loaded {
                None => {
                    ui.spinner();
                }
                Some(v) if v.is_empty() => {
                    ui.label("No operations recorded yet.");
                }
                Some(v) => {
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            egui::Grid::new("journal")
                                .striped(true)
                                .num_columns(6)
                                .show(ui, |ui| {
                                    for h in ["When", "Operation", "Path", "Size", "Result", ""] {
                                        theme::column_header(ui, h);
                                    }
                                    ui.end_row();
                                    for r in v.iter().rev().take(2000) {
                                        ui.label(format_date(r.timestamp));
                                        ui.label(&r.operation);
                                        ui.label(r.path.display().to_string()).on_hover_text(
                                            r.destination
                                                .as_ref()
                                                .map(|d| format!("→ {}", d.display()))
                                                .unwrap_or_default(),
                                        );
                                        ui.label(app.fmt(r.size));
                                        if r.succeeded() {
                                            ui.label("ok");
                                        } else {
                                            ui.label(
                                                RichText::new(
                                                    r.error
                                                        .clone()
                                                        .unwrap_or_else(|| r.result.clone()),
                                                )
                                                .color(ui.visuals().error_fg_color),
                                            );
                                        }
                                        let restorable = r.succeeded()
                                            && r.destination.as_ref().is_some_and(|d| {
                                                d.components().any(|c| {
                                                    c.as_os_str() == sr_ops::QUARANTINE_DIR_NAME
                                                })
                                            });
                                        if restorable
                                            && ui
                                                .small_button("Restore")
                                                .on_hover_text("Move back from quarantine")
                                                .clicked()
                                        {
                                            restore = Some(r.clone());
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                }
            }
        });
    if let Some(r) = restore {
        match sr_ops::restore_from_quarantine(&r) {
            Ok(()) => {
                app.log
                    .info(format!("Restored {}", r.path.display()), app.now);
                app.dialogs.journal_records = None;
            }
            Err(e) => app.log.error(format!("Restore failed: {e}"), app.now),
        }
    }
    if !open {
        app.dialogs.journal = false;
        app.dialogs.journal_records = None;
    }
}

fn quarantine(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.quarantine {
        return;
    }
    let mut open = true;
    let dirs = sr_ops::quarantine_dirs();
    egui::Window::new("Quarantine").open(&mut open).show(ctx, |ui| {
        ui.label("Where no OS trash is available, SpaceRazer moves items into a quarantine folder on the same volume. Space is only freed once the quarantine is emptied.");
        if dirs.is_empty() {
            ui.label(RichText::new("No quarantine folders exist.").weak());
        }
        for d in &dirs {
            ui.horizontal(|ui| {
                ui.label(RichText::new(d.display().to_string()).monospace());
                if ui.button("Reveal").clicked() {
                    app.actions.push(Action::Reveal(d.clone()));
                }
                if ui
                    .add(theme::danger(ui, "Empty"))
                    .on_hover_text("Permanently deletes everything in this quarantine folder")
                    .clicked()
                {
                    match sr_ops::empty_quarantine(d) {
                        Ok(b) => app.log.info(format!("Emptied quarantine, {} freed", app.fmt(b)), app.now),
                        Err(e) => app.log.error(format!("Could not empty quarantine: {e}"), app.now),
                    }
                }
            });
        }
    });
    if !open {
        app.dialogs.quarantine = false;
    }
}

fn issues(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.issues {
        return;
    }
    let Some(scan) = &app.scan else {
        app.dialogs.issues = false;
        return;
    };
    let tree = scan.tree().clone();
    let t = tree.read().unwrap_or_else(|e| e.into_inner());
    let mut open = true;
    egui::Window::new(format!("Scan issues ({})", t.issues.len()))
        .open(&mut open)
        .default_width(640.0)
        .show(ctx, |ui| {
            let denied = t.issues.iter().any(|i| {
                let e = i.error.to_lowercase();
                e.contains("not permitted") || e.contains("permission denied") || e.contains("access is denied")
            });
            if denied {
                ui.label(if cfg!(target_os = "macos") {
                    "Some locations could not be read. On macOS, grant SpaceRazer Full Disk Access (System Settings → Privacy & Security) and rescan."
                } else {
                    "Some locations could not be read due to permissions. Their size is not included."
                });
                ui.separator();
            }
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                for i in t.issues.iter().take(5000) {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(i.path.display().to_string()).monospace());
                        ui.label(RichText::new(&i.error).weak());
                    });
                }
            });
        });
    if !open {
        app.dialogs.issues = false;
    }
}

fn largest(app: &mut App, ctx: &egui::Context) {
    let Some(ids) = app.dialogs.largest.clone() else {
        return;
    };
    let Some(scan) = &app.scan else { return };
    let tree = scan.tree().clone();
    let t = tree.read().unwrap_or_else(|e| e.into_inner());
    let mut open = true;
    egui::Window::new("Top 100 largest files")
        .open(&mut open)
        .default_width(640.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(480.0)
                .show(ui, |ui| {
                    egui::Grid::new("largest")
                        .striped(true)
                        .num_columns(4)
                        .show(ui, |ui| {
                            for (i, &id) in ids.iter().enumerate() {
                                let node = t.node(id);
                                if node.flags.contains(sr_core::NodeFlags::REMOVED) {
                                    continue;
                                }
                                let path = t.path(id);
                                ui.label(format!("{}.", i + 1));
                                ui.label(app.fmt(node.size(app.settings.size_mode)));
                                ui.label(path.display().to_string()).context_menu(|ui| {
                                    if ui.button("Reveal in file manager").clicked() {
                                        app.actions.push(Action::Reveal(path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Show in chart").clicked() {
                                        app.actions.push(Action::ShowInMap(path.clone()));
                                        ui.close();
                                    }
                                });
                                let staged = app.drawer.covers(&path);
                                if ui
                                    .add_enabled(!staged, egui::Button::new("Stage").small())
                                    .clicked()
                                {
                                    app.actions.push(Action::Stage {
                                        paths: vec![path.clone()],
                                        source: Module::SpaceMap,
                                        reason: "Large file".into(),
                                    });
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    if !open {
        app.dialogs.largest = None;
    }
}

pub fn diagnostics(app: &App) -> String {
    let mut s = format!(
        "SpaceRazer {}\nOS: {} {}\nSettings: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        settings_summary(app)
    );
    if let Some(scan) = &app.scan {
        let p = &scan.handle.progress;
        s.push_str(&format!(
            "Scan: {} files, {} dirs, {} errors, done={}\n",
            p.files.load(std::sync::atomic::Ordering::Relaxed),
            p.dirs.load(std::sync::atomic::Ordering::Relaxed),
            p.errors.load(std::sync::atomic::Ordering::Relaxed),
            p.is_done()
        ));
    }
    s.push_str("Log:\n");
    for l in app.log.lines.iter().rev().take(200).rev() {
        s.push_str(&format!("{} [{:?}] {}\n", l.at, l.level, l.msg));
    }
    s
}

fn settings_summary(app: &App) -> String {
    let st = &app.settings;
    format!(
        "rings={} size_mode={:?} units={:?} follow_symlinks={} cross_fs={} exclusions={}",
        st.rings,
        st.size_mode,
        st.size_units,
        st.follow_symlinks,
        st.cross_filesystems,
        st.exclude_paths.len() + st.exclude_globs.len()
    )
}

fn log(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.log {
        return;
    }
    let mut open = true;
    egui::Window::new("Log")
        .open(&mut open)
        .default_width(640.0)
        .show(ctx, |ui| {
            if ui.button("Copy diagnostics").clicked() {
                ctx.copy_text(diagnostics(app));
            }
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for l in &app.log.lines {
                        let color = match l.level {
                            Level::Info => ui.visuals().text_color(),
                            Level::Warn => theme::of(ui).warn,
                            Level::Error => ui.visuals().error_fg_color,
                        };
                        ui.label(
                            RichText::new(format!("{}  {}", format_date(l.at), l.msg)).color(color),
                        );
                    }
                });
        });
    if !open {
        app.dialogs.log = false;
    }
}

fn help(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.help {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard shortcuts")
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            egui::Grid::new("keys").striped(true).show(ui, |ui| {
                for (k, v) in [
                    ("Ctrl/Cmd + 1/2/3", "Switch module"),
                    (
                        "Arrow keys",
                        "Move selection between arcs (←/→ siblings, ↑ outward, ↓ inward)",
                    ),
                    ("Enter", "Zoom into selected directory"),
                    ("Backspace", "Zoom out one level"),
                    ("Delete", "Stage selection in Trash Drawer"),
                    ("Ctrl/Cmd + F", "Search"),
                    ("Ctrl/Cmd + R", "Rescan current root"),
                    ("Esc", "Close dialog / cancel running job"),
                    ("?", "This overlay"),
                ] {
                    ui.label(RichText::new(k).monospace().semibold());
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
    if !open {
        app.dialogs.help = false;
    }
}

fn settings(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.settings {
        return;
    }
    let mut open = true;
    let mut changed = false;
    let before = format!("{:?}", app.settings);
    egui::Window::new("Settings").open(&mut open).default_width(520.0).vscroll(true).show(ctx, |ui| {
        let s = &mut app.settings;
        egui::CollapsingHeader::new("Appearance").default_open(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Theme");
                ui.selectable_value(&mut s.theme, Theme::System, "System");
                ui.selectable_value(&mut s.theme, Theme::Light, "Light");
                ui.selectable_value(&mut s.theme, Theme::Dark, "Dark");
            });
            ui.checkbox(&mut s.high_contrast, "High-contrast chart palette");
            ui.checkbox(&mut s.reduce_motion, "Reduce motion (disable zoom animations)");
            ui.add(egui::Slider::new(&mut s.animation_ms, 0..=1000).text("zoom animation (ms)"));
            ui.add(egui::Slider::new(&mut s.rings, 2..=10).text("rings"));
            ui.add(egui::Slider::new(&mut s.min_arc_degrees, 0.1..=5.0).text("min arc angle (°)"));
            ui.horizontal(|ui| {
                ui.label("Sizes");
                ui.selectable_value(&mut s.size_mode, SizeMode::Allocated, "On disk (allocated)");
                ui.selectable_value(&mut s.size_mode, SizeMode::Apparent, "Apparent");
            });
            ui.horizontal(|ui| {
                ui.label("Units");
                ui.selectable_value(&mut s.size_units, SizeUnits::Binary, "Binary (GiB)");
                ui.selectable_value(&mut s.size_units, SizeUnits::Decimal, "Decimal (GB)");
            });
        });
        egui::CollapsingHeader::new("Scanning").default_open(true).show(ui, |ui| {
            ui.checkbox(&mut s.follow_symlinks, "Follow symlinks / junctions (with cycle detection)");
            ui.checkbox(&mut s.cross_filesystems, "Cross filesystem boundaries");
            let mut threads = s.scan_threads.unwrap_or(0);
            if ui.add(egui::Slider::new(&mut threads, 0..=64).text("scan threads (0 = auto)")).changed() {
                s.scan_threads = (threads > 0).then_some(threads);
            }
            path_list(ui, "Excluded paths", &mut s.exclude_paths);
            string_list(ui, "Excluded glob patterns", &mut s.exclude_globs, "*.iso");
        });
        egui::CollapsingHeader::new("Safety").default_open(true).show(ui, |ui| {
            let mut gb = s.confirm_bytes_threshold as f64 / 1e9;
            if ui.add(egui::DragValue::new(&mut gb).range(0.0..=100_000.0).suffix(" GB")).on_hover_text("Typed confirmation above this size").changed() {
                s.confirm_bytes_threshold = (gb * 1e9) as u64;
            }
            ui.add(egui::DragValue::new(&mut s.confirm_count_threshold).range(1..=10_000_000).suffix(" items"));
            ui.label(RichText::new("Built-in protected paths (system folders, home root, volume roots) are always enforced.").weak());
            path_list(ui, "Additional protected paths", &mut s.protected_paths);
        });
        egui::CollapsingHeader::new("DevSweep").show(ui, |ui| {
            ui.add(egui::DragValue::new(&mut s.dev_stale_days).range(1..=10_000).prefix("Stale after ").suffix(" days"));
            ui.checkbox(&mut s.dev_check_git, "Use git for last-commit date and tracked-artifact check");
            ui.checkbox(&mut s.dev_express_mode, "Express mode: go straight to confirmation after cleaning")
                .on_hover_text("Selected artifacts are still staged and summarised before anything is removed.");
            path_list(ui, "Pinned projects", &mut s.dev_pinned);
            ui.label(format!("{} custom rules (edit settings.toml → [[dev_custom_rules]])", s.dev_custom_rules.len()));
        });
        egui::CollapsingHeader::new("DuplicateLens").show(ui, |ui| {
            let mut mb = s.dup_min_size as f64 / 1_048_576.0;
            if ui.add(egui::DragValue::new(&mut mb).range(0.000001..=100_000.0).prefix("Default min size ").suffix(" MiB")).changed() {
                s.dup_min_size = ((mb * 1_048_576.0) as u64).max(1);
            }
            ui.checkbox(&mut s.dup_paranoid, "Paranoid byte-by-byte verification by default");
            let mut t = s.dup_io_threads.unwrap_or(0);
            if ui.add(egui::Slider::new(&mut t, 0..=32).text("hashing threads (0 = auto by device type)")).changed() {
                s.dup_io_threads = (t > 0).then_some(t);
            }
            ui.add(egui::Slider::new(&mut s.dup_similarity_threshold, 0..=20).text("default similarity distance"));
            ui.horizontal(|ui| {
                ui.label("Hash cache:");
                ui.label(RichText::new(s.hash_cache.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "disabled".into())).monospace());
            });
            if let Some(p) = s.hash_cache.clone() {
                if ui.button("Clear hash cache").clicked() {
                    let _ = std::fs::remove_file(p);
                }
            }
        });
        if let Some(p) = sr_cli::settings::Settings::default_path() {
            ui.separator();
            ui.label(RichText::new(format!("Stored in {}", p.display())).weak().small());
        }
    });
    if format!("{:?}", app.settings) != before {
        changed = true;
    }
    if changed {
        app.settings_dirty = true;
    }
    if !open {
        app.dialogs.settings = false;
    }
}

fn path_list(ui: &mut egui::Ui, label: &str, list: &mut Vec<PathBuf>) {
    ui.label(label);
    let mut remove = None;
    for (i, p) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(p.display().to_string()).monospace());
            if ui.small_button("×").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    if ui.small_button("Add folder…").clicked() {
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            list.push(p);
        }
    }
}

fn string_list(ui: &mut egui::Ui, label: &str, list: &mut Vec<String>, hint: &str) {
    ui.label(label);
    let id = ui.id().with(label);
    let mut remove = None;
    for (i, p) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(p).monospace());
            if ui.small_button("×").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    let mut draft: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut draft)
                .hint_text(hint)
                .desired_width(160.0),
        );
        if ui.small_button("Add").clicked() && !draft.trim().is_empty() {
            list.push(draft.trim().to_string());
            draft.clear();
        }
    });
    ui.data_mut(|d| d.insert_temp(id, draft));
}
