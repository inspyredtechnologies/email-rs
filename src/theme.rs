//! Outlook Classic look: Office "Colorful" palette, Segoe UI, Segoe MDL2 icon glyphs.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke};
use std::sync::Arc;

pub const BLUE: Color32 = Color32::from_rgb(0x00, 0x72, 0xC6);
pub const BLUE_DARK: Color32 = Color32::from_rgb(0x10, 0x5E, 0xA0);
pub const SELECTED: Color32 = Color32::from_rgb(0xCD, 0xE6, 0xF7);
pub const HOVER: Color32 = Color32::from_rgb(0xE6, 0xF2, 0xFA);
pub const RIBBON_BG: Color32 = Color32::from_rgb(0xF3, 0xF3, 0xF3);
pub const BORDER: Color32 = Color32::from_rgb(0xD4, 0xD4, 0xD4);
pub const TEXT: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub const SUBTLE: Color32 = Color32::from_rgb(0x66, 0x66, 0x66);
pub const GROUP_HEADER: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);
pub const FLAG_RED: Color32 = Color32::from_rgb(0xC4, 0x31, 0x2B);
pub const WHITE: Color32 = Color32::WHITE;

/// Segoe MDL2 / Fluent icon code points used across the UI.
pub mod icon {
    pub const MAIL: char = '\u{E715}';
    pub const ADD: char = '\u{E710}';
    pub const DELETE: char = '\u{E74D}';
    pub const ARCHIVE: char = '\u{E7B8}';
    pub const REPLY: char = '\u{E8CA}';
    pub const REPLY_ALL: char = '\u{E8C2}';
    pub const FORWARD: char = '\u{E72A}';
    pub const MOVE: char = '\u{E8DE}';
    pub const READ: char = '\u{E8C3}';
    pub const FLAG: char = '\u{E7C1}';
    pub const SYNC: char = '\u{E895}';
    pub const SEND: char = '\u{E724}';
    pub const ATTACH: char = '\u{E723}';
    pub const SEARCH: char = '\u{E721}';
    pub const CALENDAR: char = '\u{E787}';
    pub const PEOPLE: char = '\u{E716}';
    pub const TASKS: char = '\u{E73A}';
    pub const SETTINGS: char = '\u{E713}';
    pub const BACK: char = '\u{E72B}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
}

pub fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

pub fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn icon(ch: char, size: f32) -> RichText {
    RichText::new(ch.to_string()).font(FontId::new(size, icon_family()))
}

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, bold_family()))
}

fn load_font(names: &[&str]) -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    names.iter().find_map(|n| std::fs::read(format!(r"{dir}\Fonts\{n}")).ok())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut regular = Vec::new();
    if let Some(data) = load_font(&["segoeui.ttf"]) {
        fonts.font_data.insert("segoe".into(), Arc::new(FontData::from_owned(data)));
        regular.push("segoe".to_string());
    }
    let mut bold = Vec::new();
    if let Some(data) = load_font(&["segoeuisb.ttf", "segoeuib.ttf"]) {
        fonts.font_data.insert("segoe-bold".into(), Arc::new(FontData::from_owned(data)));
        bold.push("segoe-bold".to_string());
    }
    let mut icons = Vec::new();
    if let Some(data) = load_font(&["SegoeIcons.ttf", "segmdl2.ttf"]) {
        fonts.font_data.insert("mdl2".into(), Arc::new(FontData::from_owned(data)));
        icons.push("mdl2".to_string());
    }

    let proportional: Vec<String> = regular.iter().chain(defaults.iter()).cloned().collect();
    fonts.families.insert(FontFamily::Proportional, proportional.clone());
    fonts.families.insert(bold_family(), bold.into_iter().chain(proportional.iter().cloned()).collect());
    fonts.families.insert(icon_family(), icons.into_iter().chain(defaults).collect());
    ctx.set_fonts(fonts);

    ctx.set_visuals_of(egui::Theme::Light, light_visuals());
    ctx.set_theme(egui::ThemePreference::Light);
    ctx.all_styles_mut(|style| {
        use egui::TextStyle::*;
        style.text_styles.insert(Body, FontId::proportional(13.5));
        style.text_styles.insert(Button, FontId::proportional(13.0));
        style.text_styles.insert(Small, FontId::proportional(11.5));
        style.text_styles.insert(Heading, FontId::proportional(19.0));
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 22.0;
    });
}

fn light_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = WHITE;
    v.window_fill = WHITE;
    v.extreme_bg_color = WHITE;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = SELECTED;
    v.selection.stroke = Stroke::new(1.0, BLUE);
    v.hyperlink_color = BLUE;
    v.window_corner_radius = 0.into();
    v.menu_corner_radius = 0.into();
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = 0.into();
    }
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xA0, 0xC8, 0xE8));
    v.widgets.active.weak_bg_fill = SELECTED;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v
}

/// Avatar colors Outlook picks from for the initials circle.
pub fn avatar_color(seed: &str) -> Color32 {
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(0x00, 0x78, 0xD4),
        Color32::from_rgb(0x8E, 0x56, 0x2E),
        Color32::from_rgb(0x00, 0x7C, 0x7A),
        Color32::from_rgb(0x88, 0x17, 0x98),
        Color32::from_rgb(0xC2, 0x39, 0xB3),
        Color32::from_rgb(0x49, 0x82, 0x05),
        Color32::from_rgb(0xCA, 0x50, 0x10),
        Color32::from_rgb(0x4F, 0x6B, 0xED),
    ];
    let h = seed.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    COLORS[(h as usize) % COLORS.len()]
}

pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().filter(|w| w.chars().next().is_some_and(char::is_alphanumeric)).collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => first(one),
        [a, .., b] => first(a) + &first(b),
    }
}
