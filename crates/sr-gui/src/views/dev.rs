//! DevSweep: developer build-artifact and cache cleaner (SRS §3.4).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use egui::{Color32, RichText};
use egui_extras::{Column, TableBuilder};
use sr_cli::settings::Settings;
use sr_core::{Module, now_secs};
use sr_devsweep::{
    DevEvent, DevOptions, DevReport, DockerUsage, Filter, GlobalCache, Project, Risk, RuleSet,
};

use crate::app::{Action, App};
use crate::theme::{self, Typo};
use crate::util::{Job, format_age_days, format_date};

pub enum DevMsg {
    Event(DevEvent),
    Done(DevReport),
}

pub struct CmdResult {
    argv: Vec<String>,
    output: String,
    ok: bool,
}

pub struct DevState {
    pub roots: Vec<PathBuf>,
    job: Option<Job<DevMsg>>,
    dirs_visited: u64,
    pub projects: Vec<Project>,
    errors: Vec<(PathBuf, String)>,
    caches: Vec<GlobalCache>,
    caches_job: Option<Job<Vec<GlobalCache>>>,
    docker: Option<Option<DockerUsage>>,
    docker_job: Option<Job<Option<DockerUsage>>>,
    type_filter: String,
    min_size_mb: f64,
    inactivity: Option<u64>,
    selected: HashSet<PathBuf>,
    expanded: HashSet<PathBuf>,
    focus: Option<PathBuf>,
    confirm_cmd: Option<(Vec<String>, PathBuf)>,
    cmd_job: Option<Job<CmdResult>>,
    cmd_result: Option<CmdResult>,
    pub express_pending: bool,
    analyzed_once: bool,
    show_empty: bool,
}

impl DevState {
    pub fn new(_settings: &Settings) -> Self {
        Self {
            roots: sr_platform::home_dir().into_iter().collect(),
            job: None,
            dirs_visited: 0,
            projects: Vec::new(),
            errors: Vec::new(),
            caches: Vec::new(),
            caches_job: None,
            docker: None,
            docker_job: None,
            type_filter: String::new(),
            min_size_mb: 0.0,
            inactivity: None,
            selected: HashSet::new(),
            expanded: HashSet::new(),
            focus: None,
            confirm_cmd: None,
            cmd_job: None,
            cmd_result: None,
            express_pending: false,
            analyzed_once: false,
            show_empty: false,
        }
    }

    fn running(&self) -> bool {
        self.job.is_some()
    }
}

pub fn start(app: &mut App, ctx: &egui::Context) {
    let s = &app.settings;
    let mut opts = DevOptions::new(app.dev.roots.clone());
    opts.rules = RuleSet::builtin().with_custom(s.dev_custom_rules.clone());
    opts.check_git = s.dev_check_git;
    opts.exclude = s.exclude_paths.clone();
    app.dev.projects.clear();
    app.dev.errors.clear();
    app.dev.selected.clear();
    app.dev.dirs_visited = 0;
    app.dev.analyzed_once = true;
    app.dev.job = Some(Job::spawn(ctx, "devsweep", move |tx, cancel| {
        let tx2 = tx.clone();
        let report = sr_devsweep::analyze(&opts, &cancel, &move |ev| tx2.send(DevMsg::Event(ev)));
        tx.send(DevMsg::Done(report));
    }));
    app.dev.caches_job = Some(Job::spawn(ctx, "caches", |tx, cancel| {
        tx.send(sr_devsweep::scan_global_caches(&cancel));
    }));
    app.dev.docker_job = Some(Job::spawn(ctx, "docker", |tx, _| {
        tx.send(sr_devsweep::docker_usage())
    }));
}

pub fn cancel(app: &mut App) {
    if let Some(j) = &app.dev.job {
        j.cancel.cancel();
    }
    if let Some(j) = &app.dev.caches_job {
        j.cancel.cancel();
    }
}

