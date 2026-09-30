//! Dialogs and auxiliary windows: execution confirmation and report,
//! journal, quarantine, scan issues, top files, log, help, settings.

use std::path::PathBuf;

use egui::{RichText, Stroke};
use egui_extras::{Column, TableBuilder};
use sr_cli::settings::Theme;
use sr_core::{Module, SizeMode, SizeUnits};
use sr_ops::{JournalRecord, Method, Outcome};

use crate::app::{Action, App};
use crate::theme::{self, Typo};
use crate::util::{Job, Level, format_date, group_digits};

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

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", group_digits(n as u64))
    }
}

// ------------------------------------------------------------- confirm

/// Confirmation before executing the drawer (FR-TRASH-06, UI-6).
fn confirm(app: &mut App, ctx: &egui::Context) {
    let Some(method) = app.dialogs.confirm else {
        return;
    };
    let items = app.drawer.items().to_vec();
    let bytes = app.drawer.reclaimable();
    let heavy = sr_ops::permanent_delete_threshold_exceeded(
        &items,
        app.settings.confirm_bytes_threshold,
        app.settings.confirm_count_threshold,
    );
    let permanent = method == Method::Permanent;
    let need_typed = permanent && heavy;
    let mut close = false;
    let mut go = false;

    theme::modal(ctx, "confirm_exec", 440.0, |ui| {
        let p = theme::of(ui);
        if permanent {
            theme::dialog_title(ui, "Delete permanently?", Some(p.danger));
        } else {
            theme::dialog_title(ui, "Move to Trash?", None);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(app.fmt(bytes))
                    .display_bold(34.0)
                    .color(p.ink),
            );
            ui.label(
                RichText::new(format!("from {}", plural(items.len(), "item", "items")))
                    .color(p.slate),
            );
        });

        // Breakdown by the module that staged each item.
        let mut by_source = std::collections::BTreeMap::<&str, (usize, u64)>::new();
        for it in &items {
            let e = by_source.entry(it.source.label()).or_default();
            e.0 += 1;
            e.1 += it.allocated;
        }
        ui.add_space(6.0);
        for (src, (c, b)) in by_source {
            ui.horizontal(|ui| {
                ui.label(RichText::new(src).color(p.slate));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(app.fmt(b)).color(p.ink));
                    ui.label(RichText::new(plural(c, "item", "items")).color(p.slate));
                });
            });
        }
        ui.add_space(12.0);

        if permanent {
            theme::callout(ui, p.danger, |ui| {
                ui.label(
                    RichText::new(
                        "Permanently deleted items skip the trash and cannot be recovered.",
                    )
                    .color(p.ink),
                );
            });
        } else {
            theme::callout(ui, p.accent, |ui| {
                ui.label(
                    RichText::new(
                        "Items go to the system trash, or to SpaceRazer's quarantine on disks without one. You can restore them from there.",
                    )
                    .color(p.ink),
                );
            });
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new("Each item is checked again just before removal. Anything that changed since you staged it is skipped.")
                .size(12.0)
                .color(p.slate),
        );

        if need_typed {
            ui.add_space(14.0);
            ui.label(
                RichText::new(format!(
                    "This is over your safety limit of {} or {}. Type DELETE to continue.",
                    app.fmt(app.settings.confirm_bytes_threshold),
                    plural(app.settings.confirm_count_threshold, "item", "items")
                ))
                .color(p.ink),
            );
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.dialogs.confirm_text)
                    .hint_text("DELETE")
                    .desired_width(f32::INFINITY),
            );
        }

        theme::footer(ui, |ui| {
            let ok = !need_typed || app.dialogs.confirm_text.trim() == "DELETE";
            let button = if permanent {
                theme::danger(ui, "Delete permanently")
            } else {
                theme::primary(ui, "Move to Trash")
            };
            if ui.add_enabled(ok, button).clicked() {
                go = true;
            }
            // The safe choice holds focus; destructive is never the default.
            let cancel = ui.button("Cancel");
            if !need_typed {
                cancel.request_focus();
            }
            if cancel.clicked() {
                close = true;
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

// -------------------------------------------------------------- report

/// Final report (FR-TRASH-05/08).
fn report(app: &mut App, ctx: &egui::Context) {
    let Some(r) = app.dialogs.report.clone() else {
        return;
    };
    let mut open = true;
    let dry = r.method == Method::DryRun;
    let (title, subtitle) = match r.method {
        Method::DryRun => ("Dry run", "Nothing was changed. This is what would happen."),
        Method::Trash => (
            "Moved to Trash",
            "Recoverable from the system trash or quarantine.",
        ),
        Method::Permanent => ("Deleted permanently", "These items are gone."),
    };
    theme::window(
        ctx,
        "report",
        title,
        Some(subtitle),
        640.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 32.0;
                theme::stat(
                    ui,
                    &app.fmt(r.bytes_freed),
                    if dry { "would be freed" } else { "freed" },
                    p.ink,
                );
                theme::stat(
                    ui,
                    &group_digits(r.succeeded as u64),
                    if dry { "ready" } else { "done" },
                    p.ok,
                );
                theme::stat(
                    ui,
                    &group_digits(r.skipped as u64),
                    "skipped",
                    if r.skipped > 0 { p.warn } else { p.slate },
                );
                theme::stat(
                    ui,
                    &group_digits(r.failed as u64),
                    "failed",
                    if r.failed > 0 { p.danger } else { p.slate },
                );
            });
            if r.cancelled {
                ui.add_space(8.0);
                theme::tag(ui, "Cancelled before every item was processed", p.warn);
            }
            ui.add_space(14.0);
            let row_h = 40.0;
            TableBuilder::new(ui)
                .striped(false)
                .max_scroll_height(360.0)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(16.0))
                .column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::auto().at_least(120.0))
                .body(|body| {
                    body.rows(row_h, r.entries.len(), |mut row| {
                        let e = &r.entries[row.index()];
                        let (color, detail, note) = match &e.outcome {
                            Outcome::Done {
                                bytes_freed,
                                destination,
                            } => (
                                p.ok,
                                app.fmt(*bytes_freed),
                                destination
                                    .as_ref()
                                    .map(|d| format!("Now at {}", d.display())),
                            ),
                            Outcome::WouldDo { operation, bytes } => {
                                (p.accent, app.fmt(*bytes), Some(operation.clone()))
                            }
                            Outcome::Skipped { reason } => {
                                (p.warn, "Skipped".into(), Some(reason.clone()))
                            }
                            Outcome::Failed { error } => {
                                (p.danger, "Failed".into(), Some(error.clone()))
                            }
                        };
                        row.col(|ui| theme::dot(ui, color));
                        row.col(|ui| {
                            ui.vertical(|ui| {
                                theme::path_label(ui, &e.path);
                                if let Some(n) = &note {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(n).size(12.0).color(p.slate),
                                        )
                                        .truncate(),
                                    );
                                }
                            });
                        });
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(RichText::new(detail).semibold().color(color));
                                },
                            );
                        });
                    });
                });
            if !dry && r.skipped + r.failed > 0 {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Skipped and failed items stay in the Trash Drawer.")
                        .size(12.0)
                        .color(p.slate),
                );
            }
            theme::footer(ui, |ui| {
                if ui.add(theme::primary(ui, "Done")).clicked() {
                    app.dialogs.report = None;
                }
            });
        },
    );
    if !open {
        app.dialogs.report = None;
    }
}

