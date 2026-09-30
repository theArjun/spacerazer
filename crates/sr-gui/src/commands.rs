//! Every app command in one table: its menu, label and hotkey. The menu bar,
//! global hotkeys and the shortcuts overlay are all built from this table,
//! so they cannot drift apart.

use egui::{Key, KeyboardShortcut, Modifiers};
use sr_core::SizeMode;
use sr_ops::Method;

use crate::app::{Action, App, Tab};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    // File
    NewScan,
    ScanHome,
    ChooseFolders,
    Rescan,
    ExportJson,
    ExportCsv,
    ExportSvg,
    Quit,
    // Edit
    Find,
    StageSelection,
    Settings,
    // View
    SpaceMap,
    DevSweep,
    DuplicateLens,
    LargestFiles,
    UnreadableLocations,
    ToggleSizeMode,
    ToggleDrawer,
    // Trash
    DryRun,
    MoveToTrash,
    DeletePermanently,
    ClearDrawer,
    // Tools
    Journal,
    Quarantine,
    Log,
    CopyDiagnostics,
    // Help
    Shortcuts,
}

pub const MENUS: &[(&str, &[Option<Cmd>])] = &[
    (
        "File",
        &[
            Some(Cmd::NewScan),
            Some(Cmd::ScanHome),
            Some(Cmd::ChooseFolders),
            Some(Cmd::Rescan),
            None,
            Some(Cmd::ExportJson),
            Some(Cmd::ExportCsv),
            Some(Cmd::ExportSvg),
            None,
            Some(Cmd::Quit),
        ],
    ),
    (
        "Edit",
        &[
            Some(Cmd::Find),
            Some(Cmd::StageSelection),
            None,
            Some(Cmd::Settings),
        ],
    ),
    (
        "View",
        &[
            Some(Cmd::SpaceMap),
            Some(Cmd::DevSweep),
            Some(Cmd::DuplicateLens),
            None,
            Some(Cmd::LargestFiles),
            Some(Cmd::UnreadableLocations),
            None,
            Some(Cmd::ToggleSizeMode),
            Some(Cmd::ToggleDrawer),
        ],
    ),
    (
        "Trash",
        &[
            Some(Cmd::DryRun),
            Some(Cmd::MoveToTrash),
            Some(Cmd::DeletePermanently),
            None,
            Some(Cmd::ClearDrawer),
        ],
    ),
    (
        "Tools",
        &[
            Some(Cmd::Journal),
            Some(Cmd::Quarantine),
            Some(Cmd::Log),
            None,
            Some(Cmd::CopyDiagnostics),
        ],
    ),
    ("Help", &[Some(Cmd::Shortcuts)]),
];

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};
const CMD_ALT: Modifiers = Modifiers {
    alt: true,
    ..Modifiers::COMMAND
};