pub fn poll(app: &mut App) {
    let d = &mut app.dev;
    if let Some(job) = &d.job {
        let mut done = false;
        for m in job.drain(5000) {
            match m {
                DevMsg::Event(DevEvent::DirsVisited(n)) => d.dirs_visited = n,
                DevMsg::Event(DevEvent::ProjectFound(p)) => {
                    if !d.projects.iter().any(|x| x.path == p.path) {
                        d.projects.push(p);
                    }
                }
                DevMsg::Done(r) => {
                    d.projects = r.projects;
                    d.errors = r.errors;
                    done = true;
                }
            }
        }
        if let Some(p) = job.take_panic() {
            app.log
                .error(format!("DevSweep analysis crashed: {p}"), app.now);
            done = true;
        }
        if done || job.is_finished() {
            d.job = None;
            d.projects
                .sort_by_key(|p| std::cmp::Reverse(p.artifact_size));
        }
    }
    if let Some(job) = &d.caches_job {
        if let Some(c) = job.drain(1).pop() {
            d.caches = c;
            d.caches_job = None;
        } else if job.is_finished() {
            d.caches_job = None;
        }
    }
    if let Some(job) = &d.docker_job {
        if let Some(c) = job.drain(1).pop() {
            d.docker = Some(c);
            d.docker_job = None;
        } else if job.is_finished() {
            d.docker_job = None;
        }
    }
    if let Some(job) = &d.cmd_job {
        if let Some(r) = job.drain(1).pop() {
            d.cmd_result = Some(r);
            d.cmd_job = None;
        } else if job.is_finished() {
            d.cmd_job = None;
        }
    }
    // Express mode (FR-DEV-12): once staging completes, go straight to the
    // confirmation dialog, which summarises the pending operations.
    if app.dev.express_pending && app.staging.is_empty() {
        app.dev.express_pending = false;
        if !app.drawer.is_empty() {
            app.dialogs.confirm = Some(sr_ops::Method::Trash);
        }
    }
}

pub fn focus_path(app: &mut App, path: &Path) {
    app.dev.focus = Some(path.to_path_buf());
    if !app
        .dev
        .projects
        .iter()
        .any(|p| p.path == path || path.starts_with(&p.path))
        && !app.dev.running()
    {
        app.dev.roots = vec![path.to_path_buf()];
    }
}

pub fn after_execution(app: &mut App, done: &[PathBuf]) {
    let set: HashSet<&PathBuf> = done.iter().collect();
    for p in &mut app.dev.projects {
        p.artifacts.retain(|a| !set.contains(&a.path));
        p.artifact_size = p.artifacts.iter().map(|a| a.allocated).sum();
    }
    app.dev.caches.retain(|c| !set.contains(&c.path));
    app.dev.selected.retain(|p| !set.contains(p));
}

pub fn risk_color(ui: &egui::Ui, r: Risk) -> Color32 {
    let p = theme::of(ui);
    match r {
        Risk::Safe => p.ok,
        Risk::Caution => p.warn,
        Risk::Review => p.danger,
    }
}

pub fn risk_tag(ui: &mut egui::Ui, r: Risk, reason: &str) {
    let c = risk_color(ui, r);
    let resp = theme::tag(ui, r.label(), c);
    resp.on_hover_ui(|ui| {
        ui.label(RichText::new(r.label()).semibold());
        ui.label(r.explanation());
        if !reason.is_empty() {
            ui.label(reason);
        }
    });
}

