// Release builds on Windows are GUI apps with no console window. The CLI
// ships separately as `spacerazer-cli.exe`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! SpaceRazer desktop application.
//!
//! `spacerazer` with no arguments (or with folder paths) opens the GUI;
//! `spacerazer <subcommand>` runs the headless CLI.

mod app;
mod commands;
mod sunburst;
mod theme;
mod util;
mod views;

use std::path::PathBuf;

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args
        .get(1)
        .and_then(|a| a.to_str())
        .is_some_and(|a| sr_cli::SUBCOMMANDS.contains(&a))
    {
        if let Err(e) = sr_cli::run_from(args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    let roots: Vec<PathBuf> = args[1..]
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SpaceRazer")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true)
            .with_icon(app_icon())
            // Dev screenshots need the window visible to be painted.
            .with_window_level(if std::env::var_os("SPACERAZER_SCREENSHOT").is_some() {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "SpaceRazer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, roots)))),
    ) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn app_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon-256.png"))
        .expect("bundled icon is a valid PNG")
        .to_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}
