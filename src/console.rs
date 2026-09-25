//! Interactive PC console: themed startup banner, live status panel, and a
//! settings command loop (read from stdin) that adjusts the server while it
//! runs. Everything here is best-effort presentation — the server works fine
//! with stdin closed or piped (the input thread just exits).
//!
//! Note: the phone web UI is the source of truth for DSP settings — it pushes
//! its saved volume/gain/gate/latency to the server on every (re)connect, so a
//! console change can be overridden the next time the phone pairs. The console
//! is for the PC operator; the phone UI is for the phone user.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::audio;
use crate::self_update;
use crate::server::{self, StreamState};
#[cfg(windows)]
use crate::speaker;

/// Public companion to the QuicMic 📦 Drop tab: the LocalDrop PWA for
/// sending pictures, files, text and links between nearby devices.
/// Printed at startup and by the `drop` console command.
pub const LOCALDROP_URL: &str = "https://crtghoul.github.io/localdrop/";

/// Banner/status color themes. `Plain` emits no ANSI escapes for terminals
/// that don't do color.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Ghoul,
    Neon,
    Plain,
}

impl Theme {
    pub fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ghoul" => Some(Theme::Ghoul),
            "neon" => Some(Theme::Neon),
            "plain" | "nocolor" | "no-color" => Some(Theme::Plain),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Ghoul => "ghoul",
            Theme::Neon => "neon",
            Theme::Plain => "plain",
        }
    }

    /// Glyph used for bullets and the input prompt (unpadded lines only).
    pub fn glyph(self) -> &'static str {
        match self {
            Theme::Ghoul => "☠",
            Theme::Neon => "◆",
            Theme::Plain => "*",
        }
    }
}

/// ANSI palette; every field is "" under [`Theme::Plain`].
struct Palette {
    border: &'static str,
    title: &'static str,
    label: &'static str,
    value: &'static str,
    dim: &'static str,
    accent: &'static str,
    ok: &'static str,
    reset: &'static str,
}

fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Ghoul => Palette {
            border: "\x1b[1;32m",
            title: "\x1b[1;32m",
            label: "\x1b[32m",
            value: "\x1b[1;37m",
            dim: "\x1b[2;32m",
            accent: "\x1b[1;33m",
            ok: "\x1b[1;32m",
            reset: "\x1b[0m",
        },
        Theme::Neon => Palette {
            border: "\x1b[1;36m",
            title: "\x1b[1;35m",
            label: "\x1b[36m",
            value: "\x1b[1;37m",
            dim: "\x1b[2;36m",
            accent: "\x1b[1;35m",
            ok: "\x1b[1;36m",
            reset: "\x1b[0m",
        },
        Theme::Plain => Palette {
            border: "",
            title: "",
            label: "",
            value: "",
            dim: "",
            accent: "",
            ok: "",
            reset: "",
        },
    }
}

/// Box-drawing characters per theme: (top-left, horizontal, top-right,
/// vertical, bottom-left, bottom-right).
fn box_chars(
    theme: Theme,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match theme {
        Theme::Ghoul => ("╔", "═", "╗", "║", "╚", "╝"),
        Theme::Neon => ("╭", "─", "╮", "│", "╰", "╯"),
        Theme::Plain => ("+", "-", "+", "|", "+", "+"),
    }
}

/// Visible terminal columns of a string: skips ANSI escapes and counts the
/// few wide glyphs we use as 2 columns.
fn visible_cols(s: &str) -> usize {
    let mut cols = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
            continue;
        }
        cols += match c {
            '☠' | '●' | '○' | '◆' => 2,
            _ => 1,
        };
    }
    cols
}

/// Shared handles the console commands need. Built once in `main`.
pub struct Ctx {
    pub theme: Arc<parking_lot::Mutex<Theme>>,
    pub url: String,
    pub pin: String,
    pub cert_hash: String,
    pub device_select: Arc<parking_lot::Mutex<Option<String>>>,
    pub stream: StreamState,
    /// Broadcast channel feeding `/speaker-ws`; `None` when the Speaker tab
    /// has no audio source (loopback failed to start, non-Windows without
    /// `--speaker-test-tone`).
    pub speaker_tx: Option<broadcast::Sender<Vec<f32>>>,
    /// Whether the speaker capture thread is currently running.
    pub speaker_running: Arc<AtomicBool>,
    /// `true` when the speaker source is the synthetic test tone — device
    /// selection does not apply.
    pub speaker_test_tone: bool,
    /// Selected render endpoint for speaker loopback capture
    /// (`None` = system default). Bumped through `speaker_generation` so the
    /// capture thread restarts on the new device.
    pub speaker_device: Arc<Mutex<Option<String>>>,
    /// Bumped on every device switch so the old capture thread exits.
    /// Only the Windows capture path reads it.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub speaker_generation: Arc<AtomicU64>,
    pub monitor_present: bool,
}

