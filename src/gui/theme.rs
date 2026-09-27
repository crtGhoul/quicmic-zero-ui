//! QuicMic theme, matching the Android app's visual language.
//!
//! Dark (the historical look) uses near-black surfaces (#0B0D10 family);
//! light uses white cards on light-gray chrome. Both share a single green
//! accent (#4ADE80) for status, primary actions, and the VU meter — small
//! text in the accent color gets a darker green in light mode so it stays
//! readable. Visuals only: colors, rounding, spacing, and type scale. No
//! control is added, removed, renamed, or reordered here — every widget the
//! panels build keeps its place and behavior; this module only changes how
//! they look.

use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui;

/// Light or dark GUI theme. Dark is the default (the historical look); the
/// choice is persisted in `gui_prefs.json` so it survives restarts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Dark => "Dark",
            ThemeMode::Light => "Light",
        }
    }

    pub fn toggle(self) -> ThemeMode {
        match self {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
        }
    }
}

/// The currently applied mode. Set by [`apply_theme`]; read by the color
/// helpers below so panels don't have to thread the mode through every
/// call site. 0 = Dark, 1 = Light.
static CURRENT: AtomicU8 = AtomicU8::new(0);

fn current() -> ThemeMode {
    match CURRENT.load(Ordering::Relaxed) {
        1 => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

/// Primary accent: Android green. Used for the selected tab pill, toggles,
/// links, the connection pill, and the VU meter. Identical in both modes.
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
/// Dim accent fill for selected states.
pub fn accent_dim() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(74, 222, 128, 36)
}
/// Accent text for selected labels: bright mint on dark, deep green on
/// light (the mint is unreadable on white).
pub fn accent_text() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(134, 239, 172),
        ThemeMode::Light => egui::Color32::from_rgb(21, 128, 61),
    }
}
/// Dark text drawn on top of the green accent (mic tile, pills). Works on
/// the accent fill in both modes.
pub const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(6, 20, 12);
/// The "QuicMic" title in the header.
pub fn title() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(242, 244, 246),
        ThemeMode::Light => egui::Color32::from_rgb(16, 20, 24),
    }
}
/// Section headings.
pub fn heading() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(237, 239, 242),
        ThemeMode::Light => egui::Color32::from_rgb(22, 27, 33),
    }
}
/// Body text.
pub fn text() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(221, 225, 230),
        ThemeMode::Light => egui::Color32::from_rgb(28, 33, 39),
    }
}
/// Green "live" indicator: full accent on dark, deep green on light.
pub fn live() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(74, 222, 128),
        ThemeMode::Light => egui::Color32::from_rgb(21, 128, 61),
    }
}
/// Muted gray "idle" indicator.
pub fn idle() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(120, 128, 136),
        ThemeMode::Light => egui::Color32::from_rgb(107, 118, 131),
    }
}

/// Window background.
fn bg_window() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(11, 13, 16),
        ThemeMode::Light => egui::Color32::from_rgb(242, 244, 246),
    }
}
/// Card surfaces.
fn bg_card() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(18, 22, 26),
        ThemeMode::Light => egui::Color32::from_rgb(255, 255, 255),
    }
}
/// Panels (top bar, nav rail).
fn bg_panel() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(14, 17, 20),
        ThemeMode::Light => egui::Color32::from_rgb(233, 237, 241),
    }
}
/// Text fields and other sunken inputs.
fn bg_input() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(13, 16, 19),
        ThemeMode::Light => egui::Color32::from_rgb(255, 255, 255),
    }
}
/// Buttons.
fn bg_button() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(28, 34, 40),
        ThemeMode::Light => egui::Color32::from_rgb(228, 233, 238),
    }
}
fn bg_button_hover() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(36, 44, 52),
        ThemeMode::Light => egui::Color32::from_rgb(216, 223, 231),
    }
}
fn bg_button_active() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(30, 60, 42),
        ThemeMode::Light => egui::Color32::from_rgb(205, 235, 216),
    }
}
/// Hairline borders around cards and widgets.
fn stroke_subtle() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(35, 43, 51),
        ThemeMode::Light => egui::Color32::from_rgb(211, 218, 226),
    }
}
/// VU meter track.
fn vu_track() -> egui::Color32 {
    match current() {
        ThemeMode::Dark => egui::Color32::from_rgb(24, 30, 36),
        ThemeMode::Light => egui::Color32::from_rgb(226, 232, 238),
    }
}
/// VU meter fill tip (bright end of the gradient).
const VU_TIP: egui::Color32 = egui::Color32::from_rgb(134, 239, 172);

fn widget(bg: egui::Color32, fg: egui::Color32, radius: u8) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: egui::Stroke::new(1.0, stroke_subtle()),
        corner_radius: egui::CornerRadius::same(radius),
        fg_stroke: egui::Stroke::new(1.0, fg),
        expansion: 0.0,
    }
}