// ------------------------------------------------------------- journal

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
    let records = app.dialogs.journal_records.clone();
    let mut open = true;
    let mut restore: Option<JournalRecord> = None;
    let subtitle = app.journal.as_ref().map(|j| {
        format!(
            "Every change SpaceRazer made, newest first. Saved in {}",
            j.path().display()
        )
    });
    theme::window(
        ctx,
        "journal",
        "Operation journal",
        subtitle.as_deref(),
        760.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            match &records {
                None => {
                    ui.spinner();
                }
                Some(v) if v.is_empty() => {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No changes yet").display(18.0).color(p.ink));
                        ui.label(
                            RichText::new(
                                "Items you remove from the Trash Drawer are recorded here.",
                            )
                            .color(p.slate),
                        );
                    });
                    ui.add_space(20.0);
                }
                Some(v) => {
                    let rows: Vec<&JournalRecord> = v.iter().rev().take(2000).collect();
                    TableBuilder::new(ui)
                        .striped(true)
                        .max_scroll_height(440.0)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .column(Column::exact(16.0))
                        .column(Column::auto().at_least(86.0))
                        .column(Column::auto().at_least(110.0))
                        .column(Column::remainder().at_least(200.0).clip(true))
                        .column(Column::auto().at_least(70.0))
                        .column(Column::auto().at_least(70.0))
                        .header(22.0, |mut h| {
                            for t in ["", "Date", "Operation", "Item", "Size", ""] {
                                h.col(|ui| theme::column_header(ui, t));
                            }
                        })
                        .body(|body| {
                            body.rows(30.0, rows.len(), |mut row| {
                                let r = rows[row.index()];
                                row.col(|ui| {
                                    theme::dot(ui, if r.succeeded() { p.ok } else { p.danger })
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(format_date(r.timestamp)).color(p.slate),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(&r.operation);
                                });
                                row.col(|ui| {
                                    let resp = theme::path_label(ui, &r.path);
                                    if !r.succeeded() {
                                        resp.on_hover_text(
                                            r.error.clone().unwrap_or_else(|| r.result.clone()),
                                        );
                                    }
                                });
                                row.col(|ui| {
                                    ui.label(app.fmt(r.size));
                                });
                                row.col(|ui| {
                                    let restorable = r.succeeded()
                                        && r.destination.as_ref().is_some_and(|d| {
                                            d.components().any(|c| {
                                                c.as_os_str() == sr_ops::QUARANTINE_DIR_NAME
                                            })
                                        });
                                    if restorable
                                        && ui
                                            .button("Restore")
                                            .on_hover_text(
                                                "Move back from quarantine to its original place",
                                            )
                                            .clicked()
                                    {
                                        restore = Some(r.clone());
                                    }
                                });
                            });
                        });
                }
            }
        },
    );
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