/// What the command loop should do after a command.
#[derive(PartialEq, Eq)]
pub enum Action {
    Continue,
    Quit,
}

/// A bordered panel; `body` lines are padded to the inner width.
struct Panel {
    theme: Theme,
    width: usize,
}

impl Panel {
    fn new(theme: Theme, width: usize) -> Self {
        Panel { theme, width }
    }

    fn edge(&self, left: &str, fill: &str, right: &str) {
        let p = palette(self.theme);
        println!(
            "{}{}{}{}{}",
            p.border,
            left,
            fill.repeat(self.width),
            right,
            p.reset
        );
    }

    fn top(&self) {
        let (tl, h, tr, _, _, _) = box_chars(self.theme);
        self.edge(tl, h, tr);
    }

    fn bottom(&self) {
        let (_, h, _, _, bl, br) = box_chars(self.theme);
        self.edge(bl, h, br);
    }

    fn row(&self, content: &str) {
        let p = palette(self.theme);
        let (_, _, _, v, _, _) = box_chars(self.theme);
        let pad = self.width.saturating_sub(2 + visible_cols(content));
        println!(
            "{}{} {}{} {}{}",
            p.border,
            v,
            content,
            " ".repeat(pad),
            v,
            p.reset
        );
    }

    fn blank(&self) {
        self.row("");
    }
}

/// Print the themed startup banner. Every row is padded to a fixed inner width
/// so the borders always line up regardless of URL / PIN / hash lengths.
/// Takes the banner fields directly (rather than [`Ctx`]) because it runs
/// before the shared stream state exists.
pub fn print_banner(theme_lock: &parking_lot::Mutex<Theme>, url: &str, pin: &str, cert_hash: &str) {
    let theme = *theme_lock.lock();
    let p = palette(theme);
    let panel = Panel::new(theme, 59);

    let version = format!("QuicMic v{}", env!("CARGO_PKG_VERSION"));
    let cert_prefix: &str = cert_hash.get(..20).unwrap_or(cert_hash);

    println!();
    panel.top();
    panel.row(&format!(
        "{}{} {}{}",
        p.accent,
        theme.glyph(),
        p.title,
        version
    ));
    panel.blank();
    panel.row(&format!("{}URL:{}          {}", p.label, p.reset, url));
    panel.row(&format!(
        "{}Pairing PIN:{}  {}{}{}",
        p.label, p.reset, p.value, pin, p.reset
    ));
    panel.row(&format!(
        "{}Cert SHA-256:{}{} {}",
        p.label, p.reset, p.dim, cert_prefix
    ));
    panel.blank();

    // Inner setup box, drawn with plain ASCII so it looks right in every theme.
    let iw = 55;
    let inner_top = format!("┌{}┐", "─".repeat(iw));
    let inner_bottom = format!("└{}┘", "─".repeat(iw));
    // Fall back to ASCII for the plain theme.
    let (it, ib) = match theme {
        Theme::Plain => (
            format!("+{}+", "-".repeat(iw)),
            format!("+{}+", "-".repeat(iw)),
        ),
        _ => (inner_top, inner_bottom),
    };
    panel.row(&format!("{}{}{}", p.dim, it, p.reset));
    let step = |s: &str| {
        let pad = iw.saturating_sub(2 + visible_cols(s));
        panel.row(&format!("{}│ {}{} │{}", p.dim, s, " ".repeat(pad), p.reset));
    };
    step("Setup instructions:");
    step("1. Scan the QR code below with your phone camera");
    step(&format!("2. Or open: {}", url));
    step(&format!("   and enter PIN: {}", pin));
    panel.row(&format!("{}{}{}", p.dim, ib, p.reset));

    panel.bottom();
    println!();
}

