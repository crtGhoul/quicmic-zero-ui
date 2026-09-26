//! The eframe application: window layout, tab navigation, top status bar,
//! and the per-frame snapshot read.
//!
//! eframe 0.36 splits the frame into [`eframe::App::logic`] (non-UI work with
//! the [`egui::Context`]) and [`eframe::App::ui`] (rendering into a root
//! [`egui::Ui`]). Tray menu events are handled by the main task in
//! `run_gui_mode` (so a hidden window can still be reshown); `logic` only
//! reads state, `ui` only renders.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use super::panels;
use super::theme;
use super::{GuiCtx, Snapshot};

/// The five GUI screens, mirroring the console's command groups.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Status,
    Pair,
    Devices,
    Settings,
    Diagnostics,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Status => "Status",
            Tab::Pair => "Pair",
            Tab::Devices => "Devices",
            Tab::Settings => "Settings",
            Tab::Diagnostics => "Diagnostics",
        }
    }

    fn all() -> [Tab; 5] {
        [
            Tab::Status,
            Tab::Pair,
            Tab::Devices,
            Tab::Settings,
            Tab::Diagnostics,
        ]
    }
}

pub struct GuiApp {
    pub(super) g: GuiCtx,
    pub(super) snap: Arc<parking_lot::Mutex<Snapshot>>,
    pub(super) devices_refresh: Arc<AtomicBool>,
    pub(super) tab: Tab,
    /// Cached QR texture and the PIN it was rendered for.
    pub(super) qr_texture: Option<egui::TextureHandle>,
    pub(super) qr_pin: String,
    /// Rename-mic text field buffer.
    pub(super) rename_name: String,
    /// Fixed-name buffer for the rename-mode selector below it.
    pub(super) rename_fixed_name: String,
    /// Whether the pairing PIN digits are shown (vs masked).
    pub(super) pin_visible: bool,
    /// Update-check opt-out checkbox (persisted to gui_prefs.json).
    pub(super) update_opt_out: bool,
    /// Transient feedback line shown at the bottom of the active panel.
    pub(super) status_msg: Option<(String, Instant)>,
    /// The latest snapshot, refreshed in `logic` and rendered in `ui`.
    snap_now: Snapshot,
}

impl GuiApp {
    pub fn new(
        g: GuiCtx,
        snap: Arc<parking_lot::Mutex<Snapshot>>,
        devices_refresh: Arc<AtomicBool>,
    ) -> Self {
        let update_opt_out = super::load_prefs(&g.prefs_path).update_check_opt_out;
        // Prefill the fixed-name field when the server started with one.
        let rename_fixed_name = match &*g.mic_rename_mode.lock() {
            crate::mic_name::MicRenameMode::Fixed(name) => name.clone(),
            _ => String::new(),
        };
        Self {
            g,
            snap,
            devices_refresh,
            tab: Tab::Status,
            qr_texture: None,
            qr_pin: String::new(),
            rename_name: String::new(),
            rename_fixed_name,
            pin_visible: true,
            update_opt_out,
            status_msg: None,
            snap_now: Snapshot::default(),
        }
    }

    /// Handle for the shared live-context slot, published to background
    /// watchers (Ctrl+C, `/api/update`) so they can close the window.
    pub fn egui_ctx_handle(&self) -> Arc<parking_lot::Mutex<Option<egui::Context>>> {
        self.g.egui_ctx.clone()
    }

