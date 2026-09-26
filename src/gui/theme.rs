//! Refined dark theme for the native GUI.
//!
//! Visuals only: colors, rounding, spacing, and type scale. No control is
//! added, removed, renamed, or reordered here — every widget the panels build
//! keeps its place and behavior; this module only changes how they look.

use eframe::egui;

/// Primary accent (tab underline, links, selection, toggles).
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(91, 141, 239);
/// Dim accent fill for the selected tab pill.
pub fn accent_dim() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(91, 141, 239, 40)
}
/// Bright accent text for the selected tab label.
pub const ACCENT_TEXT: egui::Color32 = egui::Color32::from_rgb(165, 190, 245);
/// Muted teal-gray used for the "QuicMic" title.
pub const TITLE: egui::Color32 = egui::Color32::from_rgb(147, 163, 155);
/// Section headings.
pub const HEADING: egui::Color32 = egui::Color32::from_rgb(235, 236, 240);
/// Body text.
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(225, 226, 232);

const BG_WINDOW: egui::Color32 = egui::Color32::from_rgb(27, 27, 33);
const BG_PANEL: egui::Color32 = egui::Color32::from_rgb(35, 35, 42);
const BG_INPUT: egui::Color32 = egui::Color32::from_rgb(20, 20, 25);
const BG_BUTTON: egui::Color32 = egui::Color32::from_rgb(46, 46, 55);
const BG_BUTTON_HOVER: egui::Color32 = egui::Color32::from_rgb(58, 58, 69);
const STROKE_SUBTLE: egui::Color32 = egui::Color32::from_rgb(62, 62, 74);

fn widget(bg: egui::Color32, fg: egui::Color32, radius: u8) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: egui::Stroke::new(1.0, STROKE_SUBTLE),
        corner_radius: egui::CornerRadius::same(radius),
        fg_stroke: egui::Stroke::new(1.0, fg),
        expansion: 0.0,
    }
}

/// Apply the QuicMic dark theme to the context. Called once when the native
/// window is created.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.override_text_color = Some(TEXT);
    visuals.window_fill = BG_WINDOW;
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.panel_fill = BG_PANEL;
    visuals.faint_bg_color = BG_PANEL;
    visuals.extreme_bg_color = BG_INPUT;
    visuals.code_bg_color = BG_INPUT;
    visuals.hyperlink_color = ACCENT;
    visuals.selection = egui::style::Selection {
        bg_fill: accent_dim(),
        stroke: egui::Stroke::new(1.0, ACCENT),
    };
    visuals.widgets.noninteractive = widget(BG_PANEL, TEXT, 8);
    visuals.widgets.inactive = widget(BG_BUTTON, TEXT, 8);
    visuals.widgets.hovered = widget(BG_BUTTON_HOVER, TEXT, 8);
    visuals.widgets.active = widget(accent_dim(), ACCENT_TEXT, 8);
    visuals.widgets.open = widget(accent_dim(), ACCENT_TEXT, 8);
    // Sliders and progress bars read the selection color for their fill.
    ctx.set_visuals_of(egui::Theme::Dark, visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.indent = 20.0;
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 10.0,
        ..Default::default()
    };
    let text_styles = &mut style.text_styles;
    text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(22.0, egui::FontFamily::Proportional),
    );
    text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(14.0, egui::FontFamily::Monospace),
    );
    text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
    );
    ctx.set_style_of(egui::Theme::Dark, style);
}