/// Live status panel: who's connected, which devices, current DSP settings.
pub fn print_status(ctx: &Ctx) {
    let theme = *ctx.theme.lock();
    let p = palette(theme);
    let panel = Panel::new(theme, 59);

    let dot = |on: bool| {
        if on {
            format!("{p_ok}●{reset}", p_ok = p.ok, reset = p.reset)
        } else {
            format!("{dim}○{reset}", dim = p.dim, reset = p.reset)
        }
    };

    let mic_peer = ctx.stream.mic_peer.lock().clone();
    let speaker_peers = ctx.stream.speaker_peers.lock().clone();
    let device_name = ctx
        .device_select
        .lock()
        .clone()
        .unwrap_or_else(|| "system default".to_string());
    let volume = f32::from_bits(ctx.stream.output_volume.load(Ordering::Relaxed));
    let gain = f32::from_bits(ctx.stream.gain.load(Ordering::Relaxed));
    let gate_db = linear_to_db(f32::from_bits(
        ctx.stream.noise_gate.load(Ordering::Relaxed),
    ));
    let latency = ctx.stream.latency_threshold.load(Ordering::Relaxed);
    let device_ok = ctx.stream.device_ok.load(Ordering::Relaxed);
    let monitor = if ctx.monitor_present {
        if ctx.stream.monitor_enabled.load(Ordering::Relaxed) {
            "on"
        } else {
            "muted"
        }
    } else {
        "n/a (restart with --monitor-device)"
    };

    println!();
    panel.top();
    panel.row(&format!("{}STATUS{}", p.title, p.reset));
    panel.blank();
    match &mic_peer {
        Some(ip) => panel.row(&format!("{} Mic client:  {}{}", dot(true), p.value, ip)),
        None => panel.row(&format!("{} Mic client:  {}--", dot(false), p.dim)),
    }
    if speaker_peers.is_empty() {
        let state = if ctx.speaker_running.load(Ordering::Relaxed) {
            "idle"
        } else {
            "not running"
        };
        panel.row(&format!("{} Speaker:     {}{}", dot(false), p.dim, state));
    } else {
        panel.row(&format!(
            "{} Speaker:     {}{} listener{} ({})",
            dot(true),
            p.value,
            speaker_peers.len(),
            if speaker_peers.len() == 1 { "" } else { "s" },
            speaker_peers.join(", ")
        ));
    }
    let dev_state = if device_ok { "" } else { " (REBUILDING…)" };
    panel.row(&format!(
        "{}Mic device:  {}{}{}",
        p.label, p.value, device_name, dev_state
    ));
    let speaker_src = if ctx.speaker_test_tone {
        "test tone (440 Hz)".to_string()
    } else {
        ctx.speaker_device
            .lock()
            .clone()
            .unwrap_or_else(|| "system default".to_string())
    };
    panel.row(&format!(
        "{}Spk source:  {}{}{}",
        p.label, p.value, speaker_src, p.reset
    ));
    panel.row(&format!(
        "{}Volume:{}{:.2}x  {}Gain:{}{:.2}x  {}Gate:{}{}  {}Latency:{}{}ms",
        p.label,
        p.value,
        volume,
        p.label,
        p.value,
        gain,
        p.label,
        p.value,
        gate_db,
        p.label,
        p.value,
        latency,
    ));
    panel.row(&format!("{}Monitor:{}     {}", p.label, p.reset, monitor));
    panel.bottom();
    println!();
}

fn linear_to_db(v: f32) -> String {
    if v <= 0.0 {
        "Off".to_string()
    } else {
        format!("{:.0}dB", 20.0 * v.log10())
    }
}

pub fn print_help(ctx: &Ctx) {
    let theme = *ctx.theme.lock();
    let p = palette(theme);
    println!();
    println!(
        "{}Console commands — type one and press Enter:{}",
        p.title, p.reset
    );
    let cmds: &[(&str, &str)] = &[
        ("status", "show the live status panel"),
        ("qr", "reprint the pairing QR code"),
        ("drop", "LocalDrop link for phone↔PC file sharing"),
        ("devices", "list audio output devices"),
        ("device <n|name>", "switch the mic output device live"),
        (
            "speaker-devices",
            "list PC playback devices (speaker capture)",
        ),
        (
            "speaker-device <n|name>",
            "switch which device the Speaker tab captures",
        ),
        (
            "mic-name [name]",
            "rename the phone-mic input (CABLE Output → QuicMic)",
        ),
        ("volume <0-5>", "PC output volume multiplier"),
        ("gain <0.2-3>", "mic gain multiplier"),
        ("gate <-100-0>", "noise-gate threshold in dB (-100 = off)"),
        ("latency <0-500>", "latency-recovery threshold in ms"),
        ("monitor <on|off>", "hear-yourself monitor mute"),
        ("theme <ghoul|neon|plain>", "banner theme"),
        (
            "update",
            "download the latest release exe and restart into it",
        ),
        ("quit", "graceful shutdown"),
    ];
    for (cmd, desc) in cmds {
        println!("  {}{:<22}{} {}", p.accent, cmd, p.reset, desc);
    }
    println!(
        "{}Note: the phone UI re-pushes its saved settings on every connect,{}",
        p.dim, p.reset
    );
    println!(
        "{}so it can override volume/gain/gate/latency changed here.{}",
        p.dim, p.reset
    );
    println!();
}