// ---------------------------------------------------------- quarantine

fn quarantine(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.quarantine {
        return;
    }
    let mut open = true;
    let dirs = sr_ops::quarantine_dirs();
    theme::window(
        ctx,
        "quarantine",
        "Quarantine",
        Some(
            "On disks without a system trash, removed items are held here. Space is freed when you empty it.",
        ),
        600.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            if dirs.is_empty() {
                ui.label(RichText::new("Nothing is in quarantine.").color(p.slate));
            }
            for d in &dirs {
                ui.horizontal(|ui| {
                    theme::path_label(ui, d);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let confirming = app.dialogs.quarantine_confirm.as_ref() == Some(d);
                        if confirming {
                            if ui.add(theme::danger(ui, "Empty now")).clicked() {
                                match sr_ops::empty_quarantine(d) {
                                    Ok(b) => app.log.info(
                                        format!("Emptied quarantine, {} freed", app.fmt(b)),
                                        app.now,
                                    ),
                                    Err(e) => app
                                        .log
                                        .error(format!("Could not empty quarantine: {e}"), app.now),
                                }
                                app.dialogs.quarantine_confirm = None;
                            }
                            if ui.button("Keep").clicked() {
                                app.dialogs.quarantine_confirm = None;
                            }
                            ui.label(
                                RichText::new("Permanently delete everything here?")
                                    .color(p.danger),
                            );
                        } else {
                            if ui.add(theme::danger(ui, "Empty…")).clicked() {
                                app.dialogs.quarantine_confirm = Some(d.clone());
                            }
                            if ui.button("Reveal").clicked() {
                                app.actions.push(Action::Reveal(d.clone()));
                            }
                        }
                    });
                });
                ui.add_space(6.0);
            }
        },
    );
    if !open {
        app.dialogs.quarantine = false;
        app.dialogs.quarantine_confirm = None;
    }
}