pub fn view(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let now = now_secs();

    ui.horizontal_wrapped(|ui| {
        ui.heading("DevSweep");
        ui.label(RichText::new("Finds build outputs and caches that tools can regenerate.").weak());
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Roots:");
        let mut remove = None;
        for (i, r) in app.dev.roots.iter().enumerate() {
            ui.label(RichText::new(r.display().to_string()).monospace());
            if ui.small_button("×").clicked() {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            app.dev.roots.remove(i);
        }
        if ui.button("Add folder…").clicked() {
            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                app.dev.roots.push(p);
            }
        }
        ui.separator();
        if app.dev.running() {
            ui.spinner();
            ui.label(format!(
                "{} dirs, {} projects",
                app.dev.dirs_visited,
                app.dev.projects.len()
            ));
            if ui.button("Cancel").clicked() {
                cancel(app);
            }
        } else if ui
            .add_enabled(!app.dev.roots.is_empty(), theme::primary(ui, "Analyze"))
            .clicked()
        {
            start(app, &ctx);
        }
    });

    if !app.dev.analyzed_once {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label("Choose one or more folders containing your projects, then press Analyze.");
            ui.label(RichText::new("Nothing is deleted from here: selected items go to the Trash Drawer for review.").weak());
        });
        return;
    }

    // Filters (FR-DEV-07).
    ui.horizontal_wrapped(|ui| {
        ui.label("Filter:");
        ui.add(egui::TextEdit::singleline(&mut app.dev.type_filter).hint_text("type, e.g. Rust").desired_width(110.0));
        ui.add(egui::DragValue::new(&mut app.dev.min_size_mb).range(0.0..=1_000_000.0).suffix(" MB min"));
        egui::ComboBox::from_id_salt("inactivity")
            .selected_text(match app.dev.inactivity {
                None => "Any activity".to_string(),
                Some(d) => format!("Untouched > {d} days"),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.dev.inactivity, None, "Any activity");
                for d in [30, 90, 180, 365] {
                    ui.selectable_value(&mut app.dev.inactivity, Some(d), format!("Untouched > {d} days"));
                }
                let custom = app.settings.dev_stale_days;
                ui.selectable_value(&mut app.dev.inactivity, Some(custom), format!("Custom: > {custom} days (Settings)"));
            });
        ui.separator();
        let threshold = app.dev.inactivity.unwrap_or(app.settings.dev_stale_days);
        if ui
            .button(format!("Select all stale (> {threshold} d)"))
            .on_hover_text("Selects artifacts of every project inactive past the threshold, except pinned projects and items tagged Review.")
            .clicked()
        {
            for a in sr_devsweep::select_stale(&app.dev.projects, threshold, &app.settings.dev_pinned, now) {
                app.dev.selected.insert(a.path.clone());
            }
        }
        if ui.button("Select none").clicked() {
            app.dev.selected.clear();
        }
        ui.checkbox(&mut app.dev.show_empty, "Show projects with nothing to clean");
    });

    let filter = Filter {
        types: if app.dev.type_filter.trim().is_empty() {
            Vec::new()
        } else {
            vec![app.dev.type_filter.trim().to_string()]
        },
        min_artifact_size: (app.dev.min_size_mb * 1_000_000.0) as u64,
        inactive_days: app.dev.inactivity,
    };
    let visible: Vec<Project> = sr_devsweep::filter_projects(&app.dev.projects, &filter, now)
        .into_iter()
        .filter(|p| {
            app.dev
                .focus
                .as_ref()
                .is_none_or(|f| p.path.starts_with(f) || f.starts_with(&p.path))
        })
        .filter(|p| app.dev.show_empty || !p.artifacts.is_empty())
        .cloned()
        .collect();

    // Selection summary + clean action (FR-DEV-12).
    let sel_size: u64 = app
        .dev
        .projects
        .iter()
        .flat_map(|p| &p.artifacts)
        .filter(|a| app.dev.selected.contains(&a.path))
        .map(|a| a.allocated)
        .sum::<u64>()
        + app
            .dev
            .caches
            .iter()
            .filter(|c| app.dev.selected.contains(&c.path))
            .map(|c| c.allocated)
            .sum::<u64>();
    ui.horizontal_wrapped(|ui| {
        let total: u64 = visible.iter().map(|p| p.artifact_size).sum();
        ui.label(format!(
            "{} projects, {} in artifacts",
            visible.len(),
            app.fmt(total)
        ));
        if let Some(f) = app.dev.focus.clone() {
            ui.label(RichText::new(format!("showing {}", f.display())).italics());
            if ui.small_button("show all").clicked() {
                app.dev.focus = None;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if app.settings.dev_express_mode {
                "Clean selected (express)"
            } else {
                "Send to Trash Drawer"
            };
            if ui
                .add_enabled(!app.dev.selected.is_empty(), theme::primary(ui, label))
                .clicked()
            {
                let paths: Vec<PathBuf> = app.dev.selected.iter().cloned().collect();
                app.actions.push(Action::Stage {
                    paths,
                    source: Module::DevSweep,
                    reason: "Regenerable build artifact / cache".into(),
                });
                app.drawer_open = true;
                if app.settings.dev_express_mode {
                    app.dev.express_pending = true;
                }
                app.dev.selected.clear();
            }
            ui.label(format!(
                "{} selected, {}",
                app.dev.selected.len(),
                app.fmt(sel_size)
            ));
        });
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            projects_table(app, ui, &visible, now);
            ui.add_space(12.0);
            caches_section(app, ui);
            ui.add_space(12.0);
            docker_section(app, ui);
            if !app.dev.errors.is_empty() {
                egui::CollapsingHeader::new(format!("{} errors", app.dev.errors.len())).show(
                    ui,
                    |ui| {
                        for (p, e) in app.dev.errors.iter().take(500) {
                            ui.label(format!("{}: {e}", p.display()));
                        }
                    },
                );
            }
        });

    command_dialogs(app, &ctx);
}