impl Cmd {
    pub fn label(self) -> &'static str {
        match self {
            Cmd::NewScan => "New scan",
            Cmd::ScanHome => "Scan home folder",
            Cmd::ChooseFolders => "Scan folders…",
            Cmd::Rescan => "Rescan",
            Cmd::ExportJson => "Export summary as JSON…",
            Cmd::ExportCsv => "Export summary as CSV…",
            Cmd::ExportSvg => "Export chart as SVG…",
            Cmd::Quit => "Quit SpaceRazer",
            Cmd::Find => "Search",
            Cmd::StageSelection => "Send selection to Trash Drawer",
            Cmd::Settings => "Settings…",
            Cmd::SpaceMap => "Space Map",
            Cmd::DevSweep => "DevSweep",
            Cmd::DuplicateLens => "DuplicateLens",
            Cmd::LargestFiles => "Largest files",
            Cmd::UnreadableLocations => "Unreadable locations",
            Cmd::ToggleSizeMode => "Switch on-disk / apparent sizes",
            Cmd::ToggleDrawer => "Show Trash Drawer items",
            Cmd::DryRun => "Dry run",
            Cmd::MoveToTrash => "Move to Trash…",
            Cmd::DeletePermanently => "Delete permanently…",
            Cmd::ClearDrawer => "Clear Trash Drawer",
            Cmd::Journal => "Operation journal",
            Cmd::Quarantine => "Quarantine",
            Cmd::Log => "Activity log",
            Cmd::CopyDiagnostics => "Copy diagnostics",
            Cmd::Shortcuts => "Keyboard shortcuts",
        }
    }

    pub fn shortcut(self) -> Option<KeyboardShortcut> {
        let s = |m, k| Some(KeyboardShortcut::new(m, k));
        match self {
            Cmd::NewScan => s(CMD, Key::N),
            Cmd::ScanHome => s(CMD_SHIFT, Key::H),
            Cmd::ChooseFolders => s(CMD, Key::O),
            Cmd::Rescan => s(CMD, Key::R),
            Cmd::ExportJson => s(CMD, Key::E),
            Cmd::Quit => s(CMD, Key::Q),
            Cmd::Find => s(CMD, Key::F),
            Cmd::StageSelection => s(Modifiers::NONE, Key::Delete),
            Cmd::Settings => s(CMD, Key::Comma),
            Cmd::SpaceMap => s(CMD, Key::Num1),
            Cmd::DevSweep => s(CMD, Key::Num2),
            Cmd::DuplicateLens => s(CMD, Key::Num3),
            Cmd::LargestFiles => s(CMD, Key::T),
            Cmd::UnreadableLocations => s(CMD, Key::I),
            Cmd::ToggleSizeMode => s(CMD_SHIFT, Key::A),
            Cmd::ToggleDrawer => s(CMD, Key::D),
            Cmd::DryRun => s(CMD_SHIFT, Key::D),
            // Finder's conventions.
            Cmd::MoveToTrash => s(CMD, Key::Backspace),
            Cmd::DeletePermanently => s(CMD_ALT, Key::Backspace),
            Cmd::ClearDrawer => s(CMD_SHIFT, Key::Backspace),
            Cmd::Journal => s(CMD, Key::J),
            Cmd::Log => s(CMD, Key::L),
            Cmd::ExportCsv
            | Cmd::ExportSvg
            | Cmd::Quarantine
            | Cmd::CopyDiagnostics
            | Cmd::Shortcuts => None,
        }
    }

    /// Text shown for the hotkey in menus and the overlay.
    pub fn shortcut_text(self, ctx: &egui::Context) -> Option<String> {
        match self {
            Cmd::Shortcuts => Some("?".into()),
            Cmd::StageSelection => Some("Delete".into()),
            _ => self.shortcut().map(|s| ctx.format_shortcut(&s)),
        }
    }

    /// Commands whose hotkey would clash with editing text (Backspace,
    /// Delete) are ignored while a text field has focus.
    fn allowed_while_typing(self) -> bool {
        !matches!(
            self,
            Cmd::StageSelection | Cmd::MoveToTrash | Cmd::DeletePermanently | Cmd::ClearDrawer
        )
    }

    pub fn enabled(self, app: &App) -> bool {
        let scanned = app.scan.as_ref().is_some_and(|s| s.handle.is_done());
        let drawer_ready = !app.drawer.is_empty() && app.exec.is_none() && app.staging.is_empty();
        match self {
            Cmd::NewScan => app.scan.is_some(),
            Cmd::Rescan => app.scan.is_some(),
            Cmd::ExportJson | Cmd::ExportCsv | Cmd::ExportSvg | Cmd::LargestFiles | Cmd::Find => {
                scanned
            }
            Cmd::UnreadableLocations => scanned,
            Cmd::StageSelection => app.tab == Tab::SpaceMap && app.map.selected.is_some(),
            Cmd::DryRun | Cmd::MoveToTrash | Cmd::DeletePermanently => drawer_ready,
            Cmd::ClearDrawer => !app.drawer.is_empty() && app.exec.is_none(),
            _ => true,
        }
    }

    pub fn all() -> impl Iterator<Item = Cmd> {
        MENUS
            .iter()
            .flat_map(|(_, items)| items.iter().flatten().copied())
    }
}

pub fn run(app: &mut App, ctx: &egui::Context, cmd: Cmd) {
    if !cmd.enabled(app) {
        return;
    }
    match cmd {
        Cmd::NewScan => {
            if let Some(s) = app.scan.take() {
                s.handle.cancel();
            }
            app.tab = Tab::SpaceMap;
        }
        Cmd::ScanHome => {
            if let Some(h) = sr_platform::home_dir() {
                app.actions.push(Action::StartScan(vec![h]));
            }
        }
        Cmd::ChooseFolders => {
            if let Some(dirs) = rfd::FileDialog::new().pick_folders() {
                if !dirs.is_empty() {
                    app.actions.push(Action::StartScan(dirs));
                }
            }
        }
        Cmd::Rescan => app.actions.push(Action::Rescan),
        Cmd::ExportJson => crate::views::map::export_current(app, "json"),
        Cmd::ExportCsv => crate::views::map::export_current(app, "csv"),
        Cmd::ExportSvg => crate::views::map::export_current(app, "svg"),
        Cmd::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        Cmd::Find => {
            app.tab = Tab::SpaceMap;
            app.map.focus_search = true;
        }
        Cmd::StageSelection => crate::views::map::stage_selection(app),
        Cmd::Settings => app.dialogs.settings = true,
        Cmd::SpaceMap => app.tab = Tab::SpaceMap,
        Cmd::DevSweep => app.tab = Tab::DevSweep,
        Cmd::DuplicateLens => app.tab = Tab::DuplicateLens,
        Cmd::LargestFiles => {
            let ids = app.scan.as_ref().and_then(|s| {
                s.tree()
                    .read()
                    .ok()
                    .map(|t| t.largest_files(100, app.settings.size_mode))
            });
            app.dialogs.largest = ids;
        }
        Cmd::UnreadableLocations => app.dialogs.issues = true,
        Cmd::ToggleSizeMode => {
            app.settings.size_mode = match app.settings.size_mode {
                SizeMode::Allocated => SizeMode::Apparent,
                SizeMode::Apparent => SizeMode::Allocated,
            };
            app.settings_dirty = true;
        }
        Cmd::ToggleDrawer => app.drawer_open = !app.drawer_open,
        Cmd::DryRun => app.actions.push(Action::Execute(Method::DryRun)),
        Cmd::MoveToTrash => app.dialogs.confirm = Some(Method::Trash),
        Cmd::DeletePermanently => {
            app.dialogs.confirm = Some(Method::Permanent);
            app.dialogs.confirm_text.clear();
        }
        Cmd::ClearDrawer => app.actions.push(Action::ClearDrawer),
        Cmd::Journal => app.dialogs.journal = true,
        Cmd::Quarantine => app.dialogs.quarantine = true,
        Cmd::Log => app.dialogs.log = true,
        Cmd::CopyDiagnostics => {
            ctx.copy_text(crate::views::dialogs::diagnostics(app));
            app.log.info("Diagnostics copied to clipboard", app.now);
        }
        Cmd::Shortcuts => app.dialogs.help = !app.dialogs.help,
    }
}

