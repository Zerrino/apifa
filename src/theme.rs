//! Colors and fonts of apifa: graphite chrome (title bar, sidebar, response column), a light card
//! for what is edited, one violet accent. The names are the roles they had (CYAN = the accent,
//! NAVY = the chrome and the text on the card), so every widget follows.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};

// Accent (links, the selected tab, primary buttons, “connected”).
pub const CYAN: Color32 = Color32::from_rgb(0x7c, 0x6c, 0xf2);
pub const CYAN_DARK: Color32 = Color32::from_rgb(0x5b, 0x4b, 0xd1);
// Graphite chrome, from light to deep.
pub const NAVY: Color32 = Color32::from_rgb(0x26, 0x28, 0x2e);
pub const NAVY_DARK: Color32 = Color32::from_rgb(0x1f, 0x21, 0x26);
pub const NAVY_DEEP: Color32 = Color32::from_rgb(0x18, 0x19, 0x1d);
pub const CODE_BG: Color32 = Color32::from_rgb(0x16, 0x17, 0x1b);
pub const NAVY_HOVER: Color32 = Color32::from_rgb(0x33, 0x36, 0x3e);
pub const WHITE: Color32 = Color32::WHITE;
pub const ROW: Color32 = Color32::from_rgb(0xf7, 0xf7, 0xf9);
pub const CLOUDS: Color32 = Color32::from_rgb(0xec, 0xec, 0xf1);
pub const LINE: Color32 = Color32::from_rgb(0xe3, 0xe3, 0xe8);
pub const SILVER: Color32 = Color32::from_rgb(0xb4, 0xb6, 0xc0);
pub const CONCRETE: Color32 = Color32::from_rgb(0x8b, 0x8e, 0x99);
pub const MUTED: Color32 = Color32::from_rgb(0x7a, 0x7d, 0x88);
// Verbs and states, as API clients color them.
pub const GREEN: Color32 = Color32::from_rgb(0x3d, 0xc5, 0x8a);
pub const RED: Color32 = Color32::from_rgb(0xef, 0x53, 0x50);
pub const REED: Color32 = Color32::from_rgb(0xb3, 0x26, 0x1e);
pub const YELLOW: Color32 = Color32::from_rgb(0xf5, 0xb7, 0x2e);
pub const ORANGE: Color32 = Color32::from_rgb(0xf2, 0x84, 0x3a);
pub const BLUE: Color32 = Color32::from_rgb(0x4a, 0x9e, 0xf2);
pub const PURPLE: Color32 = Color32::from_rgb(0xb0, 0x6c, 0xf0);

pub fn verb_color(verb: &str) -> Color32 {
    match verb {
        "GET" => GREEN,
        "POST" => YELLOW,
        "PUT" => BLUE,
        "PATCH" => PURPLE,
        "DELETE" => RED,
        "FLOW" => CYAN,
        "SUB" => ORANGE,
        _ => SILVER,
    }
}

pub fn short_verb(verb: &str) -> &str {
    if verb == "DELETE" {
        "DEL"
    } else {
        verb
    }
}

pub fn status_color(status: u16) -> Color32 {
    match status {
        200..=299 => GREEN,
        300..=399 => BLUE,
        400..=499 => ORANGE,
        _ => RED,
    }
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("lato".into(), Arc::new(FontData::from_static(include_bytes!("../assets/Lato-Regular.ttf"))));
    fonts.font_data.insert("lato-bold".into(), Arc::new(FontData::from_static(include_bytes!("../assets/Lato-Bold.ttf"))));
    // Last-resort fallback: symbols missing from the other fonts (✓, ✗, ⧉…) would render as boxes.
    fonts.font_data.insert("dejavu".into(), Arc::new(FontData::from_static(include_bytes!("../assets/DejaVuSans.ttf"))));
    fonts.families.entry(FontFamily::Monospace).or_default().push("dejavu".into());
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "lato".into());
    proportional.push("dejavu".into());
    let mut bold_family = vec!["lato-bold".to_owned()];
    bold_family.extend(proportional.iter().skip(1).cloned());
    fonts.families.insert(bold(), bold_family);
    ctx.set_fonts(fonts);

    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, bold())),
            (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.visuals = dark_visuals();
    });
}

fn corner(v: &mut Visuals, r: u8) {
    let c = CornerRadius::same(r);
    v.widgets.noninteractive.corner_radius = c;
    v.widgets.inactive.corner_radius = c;
    v.widgets.hovered.corner_radius = c;
    v.widgets.active.corner_radius = c;
    v.widgets.open.corner_radius = c;
}

