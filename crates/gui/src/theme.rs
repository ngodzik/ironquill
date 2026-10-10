//! The window's look: the terminal's colours, on a deep background, with
//! room to breathe.

use bevy_egui::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals,
};
use ironquill_ui::style::Rgb;

/// Behind everything: what the GPU clears to.
pub(crate) const BACKGROUND: Color32 = Color32::from_rgb(11, 13, 18);
/// Panels, a step above the background.
pub(crate) const PANEL: Color32 = Color32::from_rgb(16, 19, 26);
/// Cards and code blocks, a step above the panels.
pub(crate) const RAISED: Color32 = Color32::from_rgb(23, 27, 36);
/// The edge of a panel or a card.
pub(crate) const EDGE: Color32 = Color32::from_rgb(38, 44, 58);
/// The text.
pub(crate) const TEXT: Color32 = Color32::from_rgb(214, 219, 230);
/// What is said in passing.
pub(crate) const DIM: Color32 = Color32::from_rgb(112, 120, 138);
/// The model in use and what has the focus, as in the terminal.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(215, 119, 87);
/// Code in replies.
pub(crate) const CODE: Color32 = Color32::from_rgb(229, 192, 123);
/// Inline code and links.
pub(crate) const LINK: Color32 = Color32::from_rgb(130, 170, 255);
/// What went well.
pub(crate) const GREEN: Color32 = Color32::from_rgb(110, 180, 120);
/// What cannot be undone, and what failed.
pub(crate) const RED: Color32 = Color32::from_rgb(224, 108, 117);
/// What was held back.
pub(crate) const YELLOW: Color32 = Color32::from_rgb(229, 192, 123);
/// Handovers between models.
pub(crate) const MAGENTA: Color32 = Color32::from_rgb(198, 120, 221);
/// Questions that only cost money.
pub(crate) const CYAN: Color32 = Color32::from_rgb(86, 182, 194);
/// The selected row or reply.
pub(crate) const SELECTED: Color32 = Color32::from_rgb(34, 38, 50);

/// `colour` as opaque as `opacity`, from 0 to 1, for a see-through window.
pub(crate) fn see(colour: Color32, opacity: f32) -> Color32 {
    let [r, g, b, _] = colour.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (opacity.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// A colour of the state, as egui draws it.
pub(crate) fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// The size text is drawn at, in points.
pub(crate) const BODY_SIZE: f32 = 14.5;

/// Fonts the system may have, tried after egui's own for the characters
/// those lack: the spinner's braille, arrows, ✗, ⎿.
const FALLBACK_FONTS: [(&str, &str); 3] = [
    (
        "dejavu-sans",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ),
    (
        "dejavu-mono",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ),
    (
        "noto-sans",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    ),
];

/// Adds the fallback fonts the system has. One missing costs only its
/// characters, drawn as boxes.
fn fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, path) in FALLBACK_FONTS {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push(name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

/// Sets the window's look once, before the first frame.
pub(crate) fn apply(ctx: &egui::Context) {
    fonts(ctx);
    let mut visuals = Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = RAISED;
    visuals.extreme_bg_color = BACKGROUND;
    visuals.faint_bg_color = RAISED;
    visuals.override_text_color = Some(TEXT);
    visuals.window_stroke = Stroke::new(1.0, EDGE);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.selection.bg_fill = SELECTED;
    visuals.hyperlink_color = LINK;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, EDGE);
    ctx.set_visuals(visuals);

    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.window_margin = Margin::same(16);
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(20.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Body,
                FontId::new(BODY_SIZE, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(BODY_SIZE - 0.5, FontFamily::Monospace),
            ),
            (
                TextStyle::Button,
                FontId::new(BODY_SIZE, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            ),
        ]
        .into();
    });
}
