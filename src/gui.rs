//! Native GUI (egui/eframe): the default UI when a display is available.
//!
//! The GUI is a thin presentation layer over the existing server state — it
//! never touches the audio/transport core. A background poller thread reads
//! the shared [`StreamState`] atomics at ~3 Hz into a [`Snapshot`]; the UI
//! thread only ever reads the snapshot, so it can never block on audio or
//! network work. All mutations go through the same shared handles the console
//! commands use (device selection, DSP atomics, PIN rotation), so the phone
//! web UI and the GUI stay consistent.
//!
//! Layout: `app` holds the eframe application, `panels` the five screens
//! (Status, Pair, Devices, Settings, Diagnostics).

pub mod app;
mod panels;
pub(crate) mod theme;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::mic_name::MicRenameMode;
use crate::server::StreamState;

/// How often the background poller refreshes the snapshot.
const POLL_INTERVAL: Duration = Duration::from_millis(333);
/// Re-enumerate audio devices this often even without a manual rescan, so a
/// plugged-in virtual cable shows up on its own.
const DEVICE_REFRESH_POLLS: u64 = 30;

/// Everything the GUI needs, built once in `main`. All handles are the same
/// shared state the console commands and the HTTP API use — the GUI adds no
/// new state of its own (beyond the snapshot cache).
pub struct GuiCtx {
    pub stream: StreamState,
    /// Pairing URL without the PIN, e.g. `https://192.168.1.42:8443`.
    pub url: String,
    /// Shared with the API's pairing check; the GUI's "Rotate PIN" updates it
    /// live exactly like the console's `newpin`.
    pub pin: Arc<parking_lot::Mutex<String>>,
    pub data_dir: PathBuf,
    /// Base64 SHA-256 of the server certificate (diagnostics panel).
    pub cert_hash: String,
    /// Shared with the audio supervisor: changing the selection rebuilds the
    /// output stream live, same as the console's `device` command.
    pub device_select: Arc<parking_lot::Mutex<Option<String>>>,
    pub speaker_running: Arc<AtomicBool>,
    /// Live test-tone state: true while the synthetic 440 Hz tone (rather
    /// than real capture) feeds the speaker stream. Initialized from the
    /// `--speaker-test-tone` CLI flag; the GUI's Status tab can flip it at
    /// runtime. Default behavior is unchanged: off unless enabled.
    pub speaker_test_tone: Arc<AtomicBool>,
    /// Whether the process started with `--speaker-test-tone`. The CLI synth
    /// thread runs forever and can't be stopped, so switching back to real
    /// capture requires a restart in that case.
    pub cli_test_tone: bool,
    /// The speaker broadcast channel (None when speaker capture never
    /// started on this machine). Needed to start a source live.
    pub speaker_tx: Option<tokio::sync::broadcast::Sender<Vec<f32>>>,
    /// Bumping this stops the current speaker source (WASAPI thread and the
    /// GUI tone thread both exit on a generation change).
    pub speaker_generation: Arc<AtomicU64>,
    pub speaker_device: Arc<parking_lot::Mutex<Option<String>>>,
    pub monitor_present: bool,
    pub phone_device_name: Arc<parking_lot::Mutex<Option<String>>>,
    /// How the capture endpoint's display name is managed. Shared with the
    /// server so the Settings tab can switch auto/off/fixed at runtime; the
    /// pair handler reads it on every pairing.
    pub mic_rename_mode: Arc<parking_lot::Mutex<MicRenameMode>>,
    pub applied_mic_name: Arc<parking_lot::Mutex<Option<String>>>,
    pub update_status: Arc<parking_lot::Mutex<Option<String>>>,
    pub port: u16,
    pub lan_ip: String,
    /// Whether the startup update check ran (false with `--no-update-check` /
    /// env / GUI opt-out). Display-only; the check already happened or not.
    pub update_check_ran: bool,
    /// Where the GUI persists its own small preferences (update-check opt-out).
    pub prefs_path: PathBuf,
    /// Set when anything (Quit button, tray, Ctrl+C, updater) asks the app to
    /// exit; the main task then runs the normal graceful shutdown.
    pub shutdown_requested: Arc<AtomicBool>,
    /// The live egui context, filled in by the app once eframe is running, so
    /// background watchers (Ctrl+C, `/api/update`) can close the window.
    pub egui_ctx: Arc<parking_lot::Mutex<Option<eframe::egui::Context>>>,
}