// -------------------------------------------------------------- issues

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
    let subtitle = format!(
        "{} could not be read. Their sizes are not included.",
        plural(t.issues.len(), "location", "locations")
    );
    theme::window(
        ctx,
        "issues",
        "Unreadable locations",
        Some(&subtitle),
        680.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            let denied = t.issues.iter().any(|i| {
                let e = i.error.to_lowercase();
                e.contains("not permitted")
                    || e.contains("permission denied")
                    || e.contains("access is denied")
            });
            if denied && cfg!(target_os = "macos") {
                theme::callout(ui, p.warn, |ui| {
                    ui.label(
                    RichText::new(
                        "macOS blocked some folders. Grant SpaceRazer Full Disk Access in System Settings › Privacy & Security, then rescan.",
                    )
                    .color(p.ink),
                );
                });
                ui.add_space(10.0);
            }
            let rows: Vec<_> = t.issues.iter().take(5000).collect();
            TableBuilder::new(ui)
                .striped(true)
                .max_scroll_height(400.0)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(260.0).clip(true))
                .column(Column::auto().at_least(160.0).clip(true))
                .body(|body| {
                    body.rows(28.0, rows.len(), |mut row| {
                        let i = rows[row.index()];
                        row.col(|ui| {
                            theme::path_label(ui, &i.path);
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&i.error).size(12.0).color(p.slate));
                        });
                    });
                });
        },
    );
    if !open {
        app.dialogs.issues = false;
    }
}

// ------------------------------------------------------------- largest

fn largest(app: &mut App, ctx: &egui::Context) {
    let Some(ids) = app.dialogs.largest.clone() else {
        return;
    };
    let Some(scan) = &app.scan else { return };
    let tree = scan.tree().clone();
    let t = tree.read().unwrap_or_else(|e| e.into_inner());
    let rows: Vec<_> = ids
        .iter()
        .copied()
        .filter(|&id| !t.node(id).flags.contains(sr_core::NodeFlags::REMOVED))
        .collect();
    let mut open = true;
    theme::window(
        ctx,
        "largest",
        "Largest files",
        Some("The 100 biggest files in this scan. Right-click a row for more."),
        680.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            TableBuilder::new(ui)
                .striped(true)
                .max_scroll_height(480.0)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(34.0))
                .column(Column::auto().at_least(84.0))
                .column(Column::remainder().at_least(240.0).clip(true))
                .column(Column::auto().at_least(70.0))
                .body(|body| {
                    body.rows(30.0, rows.len(), |mut row| {
                        let i = row.index();
                        let id = rows[i];
                        let path = t.path(id);
                        row.col(|ui| {
                            ui.label(RichText::new(format!("{}", i + 1)).color(p.slate));
                        });
                        row.col(|ui| {
                            ui.label(
                                RichText::new(app.fmt(t.node(id).size(app.settings.size_mode)))
                                    .display(15.0)
                                    .color(p.ink),
                            );
                        });
                        row.col(|ui| {
                            theme::path_label(ui, &path).context_menu(|ui| {
                                if ui.button("Reveal in file manager").clicked() {
                                    app.actions.push(Action::Reveal(path.clone()));
                                    ui.close();
                                }
                                if ui.button("Show in chart").clicked() {
                                    app.actions.push(Action::ShowInMap(path.clone()));
                                    ui.close();
                                }
                            });
                        });
                        row.col(|ui| {
                            if app.drawer.covers(&path) {
                                ui.label(RichText::new("Staged").color(p.slate));
                            } else if ui.button("Stage").clicked() {
                                app.actions.push(Action::Stage {
                                    paths: vec![path.clone()],
                                    source: Module::SpaceMap,
                                    reason: "Large file".into(),
                                });
                            }
                        });
                    });
                });
        },
    );
    if !open {
        app.dialogs.largest = None;
    }
}