fn projects_table(app: &mut App, ui: &mut egui::Ui, visible: &[Project], now: i64) {
    ui.push_id("projects", |ui| {
        TableBuilder::new(ui)
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(24.0))
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(60.0))
            .header(20.0, |mut h| {
                for t in ["", "Project", "Type", "Artifacts", "Source", "Last activity", "Last commit", ""] {
                    h.col(|ui| {
                        theme::column_header(ui, t);
                    });
                }
            })
            .body(|mut body| {
                for p in visible {
                    let pinned = app.settings.dev_pinned.iter().any(|x| p.path.starts_with(x));
                    let all_sel = !p.artifacts.is_empty() && p.artifacts.iter().all(|a| app.dev.selected.contains(&a.path));
                    body.row(24.0, |mut row| {
                        row.col(|ui| {
                            let mut v = all_sel;
                            if ui.add_enabled(!pinned && !p.artifacts.is_empty(), egui::Checkbox::without_text(&mut v)).changed() {
                                for a in &p.artifacts {
                                    if v {
                                        app.dev.selected.insert(a.path.clone());
                                    } else {
                                        app.dev.selected.remove(&a.path);
                                    }
                                }
                            }
                        });
                        row.col(|ui| {
                            let open = app.dev.expanded.contains(&p.path);
                            if ui.small_button(if open { "⏷" } else { "⏵" }).clicked() {
                                if open {
                                    app.dev.expanded.remove(&p.path);
                                } else {
                                    app.dev.expanded.insert(p.path.clone());
                                }
                            }
                            let r = ui.label(RichText::new(&p.name).semibold());
                            r.on_hover_text(p.path.display().to_string()).context_menu(|ui| {
                                if ui.button("Reveal in file manager").clicked() {
                                    app.actions.push(Action::Reveal(p.path.clone()));
                                    ui.close();
                                }
                                if ui.button("Show in Space Map").clicked() {
                                    app.actions.push(Action::ShowInMap(p.path.clone()));
                                    ui.close();
                                }
                            });
                            if pinned {
                                ui.label("📌");
                            }
                        });
                        row.col(|ui| {
                            ui.label(p.types.join(", "));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(app.fmt(p.artifact_size)).semibold());
                        });
                        row.col(|ui| {
                            ui.label(app.fmt(p.source_size));
                        });
                        row.col(|ui| {
                            let days = p.inactive_days(now);
                            let stale = p.is_stale(app.dev.inactivity.unwrap_or(app.settings.dev_stale_days), now);
                            let t = RichText::new(format_age_days(days));
                            ui.label(if stale { t.color(risk_color(ui, Risk::Caution)) } else { t })
                                .on_hover_text(format!(
                                    "Last source change {} (build outputs and VCS metadata excluded)",
                                    format_date(p.last_source_mtime)
                                ));
                        });
                        row.col(|ui| {
                            ui.label(p.last_commit.map(format_date).unwrap_or_else(|| "—".into()));
                        });
                        row.col(|ui| {
                            ui.menu_button("…", |ui| {
                                let label = if pinned { "Unpin" } else { "Pin (never suggest cleaning)" };
                                if ui.button(label).clicked() {
                                    if pinned {
                                        app.settings.dev_pinned.retain(|x| !p.path.starts_with(x));
                                    } else {
                                        app.settings.dev_pinned.push(p.path.clone());
                                        for a in &p.artifacts {
                                            app.dev.selected.remove(&a.path);
                                        }
                                    }
                                    app.settings_dirty = true;
                                    ui.close();
                                }
                                if let Some(cmd) = &p.clean_command {
                                    if ui
                                        .button(format!("Run `{}`…", cmd.join(" ")))
                                        .on_hover_text("Use the tool's own clean command instead of deleting files (FR-DEV-11)")
                                        .clicked()
                                    {
                                        app.dev.confirm_cmd = Some((cmd.clone(), p.path.clone()));
                                        ui.close();
                                    }
                                }
                            });
                        });
                    });
                    if app.dev.expanded.contains(&p.path) {
                        for a in &p.artifacts {
                            body.row(22.0, |mut row| {
                                row.col(|ui| {
                                    let mut v = app.dev.selected.contains(&a.path);
                                    if ui.add_enabled(!pinned, egui::Checkbox::without_text(&mut v)).changed() {
                                        if v {
                                            app.dev.selected.insert(a.path.clone());
                                        } else {
                                            app.dev.selected.remove(&a.path);
                                        }
                                    }
                                });
                                row.col(|ui| {
                                    ui.add_space(24.0);
                                    let rel = a.path.strip_prefix(&p.path).unwrap_or(&a.path);
                                    ui.label(RichText::new(rel.display().to_string()).monospace())
                                        .on_hover_text(a.path.display().to_string());
                                });
                                row.col(|ui| {
                                    risk_tag(ui, a.risk, &a.risk_reason);
                                });
                                row.col(|ui| {
                                    ui.label(app.fmt(a.allocated));
                                });
                                row.col(|ui| {
                                    ui.label(format!("{} items", a.entries));
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(format!("↻ {}", a.regenerate)).weak())
                                        .on_hover_text("How to regenerate this artifact");
                                });
                                row.col(|ui| {
                                    ui.label(&a.ecosystem);
                                });
                                row.col(|_| {});
                            });
                        }
                    }
                }
            });
    });
}