/// Consume pressed hotkeys and run their commands. More specific
/// combinations are matched first, because egui's matching ignores extra
/// Shift/Alt (⌥⌘⌫ would otherwise also trigger ⌘⌫).
pub fn handle_hotkeys(app: &mut App, ctx: &egui::Context) {
    let typing = ctx.egui_wants_keyboard_input();
    let modal_open = app.dialogs.confirm.is_some();
    let mut cmds: Vec<(Cmd, KeyboardShortcut)> = Cmd::all()
        .filter_map(|c| c.shortcut().map(|s| (c, s)))
        .filter(|(c, _)| !typing || c.allowed_while_typing())
        // Delete alone is handled by the Space Map's own keyboard focus.
        .filter(|(c, _)| *c != Cmd::StageSelection)
        // The macOS menu bar receives its key equivalents first.
        .filter(|(c, _)| !(app.has_native_menu() && native_handles(*c)))
        .collect();
    cmds.sort_by_key(|(_, s)| std::cmp::Reverse(modifier_count(s.modifiers)));
    let mut fired = Vec::new();
    ctx.input_mut(|i| {
        for (c, s) in &cmds {
            if i.consume_shortcut(s) {
                fired.push(*c);
            }
        }
    });
    for c in fired {
        // A confirmation dialog captures everything except quitting.
        if modal_open && c != Cmd::Quit {
            continue;
        }
        run(app, ctx, c);
    }
}

fn native_handles(c: Cmd) -> bool {
    #[cfg(any(target_os = "macos", windows))]
    return crate::native_menu::handles_hotkey(c);
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = c;
        false
    }
}

fn modifier_count(m: Modifiers) -> u8 {
    m.alt as u8 + m.shift as u8 + m.ctrl as u8 + m.command as u8 + m.mac_cmd as u8
}

/// The menu bar.
pub fn menu_bar(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let mut chosen = None;
    // MenuBar claims the full row width; give it only what its titles need.
    let width: f32 = MENUS
        .iter()
        .map(|(t, _)| {
            ui.fonts_mut(|f| {
                f.layout_no_wrap(
                    t.to_string(),
                    egui::FontId::proportional(14.0),
                    egui::Color32::WHITE,
                )
                .size()
                .x
            }) + 14.0
        })
        .sum();
    let bar = egui::vec2(width, 34.0);
    ui.allocate_ui_with_layout(
        bar,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for (title, items) in MENUS {
                    ui.menu_button(*title, |ui| {
                        ui.set_min_width(260.0);
                        for item in *items {
                            match item {
                                None => {
                                    ui.separator();
                                }
                                Some(cmd) => {
                                    let mut b = egui::Button::new(cmd.label());
                                    if let Some(t) = cmd.shortcut_text(&ctx) {
                                        b = b.shortcut_text(
                                            egui::RichText::new(t)
                                                .color(crate::theme::of(ui).slate),
                                        );
                                    }
                                    if ui.add_enabled(cmd.enabled(app), b).clicked() {
                                        chosen = Some(*cmd);
                                        ui.close();
                                    }
                                }
                            }
                        }
                    });
                }
            });
        },
    );
    if let Some(c) = chosen {
        run(app, &ctx, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in Cmd::all() {
            if let Some(s) = c.shortcut() {
                let key = (
                    s.logical_key,
                    s.modifiers.alt,
                    s.modifiers.shift,
                    s.modifiers.command,
                );
                assert!(seen.insert(key), "{c:?} reuses a hotkey");
            }
        }
    }

    #[test]
    fn every_command_is_in_a_menu() {
        assert!(Cmd::all().count() >= 25);
    }
}
