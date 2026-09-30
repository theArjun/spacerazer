//! Trash Drawer bottom panel (SRS §3.3).

use egui::RichText;
use egui_extras::{Column, TableBuilder};
use sr_core::Module;
use sr_ops::Method;

use crate::app::{Action, App, is_cow_fs};
use crate::commands::Cmd;
use crate::theme::{self, Typo};
use crate::views::map::DragPath;

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    // Accept items dragged from the chart or list (FR-MAP-12).
    let rect = ui.max_rect();
    let drop = ui.interact(rect, egui::Id::new("drawer_drop"), egui::Sense::hover());
    if drop.dnd_hover_payload::<DragPath>().is_some() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            4.0,
            egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
            egui::StrokeKind::Inside,
        );
    }
    if let Some(p) = drop.dnd_release_payload::<DragPath>() {
        app.actions.push(Action::Stage {
            paths: vec![p.0.clone()],
            source: Module::SpaceMap,
            reason: "Dragged to Trash Drawer".into(),
        });
    }

    let count = app.drawer.len();
    let reclaim = app.drawer.reclaimable();
    let up_to = app
        .scan
        .as_ref()
        .and_then(|s| s.volume.as_ref())
        .is_some_and(|v| is_cow_fs(&v.file_system));

    ui.horizontal(|ui| {
        let pal = theme::of(ui);
        let arrow = if app.drawer_open { "⏷" } else { "⏶" };
        if ui.add(theme::ghost(ui, arrow)).on_hover_text("Show or hide staged items").clicked() {
            app.drawer_open = !app.drawer_open;
        }
        ui.label(RichText::new("Trash Drawer").semibold().color(pal.ink));
        ui.add_space(6.0);
        let figure = if count == 0 {
            String::new()
        } else if up_to {
            format!("up to {}", app.fmt(reclaim))
        } else {
            app.fmt(reclaim)
        };
        ui.label(RichText::new(figure).display_bold(20.0).color(pal.ink))
        .on_hover_text(
            "Nested items are counted once; hard-linked files whose other links survive free nothing. \
             On copy-on-write filesystems (APFS, Btrfs) cloned blocks may be shared, so the figure is an upper bound.",
        );
        ui.label(theme::muted(
            ui,
            match count {
                0 => "nothing staged".to_string(),
                1 => "reclaimable from 1 item".to_string(),
                n => format!("reclaimable from {n} items"),
            },
        ))
        .on_hover_text(
            "Nested items are counted once; hard-linked files whose other links survive free nothing. \
             On copy-on-write filesystems (APFS, Btrfs) cloned blocks may be shared, so the figure is an upper bound.",
        );
        if !app.staging.is_empty() {
            ui.spinner();
            ui.label(RichText::new("staging…").weak());
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(exec) = &app.exec {
                if ui.button("Cancel").on_hover_text("Stops between items").clicked() {
                    exec.job.cancel.cancel();
                }
                let frac = if exec.total > 0 { exec.done as f32 / exec.total as f32 } else { 0.0 };
                ui.add(egui::ProgressBar::new(frac).desired_width(200.0).text(format!(
                    "{} {}/{}",
                    crate::app::method_label(exec.method),
                    exec.done,
                    exec.total
                )));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            let enabled = count > 0 && app.staging.is_empty();
            if ui
                .add_enabled(enabled, theme::danger(ui, "Delete permanently…"))
                .on_hover_text(format!(
                    "Cannot be undone. Asks for confirmation. ({})",
                    Cmd::DeletePermanently.shortcut_text(ui.ctx()).unwrap_or_default()
                ))
                .clicked()
            {
                app.dialogs.confirm = Some(Method::Permanent);
                app.dialogs.confirm_text.clear();
            }
            if ui
                .add_enabled(enabled, theme::primary(ui, "Move to Trash"))
                .on_hover_text(format!(
                    "Recoverable: items go to the system trash, or to quarantine where there is none. ({})",
                    Cmd::MoveToTrash.shortcut_text(ui.ctx()).unwrap_or_default()
                ))
                .clicked()
            {
                app.dialogs.confirm = Some(Method::Trash);
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Dry run"))
                .on_hover_text(format!(
                    "See exactly what would happen, without changing anything. ({})",
                    Cmd::DryRun.shortcut_text(ui.ctx()).unwrap_or_default()
                ))
                .clicked()
            {
                app.actions.push(Action::Execute(Method::DryRun));
            }
            if ui.add_enabled(count > 0, egui::Button::new("Clear")).on_hover_text("Unstage everything (no filesystem effect)").clicked() {
                app.actions.push(Action::ClearDrawer);
            }
        });
    });

    if !app.drawer_open {
        return;
    }
    ui.separator();
    if count == 0 {
        ui.label(
            RichText::new("Nothing staged. Right-click an item, press Delete, or drag it here to stage it for review.")
                .weak(),
        );
        return;
    }
    let items = app.drawer.items().to_vec();
    TableBuilder::new(ui)
        .striped(true)
        .min_scrolled_height(80.0)
        .max_scroll_height(260.0)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::remainder().at_least(200.0).clip(true))
        .column(Column::auto().at_least(80.0))
        .column(Column::auto().at_least(90.0))
        .column(Column::auto().at_least(160.0).clip(true))
        .column(Column::exact(28.0))
        .header(20.0, |mut h| {
            for t in ["Path", "Size", "Source", "Reason", ""] {
                h.col(|ui| {
                    theme::column_header(ui, t);
                });
            }
        })
        .body(|body| {
            body.rows(22.0, items.len(), |mut row| {
                let it = &items[row.index()];
                row.col(|ui| {
                    let icon = match it.kind {
                        sr_core::NodeKind::Dir => "",
                        sr_core::NodeKind::Symlink => "↪ ",
                        _ => "",
                    };
                    ui.label(format!("{icon}{}", it.path.display()))
                        .context_menu(|ui| {
                            if ui.button("Reveal in file manager").clicked() {
                                app.actions.push(Action::Reveal(it.path.clone()));
                                ui.close();
                            }
                            if ui.button("Show in Space Map").clicked() {
                                app.actions.push(Action::ShowInMap(it.path.clone()));
                                ui.close();
                            }
                        });
                });
                row.col(|ui| {
                    let t = RichText::new(app.fmt(it.allocated));
                    if it.nlink > 1 && it.kind != sr_core::NodeKind::Dir {
                        ui.label(t.weak()).on_hover_text(format!(
                            "Hard-linked ({} links): frees space only if every link is removed",
                            it.nlink
                        ));
                    } else {
                        ui.label(t);
                    }
                });
                row.col(|ui| {
                    ui.label(it.source.label());
                });
                row.col(|ui| {
                    ui.label(RichText::new(&it.reason).weak());
                });
                row.col(|ui| {
                    if ui.small_button("×").on_hover_text("Unstage").clicked() {
                        app.actions.push(Action::Unstage(it.path.clone()));
                    }
                });
            });
        });
}