// ----------------------------------------------------------------- log

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
    let mut copy = false;
    theme::window(
        ctx,
        "log",
        "Activity log",
        Some("What SpaceRazer did this session. File contents are never logged."),
        640.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            if app.log.lines.is_empty() {
                ui.label(RichText::new("Nothing logged yet.").color(p.slate));
            }
            egui::ScrollArea::vertical()
                .max_height(400.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for l in &app.log.lines {
                        let color = match l.level {
                            Level::Info => p.accent,
                            Level::Warn => p.warn,
                            Level::Error => p.danger,
                        };
                        ui.horizontal(|ui| {
                            theme::dot(ui, color);
                            ui.label(RichText::new(format_date(l.at)).size(12.0).color(p.slate));
                            ui.add(egui::Label::new(RichText::new(&l.msg).color(p.ink)).wrap());
                        });
                    }
                });
            theme::footer(ui, |ui| {
                if ui
                    .button("Copy diagnostics")
                    .on_hover_text("Version, platform, settings and this log, for bug reports")
                    .clicked()
                {
                    copy = true;
                }
            });
        },
    );
    if copy {
        ctx.copy_text(diagnostics(app));
        app.log.info("Diagnostics copied to clipboard", app.now);
    }
    if !open {
        app.dialogs.log = false;
    }
}

// ---------------------------------------------------------------- help

fn help(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.help {
        return;
    }
    // Menu hotkeys come straight from the command table.
    let mut groups: Vec<(&str, Vec<(String, &str)>)> = crate::commands::MENUS
        .iter()
        .map(|(title, items)| {
            let rows = items
                .iter()
                .flatten()
                .filter_map(|c| c.shortcut_text(ctx).map(|k| (k, c.label())))
                .collect::<Vec<_>>();
            (*title, rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect();
    groups.push((
        "In the chart",
        vec![
            ("← →".into(), "Previous or next item in the ring"),
            ("↑".into(), "Move outward into the selected folder"),
            ("↓".into(), "Move inward to the parent"),
            ("Enter".into(), "Open the selected folder"),
            ("Backspace".into(), "Go up one level"),
            ("Esc".into(), "Close a dialog, or cancel the running job"),
        ],
    ));
    let mut open = true;
    theme::window(
        ctx,
        "help",
        "Keyboard shortcuts",
        Some("Every menu command, with its key."),
        620.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            egui::ScrollArea::vertical()
                .max_height(520.0)
                .show(ui, |ui| {
                    // Two columns of groups.
                    ui.columns(2, |cols| {
                        for (i, (title, rows)) in groups.iter().enumerate() {
                            let ui = &mut cols[i % 2];
                            ui.label(RichText::new(*title).semibold().color(p.slate));
                            ui.add_space(4.0);
                            for (keys, what) in rows {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(*what).color(p.ink));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            theme::keycap(ui, keys);
                                        },
                                    );
                                });
                            }
                            ui.add_space(14.0);
                        }
                    });
                });
        },
    );
    if !open {
        app.dialogs.help = false;
    }
}

// ------------------------------------------------------------ settings

const SECTIONS: [&str; 5] = [
    "Appearance",
    "Scanning",
    "Safety",
    "DevSweep",
    "DuplicateLens",
];

fn settings(app: &mut App, ctx: &egui::Context) {
    if !app.dialogs.settings {
        return;
    }
    let mut open = true;
    let before = format!("{:?}", app.settings);
    let stored = sr_cli::settings::Settings::default_path()
        .map(|p| format!("Saved automatically to {}", p.display()));
    theme::window(
        ctx,
        "settings",
        "Settings",
        stored.as_deref(),
        760.0,
        &mut open,
        |ui| {
            let p = theme::of(ui);
            ui.horizontal_top(|ui| {
                // Section list.
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    for (i, name) in SECTIONS.iter().enumerate() {
                        let sel = app.dialogs.settings_section == i;
                        let rt = RichText::new(*name).color(if sel { p.ink } else { p.slate });
                        let b = egui::Button::new(if sel { rt.semibold() } else { rt })
                            .fill(if sel {
                                p.mist
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .frame_when_inactive(sel)
                            .min_size(egui::vec2(150.0, 30.0));
                        if ui.add(b).clicked() {
                            app.dialogs.settings_section = i;
                        }
                    }
                });
                let r = ui.available_rect_before_wrap();
                ui.painter()
                    .vline(r.left(), r.y_range(), Stroke::new(1.0, p.line));
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical()
                        .max_height(480.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| settings_section(app, ui));
                });
            });
        },
    );
    if format!("{:?}", app.settings) != before {
        app.settings_dirty = true;
    }
    if !open {
        app.dialogs.settings = false;
    }
}

