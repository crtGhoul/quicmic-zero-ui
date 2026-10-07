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
use super::{GuiCtx, Snapshot};

/// The GUI screens. Home is the WO Mic-style single screen (status + pair
/// QR + hear-yourself); the rest mirror the console's command groups and
/// hide behind "Advanced" while simple mode is on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Home,
    Status,
    Pair,
    Devices,
    Settings,
    Diagnostics,
    Guide,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Status => "Status",
            Tab::Pair => "Pair",
            Tab::Devices => "Devices",
            Tab::Settings => "Settings",
            Tab::Diagnostics => "Diagnostics",
            Tab::Guide => "Guide",
        }
    }

    fn all() -> [Tab; 7] {
        [
            Tab::Home,
            Tab::Status,
            Tab::Pair,
            Tab::Devices,
            Tab::Settings,
            Tab::Diagnostics,
            Tab::Guide,
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
    /// Rename-mic text field buffer. Used by Fixed mode and by "Apply now".
    pub(super) rename_name: String,
    /// Whether the pairing PIN digits are shown (vs masked).
    pub(super) pin_visible: bool,
    /// WO Mic-style single-screen UI. When true the nav shows only Home and
    /// the full tab set hides behind "Advanced".
    pub(super) simple_mode: bool,
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
        let prefs = super::load_prefs(&g.prefs_path);
        let update_opt_out = prefs.update_check_opt_out;
        let simple_mode = prefs.simple_mode;
        // Prefill the name field when the server started with a fixed name.
        let rename_name = match &*g.mic_rename_mode.lock() {
            crate::mic_name::MicRenameMode::Fixed(name) => name.clone(),
            _ => String::new(),
        };
        Self {
            g,
            snap,
            devices_refresh,
            tab: Tab::Home,
            qr_texture: None,
            qr_pin: String::new(),
            rename_name,
            pin_visible: true,
            update_opt_out,
            simple_mode,
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

    /// Flip simple mode and persist it. Turning it on lands on Home;
    /// turning it off reveals the full tab set.
    pub(super) fn set_simple_mode(&mut self, on: bool) {
        self.simple_mode = on;
        if on {
            self.tab = Tab::Home;
        }
        let prefs = super::GuiPrefs {
            update_check_opt_out: self.update_opt_out,
            simple_mode: on,
        };
        match super::save_prefs(&self.g.prefs_path, &prefs) {
            Ok(()) => self.notify(if on {
                "Simple mode on — Home screen only."
            } else {
                "Advanced mode — all tabs visible."
            }),
            Err(e) => self.notify(format!("Could not save preference: {e:#}")),
        }
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

        // Surface the self-updater's result: the install button's background
        // task reports here, so a failed update tells the user what went
        // wrong instead of silently doing nothing.
        let update_notice = self.g.update_notice.lock().take();
        if let Some(msg) = update_notice {
            self.notify(msg);
        }

        // ── Top bar ────────────────────────────────────────────────
        egui::Panel::top("topbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("QuicMic");
                ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                ui.separator();
                let (dot, text) = if snap.connected {
                    // Identity slot: the phone's name, never its IP address
                    // (the IP is shown in Diagnostics where it belongs).
                    let who = snap
                        .phone_name
                        .clone()
                        .unwrap_or_else(|| "phone".to_string());
                    ("🟢", format!("Connected: {who}"))
                } else {
                    ("🔴", "Waiting for phone…".to_string())
                };
                ui.label(format!("{dot} {text}"));
                if let Some(tag) = &snap.update_available {
                    ui.separator();
                    if ui
                        .link(format!("⬆ Update {tag} available"))
                        .on_hover_text("Open Settings to install")
                        .clicked()
                    {
                        if self.simple_mode {
                            // Settings hides behind Advanced in simple mode.
                            self.set_simple_mode(false);
                        }
                        self.tab = Tab::Settings;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⏻ Quit").clicked() {
                        self.g.shutdown_requested.store(true, Ordering::SeqCst);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });

        // ── Left nav ───────────────────────────────────────────────
        // Simple mode: Home plus a single way out. Advanced mode: the full
        // tab set (Home stays first — it's a fine landing either way).
        egui::Panel::left("nav")
            .resizable(false)
            .default_size(150.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                if self.simple_mode {
                    let selected = self.tab == Tab::Home;
                    if ui.selectable_label(selected, "  Home").clicked() {
                        self.tab = Tab::Home;
                    }
                    ui.add_space(2.0);
                    if ui.selectable_label(false, "  ⚙ Advanced").clicked() {
                        self.set_simple_mode(false);
                        self.tab = Tab::Status;
                    }
                } else {
                    for tab in Tab::all() {
                        let selected = self.tab == tab;
                        if ui
                            .selectable_label(selected, format!("  {}", tab.label()))
                            .clicked()
                        {
                            self.tab = tab;
                            if tab == Tab::Devices {
                                // Fresh enumeration every time the tab opens.
                                self.devices_refresh.store(true, Ordering::Relaxed);
                            }
                        }
                        ui.add_space(2.0);
                    }
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

        // ── Content ────────────────────────────────────────────────
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            match self.tab {
                Tab::Home => panels::home(self, ui, &snap),
                Tab::Status => panels::status(self, ui, &snap),
                Tab::Pair => panels::pairing(self, ui, &snap),
                Tab::Devices => panels::devices(self, ui, &snap),
                Tab::Settings => panels::settings(self, ui, &snap),
                Tab::Diagnostics => panels::diagnostics(self, ui, &snap),
                Tab::Guide => panels::guide(self, ui, &snap),
            }
            if let Some(msg) = self.take_status_msg() {
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new(msg).italics().weak());
            }
        });
    }
}
