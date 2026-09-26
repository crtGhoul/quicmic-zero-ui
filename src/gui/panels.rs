//! The five GUI screens. Each is a pure function of the live [`Snapshot`]
//! plus the shared handles in [`GuiCtx`]; mutations go straight to the same
//! atomics/locks the console commands use.

use std::sync::atomic::Ordering;

use eframe::egui;
use eframe::egui::RichText;

use super::app::GuiApp;
use super::{db_to_linear, is_recommended_device, open_url, rotate_pin, Snapshot};
use crate::server::{
    GAIN_MAX, GAIN_MIN, LATENCY_THRESHOLD_MAX_MS, NOISE_GATE_MAX, NOISE_GATE_MIN,
    OUTPUT_VOLUME_MAX, OUTPUT_VOLUME_MIN,
};

/// A `label: value` row with a Copy button.
fn copy_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).weak());
        ui.monospace(value);
        if ui.small_button("Copy").clicked() {
            ui.ctx().copy_text(value.to_owned());
        }
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.heading(RichText::new(title).size(16.0));
    ui.add_space(2.0);
}

fn status_dot(connected: bool) -> RichText {
    if connected {
        RichText::new("● Connected").color(egui::Color32::from_rgb(80, 200, 120))
    } else {
        RichText::new("○ Idle").color(egui::Color32::GRAY)
    }
}

// ── Status ────────────────────────────────────────────────────────────────

pub(super) fn status(_app: &mut GuiApp, ui: &mut egui::Ui, snap: &Snapshot) {
    section(ui, "Microphone");
    egui::Grid::new("mic-grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new("State").weak());
            ui.horizontal(|ui| {
                ui.label(status_dot(snap.connected));
                if snap.connected {
                    let who = snap
                        .phone_name
                        .clone()
                        .or(snap.mic_peer.clone())
                        .unwrap_or_else(|| "phone".to_string());
                    ui.label(RichText::new(who).strong());
                } else {
                    ui.label("Waiting for phone — pair from the Pair tab.");
                }
            });
            ui.end_row();

            if let Some(applied) = &snap.applied_mic_name {
                ui.label(RichText::new("Discord sees").weak());
                ui.monospace(applied);
                ui.end_row();
            }
            ui.label(RichText::new("Packets").weak());
            ui.label(format!(
                "{} received · {} lost ({:.2}%)",
                snap.packets_received,
                snap.packets_lost,
                snap.loss_percent()
            ));
            ui.end_row();

            ui.label(RichText::new("Buffer").weak());
            ui.label(format!(
                "{:.0} ms ({} / {} samples @ {} Hz)",
                snap.buffer_ms, snap.buffer_samples, snap.buffer_capacity, snap.source_sample_rate
            ));
            ui.end_row();

            ui.label(RichText::new("Output device").weak());
            ui.horizontal(|ui| {
                ui.label(
                    snap.device_name
                        .clone()
                        .unwrap_or_else(|| "system default".to_string()),
                );
                if !snap.device_ok {
                    ui.label(RichText::new("REBUILDING…").color(egui::Color32::YELLOW));
                }
            });
            ui.end_row();
        });

    section(ui, "Speaker (PC → phone)");
    if snap.speaker_running {
        ui.horizontal(|ui| {
            ui.label(status_dot(true));
            if snap.speaker_peers.is_empty() {
                ui.label("Capture running, no listeners.");
            } else {
                ui.label(format!(
                    "{} listener{}: {}",
                    snap.speaker_peers.len(),
                    if snap.speaker_peers.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    snap.speaker_peers.join(", ")
                ));
            }
        });
        ui.label(RichText::new(format!("Source: {}", snap.speaker_source)).weak());
    } else {
        ui.horizontal(|ui| {
            ui.label(status_dot(false));
            ui.label("Not running on this machine.");
        });
    }

    section(ui, "Hear-yourself monitor");
    ui.label(if snap.monitor_present {
        if snap.monitor_enabled {
            "On — mic audio also plays through the monitor device."
        } else {
            "Muted (toggle in Settings)."
        }
    } else {
        "Not enabled — restart with --monitor-device to create it."
    });

    section(ui, "File sharing");
    ui.horizontal(|ui| {
        ui.label("Phone ↔ PC file sharing:");
        if ui.link(crate::console::LOCALDROP_URL).clicked() {
            open_url(crate::console::LOCALDROP_URL);
        }
    });

    section(ui, "Audio settings (live)");
    ui.label(format!(
        "Volume {:.2}x · Gain {:.2}x · Gate {} · Latency {} ms",
        snap.volume,
        snap.gain,
        gate_label(snap.noise_gate_db),
        snap.latency_ms,
    ));
    ui.label(
        RichText::new("Note: the phone UI re-pushes its saved settings on every connect, so it can override values changed here.")
            .weak()
            .small(),
    );
}

