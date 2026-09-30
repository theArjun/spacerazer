//! Visual identity: typefaces, type scale, palettes and widget styling.
//!
//! Direction: a survey instrument. Quiet, exact chrome in cool paper and
//! graphite, one survey-teal accent, and the sunburst's map tints as the only
//! loud colour on screen. Bricolage Grotesque carries headings and the big
//! size figures; Atkinson Hyperlegible Next carries everything that has to be
//! read quickly (tables, paths, dense status text).

use std::sync::Arc;

use egui::epaint::Shadow;
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Stroke,
    TextStyle, Theme, Vec2, Visuals,
};

const BODY: &[u8] = include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-400.ttf");
const BODY_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-600.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-400.ttf");
const DISPLAY: &[u8] = include_bytes!("../assets/fonts/BricolageGrotesque-600.ttf");
const DISPLAY_BOLD: &[u8] = include_bytes!("../assets/fonts/BricolageGrotesque-700.ttf");

pub const SEMIBOLD: &str = "body-semibold";
pub const DISPLAY_FAMILY: &str = "display";
pub const DISPLAY_BOLD_FAMILY: &str = "display-bold";

/// Named colours for one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub ink: Color32,
    pub slate: Color32,
    pub paper: Color32,
    pub surface: Color32,
    pub mist: Color32,
    pub line: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warn: Color32,
    pub ok: Color32,
}

pub const LIGHT: Palette = Palette {
    ink: Color32::from_rgb(0x1C, 0x24, 0x30),
    slate: Color32::from_rgb(0x5E, 0x68, 0x75),
    paper: Color32::from_rgb(0xF7, 0xF8, 0xF6),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    mist: Color32::from_rgb(0xEC, 0xEF, 0xEC),
    line: Color32::from_rgb(0xD9, 0xDE, 0xD9),
    accent: Color32::from_rgb(0x0C, 0x7A, 0x72),
    accent_soft: Color32::from_rgb(0xD3, 0xEC, 0xE8),
    on_accent: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    danger: Color32::from_rgb(0xC2, 0x3A, 0x2B),
    warn: Color32::from_rgb(0xA8, 0x6A, 0x0C),
    ok: Color32::from_rgb(0x2E, 0x7D, 0x32),
};

pub const DARK: Palette = Palette {
    ink: Color32::from_rgb(0xE6, 0xEA, 0xEE),
    slate: Color32::from_rgb(0x9A, 0xA4, 0xAF),
    paper: Color32::from_rgb(0x1B, 0x20, 0x27),
    surface: Color32::from_rgb(0x22, 0x28, 0x31),
    mist: Color32::from_rgb(0x2A, 0x31, 0x3A),
    line: Color32::from_rgb(0x36, 0x3E, 0x48),
    accent: Color32::from_rgb(0x44, 0xC2, 0xB5),
    accent_soft: Color32::from_rgb(0x1D, 0x45, 0x42),
    on_accent: Color32::from_rgb(0x0E, 0x1A, 0x19),
    danger: Color32::from_rgb(0xF0, 0x6F, 0x5F),
    warn: Color32::from_rgb(0xE8, 0xB0, 0x4B),
    ok: Color32::from_rgb(0x7B, 0xC6, 0x7E),
};

pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

pub fn of(ui: &egui::Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// Map tints for the sunburst's top-level branches: hues drawn from
/// topographic and hydrographic map printing, balanced in lightness so no
/// branch dominates.
pub const MAP_TINTS: [Color32; 10] = [
    Color32::from_rgb(0x3A, 0x9E, 0x8F), // lake teal
    Color32::from_rgb(0xE0, 0xA9, 0x3B), // ochre
    Color32::from_rgb(0x6C, 0x8F, 0xD6), // survey blue
    Color32::from_rgb(0x8C, 0xB3, 0x5B), // moss
    Color32::from_rgb(0xB0, 0x78, 0xC9), // heather
    Color32::from_rgb(0xE5, 0x7A, 0x6E), // coral
    Color32::from_rgb(0x4F, 0xB6, 0xD1), // glacier
    Color32::from_rgb(0xC9, 0x9A, 0x6B), // sandstone
    Color32::from_rgb(0xD4, 0x6F, 0x9C), // rhodolite
    Color32::from_rgb(0x7E, 0x86, 0xC9), // slate violet
];

/// Linear blend of `a` toward `b` by `t` (0 = a, 1 = b).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    for theme in [Theme::Light, Theme::Dark] {
        let p = palette(theme == Theme::Dark);
        ctx.style_mut_of(theme, |s| {
            s.text_styles = text_styles();
            s.visuals = visuals(theme, &p);
            let sp = &mut s.spacing;
            sp.item_spacing = Vec2::new(8.0, 6.0);
            sp.button_padding = Vec2::new(10.0, 4.0);
            sp.interact_size = Vec2::new(40.0, 26.0);
            sp.menu_margin = Margin::same(6);
            sp.window_margin = Margin::same(16);
            sp.indent = 16.0;
            sp.slider_width = 120.0;
            s.interaction.selectable_labels = false;
        });
    }
}

fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    for (name, bytes) in [
        ("atkinson", BODY),
        ("atkinson-semibold", BODY_SEMIBOLD),
        ("atkinson-mono", MONO),
        ("bricolage", DISPLAY),
        ("bricolage-bold", DISPLAY_BOLD),
    ] {
        f.font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    // egui's bundled fonts stay as fallbacks for symbols and emoji.
    let mut fallback: Vec<String> = f
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    // Hack (egui's monospace) covers arrows and box-drawing glyphs.
    fallback.push("Hack".into());
    let with = |first: &str| -> Vec<String> {
        std::iter::once(first.to_string())
            .chain(fallback.iter().cloned())
            .collect()
    };
    f.families
        .insert(FontFamily::Proportional, with("atkinson"));
    f.families
        .insert(FontFamily::Name(SEMIBOLD.into()), with("atkinson-semibold"));
    f.families
        .insert(FontFamily::Name(DISPLAY_FAMILY.into()), with("bricolage"));
    f.families.insert(
        FontFamily::Name(DISPLAY_BOLD_FAMILY.into()),
        with("bricolage-bold"),
    );
    let mono_fallback = f
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    f.families.insert(
        FontFamily::Monospace,
        std::iter::once("atkinson-mono".to_string())
            .chain(mono_fallback)
            .collect(),
    );
    f
}

fn family(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

/// Type scale, roughly ×1.25 from a 14 px body.
fn text_styles() -> std::collections::BTreeMap<TextStyle, FontId> {
    [
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, family(SEMIBOLD))),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
        (
            TextStyle::Heading,
            FontId::new(22.0, family(DISPLAY_FAMILY)),
        ),
    ]
    .into()
}

fn visuals(theme: Theme, p: &Palette) -> Visuals {
    let dark = theme == Theme::Dark;
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let r6 = CornerRadius::same(6);
    v.panel_fill = p.paper;
    v.window_fill = p.surface;
    v.extreme_bg_color = if dark {
        mix(p.paper, Color32::BLACK, 0.25)
    } else {
        p.surface
    };
    v.faint_bg_color = if dark {
        mix(p.paper, p.mist, 0.45)
    } else {
        mix(p.paper, p.mist, 0.55)
    };
    v.code_bg_color = p.mist;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 34 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 28 }),
    };
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.5, p.accent);
    v.slider_trailing_fill = true;
    v.striped = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.paper;
    w.noninteractive.weak_bg_fill = p.paper;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
    w.noninteractive.corner_radius = r6;

    let hover = mix(p.mist, p.line, 0.6);
    let press = p.line;
    for (state, bg, stroke) in [
        (&mut w.inactive, p.mist, Stroke::NONE),
        (
            &mut w.hovered,
            hover,
            Stroke::new(1.0, mix(p.line, p.slate, 0.35)),
        ),
        (&mut w.active, press, Stroke::new(1.0, p.accent)),
        (&mut w.open, hover, Stroke::new(1.0, p.line)),
    ] {
        state.weak_bg_fill = bg;
        state.bg_fill = bg;
        state.bg_stroke = stroke;
        state.fg_stroke = Stroke::new(1.0, p.ink);
        state.corner_radius = r6;
        state.expansion = 0.0;
    }
    // Checkbox / radio / slider rail backgrounds need a little more weight.
    w.inactive.bg_fill = mix(p.mist, p.line, 0.7);
    v
}

// ------------------------------------------------------------ typography

pub trait Typo {
    fn semibold(self) -> Self;
    fn display(self, size: f32) -> Self;
    fn display_bold(self, size: f32) -> Self;
}

impl Typo for RichText {
    fn semibold(self) -> Self {
        self.family(family(SEMIBOLD))
    }
    fn display(self, size: f32) -> Self {
        self.family(family(DISPLAY_FAMILY)).size(size)
    }
    fn display_bold(self, size: f32) -> Self {
        self.family(family(DISPLAY_BOLD_FAMILY)).size(size)
    }
}

pub fn display_font(size: f32) -> FontId {
    FontId::new(size, family(DISPLAY_BOLD_FAMILY))
}

pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, family(SEMIBOLD))
}