/// Apply the QuicMic theme to the context. Called once when the native
/// window is created (with the saved mode) and again on every toggle.
pub fn apply_theme(ctx: &egui::Context, mode: ThemeMode) {
    CURRENT.store(mode as u8, Ordering::Relaxed);
    let egui_theme = match mode {
        ThemeMode::Dark => egui::Theme::Dark,
        ThemeMode::Light => egui::Theme::Light,
    };
    let mut visuals = match mode {
        ThemeMode::Dark => egui::Visuals::dark(),
        ThemeMode::Light => egui::Visuals::light(),
    };
    visuals.dark_mode = mode == ThemeMode::Dark;
    visuals.override_text_color = Some(text());
    visuals.window_fill = bg_window();
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.panel_fill = bg_panel();
    visuals.faint_bg_color = bg_card();
    visuals.extreme_bg_color = bg_input();
    visuals.code_bg_color = bg_input();
    visuals.hyperlink_color = accent_text();
    visuals.selection = egui::style::Selection {
        bg_fill: accent_dim(),
        stroke: egui::Stroke::new(1.0, ACCENT),
    };
    visuals.widgets.noninteractive = widget(bg_card(), text(), 12);
    visuals.widgets.inactive = widget(bg_button(), text(), 10);
    visuals.widgets.hovered = widget(bg_button_hover(), text(), 10);
    visuals.widgets.active = widget(bg_button_active(), accent_text(), 10);
    visuals.widgets.open = widget(bg_button_active(), accent_text(), 10);
    // Sliders and progress bars read the selection color for their fill.
    ctx.set_visuals_of(egui_theme, visuals);

    let mut style = (*ctx.style_of(egui_theme)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
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
    ctx.set_style_of(egui_theme, style);
}

/// A rounded dark card: the standard content container, matching the
/// Android cards.
pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(bg_card())
        .stroke(egui::Stroke::new(1.0, stroke_subtle()))
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::same(16))
        .show(ui, add_contents)
        .inner
}

/// Green mic tile used in the header: a rounded green square with a dark
/// microphone glyph, echoing the Android app icon treatment.
pub fn mic_tile(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, size * 0.28, ACCENT);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "🎤",
        egui::FontId::new(size * 0.52, egui::FontFamily::Proportional),
        ON_ACCENT,
    );
}

/// Connection status pill for the header: green when live, dark when idle.
pub fn status_pill(ui: &mut egui::Ui, connected: bool, text: &str) {
    let (fill, fg) = if connected {
        (ACCENT, ON_ACCENT)
    } else {
        (bg_button(), idle())
    };
    egui::Frame::default()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(255))
        .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 7,
            bottom: 7,
        })
        .show(ui, |ui| {
            let dot = if connected { "●" } else { "○" };
            ui.label(
                egui::RichText::new(format!("{dot} {text}"))
                    .color(fg)
                    .strong()
                    .size(14.0),
            );
        });
}

/// Rounded green-gradient VU meter, matching the Android level bar. `frac`
/// is 0.0–1.0.
pub fn vu_meter(ui: &mut egui::Ui, frac: f32) {
    let frac = frac.clamp(0.0, 1.0);
    let width = ui.available_width().min(440.0);
    let height = 18.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let p = ui.painter();
    let radius = height / 2.0;
    p.rect_filled(rect, radius, vu_track());
    p.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, stroke_subtle()),
        egui::StrokeKind::Inside,
    );
    if frac > 0.0 {
        // Segmented green gradient: darker at the tail, bright at the tip.
        const SEGMENTS: usize = 24;
        let fill_w = rect.width() * frac;
        let seg_w = fill_w / SEGMENTS as f32;
        for i in 0..SEGMENTS {
            let t0 = i as f32 / SEGMENTS as f32;
            let t1 = (i + 1) as f32 / SEGMENTS as f32;
            let seg = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + seg_w * i as f32, rect.min.y),
                egui::pos2(rect.min.x + seg_w * (i + 1) as f32, rect.max.y),
            );
            // Blend ACCENT -> VU_TIP across the bar.
            let c = egui::Color32::from_rgb(
                (ACCENT.r() as f32 + (VU_TIP.r() as f32 - ACCENT.r() as f32) * t1) as u8,
                (ACCENT.g() as f32 + (VU_TIP.g() as f32 - ACCENT.g() as f32) * t1) as u8,
                (ACCENT.b() as f32 + (VU_TIP.b() as f32 - ACCENT.b() as f32) * t1) as u8,
            );
            // Round only the outer ends of the whole fill.
            let rounding = egui::CornerRadius {
                nw: if i == 0 { (radius - 1.0) as u8 } else { 0 },
                ne: 0,
                sw: if i == 0 { (radius - 1.0) as u8 } else { 0 },
                se: 0,
            };
            let _ = t0;
            p.rect_filled(seg, rounding, c);
        }
        // Bright rounded tip cap.
        let tip_x = rect.min.x + fill_w;
        if fill_w >= radius {
            p.circle_filled(
                egui::pos2(tip_x - radius / 2.0, rect.center().y),
                radius / 2.0,
                VU_TIP,
            );
        }
    }
}

/// A primary (green) button. Use sparingly — the main action per screen.
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .color(ON_ACCENT)
                .strong()
                .size(15.0),
        )
        .fill(ACCENT)
        .corner_radius(10.0),
    )
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::{apply_theme, ThemeMode};

    #[test]
    fn theme_mode_default_is_dark() {
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
    }

    #[test]
    fn theme_mode_toggle_flips() {
        assert_eq!(ThemeMode::Dark.toggle(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.toggle(), ThemeMode::Dark);
    }

    #[test]
    fn theme_mode_serde_roundtrip() {
        let dark: ThemeMode = serde_json::from_str("\"dark\"").unwrap();
        assert_eq!(dark, ThemeMode::Dark);
        let light: ThemeMode = serde_json::from_str("\"light\"").unwrap();
        assert_eq!(light, ThemeMode::Light);
        assert_eq!(
            serde_json::to_string(&ThemeMode::Light).unwrap(),
            "\"light\""
        );
    }

    #[test]
    fn apply_both_themes_switches_colors() {
        let ctx = egui::Context::default();
        apply_theme(&ctx, ThemeMode::Light);
        assert_eq!(super::title(), egui::Color32::from_rgb(16, 20, 24));
        apply_theme(&ctx, ThemeMode::Dark);
        assert_eq!(super::title(), egui::Color32::from_rgb(242, 244, 246));
    }
}