fn gate_label(db: f32) -> String {
    if db <= -100.0 {
        "Off".to_string()
    } else {
        format!("{db:.0} dB")
    }
}

// ── Pair ──────────────────────────────────────────────────────────────────

pub(super) fn pairing(app: &mut GuiApp, ui: &mut egui::Ui, snap: &Snapshot) {
    ui.vertical_centered(|ui| {
        section(ui, "Pair your phone");
        if let Some(tex) = &app.qr_texture {
            let size = egui::vec2(300.0, 300.0);
            ui.image((tex.id(), size));
        } else {
            ui.label("Could not render the QR code.");
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.pin_visible, "Show PIN");
        });
        if app.pin_visible {
            ui.heading(RichText::new(&snap.pin).size(52.0).monospace());
            ui.label(RichText::new("Pairing PIN").weak());
        } else {
            ui.heading(RichText::new("••••••").size(52.0).monospace());
            ui.label(RichText::new("Pairing PIN (hidden)").weak());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("🔄 Rotate PIN").clicked() {
                match rotate_pin(&app.g) {
                    Ok(new_pin) => app.notify(format!("New pairing PIN: {new_pin}")),
                    Err(e) => app.notify(format!("Could not rotate PIN: {e:#}")),
                }
            }
            if ui.button("📋 Copy PIN").clicked() {
                ui.ctx().copy_text(snap.pin.clone());
                app.notify("PIN copied to clipboard.");
            }
            let qr_url = format!("https://{}:{}/qr", app.g.lan_ip, app.g.port);
            if ui.button("🌐 Open QR page").clicked() {
                open_url(&qr_url);
            }
        });
    });
    ui.add_space(8.0);
    ui.separator();
    ui.label(RichText::new("How to connect").strong());
    ui.label(format!(
        "1. Scan the QR code with your phone camera, or open\n   {} and enter the PIN.",
        app.g.url
    ));
    ui.label("2. Accept the certificate warning (self-signed LAN cert).");
    ui.label("3. Tap the mic button — you're live.");
    ui.add_space(4.0);
    ui.label(
        RichText::new("The PIN travels in the URL hash, never to the server. Rotating it revokes the current session — paired phones must pair again.")
            .weak()
            .small(),
    );
}

// ── Devices ───────────────────────────────────────────────────────────────

