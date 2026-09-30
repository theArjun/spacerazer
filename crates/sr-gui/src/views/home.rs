//! Home screen: volumes and scan targets (FR-SCAN-01/02).

use egui::RichText;

use crate::app::{Action, App};

pub fn view(app: &mut App, ui: &mut egui::Ui) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.heading(RichText::new("Where did my space go?").size(26.0));
        ui.label(RichText::new("Pick a volume or folder to scan. Nothing is changed until you confirm it in the Trash Drawer.").weak());
    });
    ui.add_space(20.0);

    ui.horizontal(|ui| {
        if let Some(home) = sr_platform::home_dir() {
            if ui
                .button(RichText::new(format!("Scan home folder ({})", home.display())).strong())
                .clicked()
            {
                app.actions.push(Action::StartScan(vec![home]));
            }
        }
        if ui.button("Choose folder(s)…").clicked() {
            if let Some(dirs) = rfd::FileDialog::new().pick_folders() {
                if !dirs.is_empty() {
                    app.actions.push(Action::StartScan(dirs));
                }
            }
        }
        ui.label(RichText::new("…or drop a folder onto this window.").weak());
    });
    ui.add_space(16.0);
    ui.heading("Volumes");
    if app.volumes_job.is_some() {
        ui.spinner();
        return;
    }
    let volumes = app.volumes.clone();
    egui::Grid::new("volumes")
        .striped(true)
        .num_columns(5)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            for v in &volumes {
                ui.vertical(|ui| {
                    ui.set_min_width(260.0);
                    ui.label(RichText::new(v.mount_point.display().to_string()).strong());
                    ui.label(
                        RichText::new(format!(
                            "{} · {}{}",
                            v.name,
                            v.file_system,
                            if v.removable { " · removable" } else { "" }
                        ))
                        .weak(),
                    );
                });
                let frac = if v.total > 0 {
                    v.used() as f32 / v.total as f32
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(220.0)
                        .text(format!("{:.0}% used", frac * 100.0)),
                );
                ui.label(format!("{} used", app.fmt(v.used())));
                ui.label(format!(
                    "{} free of {}",
                    app.fmt(v.available),
                    app.fmt(v.total)
                ));
                if ui.button("Scan").clicked() {
                    app.actions
                        .push(Action::StartScan(vec![v.mount_point.clone()]));
                }
                ui.end_row();
            }
        });
    if cfg!(target_os = "macos") {
        ui.add_space(16.0);
        ui.label(
            RichText::new(
                "Tip: some folders (Mail, Messages, Safari…) can only be measured after granting SpaceRazer \
                 Full Disk Access in System Settings → Privacy & Security.",
            )
            .weak(),
        );
    }
}