/// Section title in the display face.
pub fn heading(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.label(RichText::new(text).display(20.0).color(of(ui).ink))
}

/// Secondary text.
pub fn muted(ui: &egui::Ui, text: impl Into<String>) -> RichText {
    RichText::new(text).color(of(ui).slate)
}

/// Column header in tables.
pub fn column_header(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .semibold()
            .size(12.0)
            .color(of(ui).slate),
    );
}

// --------------------------------------------------------------- widgets

/// Filled accent button for the main action in a context.
pub fn primary(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let p = of(ui);
    egui::Button::new(RichText::new(text).semibold().color(p.on_accent))
        .fill(p.accent)
        .corner_radius(6)
}

/// Outlined destructive button. Never the default focus.
pub fn danger(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let p = of(ui);
    egui::Button::new(RichText::new(text).semibold().color(p.danger))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, mix(p.danger, p.paper, 0.35)))
        .corner_radius(6)
}

/// Borderless button for secondary actions.
pub fn ghost(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let _ = ui;
    egui::Button::new(RichText::new(text))
        .fill(Color32::TRANSPARENT)
        .frame_when_inactive(false)
}

/// A tab in a segmented control: selected tabs sit on a raised surface.
pub fn tab(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let p = of(ui);
    let rt = RichText::new(text)
        .semibold()
        .size(14.0)
        .color(if selected { p.ink } else { p.slate });
    let btn = egui::Button::new(rt)
        .fill(if selected {
            p.surface
        } else {
            Color32::TRANSPARENT
        })
        .stroke(if selected {
            Stroke::new(1.0, p.line)
        } else {
            Stroke::NONE
        })
        .corner_radius(6)
        .min_size(Vec2::new(0.0, 28.0));
    ui.add(btn)
}

/// A small coloured tag with an explanation tooltip.
pub fn tag(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let p = of(ui);
    let bg = mix(
        color,
        p.paper,
        if ui.visuals().dark_mode { 0.72 } else { 0.82 },
    );
    let fg = if ui.visuals().dark_mode {
        mix(color, Color32::WHITE, 0.25)
    } else {
        mix(color, Color32::BLACK, 0.35)
    };
    egui::Frame::new()
        .fill(bg)
        .corner_radius(4)
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).semibold().size(12.0).color(fg))
        })
        .response
}

/// Horizontal capacity bar.
pub fn meter(ui: &mut egui::Ui, frac: f32, width: f32, color: Color32) -> egui::Response {
    let p = of(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 8.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, p.mist);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, 4.0, color);
    resp
}

/// Bar for work of unknown length: a segment sliding across the track.
/// Static (half-filled, striped) when `animate` is false.
pub fn indeterminate(
    ui: &mut egui::Ui,
    width: f32,
    color: Color32,
    animate: bool,
) -> egui::Response {
    let p = of(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 8.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, p.mist);
    if animate {
        let t = ui.input(|i| i.time) as f32;
        let seg = rect.width() * 0.3;
        // Travel from fully left of the track to fully right, every 1.4 s.
        let phase = (t / 1.4).fract();
        let x = rect.left() - seg + (rect.width() + seg) * phase;
        let bar =
            egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(seg, rect.height()));
        painter.rect_filled(bar.intersect(rect), 4.0, color);
        ui.ctx().request_repaint();
    } else {
        let mut fill = rect;
        fill.set_width(rect.width() * 0.5);
        painter.rect_filled(fill, 4.0, mix(color, p.mist, 0.4));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_parse() {
        let ctx = egui::Context::default();
        install(&ctx);
        // Font loading happens lazily on first layout; panics on bad data.
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.label(RichText::new("Sunburst 19.9 GB").display_bold(30.0));
            ui.label(RichText::new("node_modules").semibold());
            ui.monospace("/Users/x/target");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn contrast_of_text_on_paper() {
        fn lum(c: Color32) -> f32 {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        fn ratio(a: Color32, b: Color32) -> f32 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        // WCAG 2.2: body text >= 4.5:1.
        for p in [LIGHT, DARK] {
            assert!(ratio(p.ink, p.paper) >= 7.0);
            assert!(
                ratio(p.slate, p.paper) >= 4.5,
                "slate {}",
                ratio(p.slate, p.paper)
            );
            assert!(
                ratio(p.accent, p.paper) >= 4.5,
                "accent {}",
                ratio(p.accent, p.paper)
            );
            assert!(
                ratio(p.on_accent, p.accent) >= 4.5,
                "on_accent {}",
                ratio(p.on_accent, p.accent)
            );
            assert!(
                ratio(p.danger, p.paper) >= 4.5,
                "danger {}",
                ratio(p.danger, p.paper)
            );
        }
    }
}