fn settings_section(app: &mut App, ui: &mut egui::Ui) {
    let s = &mut app.settings;
    match app.dialogs.settings_section {
        0 => {
            theme::setting_row(ui, "Theme", "", |ui| {
                theme::segmented(
                    ui,
                    &mut s.theme,
                    &[
                        (Theme::System, "System"),
                        (Theme::Light, "Light"),
                        (Theme::Dark, "Dark"),
                    ],
                );
            });
            theme::setting_row(
                ui,
                "Sizes",
                "On disk counts the space files really use",
                |ui| {
                    theme::segmented(
                        ui,
                        &mut s.size_mode,
                        &[
                            (SizeMode::Allocated, "On disk"),
                            (SizeMode::Apparent, "Apparent"),
                        ],
                    );
                },
            );
            theme::setting_row(ui, "Units", "", |ui| {
                theme::segmented(
                    ui,
                    &mut s.size_units,
                    &[(SizeUnits::Decimal, "GB"), (SizeUnits::Binary, "GiB")],
                );
            });
            theme::setting_row(
                ui,
                "Rings",
                "How many folder levels the chart shows",
                |ui| {
                    ui.add(egui::Slider::new(&mut s.rings, 2..=10));
                },
            );
            theme::setting_row(
                ui,
                "Smallest arc",
                "Items narrower than this are grouped together",
                |ui| {
                    ui.add(egui::Slider::new(&mut s.min_arc_degrees, 0.1..=5.0).suffix("°"));
                },
            );
            theme::setting_row(
                ui,
                "High-contrast chart",
                "Stronger colours for the sunburst",
                |ui| {
                    theme::toggle(ui, &mut s.high_contrast);
                },
            );
            theme::setting_row(ui, "Reduce motion", "Turn off zoom animations", |ui| {
                theme::toggle(ui, &mut s.reduce_motion);
            });
            theme::setting_row(ui, "Zoom animation", "", |ui| {
                ui.add_enabled(
                    !s.reduce_motion,
                    egui::Slider::new(&mut s.animation_ms, 0..=1000).suffix(" ms"),
                );
            });
        }
        1 => {
            theme::setting_row(
                ui,
                "Follow symbolic links",
                "Loops are detected and skipped",
                |ui| {
                    theme::toggle(ui, &mut s.follow_symlinks);
                },
            );
            theme::setting_row(
                ui,
                "Include other disks",
                "Enter mounted disks found inside a scan",
                |ui| {
                    theme::toggle(ui, &mut s.cross_filesystems);
                },
            );
            theme::setting_row(ui, "Scan threads", "0 uses every core", |ui| {
                let mut threads = s.scan_threads.unwrap_or(0);
                if ui.add(egui::Slider::new(&mut threads, 0..=64)).changed() {
                    s.scan_threads = (threads > 0).then_some(threads);
                }
            });
            list_heading(
                ui,
                "Skipped folders",
                "Never scanned, including everything inside",
            );
            path_list(ui, &mut s.exclude_paths);
            list_heading(
                ui,
                "Skipped patterns",
                "Names or paths matching these are skipped, e.g. *.iso",
            );
            string_list(ui, "exclude_globs", &mut s.exclude_globs, "*.iso");
        }
        2 => {
            theme::setting_row(
                ui,
                "Ask to type DELETE above",
                "For permanent deletion of this much data",
                |ui| {
                    let mut gb = s.confirm_bytes_threshold as f64 / 1e9;
                    if ui
                        .add(
                            egui::DragValue::new(&mut gb)
                                .range(0.0..=100_000.0)
                                .suffix(" GB"),
                        )
                        .changed()
                    {
                        s.confirm_bytes_threshold = (gb * 1e9) as u64;
                    }
                },
            );
            theme::setting_row(ui, "…or this many items", "", |ui| {
                ui.add(egui::DragValue::new(&mut s.confirm_count_threshold).range(1..=10_000_000));
            });
            list_heading(
                ui,
                "Protected folders",
                "Can never be staged. System folders, your home folder and disk roots are always protected.",
            );
            path_list(ui, &mut s.protected_paths);
        }
        3 => {
            theme::setting_row(
                ui,
                "Stale after",
                "Projects untouched this long count as stale",
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut s.dev_stale_days)
                            .range(1..=10_000)
                            .suffix(" days"),
                    );
                },
            );
            theme::setting_row(
                ui,
                "Use git",
                "Read last-commit dates and spot tracked build folders",
                |ui| {
                    theme::toggle(ui, &mut s.dev_check_git);
                },
            );
            theme::setting_row(
                ui,
                "Express mode",
                "Open the confirmation right after staging. You still review before anything is removed.",
                |ui| {
                    theme::toggle(ui, &mut s.dev_express_mode);
                },
            );
            list_heading(ui, "Pinned projects", "Never suggested for cleaning");
            path_list(ui, &mut s.dev_pinned);
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "{} custom rules. Add more under [[dev_custom_rules]] in settings.toml.",
                    s.dev_custom_rules.len()
                ))
                .size(12.0)
                .color(theme::of(ui).slate),
            );
        }
        _ => {
            theme::setting_row(
                ui,
                "Smallest file",
                "Files below this size are ignored",
                |ui| {
                    let mut mb = s.dup_min_size as f64 / 1_048_576.0;
                    if ui
                        .add(
                            egui::DragValue::new(&mut mb)
                                .range(0.000001..=100_000.0)
                                .suffix(" MiB"),
                        )
                        .changed()
                    {
                        s.dup_min_size = ((mb * 1_048_576.0) as u64).max(1);
                    }
                },
            );
            theme::setting_row(
                ui,
                "Byte-by-byte check",
                "Compare every byte of matches. Slower, fully certain.",
                |ui| {
                    theme::toggle(ui, &mut s.dup_paranoid);
                },
            );
            theme::setting_row(
                ui,
                "Hashing threads",
                "0 picks a number that suits the disk",
                |ui| {
                    let mut t = s.dup_io_threads.unwrap_or(0);
                    if ui.add(egui::Slider::new(&mut t, 0..=32)).changed() {
                        s.dup_io_threads = (t > 0).then_some(t);
                    }
                },
            );
            theme::setting_row(
                ui,
                "Image similarity",
                "Lower is stricter; 0 means visually identical",
                |ui| {
                    ui.add(egui::Slider::new(&mut s.dup_similarity_threshold, 0..=20));
                },
            );
            let cache = s.hash_cache.clone();
            theme::setting_row(
                ui,
                "Hash cache",
                "Remembers file hashes so rescans are faster",
                |ui| {
                    if let Some(p) = cache {
                        if ui.button("Clear").clicked() {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                },
            );
        }
    }
}