/// Parse and run one console command line.
pub async fn handle_command(line: &str, ctx: &Ctx) -> Action {
    let mut parts = line.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c.to_ascii_lowercase(),
        None => return Action::Continue,
    };
    let arg: String = parts.collect::<Vec<_>>().join(" ");

    match cmd.as_str() {
        "help" | "?" | "commands" => print_help(ctx),
        "status" => print_status(ctx),
        "qr" => {
            let qr_url = format!("{}#{}", ctx.url, ctx.pin);
            if let Err(e) = qr2term::print_qr(&qr_url) {
                println!("Could not print QR code: {e}");
            }
        }
        "devices" => {
            println!("Audio output devices:");
            for (i, name) in audio::list_output_devices().iter().enumerate() {
                println!("  [{i}] {name}");
            }
        }
        "device" => cmd_device(ctx, &arg),
        "drop" => {
            println!("LocalDrop — send pictures, files, text and links between nearby devices:");
            println!("  {LOCALDROP_URL}");
            println!("Open it on any device on your Wi-Fi; no install needed (PWA).");
        }
        "speaker-devices" => cmd_speaker_devices(),
        "speaker-device" => cmd_speaker_device(ctx, &arg),
        "mic-name" => cmd_mic_name(&arg),
        "volume" => cmd_set_f32(
            "volume",
            &arg,
            &ctx.stream.output_volume,
            server::OUTPUT_VOLUME_MIN,
            server::OUTPUT_VOLUME_MAX,
            |v| format!("{v:.2}x"),
        ),
        "gain" => cmd_set_f32(
            "gain",
            &arg,
            &ctx.stream.gain,
            server::GAIN_MIN,
            server::GAIN_MAX,
            |v| format!("{v:.2}x"),
        ),
        "gate" => match arg.parse::<f32>() {
            Ok(db) if (-100.0..=0.0).contains(&db) => {
                let linear = if db <= -100.0 {
                    0.0
                } else {
                    10f32.powf(db / 20.0)
                }
                .clamp(server::NOISE_GATE_MIN, server::NOISE_GATE_MAX);
                ctx.stream
                    .noise_gate
                    .store(linear.to_bits(), Ordering::Relaxed);
                println!("Noise gate set to {db:.0} dB.");
            }
            _ => println!("Usage: gate <-100-0>  (dB, -100 = off)"),
        },
        "latency" => match arg.parse::<u32>() {
            Ok(ms) if ms <= server::LATENCY_THRESHOLD_MAX_MS => {
                ctx.stream.latency_threshold.store(ms, Ordering::Relaxed);
                println!("Latency-recovery threshold set to {ms} ms.");
            }
            _ => println!("Usage: latency <0-500>  (ms, 0 = disabled)"),
        },
        "monitor" => {
            if !ctx.monitor_present {
                println!("No monitor stream — restart with --monitor-device to enable it.");
            } else {
                match arg.to_ascii_lowercase().as_str() {
                    "on" => {
                        ctx.stream.monitor_enabled.store(true, Ordering::Relaxed);
                        println!("Monitor unmuted.");
                    }
                    "off" => {
                        ctx.stream.monitor_enabled.store(false, Ordering::Relaxed);
                        println!("Monitor muted.");
                    }
                    _ => println!("Usage: monitor <on|off>"),
                }
            }
        }
        "theme" => match Theme::parse(&arg) {
            Some(t) => {
                *ctx.theme.lock() = t;
                println!("Theme set to '{}'.", t.name());
                print_banner(&ctx.theme, &ctx.url, &ctx.pin, &ctx.cert_hash);
            }
            None => println!("Usage: theme <ghoul|neon|plain>"),
        },
        "update" => {
            // Self-updater: downloads the latest release's exe and restarts
            // into it, so you don't have to fetch it from GitHub by hand.
            match self_update::run_update().await {
                Ok(true) => return Action::Quit, // updater batch takes over
                Ok(false) => {}
                Err(e) => println!("Update failed: {e:#}"),
            }
        }
        "quit" | "exit" => return Action::Quit,
        _ => println!("Unknown command '{cmd}'. Type 'help' for the list."),
    }
    Action::Continue
}

fn cmd_set_f32(
    name: &str,
    arg: &str,
    atomic: &Arc<std::sync::atomic::AtomicU32>,
    min: f32,
    max: f32,
    fmt: impl Fn(f32) -> String,
) {
    match arg.parse::<f32>() {
        Ok(v) if (min..=max).contains(&v) => {
            atomic.store(v.to_bits(), Ordering::Relaxed);
            println!("{} set to {}.", name, fmt(v));
        }
        _ => println!("Usage: {name} <{min}-{max}>"),
    }
}