/// Point-in-time copy of the live server state, refreshed by the poller
/// thread. The UI thread reads this and nothing else.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub connected: bool,
    pub mic_peer: Option<String>,
    pub phone_name: Option<String>,
    pub packets_received: u64,
    pub packets_lost: u64,
    pub buffer_samples: usize,
    pub buffer_capacity: usize,
    /// Samples currently buffered, in milliseconds at the active source rate.
    pub buffer_ms: f64,
    pub source_sample_rate: u32,
    pub device_ok: bool,
    /// Current mic output device selection (`None` = system default).
    pub device_name: Option<String>,
    /// Output device list. `None` = no refresh this tick (keep the previous
    /// list); `Some` (possibly empty) = freshly enumerated — always replaces.
    pub devices: Option<Vec<String>>,
    pub speaker_peers: Vec<String>,
    pub speaker_running: bool,
    pub speaker_source: String,
    /// Live test-tone state (mirrors `GuiCtx::speaker_test_tone`).
    pub speaker_test_tone: bool,
    /// Noise gate in dB for display (-100 = off).
    pub noise_gate_db: f32,
    pub gain: f32,
    pub volume: f32,
    pub latency_ms: u32,
    pub monitor_present: bool,
    pub monitor_enabled: bool,
    pub pin: String,
    pub applied_mic_name: Option<String>,
    pub update_available: Option<String>,
}

impl Snapshot {
    pub fn loss_percent(&self) -> f64 {
        let total = self.packets_received + self.packets_lost;
        if total == 0 {
            0.0
        } else {
            100.0 * self.packets_lost as f64 / total as f64
        }
    }
}

/// The subset of [`GuiCtx`] the background poller thread reads. Kept separate
/// so the poller never needs the egui context or other UI-only handles.
#[derive(Clone)]
pub struct PollInputs {
    pub stream: StreamState,
    pub pin: Arc<parking_lot::Mutex<String>>,
    pub device_select: Arc<parking_lot::Mutex<Option<String>>>,
    pub speaker_running: Arc<AtomicBool>,
    pub speaker_test_tone: Arc<AtomicBool>,
    pub speaker_device: Arc<parking_lot::Mutex<Option<String>>>,
    pub phone_device_name: Arc<parking_lot::Mutex<Option<String>>>,
    pub applied_mic_name: Arc<parking_lot::Mutex<Option<String>>>,
    pub update_status: Arc<parking_lot::Mutex<Option<String>>>,
    pub monitor_present: bool,
}

impl GuiCtx {
    pub fn poll_inputs(&self) -> PollInputs {
        PollInputs {
            stream: self.stream.clone(),
            pin: self.pin.clone(),
            device_select: self.device_select.clone(),
            speaker_running: self.speaker_running.clone(),
            speaker_test_tone: self.speaker_test_tone.clone(),
            speaker_device: self.speaker_device.clone(),
            phone_device_name: self.phone_device_name.clone(),
            applied_mic_name: self.applied_mic_name.clone(),
            update_status: self.update_status.clone(),
            monitor_present: self.monitor_present,
        }
    }
}

/// Whether a display server is reachable. GUI mode is only offered when this
/// is true; otherwise the app falls back to the terminal console.
pub fn display_available() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        // X11 or Wayland session.
        std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
    }
}

/// Open a URL in the default browser, best-effort. The `open` crate is a
/// Windows-only dependency, so other platforms shell out to `xdg-open`.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = open::that_detached(url);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Convert the linear noise-gate amplitude to dB for display (-100 = off),
/// mirroring the console's `linear_to_db`.
pub fn linear_to_db(v: f32) -> f32 {
    if v <= 0.0 {
        -100.0
    } else {
        20.0 * v.log10()
    }
}