pub(super) fn devices(app: &mut GuiApp, ui: &mut egui::Ui, snap: &Snapshot) {
    section(ui, "Mic output device");
    ui.horizontal(|ui| {
        ui.label("Where the phone's audio plays on this PC.");
        if ui.button("🔄 Rescan").clicked() {
            app.devices_refresh.store(true, Ordering::Relaxed);
        }
    });
    ui.add_space(4.0);

    let current = snap.device_name.clone();
    let mut pick: Option<Option<String>> = None;

    if ui
        .radio(current.is_none(), "Automatic (recommended virtual cable)")
        .on_hover_text(
            "Find the platform virtual audio device automatically \
             (VB-Cable on Windows, BlackHole on macOS, VirtualQuicMic on Linux)",
        )
        .clicked()
    {
        pick = Some(None);
    }
    for name in snap.devices.iter().flatten() {
        let selected = current.as_deref() == Some(name.as_str());
        let mut label = name.clone();
        if is_recommended_device(name) {
            label.push_str("  ★ recommended");
        }
        if ui.radio(selected, label).clicked() {
            pick = Some(Some(name.clone()));
        }
    }
    if snap.devices.as_ref().is_none_or(Vec::is_empty) {
        ui.label(RichText::new("No devices found — is audio available?").weak());
    }

    if let Some(choice) = pick {
        *app.g.device_select.lock() = choice.clone();
        app.notify(format!(
            "Switching mic output to {}…",
            choice.as_deref().unwrap_or("automatic")
        ));
    }

    ui.add_space(8.0);
    ui.separator();
    ui.label(
        RichText::new("Switching rebuilds the audio stream live — no restart needed. The virtual cable (★ recommended) is what apps like Discord should use as their mic input.")
            .weak()
            .small(),
    );
}

// ── Settings ──────────────────────────────────────────────────────────────