/// `device <n|name|default>`: validate against the OS device list, then hand
/// the new selection to the audio supervisor, which rebuilds the stream.
fn cmd_device(ctx: &Ctx, arg: &str) {
    if arg.is_empty() {
        println!("Usage: device <n|name|default>");
        return;
    }
    let devices = audio::list_output_devices();
    let pick: Option<String> = if arg.eq_ignore_ascii_case("default") {
        None
    } else if let Ok(i) = arg.parse::<usize>() {
        match devices.get(i) {
            Some(n) => Some(n.clone()),
            None => {
                println!("No device [{i}]. Type 'devices' for the list.");
                return;
            }
        }
    } else {
        let needle = arg.to_ascii_lowercase();
        match devices.iter().find(|n| n.to_lowercase().contains(&needle)) {
            Some(n) => Some(n.clone()),
            None => {
                println!("No device matching '{arg}'. Type 'devices' for the list.");
                return;
            }
        }
    };
    *ctx.device_select.lock() = pick.clone();
    println!(
        "Switching mic output to {}…",
        pick.as_deref().unwrap_or("system default")
    );
}

/// `mic-name [name]`: rename the VB-Cable recording endpoint (the "microphone"
/// your apps see) so it shows up as "QuicMic" instead of "CABLE Output".
/// Windows only; needs one elevated run (the name lives in HKLM).
fn cmd_mic_name(arg: &str) {
    let wanted = if arg.trim().is_empty() {
        None
    } else {
        Some(arg.trim())
    };
    match crate::mic_name::rename_mic(wanted) {
        Ok(name) => {
            println!("Mic input renamed to \"{name}\".");
            println!("Restart your apps (Discord, Serein, …) and they'll list it as \"{name}\".");
        }
        Err(e) => println!("{e:#}"),
    }
}

/// `speaker-devices`: list the PC playback endpoints the 🔊 Speaker tab can
/// capture from (WASAPI loopback sources). Windows only.
fn cmd_speaker_devices() {
    #[cfg(windows)]
    {
        match speaker::wasapi::list_render_devices() {
            Ok(devices) => {
                println!("PC playback devices (speaker capture sources):");
                for (i, name) in devices.iter().enumerate() {
                    println!("  [{i}] {name}");
                }
            }
            Err(e) => println!("Could not list playback devices: {e:#}"),
        }
    }
    #[cfg(not(windows))]
    {
        println!("Speaker capture needs Windows (or --speaker-test-tone).");
    }
}

/// `speaker-device <n|name|default>`: switch which PC playback device the
/// 🔊 Speaker tab captures, live. Bumps the capture generation so the old
/// capture thread exits, then starts a fresh one on the new endpoint.
fn cmd_speaker_device(ctx: &Ctx, arg: &str) {
    if arg.is_empty() {
        println!("Usage: speaker-device <n|name|default>");
        return;
    }
    if ctx.speaker_test_tone {
        println!("Speaker is in --speaker-test-tone mode: no capture device to switch.");
        println!("Restart without --speaker-test-tone to capture system audio.");
        return;
    }
    let Some(tx) = ctx.speaker_tx.clone() else {
        println!("Speaker capture isn't running on this machine.");
        return;
    };
    #[cfg(windows)]
    {
        let names = match speaker::wasapi::list_render_devices() {
            Ok(n) => n,
            Err(e) => {
                println!("Could not list playback devices: {e:#}");
                return;
            }
        };
        let pick = match speaker::resolve_name(&names, arg) {
            Ok(p) => p,
            Err(e) => {
                println!("{e}");
                return;
            }
        };
        // Tell the old capture thread to exit, give it a beat to release the
        // endpoint, then start the new one.
        *ctx.speaker_device.lock() = pick.clone();
        ctx.speaker_generation.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(200));
        match speaker::wasapi::spawn(tx, pick.clone(), ctx.speaker_generation.clone()) {
            Ok(()) => {
                ctx.speaker_running.store(true, Ordering::Relaxed);
                println!(
                    "Speaker now capturing from {}.",
                    pick.as_deref().unwrap_or("system default")
                );
            }
            Err(e) => {
                ctx.speaker_running.store(false, Ordering::Relaxed);
                println!("Capture failed to restart: {e:#}");
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = tx;
        println!("Speaker capture needs Windows (or --speaker-test-tone).");
    }
}