/// The "space-ship" navy chrome around the content (top bar, sidebar, response column).
pub fn dark_visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = NAVY;
    v.window_fill = NAVY_DARK;
    v.window_stroke = Stroke::new(1.0_f32, NAVY_DEEP);
    v.extreme_bg_color = CODE_BG;
    v.faint_bg_color = NAVY_DEEP;
    v.code_bg_color = CODE_BG;
    v.hyperlink_color = CYAN;
    v.selection.bg_fill = CYAN.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0_f32, WHITE);
    v.widgets.noninteractive.bg_fill = NAVY;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, NAVY_HOVER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, CLOUDS);
    v.widgets.inactive.bg_fill = NAVY_DARK;
    v.widgets.inactive.weak_bg_fill = NAVY_DARK;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, NAVY_HOVER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, CLOUDS);
    v.widgets.hovered.bg_fill = NAVY_HOVER;
    v.widgets.hovered.weak_bg_fill = NAVY_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, CYAN);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, WHITE);
    v.widgets.active.bg_fill = CYAN_DARK;
    v.widgets.active.weak_bg_fill = CYAN_DARK;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, CYAN);
    v.widgets.active.fg_stroke = Stroke::new(1.5_f32, WHITE);
    v.widgets.open.bg_fill = NAVY_DARK;
    v.widgets.open.weak_bg_fill = NAVY_DARK;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, CYAN);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(4);
    corner(&mut v, 3);
    v
}

/// The white "main-content-container" card with #34495E text.
pub fn card_visuals() -> Visuals {
    let mut v = Visuals::light();
    v.override_text_color = Some(NAVY);
    v.panel_fill = WHITE;
    v.window_fill = WHITE;
    v.window_stroke = Stroke::new(1.0_f32, SILVER);
    v.extreme_bg_color = WHITE;
    v.faint_bg_color = ROW;
    v.code_bg_color = CLOUDS;
    v.hyperlink_color = CYAN;
    v.selection.bg_fill = CYAN.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, CYAN_DARK);
    v.widgets.noninteractive.bg_fill = WHITE;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, NAVY);
    v.widgets.inactive.bg_fill = CLOUDS;
    v.widgets.inactive.weak_bg_fill = CLOUDS;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, SILVER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, NAVY);
    v.widgets.hovered.bg_fill = LINE;
    v.widgets.hovered.weak_bg_fill = LINE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, CYAN);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, NAVY);
    v.widgets.active.bg_fill = CYAN;
    v.widgets.active.weak_bg_fill = CYAN;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, CYAN_DARK);
    v.widgets.active.fg_stroke = Stroke::new(1.5_f32, WHITE);
    v.widgets.open.bg_fill = CLOUDS;
    v.widgets.open.weak_bg_fill = CLOUDS;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, CYAN);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(4);
    corner(&mut v, 3);
    v
}

#[cfg(test)]
mod glyph_check {
    /// Every non-ASCII character of the source's string literals has a glyph in the app's fonts
    /// (egui 0.37 dropped the icon font: the magnifier and pause signs drew as empty boxes). `visualize.rs` is left
    /// out: its text is for the browser.
    #[test]
    fn every_character_shown_has_a_glyph() {
        let mut text = String::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(dir) = dirs.pop() {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("visualize.rs") {
                    let src = std::fs::read_to_string(&p).unwrap();
                    // Characters inside "…" literals only (comments can say anything).
                    let mut in_str = false;
                    let mut prev = ' ';
                    for c in src.chars() {
                        if c == '"' && prev != '\\' {
                            in_str = !in_str;
                        } else if in_str && !c.is_ascii() {
                            text.push(c);
                        }
                        prev = c;
                    }
                }
            }
        }
        let ctx = egui::Context::default();
        super::install(&ctx);
        let mut out = ctx.run_ui(Default::default(), |_| {});
        out.textures_delta.clear();
        let mut missing = String::new();
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace, super::bold()] {
            let font = egui::FontId::new(14.0, family);
            for c in text.chars() {
                if !missing.contains(c) && !ctx.fonts_mut(|f| f.has_glyph(&font, c)) {
                    missing.push(c);
                }
            }
        }
        assert!(missing.is_empty(), "no glyph in the app's fonts for: {missing}");
    }
}
