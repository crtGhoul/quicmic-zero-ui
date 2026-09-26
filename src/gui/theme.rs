//! QuicMic dark theme, matching the Android app's visual language.
//!
//! Near-black surfaces (#0B0D10 family) with a single green accent (#4ADE80)
//! reserved for status, primary actions, and the VU meter. Visuals only:
//! colors, rounding, spacing, and type scale. No control is added, removed,
//! renamed, or reordered here — every widget the panels build keeps its place
//! and behavior; this module only changes how they look.

use eframe::egui;

/// Primary accent: Android green. Used for the selected tab pill, toggles,
/// links, the connection pill, and the VU meter.
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
/// Dim accent fill for selected states.
pub fn accent_dim() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(74, 222, 128, 36)
}
/// Bright accent text for selected labels.
pub const ACCENT_TEXT: egui::Color32 = egui::Color32::from_rgb(134, 239, 172);
/// Dark text drawn on top of the green accent (mic tile, pills).
pub const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(6, 20, 12);
/// The "QuicMic" title in the header.
pub const TITLE: egui::Color32 = egui::Color32::from_rgb(242, 244, 246);
/// Section headings.
pub const HEADING: egui::Color32 = egui::Color32::from_rgb(237, 239, 242);
/// Body text.
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(221, 225, 230);
/// Green "live" indicator.
pub const LIVE: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
/// Muted gray "idle" indicator.
pub const IDLE: egui::Color32 = egui::Color32::from_rgb(120, 128, 136);

/// Window background: near-black.
const BG_WINDOW: egui::Color32 = egui::Color32::from_rgb(11, 13, 16);
/// Card surfaces.
pub const BG_CARD: egui::Color32 = egui::Color32::from_rgb(18, 22, 26);
/// Panels (top bar, nav rail).
const BG_PANEL: egui::Color32 = egui::Color32::from_rgb(14, 17, 20);
/// Text fields and other sunken inputs.
const BG_INPUT: egui::Color32 = egui::Color32::from_rgb(13, 16, 19);
/// Buttons.
const BG_BUTTON: egui::Color32 = egui::Color32::from_rgb(28, 34, 40);
const BG_BUTTON_HOVER: egui::Color32 = egui::Color32::from_rgb(36, 44, 52);
const BG_BUTTON_ACTIVE: egui::Color32 = egui::Color32::from_rgb(30, 60, 42);
/// Hairline borders around cards and widgets.
const STROKE_SUBTLE: egui::Color32 = egui::Color32::from_rgb(35, 43, 51);
/// VU meter track.
const VU_TRACK: egui::Color32 = egui::Color32::from_rgb(24, 30, 36);
/// VU meter fill tip (bright end of the gradient).
const VU_TIP: egui::Color32 = egui::Color32::from_rgb(134, 239, 172);

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
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.panel_fill = BG_PANEL;
    visuals.faint_bg_color = BG_CARD;
    visuals.extreme_bg_color = BG_INPUT;
    visuals.code_bg_color = BG_INPUT;
    visuals.hyperlink_color = ACCENT_TEXT;
    visuals.selection = egui::style::Selection {
        bg_fill: accent_dim(),
        stroke: egui::Stroke::new(1.0, ACCENT),
    };
    visuals.widgets.noninteractive = widget(BG_CARD, TEXT, 12);
    visuals.widgets.inactive = widget(BG_BUTTON, TEXT, 10);
    visuals.widgets.hovered = widget(BG_BUTTON_HOVER, TEXT, 10);
    visuals.widgets.active = widget(BG_BUTTON_ACTIVE, ACCENT_TEXT, 10);
    visuals.widgets.open = widget(BG_BUTTON_ACTIVE, ACCENT_TEXT, 10);
    // Sliders and progress bars read the selection color for their fill.
    ctx.set_visuals_of(egui::Theme::Dark, visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
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
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// A rounded dark card: the standard content container, matching the
/// Android cards.
pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(BG_CARD)
        .stroke(egui::Stroke::new(1.0, STROKE_SUBTLE))
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
        (BG_BUTTON, IDLE)
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
    p.rect_filled(rect, radius, VU_TRACK);
    p.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, STROKE_SUBTLE),
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