/// Convert a dB slider value back to the linear amplitude the audio path uses
/// (-100 dB = hard 0.0, gate disabled), mirroring `noise_gate_db_to_linear`.
pub fn db_to_linear(db: f32) -> f32 {
    if db <= -100.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Render `text` as a grayscale QR bitmap (0 = black, 255 = white) with a
/// quiet-zone border, scaled so each module is `scale` pixels. Returns the
/// pixels and the image width (= height). `None` when the text does not fit
/// in a QR code.
pub fn render_qr_gray(text: &str, scale: usize, border_modules: usize) -> Option<(Vec<u8>, usize)> {
    use qrcode::Color;
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let modules = code.width();
    let side_modules = modules + 2 * border_modules;
    let side_px = side_modules * scale;
    let mut px = vec![255u8; side_px * side_px];
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color == Color::Dark {
            let mx = i % modules;
            let my = i / modules;
            for dy in 0..scale {
                for dx in 0..scale {
                    let x = (mx + border_modules) * scale + dx;
                    let y = (my + border_modules) * scale + dy;
                    px[y * side_px + x] = 0;
                }
            }
        }
    }
    Some((px, side_px))
}

/// Whether `name` looks like the platform's recommended virtual-cable device.
/// Mirrors the `DEFAULT_DEVICE` substring matching in `audio::output`.
pub fn is_recommended_device(name: &str) -> bool {
    let n = name.to_lowercase();
    #[cfg(target_os = "windows")]
    {
        n.contains("cable input")
    }
    #[cfg(target_os = "macos")]
    {
        n.contains("blackhole")
    }
    #[cfg(target_os = "linux")]
    {
        n.contains("virtualquicmic")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = n;
        false
    }
}

/// Small GUI-owned preferences, persisted next to the server identity so they
/// survive restarts. Only holds things the server itself has no place for.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct GuiPrefs {
    /// Skip the startup update check. Read by `main` before the check runs;
    /// the CLI flag and `QUICMIC_NO_UPDATE_CHECK` still take precedence.
    pub update_check_opt_out: bool,
}

pub fn prefs_path(data_dir: &Path) -> PathBuf {
    data_dir.join("gui_prefs.json")
}