pub(super) fn settings(app: &mut GuiApp, ui: &mut egui::Ui, snap: &Snapshot) {
    section(ui, "Audio");
    egui::Grid::new("settings-grid")
        .num_columns(3)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            // Output volume
            ui.label("Output volume");
            let mut volume = snap.volume;
            let changed = ui
                .add(
                    egui::Slider::new(&mut volume, OUTPUT_VOLUME_MIN..=OUTPUT_VOLUME_MAX)
                        .step_by(0.05),
                )
                .changed();
            ui.label(format!("{volume:.2}x"));
            if changed {
                let v = volume.clamp(OUTPUT_VOLUME_MIN, OUTPUT_VOLUME_MAX);
                app.g
                    .stream
                    .output_volume
                    .store(v.to_bits(), Ordering::Relaxed);
                app.snap.lock().volume = v;
            }
            ui.end_row();

            // Gain
            ui.label("Mic gain");
            let mut gain = snap.gain;
            let changed = ui
                .add(egui::Slider::new(&mut gain, GAIN_MIN..=GAIN_MAX).step_by(0.05))
                .changed();
            ui.label(format!("{gain:.2}x"));
            if changed {
                let v = gain.clamp(GAIN_MIN, GAIN_MAX);
                app.g.stream.gain.store(v.to_bits(), Ordering::Relaxed);
                app.snap.lock().gain = v;
            }
            ui.end_row();

            // Noise gate (dB)
            ui.label("Noise gate");
            let mut gate_db = snap.noise_gate_db;
            let changed = ui
                .add(egui::Slider::new(&mut gate_db, -100.0..=0.0).step_by(1.0))
                .changed();
            ui.label(gate_label(gate_db));
            if changed {
                let linear = db_to_linear(gate_db).clamp(NOISE_GATE_MIN, NOISE_GATE_MAX);
                app.g
                    .stream
                    .noise_gate
                    .store(linear.to_bits(), Ordering::Relaxed);
                app.snap.lock().noise_gate_db = gate_db;
            }
            ui.end_row();

            // Latency recovery
            ui.label("Latency recovery");
            let mut latency = snap.latency_ms;
            let changed = ui
                .add(egui::Slider::new(&mut latency, 0..=LATENCY_THRESHOLD_MAX_MS).step_by(5.0))
                .changed();
            ui.label(format!("{latency} ms"));
            if changed {
                let ms = latency.min(LATENCY_THRESHOLD_MAX_MS);
                app.g.stream.latency_threshold.store(ms, Ordering::Relaxed);
                app.snap.lock().latency_ms = ms;
            }
            ui.end_row();
        });
    ui.label(
        RichText::new("The phone UI re-pushes its saved volume/gain/gate/latency on every connect, so it can override values changed here.")
            .weak()
            .small(),
    );

    section(ui, "Hear-yourself monitor");
    let mut monitor = snap.monitor_enabled;
    ui.add_enabled(
        snap.monitor_present,
        egui::Checkbox::new(&mut monitor, "Monitor audible"),
    );
    if monitor != snap.monitor_enabled && snap.monitor_present {
        app.g
            .stream
            .monitor_enabled
            .store(monitor, Ordering::Relaxed);
        app.snap.lock().monitor_enabled = monitor;
        app.notify(if monitor {
            "Monitor unmuted."
        } else {
            "Monitor muted."
        });
    }
    if !snap.monitor_present {
        ui.label(
            RichText::new("No monitor stream — restart with --monitor-device to enable it.")
                .weak()
                .small(),
        );
    }

    section(ui, "Mic input name (what Discord lists)");
    // Runtime rename-mode selector. Auto renames the endpoint to the paired
    // phone's name on every pairing; Fixed uses the name below; Off never
    // renames automatically. Takes effect immediately (the pair handler reads
    // the shared mode); the startup `--rename-mic` flag only sets the
    // initial value.
    {
        use crate::mic_name::MicRenameMode;
        let mut sel: u8 = match *app.g.mic_rename_mode.lock() {
            MicRenameMode::Auto => 0,
            MicRenameMode::Off => 1,
            MicRenameMode::Fixed(_) => 2,
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Mode").weak());
            ui.radio_value(&mut sel, 0, "Auto");
            ui.radio_value(&mut sel, 1, "Off");
            ui.radio_value(&mut sel, 2, "Fixed");
        });
        if sel == 2 {
            ui.horizontal(|ui| {
                ui.label("Fixed name:");
                ui.text_edit_singleline(&mut app.rename_fixed_name);
            });
        }
        let fixed = app.rename_fixed_name.trim().to_string();
        // An empty fixed name would be meaningless — keep the previous mode
        // until the user types one.
        let new_mode = match sel {
            0 => Some(MicRenameMode::Auto),
            1 => Some(MicRenameMode::Off),
            _ if !fixed.is_empty() => Some(MicRenameMode::Fixed(fixed)),
            _ => None,
        };
        if let Some(new_mode) = new_mode {
            let mut guard = app.g.mic_rename_mode.lock();
            if *guard != new_mode {
                *guard = new_mode;
                drop(guard);
                app.notify("Mic rename mode updated.");
            }
        }
        if sel == 2 && app.rename_fixed_name.trim().is_empty() {
            ui.label(
                RichText::new("Type a fixed name to switch to Fixed mode.")
                    .weak()
                    .small(),
            );
        }
    }
    ui.horizontal(|ui| {
        ui.label("Rename now to:");
        ui.text_edit_singleline(&mut app.rename_name);
        if ui.button("Apply").clicked() {
            let wanted = app.rename_name.trim();
            match crate::mic_name::rename_mic(Some(wanted)) {
                Ok(name) => {
                    *app.g.applied_mic_name.lock() = Some(name.clone());
                    app.notify(format!("Mic input renamed to \"{name}\"."));
                }
                Err(e) => app.notify(format!("{e:#}")),
            }
        }
    });
    if let Some(applied) = &snap.applied_mic_name {
        ui.label(RichText::new(format!("Currently applied: {applied}")).weak());
    }

    section(ui, "Updates");
    // The checkbox is phrased positively ("check for updates") while the
    // preference stores the opt-out: invert in both directions.
    let mut update_check = !app.update_opt_out;
    if ui
        .checkbox(&mut update_check, "Check for updates at startup")
        .changed()
    {
        app.update_opt_out = !update_check;
        let prefs = super::GuiPrefs {
            update_check_opt_out: app.update_opt_out,
        };
        match super::save_prefs(&app.g.prefs_path, &prefs) {
            Ok(()) => app.notify("Saved — takes effect on next start."),
            Err(e) => app.notify(format!("Could not save preference: {e:#}")),
        }
    }
    ui.label(
        RichText::new(if app.g.update_check_ran {
            "The startup update check ran this session."
        } else {
            "The startup update check was skipped (--no-update-check, env, or this opt-out)."
        })
        .weak()
        .small(),
    );
    ui.horizontal(|ui| {
        if ui.button("Check now").clicked() {
            let slot = app.g.update_status.clone();
            tokio::spawn(async move {
                if let Some(tag) = crate::update_check::latest_if_newer().await {
                    *slot.lock() = Some(tag);
                }
            });
            app.notify("Checking GitHub for a newer release…");
        }
        if ui.button("⬆ Install update & restart").clicked() {
            app.notify("Downloading the latest release…");
            let shutdown = app.g.shutdown_requested.clone();
            let egui_ctx = app.g.egui_ctx.clone();
            tokio::spawn(async move {
                match crate::self_update::run_update().await {
                    Ok(true) => {
                        // The updater staged a new exe; shut down so its
                        // restart script can swap it in.
                        shutdown.store(true, Ordering::SeqCst);
                        if let Some(ctx) = egui_ctx.lock().clone() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!("self-update failed: {e:#}"),
                }
            });
        }
    });
    if let Some(tag) = &snap.update_available {
        ui.label(RichText::new(format!("Update available: {tag}")).strong());
    }
}