fn caches_section(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("Global caches");
        if app.dev.caches_job.is_some() {
            ui.spinner();
        }
    });
    if app.dev.caches.is_empty() && app.dev.caches_job.is_none() {
        ui.label(RichText::new("No known toolchain caches found.").weak());
    }
    let mut caches = app.dev.caches.clone();
    caches.sort_by_key(|c| std::cmp::Reverse(c.allocated));
    for c in &caches {
        ui.horizontal(|ui| {
            let mut v = app.dev.selected.contains(&c.path);
            if ui.checkbox(&mut v, "").changed() {
                if v {
                    app.dev.selected.insert(c.path.clone());
                } else {
                    app.dev.selected.remove(&c.path);
                }
            }
            ui.label(RichText::new(&c.name).semibold());
            ui.label(app.fmt(c.allocated));
            risk_tag(ui, c.risk, &c.hint);
            ui.label(
                RichText::new(c.path.display().to_string())
                    .monospace()
                    .weak(),
            );
            if !c.hint.is_empty() {
                ui.label(RichText::new(&c.hint).weak());
            }
        });
    }
}

fn docker_section(app: &mut App, ui: &mut egui::Ui) {
    // FR-DEV-04: Docker data is only ever cleaned with Docker's own prune
    // commands. Hidden entirely when Docker is not available.
    let Some(Some(usage)) = app.dev.docker.clone() else {
        return;
    };
    ui.heading("Docker");
    egui::Grid::new("docker").striped(true).show(ui, |ui| {
        for h in ["Type", "Total", "Active", "Size", "Reclaimable", ""] {
            theme::column_header(ui, h);
        }
        ui.end_row();
        for row in &usage.rows {
            ui.label(&row.kind);
            ui.label(&row.total);
            ui.label(&row.active);
            ui.label(&row.size);
            ui.label(&row.reclaimable);
            if let Some(cmd) = sr_devsweep::docker_prune_command(&row.kind) {
                if ui.button(format!("{}…", cmd.join(" "))).clicked() {
                    let cwd = sr_platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
                    app.dev.confirm_cmd = Some((cmd, cwd));
                }
            } else {
                ui.label("");
            }
            ui.end_row();
        }
    });
}

fn command_dialogs(app: &mut App, ctx: &egui::Context) {
    if let Some((argv, cwd)) = app.dev.confirm_cmd.clone() {
        let mut close = false;
        egui::Modal::new(egui::Id::new("confirm_cmd")).show(ctx, |ui| {
            ui.heading("Run clean command?");
            ui.label("This runs the tool's own command. It is not staged in the Trash Drawer and cannot be undone from SpaceRazer.");
            ui.label(RichText::new(argv.join(" ")).monospace());
            ui.label(format!("in {}", cwd.display()));
            ui.horizontal(|ui| {
                let cancel = ui.button("Cancel");
                cancel.request_focus();
                if cancel.clicked() {
                    close = true;
                }
                if ui.add(theme::danger(ui, "Run")).clicked() {
                    let (a, c) = (argv.clone(), cwd.clone());
                    app.dev.cmd_job = Some(Job::spawn(ctx, "cmd", move |tx, _| {
                        let r = match sr_devsweep::run_command(&a, &c) {
                            Ok(out) => CmdResult {
                                ok: out.status.success(),
                                output: format!(
                                    "{}{}",
                                    String::from_utf8_lossy(&out.stdout),
                                    String::from_utf8_lossy(&out.stderr)
                                ),
                                argv: a,
                            },
                            Err(e) => CmdResult {
                                ok: false,
                                output: e.to_string(),
                                argv: a,
                            },
                        };
                        tx.send(r);
                    }));
                    close = true;
                }
            });
        });
        if close {
            app.dev.confirm_cmd = None;
        }
    }
    if app.dev.cmd_job.is_some() {
        egui::Modal::new(egui::Id::new("cmd_running")).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Running command…");
            });
        });
    }
    let mut close = false;
    if let Some(r) = &app.dev.cmd_result {
        egui::Window::new("Command output")
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(r.argv.join(" ")).monospace());
                ui.label(if r.ok {
                    "Finished successfully."
                } else {
                    "Command failed."
                });
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(&r.output).monospace());
                    });
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
    }
    if close {
        app.dev.cmd_result = None;
        // Sizes changed on disk; the user can re-analyze.
        app.log.info("Re-run Analyze to refresh sizes.", app.now);
    }
}