pub fn load_prefs(path: &Path) -> GuiPrefs {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_prefs(path: &Path, prefs: &GuiPrefs) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(prefs)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

/// Rotate the pairing PIN live and revoke the current session token, exactly
/// like the console's `newpin` command. Returns the new PIN.
pub fn rotate_pin(g: &GuiCtx) -> anyhow::Result<String> {
    let new_pin = format!("{:06}", rand::random_range(0..1_000_000u32));
    crate::identity::save_pin(&g.data_dir, &new_pin)?;
    *g.pin.lock() = new_pin.clone();
    // Revoking the token forces already-paired phones to pair again — a
    // rotated PIN must truly lock them out.
    *g.stream.session_token.lock() = None;
    Ok(new_pin)
}

/// Switch the speaker source between real system-audio capture and the
/// synthetic 440 Hz test tone, live at runtime. Lets the user verify the
/// PC -> phone audio path without restarting the app.
///
/// Stops the current source by bumping the capture generation (the WASAPI
/// thread and the GUI tone thread both exit on a generation change), then
/// starts the new one on the same broadcast channel, so only one producer
/// ever feeds `/speaker-ws`.
///
/// Not supported when the process started with `--speaker-test-tone`: that
/// CLI synth thread runs forever and can't be stopped — going back to real
/// capture needs a restart (same as the console's `speaker-device` rule).
pub fn set_speaker_test_tone(g: &GuiCtx, on: bool) -> anyhow::Result<()> {
    if on == g.speaker_test_tone.load(Ordering::Relaxed) {
        return Ok(()); // already in the requested state
    }
    if !on && g.cli_test_tone {
        anyhow::bail!(
            "the test tone was enabled with --speaker-test-tone; \
             restart without the flag to go back to system-audio capture"
        );
    }
    let tx = g
        .speaker_tx
        .clone()
        .ok_or_else(|| anyhow::anyhow!("speaker capture isn't running on this machine"))?;
    // Stop the current source first. The WASAPI thread exits within ~50 ms
    // of the bump and the GUI tone thread on its next 20 ms tick, so the two
    // sources overlap for at most a couple of frames.
    g.speaker_generation.fetch_add(1, Ordering::Relaxed);
    if on {
        spawn_gui_test_tone(tx, g.speaker_generation.clone());
        g.speaker_test_tone.store(true, Ordering::Relaxed);
        g.speaker_running.store(true, Ordering::Relaxed);
        tracing::info!("speaker: test tone enabled from the GUI (440 Hz synthetic)");
    } else {
        #[cfg(windows)]
        {
            let device = g.speaker_device.lock().clone();
            match crate::speaker::wasapi::spawn(tx, device, g.speaker_generation.clone()) {
                Ok(()) => {
                    g.speaker_running.store(true, Ordering::Relaxed);
                    tracing::info!("speaker: back to WASAPI loopback capture");
                }
                Err(e) => {
                    g.speaker_running.store(false, Ordering::Relaxed);
                    anyhow::bail!("capture failed to restart: {e:#}");
                }
            }
        }
        #[cfg(not(windows))]
        {
            // System-audio capture needs Windows; the tone is stopped and
            // there is nothing to switch back to.
            g.speaker_running.store(false, Ordering::Relaxed);
            tracing::info!("speaker: test tone disabled (no capture backend on this OS)");
        }
        g.speaker_test_tone.store(false, Ordering::Relaxed);
    }
    Ok(())
}

/// Spawn the 440 Hz test tone onto the speaker broadcast channel, stopping
/// when the capture generation bumps. This is the runtime-toggle twin of
/// `speaker::synth::spawn` (which the `--speaker-test-tone` CLI flag uses and
/// which runs forever): the synth module is owned by another agent, so the
/// stop hook lives here instead of changing its signature. Same wire format
/// — 20 ms stereo f32 frames at 48 kHz, ±0.5 amplitude.
fn spawn_gui_test_tone(tx: tokio::sync::broadcast::Sender<Vec<f32>>, generation: Arc<AtomicU64>) {
    use crate::speaker::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};
    let my_generation = generation.load(Ordering::Relaxed);
    std::thread::Builder::new()
        .name("gui-test-tone".into())
        .spawn(move || {
            let mut phase: f64 = 0.0;
            let step = 2.0 * std::f64::consts::PI * 440.0 / SAMPLE_RATE as f64;
            let interval = std::time::Duration::from_millis(20);
            loop {
                // A newer source took over — exit so only one producer ever
                // feeds the channel.
                if generation.load(Ordering::Relaxed) != my_generation {
                    break;
                }
                let start = std::time::Instant::now();
                let mut frame = Vec::with_capacity(FRAME_SAMPLES * CHANNELS);
                for _ in 0..FRAME_SAMPLES {
                    let s = (phase.sin() * 0.5) as f32;
                    frame.push(s);
                    frame.push(s);
                    phase += step;
                }
                // Err = no listeners right now (phone hasn't connected yet) —
                // keep the tone running anyway so late joiners get audio.
                // NOTE: no `continue` here — skipping the sleep below would
                // busy-spin this thread at 100% CPU whenever nobody is
                // listening.
                let _ = tx.send(frame);
                let elapsed = start.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
            }
        })
        .expect("failed to spawn GUI test-tone thread");
}

/// Read every live value the panels need. All reads are lock-free atomics or
/// very short parking_lot locks; this runs on a plain thread, never on the
/// UI thread and never across an `.await`.
fn poll_snapshot(p: &PollInputs, devices_refresh: &AtomicBool, ticks: u64) -> Snapshot {
    let s = &p.stream;
    let linear_gate = f32::from_bits(s.noise_gate.load(Ordering::Relaxed));
    let buffer_samples = s.ring.len();
    let buffer_capacity = s.ring.capacity().max(1);
    let sample_rate = s.source_sample_rate.load(Ordering::Relaxed).max(1);
    if ticks.is_multiple_of(DEVICE_REFRESH_POLLS) {
        devices_refresh.store(true, Ordering::Relaxed);
    }
    let devices = if devices_refresh.swap(false, Ordering::Relaxed) {
        // Fresh enumeration — even an empty list replaces the old one, so a
        // genuinely device-less machine can't show stale entries.
        Some(crate::audio::list_output_devices())
    } else {
        // No refresh this tick; the poller keeps the previous list.
        None
    };
    Snapshot {
        connected: s.is_connected.load(Ordering::Relaxed),
        mic_peer: s.mic_peer.lock().clone(),
        phone_name: p.phone_device_name.lock().clone(),
        packets_received: s.packets_received.load(Ordering::Relaxed),
        packets_lost: s.packets_lost.load(Ordering::Relaxed),
        buffer_samples,
        buffer_capacity,
        buffer_ms: buffer_samples as f64 * 1000.0 / sample_rate as f64,
        source_sample_rate: sample_rate,
        device_ok: s.device_ok.load(Ordering::Relaxed),
        device_name: p.device_select.lock().clone(),
        devices,
        speaker_peers: s.speaker_peers.lock().clone(),
        speaker_running: p.speaker_running.load(Ordering::Relaxed),
        speaker_test_tone: p.speaker_test_tone.load(Ordering::Relaxed),
        speaker_source: if p.speaker_test_tone.load(Ordering::Relaxed) {
            "test tone (440 Hz)".to_string()
        } else {
            p.speaker_device
                .lock()
                .clone()
                .unwrap_or_else(|| "system default".to_string())
        },
        noise_gate_db: linear_to_db(linear_gate),
        gain: f32::from_bits(s.gain.load(Ordering::Relaxed)),
        volume: f32::from_bits(s.output_volume.load(Ordering::Relaxed)),
        latency_ms: s.latency_threshold.load(Ordering::Relaxed),
        monitor_present: p.monitor_present,
        monitor_enabled: s.monitor_enabled.load(Ordering::Relaxed),
        pin: p.pin.lock().clone(),
        applied_mic_name: p.applied_mic_name.lock().clone(),
        update_available: p.update_status.lock().clone(),
    }
}