fn list_heading(ui: &mut egui::Ui, title: &str, help: &str) {
    let p = theme::of(ui);
    ui.add_space(16.0);
    ui.label(RichText::new(title).semibold().color(p.ink));
    ui.label(RichText::new(help).size(12.0).color(p.slate));
    ui.add_space(6.0);
}

fn path_list(ui: &mut egui::Ui, list: &mut Vec<PathBuf>) {
    let p = theme::of(ui);
    let mut remove = None;
    for (i, path) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            theme::path_label(ui, path);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(theme::ghost(ui, RichText::new("Remove").color(p.slate)))
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    if list.is_empty() {
        ui.label(RichText::new("None").color(p.slate));
    }
    if ui.button("Add folder…").clicked() {
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            list.push(p);
        }
    }
}

fn string_list(ui: &mut egui::Ui, key: &str, list: &mut Vec<String>, hint: &str) {
    let p = theme::of(ui);
    let id = ui.id().with(key);
    let mut remove = None;
    for (i, s) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(s).monospace().color(p.ink));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(theme::ghost(ui, RichText::new("Remove").color(p.slate)))
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    let mut draft: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut draft)
                .hint_text(hint)
                .desired_width(200.0),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("Add").clicked() || enter) && !draft.trim().is_empty() {
            list.push(draft.trim().to_string());
            draft.clear();
        }
    });
    ui.data_mut(|d| d.insert_temp(id, draft));
}