    pub(super) fn notify(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    fn take_status_msg(&mut self) -> Option<String> {
        let (msg, at) = self.status_msg.take()?;
        if at.elapsed() < Duration::from_secs(8) {
            Some(msg)
        } else {
            None
        }
    }

    /// Rebuild the QR texture when the PIN changed (rotation via the GUI's
    /// "Rotate PIN" button is the only thing that changes it).
    fn refresh_qr(&mut self, ctx: &egui::Context, pin: &str) {
        if *pin == self.qr_pin && self.qr_texture.is_some() {
            return;
        }
        let url = format!("{}#{}", self.g.url, pin);
        // 5 px per module, 4-module quiet zone: crisp at 300 px display size.
        if let Some((px, side)) = super::render_qr_gray(&url, 5, 4) {
            let image = egui::ColorImage::from_gray([side, side], &px);
            let tex = ctx.load_texture("pairing-qr", image, egui::TextureOptions::NEAREST);
            self.qr_texture = Some(tex);
            self.qr_pin = pin.to_string();
        }
    }
}

impl eframe::App for GuiApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // An external shutdown request (Ctrl+C watcher, `/api/update`) closes
        // the window; the main task then runs the graceful shutdown.
        if self.g.shutdown_requested.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // One cheap clone per frame; the poller thread owns the write side.
        let snap = self.snap.lock().clone();
        self.refresh_qr(ctx, &snap.pin);
        self.snap_now = snap;

        // Live stats without burning CPU: repaint twice a second; input events
        // (clicks, typing) still repaint immediately.
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let snap = self.snap_now.clone();

        // ── Header card: mic tile, title, connection pill ─────────────
        egui::Panel::top("topbar").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                theme::mic_tile(ui, 40.0);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.heading(
                        egui::RichText::new("QuicMic")
                            .color(theme::TITLE)
                            .size(24.0),
                    );
                    ui.label(
                        egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                            .weak()
                            .small(),
                    );
                });
                ui.add_space(12.0);
                if snap.connected {
                    let who = snap
                        .phone_name
                        .clone()
                        .or(snap.mic_peer.clone())
                        .unwrap_or_else(|| "phone".to_string());
                    theme::status_pill(ui, true, &format!("Connected: {who}"));
                } else {
                    theme::status_pill(ui, false, "Waiting for phone…");
                }
                if let Some(tag) = &snap.update_available {
                    ui.add_space(8.0);
                    if ui
                        .link(format!("⬆ Update {tag} available"))
                        .on_hover_text("Open the Settings tab to install")
                        .clicked()
                    {
                        self.tab = Tab::Settings;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    if ui.button("⏻ Quit").clicked() {
                        self.g.shutdown_requested.store(true, Ordering::SeqCst);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(4.0);
        });

        // ── Left nav ───────────────────────────────────────────────
        egui::Panel::left("nav")
            .resizable(false)
            .default_size(160.0)
            .show(ui, |ui| {
                ui.add_space(10.0);
                for tab in Tab::all() {
                    let selected = self.tab == tab;
                    let label = egui::RichText::new(tab.label())
                        .size(15.0)
                        .color(if selected {
                            theme::ACCENT_TEXT
                        } else {
                            theme::TEXT
                        });
                    let btn = egui::Button::new(label)
                        .fill(if selected {
                            theme::accent_dim()
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .corner_radius(10.0);
                    if ui
                        .add_sized(egui::vec2(ui.available_width(), 38.0), btn)
                        .clicked()
                    {
                        self.tab = tab;
                        if tab == Tab::Devices {
                            // Fresh enumeration every time the tab opens.
                            self.devices_refresh.store(true, Ordering::Relaxed);
                        }
                    }
                    ui.add_space(4.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("{}:{}", self.g.lan_ip, self.g.port))
                            .weak()
                            .small(),
                    );
                });
            });

        // ── Bottom status bar ──────────────────────────────────────
        egui::Panel::bottom("statusbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let state = if snap.connected { "Live" } else { "Ready" };
                ui.label(
                    egui::RichText::new(format!(
                        "{state} · v{} · {}:{}",
                        env!("CARGO_PKG_VERSION"),
                        self.g.lan_ip,
                        self.g.port
                    ))
                    .weak()
                    .small(),
                );
            });
        });

        // ── Content ────────────────────────────────────────────────
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            match self.tab {
                Tab::Status => panels::status(self, ui, &snap),
                Tab::Pair => panels::pairing(self, ui, &snap),
                Tab::Devices => panels::devices(self, ui, &snap),
                Tab::Settings => panels::settings(self, ui, &snap),
                Tab::Diagnostics => panels::diagnostics(self, ui, &snap),
            }
            if let Some(msg) = self.take_status_msg() {
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new(msg).italics().weak());
            }
        });
    }
}