// ── Diagnostics ───────────────────────────────────────────────────────────

pub(super) fn diagnostics(app: &mut GuiApp, ui: &mut egui::Ui, snap: &Snapshot) {
    section(ui, "Diagnostics");
    copy_row(ui, "Version:", env!("CARGO_PKG_VERSION"));
    copy_row(ui, "LAN IP:", &app.g.lan_ip);
    copy_row(ui, "Port:", &app.g.port.to_string());
    copy_row(ui, "URL:", &app.g.url);
    copy_row(ui, "Pairing PIN:", &snap.pin);
    copy_row(ui, "Cert SHA-256:", &app.g.cert_hash);
    ui.add_space(4.0);
    ui.separator();

    section(ui, "Runtime");
    egui::Grid::new("diag-grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Mic client").weak());
            ui.label(snap.mic_peer.clone().unwrap_or_else(|| "—".to_string()));
            ui.end_row();

            ui.label(RichText::new("Transport").weak());
            // The server does not record which transport the active session
            // uses; both WebTransport (QUIC/UDP) and WebSocket (TCP) feed the
            // same pipeline. Tracked as a "Needs coordinator" item.
            ui.label("not tracked by the server");
            ui.end_row();

            ui.label(RichText::new("Audio device").weak());
            ui.horizontal(|ui| {
                ui.label(
                    snap.device_name
                        .clone()
                        .unwrap_or_else(|| "system default".to_string()),
                );
                ui.label(if snap.device_ok {
                    RichText::new("OK").color(egui::Color32::from_rgb(80, 200, 120))
                } else {
                    RichText::new("REBUILDING").color(egui::Color32::YELLOW)
                });
            });
            ui.end_row();

            ui.label(RichText::new("Speaker capture").weak());
            ui.label(if snap.speaker_running {
                format!("running ({})", snap.speaker_source)
            } else {
                "not running".to_string()
            });
            ui.end_row();

            ui.label(RichText::new("Update check").weak());
            ui.label(if let Some(tag) = &snap.update_available {
                format!("newer release available: {tag}")
            } else if app.g.update_check_ran {
                "up to date (this session)".to_string()
            } else {
                "skipped".to_string()
            });
            ui.end_row();

            ui.label(RichText::new("Data dir").weak());
            ui.monospace(app.g.data_dir.display().to_string());
            ui.end_row();
        });

    ui.add_space(4.0);
    ui.separator();
    copy_row(
        ui,
        "Connection QR page:",
        &format!("https://{}:{}/qr", app.g.lan_ip, app.g.port),
    );
}
