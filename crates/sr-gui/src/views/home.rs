//! Home screen: volumes and scan targets (FR-SCAN-01/02).

use egui::{RichText, Stroke};

use crate::app::{Action, App};
use crate::theme::{self, Typo};

pub fn view(app: &mut App, ui: &mut egui::Ui) {
    let p = theme::of(ui);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.set_max_width(780.0);
        ui.add_space(36.0);
        ui.label(RichText::new("Where did your space go?").display_bold(40.0).color(p.ink));
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Scan a disk or folder to see what fills it. Nothing is removed until you confirm it in the Trash Drawer.",
            )
            .size(15.0)
            .color(p.slate),
        );
        ui.add_space(22.0);

        ui.horizontal(|ui| {
            if let Some(home) = sr_platform::home_dir() {
                if ui
                    .add(theme::primary(ui, "Scan home folder").min_size(egui::vec2(0.0, 32.0)))
                    .on_hover_text(home.display().to_string())
                    .clicked()
                {
                    app.actions.push(Action::StartScan(vec![home]));
                }
            }
            if ui
                .add(egui::Button::new("Choose folders…").min_size(egui::vec2(0.0, 32.0)))
                .clicked()
            {
                if let Some(dirs) = rfd::FileDialog::new().pick_folders() {
                    if !dirs.is_empty() {
                        app.actions.push(Action::StartScan(dirs));
                    }
                }
            }
            ui.label(theme::muted(ui, "or drop a folder onto this window"));
        });

        ui.add_space(40.0);
        theme::heading(ui, "Disks");
        ui.add_space(6.0);
        if app.volumes_job.is_some() {
            ui.spinner();
            return;
        }
        let volumes = app.volumes.clone();
        for v in &volumes {
            let frac = if v.total > 0 { v.used() as f32 / v.total as f32 } else { 0.0 };
            let bar = if frac > 0.95 {
                p.danger
            } else if frac > 0.85 {
                p.warn
            } else {
                p.accent
            };
            let rect = ui.available_rect_before_wrap();
            ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.line));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(250.0);
                    let name = if v.name.is_empty() {
                        v.mount_point.display().to_string()
                    } else {
                        v.name.clone()
                    };
                    ui.label(RichText::new(name).semibold().size(15.0).color(p.ink));
                    let mut detail = format!("{}, {}", v.mount_point.display(), v.file_system);
                    if v.removable {
                        detail.push_str(", removable");
                    }
                    ui.label(RichText::new(detail).size(12.0).color(p.slate));
                });
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    theme::meter(ui, frac, 260.0, bar);
                    ui.label(
                        RichText::new(format!("{} free of {}", app.fmt(v.available), app.fmt(v.total)))
                            .size(12.0)
                            .color(p.slate),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Scan").clicked() {
                        app.actions.push(Action::StartScan(vec![v.mount_point.clone()]));
                    }
                    ui.label(RichText::new(format!("{:.0}%", frac * 100.0)).display(18.0).color(p.ink));
                });
            });
            ui.add_space(10.0);
        }
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.line));

        if cfg!(target_os = "macos") {
            ui.add_space(24.0);
            ui.label(
                RichText::new(
                    "Some folders (Mail, Messages, Safari) can only be measured after you grant SpaceRazer \
                     Full Disk Access in System Settings › Privacy & Security.",
                )
                .size(12.0)
                .color(p.slate),
            );
        }
    });
}
