//! Platform menu bars built from the command table.
//!
//! - macOS: the global menu bar, with the standard app menu (About,
//!   Settings, Services, Hide, Quit) and Window menu. macOS handles the key
//!   equivalents itself.
//! - Windows: the native menu bar attached to the window. Shortcuts are shown
//!   next to items and handled by the app's own hotkey code.
//!
//! Linux has no single native menu convention across desktops, so it keeps
//! the in-window menu bar drawn by egui.

use std::collections::HashMap;

use crossbeam_channel::Receiver;
use muda::accelerator::{Accelerator, CMD_OR_CTRL, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::app::App;
use crate::commands::{Cmd, MENUS};

pub struct NativeMenu {
    _menu: Menu,
    items: Vec<(Cmd, MenuItem, bool)>,
    by_id: HashMap<String, Cmd>,
    events: Receiver<muda::MenuEvent>,
}

/// On macOS, these commands live in the app menu instead of their usual one.
const MACOS_APP_MENU: [Cmd; 2] = [Cmd::Settings, Cmd::Quit];

impl NativeMenu {
    pub fn install(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let menu = Menu::new();
        let mut items = Vec::new();
        let mut by_id = HashMap::new();
        let mut make = |cmd: Cmd| -> MenuItem {
            let id = format!("{cmd:?}");
            let label = if cfg!(windows) && cmd == Cmd::Quit {
                "Exit"
            } else {
                cmd.label()
            };
            let item = MenuItem::with_id(id.clone(), label, true, accelerator(cmd));
            by_id.insert(id, cmd);
            items.push((cmd, item.clone(), true));
            item
        };

        if cfg!(target_os = "macos") {
            let about = PredefinedMenuItem::about(
                Some("About SpaceRazer"),
                Some(AboutMetadata {
                    name: Some("SpaceRazer".into()),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    copyright: Some("MIT OR Apache-2.0".into()),
                    website: Some("https://github.com/theArjun/spacerazer".into()),
                    ..Default::default()
                }),
            );
            let settings = make(Cmd::Settings);
            let quit = make(Cmd::Quit);
            let app_menu = Submenu::with_items(
                "SpaceRazer",
                true,
                &[
                    &about,
                    &PredefinedMenuItem::separator(),
                    &settings,
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                    &quit,
                ],
            )
            .ok()?;
            menu.append(&app_menu).ok()?;
        }

        for (title, entries) in MENUS {
            let submenu = Submenu::new(*title, true);
            let mut last_was_separator = true;
            for entry in *entries {
                match entry {
                    Some(cmd) if cfg!(target_os = "macos") && MACOS_APP_MENU.contains(cmd) => {}
                    Some(cmd) => {
                        submenu.append(&make(*cmd)).ok()?;
                        last_was_separator = false;
                    }
                    None if !last_was_separator => {
                        submenu.append(&PredefinedMenuItem::separator()).ok()?;
                        last_was_separator = true;
                    }
                    None => {}
                }
            }
            if cfg!(target_os = "macos") && *title == "View" {
                submenu
                    .append_items(&[
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::fullscreen(None),
                    ])
                    .ok()?;
            }
            if cfg!(windows) && *title == "Help" {
                submenu
                    .append_items(&[
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::about(Some("About SpaceRazer"), None),
                    ])
                    .ok()?;
            }
            if cfg!(target_os = "macos") && *title == "Help" {
                let window = Submenu::with_items(
                    "Window",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(None),
                        &PredefinedMenuItem::maximize(Some("Zoom")),
                        &PredefinedMenuItem::separator(),
                        &PredefinedMenuItem::bring_all_to_front(None),
                    ],
                )
                .ok()?;
                menu.append(&window).ok()?;
                #[cfg(target_os = "macos")]
                window.set_as_windows_menu_for_nsapp();
            }
            menu.append(&submenu).ok()?;
            #[cfg(target_os = "macos")]
            if *title == "Help" {
                submenu.set_as_help_menu_for_nsapp();
            }
        }

        attach(&menu, cc)?;

        // Deliver clicks through our own channel and wake the UI at once.
        let (tx, rx) = crossbeam_channel::unbounded();
        let ctx = cc.egui_ctx.clone();
        muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
            let _ = tx.send(e);
            ctx.request_repaint();
        }));

        Some(Self {
            _menu: menu,
            items,
            by_id,
            events: rx,
        })
    }

    /// Commands chosen from the menu since the last frame.
    pub fn take_commands(&self) -> Vec<Cmd> {
        self.events
            .try_iter()
            .filter_map(|e| self.by_id.get(e.id.as_ref()).copied())
            .collect()
    }

    /// Grey out commands that can't run right now. Only touches items whose
    /// state changed.
    pub fn sync_enabled(&mut self, app: &App) {
        for (cmd, item, shown) in &mut self.items {
            let now = cmd.enabled(app);
            if now != *shown {
                item.set_enabled(now);
                *shown = now;
            }
        }
    }
}

/// Whether the native menu handles this command's shortcut itself. On macOS
/// the menu bar receives key equivalents before the window does.
pub fn handles_hotkey(cmd: Cmd) -> bool {
    cfg!(target_os = "macos") && accelerator(cmd).is_some()
}

#[cfg(target_os = "macos")]
fn attach(menu: &Menu, _cc: &eframe::CreationContext<'_>) -> Option<()> {
    menu.init_for_nsapp();
    Some(())
}

#[cfg(windows)]
fn attach(menu: &Menu, cc: &eframe::CreationContext<'_>) -> Option<()> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = cc.window_handle().ok()?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return None;
    };
    // SAFETY: the handle comes from eframe for the app's own, live main
    // window, on the thread that owns it.
    unsafe { menu.init_for_hwnd_with_theme(h.hwnd.get(), muda::MenuTheme::Auto) }.ok()
}

/// The command's hotkey as a native accelerator.
fn accelerator(cmd: Cmd) -> Option<Accelerator> {
    // Plain Delete stays with the chart (it must not fire while typing).
    if cmd == Cmd::StageSelection {
        return None;
    }
    let s = cmd.shortcut()?;
    let mut mods = Modifiers::empty();
    if s.modifiers.command || s.modifiers.mac_cmd || s.modifiers.ctrl {
        mods |= CMD_OR_CTRL;
    }
    if s.modifiers.shift {
        mods |= Modifiers::SHIFT;
    }
    if s.modifiers.alt {
        mods |= Modifiers::ALT;
    }
    use egui::Key as K;
    let code = match s.logical_key {
        K::A => Code::KeyA,
        K::D => Code::KeyD,
        K::E => Code::KeyE,
        K::F => Code::KeyF,
        K::H => Code::KeyH,
        K::I => Code::KeyI,
        K::J => Code::KeyJ,
        K::L => Code::KeyL,
        K::N => Code::KeyN,
        K::O => Code::KeyO,
        K::Q => Code::KeyQ,
        K::R => Code::KeyR,
        K::T => Code::KeyT,
        K::Num1 => Code::Digit1,
        K::Num2 => Code::Digit2,
        K::Num3 => Code::Digit3,
        K::Comma => Code::Comma,
        K::Backspace => Code::Backspace,
        _ => return None,
    };
    Some(Accelerator::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hotkey_maps_to_an_accelerator() {
        for cmd in Cmd::all() {
            if cmd.shortcut().is_some() && cmd != Cmd::StageSelection {
                assert!(
                    accelerator(cmd).is_some(),
                    "{cmd:?} has no native accelerator"
                );
            }
        }
    }
}