/// Spawn the background poller thread. It owns no locks across sleeps and
/// touches no async code, so it can never stall the UI.
pub fn spawn_poller(
    p: PollInputs,
    snap: Arc<parking_lot::Mutex<Snapshot>>,
    devices_refresh: Arc<AtomicBool>,
) {
    // Prime the device list immediately so the Devices tab is populated on
    // first open.
    devices_refresh.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("gui-poller".into())
        .spawn(move || {
            let mut ticks: u64 = 0;
            loop {
                let mut next = poll_snapshot(&p, &devices_refresh, ticks);
                // The poller only refills the device list on refresh ticks;
                // keep the previous list otherwise.
                if next.devices.is_none() {
                    next.devices = snap.lock().devices.clone();
                }
                *snap.lock() = next;
                ticks += 1;
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("failed to spawn GUI poller thread");
}

/// Run the native GUI. Blocks until the window closes; the caller then runs
/// the normal graceful shutdown.
pub fn run(g: GuiCtx) -> anyhow::Result<()> {
    let snap = Arc::new(parking_lot::Mutex::new(Snapshot::default()));
    let devices_refresh = Arc::new(AtomicBool::new(false));
    spawn_poller(g.poll_inputs(), snap.clone(), devices_refresh.clone());

    let mut native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([800.0, 560.0])
            .with_title("QuicMic"),
        ..Default::default()
    };
    // Note for headless testing: eframe 0.36's default features only enable
    // the wgpu renderer. Under Xvfb, force wgpu's software GL backend with
    // WGPU_BACKEND=gl plus a Mesa EGL build (libEGL_mesa + swrast).
    if let Some(icon) = window_icon() {
        native_options.viewport = native_options.viewport.with_icon(icon);
    }

    let app = app::GuiApp::new(g, snap, devices_refresh);
    eframe::run_native(
        "QuicMic",
        native_options,
        Box::new(move |cc| {
            // Refined dark theme (visuals only) before anything renders.
            theme::apply(&cc.egui_ctx);
            // Publish the live context so background watchers (Ctrl+C,
            // `/api/update`) can close the window from another thread.
            *app.egui_ctx_handle().lock() = Some(cc.egui_ctx.clone());
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed to start: {e}"))
}

/// Window icon from the embedded app icon, best-effort (a missing icon must
/// never stop the GUI from starting).
fn window_icon() -> Option<eframe::egui::IconData> {
    let png = include_bytes!("../web/icons/icon-192.png");
    eframe::icon_data::from_png_bytes(png).ok()
}

#[cfg(test)]
mod tests {
    use super::{db_to_linear, is_recommended_device, linear_to_db, render_qr_gray};
    use super::{set_speaker_test_tone, GuiCtx};
    use crate::mic_name::MicRenameMode;
    use crate::server::StreamState;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn qr_renders_square_bitmap_with_both_colors() {
        let (px, side) = render_qr_gray("https://192.168.1.42:8443#123456", 4, 4)
            .expect("a short URL must fit in a QR code");
        assert!(side > 0);
        assert_eq!(px.len(), side * side);
        assert!(px.contains(&0), "QR must contain black modules");
        assert!(px.contains(&255), "QR must contain white modules");
        // The quiet-zone border is white.
        assert_eq!(px[0], 255);
        assert_eq!(px[side - 1], 255);
    }

    #[test]
    fn qr_rejects_unencodable_input() {
        // 4000 bytes of binary garbage exceeds QR capacity.
        let big = "x".repeat(4000);
        assert!(render_qr_gray(&big, 4, 4).is_none());
    }

    #[test]
    fn gate_db_roundtrip_matches_console_semantics() {
        // -100 dB disables the gate (hard 0.0), like the console command.
        assert_eq!(db_to_linear(-100.0), 0.0);
        assert_eq!(linear_to_db(0.0), -100.0);
        // -6 dB is ~0.5 linear; the roundtrip is stable to display precision.
        let linear = db_to_linear(-6.0);
        assert!((linear - 0.5011872).abs() < 1e-4);
        assert!((linear_to_db(linear) - -6.0).abs() < 1e-3);
    }

    #[test]
    fn recommended_device_matches_platform_virtual_cable() {
        // Mirrors audio::output::DEFAULT_DEVICE per platform.
        #[cfg(target_os = "windows")]
        {
            assert!(is_recommended_device(
                "CABLE Input (VB-Audio Virtual Cable)"
            ));
            assert!(!is_recommended_device("Speakers (Realtek Audio)"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(is_recommended_device("BlackHole 16ch"));
            assert!(!is_recommended_device("MacBook Pro Speakers"));
        }
        #[cfg(target_os = "linux")]
        {
            assert!(is_recommended_device("VirtualQuicMic"));
            assert!(!is_recommended_device("Built-in Audio Analog Stereo"));
        }
    }

    /// Minimal [`GuiCtx`] for the test-tone toggle tests. Only the speaker
    /// plumbing is live; everything else is inert.
    fn test_ctx(
        tx: Option<tokio::sync::broadcast::Sender<Vec<f32>>>,
        cli_tone: bool,
        live_tone: bool,
    ) -> GuiCtx {
        let (cancel_tx, _) = tokio::sync::broadcast::channel::<()>(1);
        GuiCtx {
            stream: StreamState {
                ring: Arc::new(crate::audio::RingBuffer::new(24000)),
                is_connected: Arc::new(AtomicBool::new(false)),
                session_token: Arc::new(parking_lot::Mutex::new(None)),
                noise_gate: Arc::new(AtomicU32::new(0)),
                gain: Arc::new(AtomicU32::new(1f32.to_bits())),
                latency_threshold: Arc::new(AtomicU32::new(150)),
                output_volume: Arc::new(AtomicU32::new(1f32.to_bits())),
                monitor_ring: None,
                monitor_enabled: Arc::new(AtomicBool::new(false)),
                packets_received: Arc::new(AtomicU64::new(0)),
                packets_lost: Arc::new(AtomicU64::new(0)),
                source_sample_rate: Arc::new(AtomicU32::new(48000)),
                cancel_tx,
                is_shutdown: Arc::new(AtomicBool::new(false)),
                device_ok: Arc::new(AtomicBool::new(true)),
                mic_peer: Arc::new(parking_lot::Mutex::new(None)),
                speaker_peers: Arc::new(parking_lot::Mutex::new(Vec::new())),
            },
            url: String::new(),
            pin: Arc::new(parking_lot::Mutex::new(String::new())),
            data_dir: PathBuf::from("/tmp"),
            cert_hash: String::new(),
            device_select: Arc::new(parking_lot::Mutex::new(None)),
            speaker_running: Arc::new(AtomicBool::new(false)),
            speaker_test_tone: Arc::new(AtomicBool::new(live_tone)),
            cli_test_tone: cli_tone,
            speaker_tx: tx,
            speaker_generation: Arc::new(AtomicU64::new(0)),
            speaker_device: Arc::new(parking_lot::Mutex::new(None)),
            monitor_present: false,
            phone_device_name: Arc::new(parking_lot::Mutex::new(None)),
            mic_rename_mode: Arc::new(parking_lot::Mutex::new(MicRenameMode::Off)),
            applied_mic_name: Arc::new(parking_lot::Mutex::new(None)),
            update_status: Arc::new(parking_lot::Mutex::new(None)),
            port: 0,
            lan_ip: String::new(),
            update_check_ran: false,
            prefs_path: PathBuf::from("/tmp"),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            egui_ctx: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    #[test]
    fn poll_snapshot_reflects_live_test_tone_flag() {
        use super::poll_snapshot;

        let g = test_ctx(None, false, false);
        let inputs = g.poll_inputs();
        let no_refresh = AtomicBool::new(false);
        // Sanity: PollInputs carries the same shared flag the toggle flips.
        assert!(Arc::ptr_eq(&inputs.speaker_test_tone, &g.speaker_test_tone));

        let snap = poll_snapshot(&inputs, &no_refresh, 1);
        assert!(!snap.speaker_test_tone);
        assert_eq!(snap.speaker_source, "system default");

        g.speaker_test_tone.store(true, Ordering::Relaxed);
        let snap = poll_snapshot(&inputs, &no_refresh, 1);
        assert!(snap.speaker_test_tone);
        assert_eq!(snap.speaker_source, "test tone (440 Hz)");
    }

    #[test]
    fn test_tone_toggle_needs_a_speaker_channel() {
        // Speaker capture never started on this machine: nothing to switch.
        let g = test_ctx(None, false, false);
        assert!(set_speaker_test_tone(&g, true).is_err());
        assert!(!g.speaker_test_tone.load(Ordering::Relaxed));
    }

    #[test]
    fn test_tone_toggle_is_a_noop_when_already_in_state() {
        let (tx, _) = tokio::sync::broadcast::channel::<Vec<f32>>(64);
        let g = test_ctx(Some(tx), false, false);
        let gen_before = g.speaker_generation.load(Ordering::Relaxed);
        assert!(set_speaker_test_tone(&g, false).is_ok());
        // No generation bump, no thread spawned for a no-op.
        assert_eq!(g.speaker_generation.load(Ordering::Relaxed), gen_before);
    }

    #[test]
    fn test_tone_toggle_cli_flag_needs_restart_to_go_back() {
        // The --speaker-test-tone synth thread runs forever and can't be
        // stopped, so switching back to capture requires a restart.
        let (tx, _) = tokio::sync::broadcast::channel::<Vec<f32>>(64);
        let g = test_ctx(Some(tx), true, true);
        assert!(set_speaker_test_tone(&g, false).is_err());
        assert!(g.speaker_test_tone.load(Ordering::Relaxed));
    }

    /// Full toggle lifecycle on non-Windows: tone on produces valid wire
    /// frames, tone off stops the producer (single-producer invariant).
    /// The Windows off-path restarts WASAPI capture, which needs real audio
    /// hardware — not testable here.
    #[cfg(not(windows))]
    #[test]
    fn test_tone_toggle_produces_then_stops_frames() {
        use crate::speaker::{CHANNELS, FRAME_SAMPLES};

        let (tx, _) = tokio::sync::broadcast::channel::<Vec<f32>>(64);
        let mut rx = tx.subscribe();
        let g = test_ctx(Some(tx), false, false);

        assert!(set_speaker_test_tone(&g, true).is_ok());
        assert!(g.speaker_test_tone.load(Ordering::Relaxed));
        assert!(g.speaker_running.load(Ordering::Relaxed));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        // NOTE: tokio::time::timeout must be created inside the runtime.
        let frame: Vec<f32> = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("tone must produce a frame quickly")
                .expect("channel open")
        });
        // Wire format: 20 ms stereo f32, channels equal, ±0.5 amplitude.
        assert_eq!(frame.len(), FRAME_SAMPLES * CHANNELS);
        for pair in frame.as_chunks::<2>().0 {
            assert_eq!(pair[0], pair[1]);
            assert!(pair[0].abs() <= 0.5);
        }
        // The tone actually oscillates.
        assert!(frame.iter().any(|&s| s > 0.4));

        assert!(set_speaker_test_tone(&g, false).is_ok());
        assert!(!g.speaker_test_tone.load(Ordering::Relaxed));
        assert!(!g.speaker_running.load(Ordering::Relaxed));
        // Drain any frames queued before the stop, then prove the producer
        // is gone: nothing new may arrive.
        while rx.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(150));
        assert!(rx.try_recv().is_err());
    }
}
