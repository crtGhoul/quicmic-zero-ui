mod audio;
mod console;
mod gui;
mod identity;
mod mic_name;
mod self_update;
mod server;
mod speaker;
mod tls;
#[cfg(windows)]
mod tray;
mod update_check;

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Ring-buffer depth, sized at a nominal 48 kHz (the rate browsers capture at).
/// The ring is allocated once at startup and never resized, so its capacity is
/// fixed at this nominal rate. ~500ms comfortably absorbs Wi-Fi jitter; actual
/// latency is governed by PREBUFFER_MS + the latency-recovery threshold, not this
/// ceiling.
const RING_BUFFER_MS: usize = 500;
const RING_BUFFER_SAMPLES: usize = 48_000 * RING_BUFFER_MS / 1000;

/// After Ctrl+C the HTTP API keeps replying 503 for this long so a streaming
/// client's ~1s liveness poll reliably observes the shutdown before the process
/// exits. The transport close event is unreliable/late on iOS Safari, so this
/// HTTP signal — not a close frame — is what clients actually detect.
const SHUTDOWN_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_millis(1200);

/// Noise-gate threshold range accepted on the CLI, in dB. Mirrors the web UI
/// slider: -100 dB disables the gate, 0 dB is the maximum.
const NOISE_GATE_DB_MIN: f32 = -100.0;
const NOISE_GATE_DB_MAX: f32 = 0.0;

/// Convert a noise-gate threshold from dB (CLI / web-UI units) to the linear
/// amplitude used internally by the audio thread and HTTP API. Mirrors the
/// client's `dbToNoiseGate`: the value is clamped to [-100, 0] dB and -100 dB maps
/// to a hard 0.0 (gate disabled), rather than to a tiny-but-nonzero amplitude.
fn noise_gate_db_to_linear(db: f32) -> f32 {
    let db = db.clamp(NOISE_GATE_DB_MIN, NOISE_GATE_DB_MAX);
    if db <= NOISE_GATE_DB_MIN {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Address ranges that can never serve as a LAN pairing target reachable from
/// another device. The load-bearing exclusion is 198.18.0.0/15 — the IETF
/// benchmarking range (RFC 2544) that fake-ip proxy TUNs (mihomo/Clash,
/// sing-box, Surge, etc.) all use as their default fake-ip range: the TUN
/// adapter holding e.g. 198.18.0.1 is a fake address that nothing on the real
/// LAN can route to.
fn is_unusable_lan_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 0 // 0.0.0.0/8 "this network"
                || v4.is_loopback()
                || v4.is_link_local()
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // RFC 2544 benchmark = fake-ip
                || o[0] >= 224 // multicast / reserved
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Link-local IPv6 needs a zone id to route and is explicitly out
                // of scope for QuicMic's pairing URL.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Rank how usable an address is as a LAN pairing target; higher wins. RFC 1918
/// private addresses are the most likely to be reachable by a phone on the same
/// LAN (2). CGNAT (100.64/10, used by Tailscale and some ISPs) and any IPv6
/// (global/ULA) are the second tier (1). Public IPv4 is the last resort (0).
/// The caller breaks rank ties in favour of IPv4, so a same-rank IPv4 always
/// beats a same-rank IPv6 regardless of enumeration order.
fn lan_addr_rank(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) if v4.is_private() => 2,
        IpAddr::V4(v4) if v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]) => 1,
        IpAddr::V4(_) => 0,
        IpAddr::V6(_) => 1,
    }
}

/// Name patterns of common virtual adapters. Same-rank ties are broken in
/// favour of physical NICs, so a Hyper-V/WSL/Docker/VMware adapter holding an
/// otherwise-valid LAN address (a `vEthernet`/`docker0`/`vmnet` … with an RFC
/// 1918 address) loses to the real Wi-Fi/Ethernet even when both are equally
/// ranked. Keep this list conservative: a false positive only demotes an
/// equally-ranked candidate, never the only candidate.
fn is_virtual_adapter(name: &str) -> bool {
    const KEYWORDS: [&str; 12] = [
        "vethernet",
        "wsl",
        "docker",
        "veth",
        "vmnet",
        "vbox",
        "virbr",
        "tailscale",
        "zerotier",
        "tun",
        "tap",
        "awdl",
    ];
    let n = name.to_ascii_lowercase();
    KEYWORDS.iter().any(|k| n.contains(k))
}

/// Pick the best LAN address from an interface scan: drop unusable ranges, then
/// prefer the highest-ranked address class, breaking rank ties in favour of
/// IPv4 (matching the historical IPv4-first detection).
///
/// Two same-rank, same-family candidates (e.g. a real Wi-Fi `192.168.x` and a
/// Hyper-V/VPN `172.x`) are both valid; virtual adapters (`is_virtual_adapter`)
/// lose such ties, and the remaining ones resolve by `max_by_key`'s "last
/// maximum" rule — i.e. OS enumeration order. This only happens on the
/// fallback path (when the default-route pick was unusable); `--ip` remains
/// the manual override for multi-homed setups where the wrong address would be
/// advertised.
fn pick_lan_ip(ifas: Vec<(String, IpAddr)>) -> Option<IpAddr> {
    ifas.into_iter()
        .filter(|(_, ip)| !is_unusable_lan_addr(*ip))
        .max_by_key(|(name, ip)| (lan_addr_rank(*ip), ip.is_ipv4(), !is_virtual_adapter(name)))
        .map(|(_, ip)| ip)
}

/// Detect an IP address that other devices on the LAN can actually reach.
///
/// `local_ip_address::local_ip()` returns the first IPv4 unicast address among
/// the adapters that own a default route. That is usually right, but goes wrong
/// when a proxy TUN in fake-ip mode (mihomo/Clash, sing-box, Surge, etc.) owns
/// a default route with a low metric: the TUN's fake address (198.18.0.1) wins
/// even though nothing on the LAN can route to it. So the default-route pick is
/// trusted only
/// when it is a usable LAN address; otherwise every interface is scanned and the
/// best-ranked candidate (typically the real Ethernet/Wi-Fi address) is used.
fn detect_lan_ip() -> anyhow::Result<IpAddr> {
    if let Ok(ip) = local_ip_address::local_ip() {
        if !is_unusable_lan_addr(ip) {
            return Ok(ip);
        }
    }
    let ifas = local_ip_address::list_afinet_netifas()
        .map_err(|e| anyhow::anyhow!("Failed to enumerate network interfaces: {e}"))?;
    pick_lan_ip(ifas)
        .ok_or_else(|| anyhow::anyhow!("No usable LAN address found among the network interfaces"))
}

/// QuicMic — Turn any device with a microphone and a web browser into a wireless PC microphone.
#[derive(Parser)]
#[command(name = "quicmic", version, about)]
struct Cli {
    /// Port for both HTTPS (TCP) and WebTransport (UDP).
    #[arg(short, long, default_value = "8443", env = "QUICMIC_PORT")]
    port: u16,

    /// Audio output device name (substring match, case-insensitive).
    /// Defaults to "CABLE Input" (Windows), "BlackHole" (macOS), or "VirtualQuicMic" (Linux).
    #[arg(short, long, env = "QUICMIC_DEVICE")]
    device: Option<String>,

    /// Override the auto-detected LAN IP address. Accepts a bare or bracketed IPv6
    /// literal (e.g. `fe80::1` or `[fe80::1]`).
    #[arg(long)]
    ip: Option<String>,

    /// Set a custom 6-digit pairing PIN. The PIN is remembered across restarts
    /// (that is what makes phone auto-connect work); this flag overrides the
    /// remembered PIN and becomes the new remembered one.
    #[arg(long)]
    pin: Option<String>,

    /// Directory for the persistent server identity (pairing PIN + TLS
    /// certificate). Defaults to the platform app-data directory. Keeping the
    /// same identity across restarts is what lets an already-paired phone
    /// reconnect automatically instead of needing a fresh QR scan.
    #[arg(long, env = "QUICMIC_DATA_DIR")]
    data_dir: Option<String>,

    /// Dump TLS certificates to the certs/ directory for debugging.
    #[arg(long)]
    dump_certs: bool,

    /// Initial noise gate threshold in dB: -100 = Off, 0 = max (default -50).
    /// Mirrors the web UI slider; can be adjusted at runtime from the phone.
    #[arg(long, default_value = "-50", allow_hyphen_values = true)]
    noise_gate: f32,

    /// Initial audio gain multiplier (1.0 = unity, e.g. 1.5).
    /// Can be adjusted at runtime via the web UI.
    #[arg(long, default_value = "1.0")]
    gain: f32,

    /// Initial PC-side output volume multiplier (1.0 = unity, e.g. 1.5).
    /// Applied in the output stage after resampling, so it scales both the
    /// virtual-device stream and the monitor stream (if enabled).
    /// Can be adjusted at runtime from the phone's settings panel.
    #[arg(long, default_value = "1.0")]
    volume: f32,

    /// Enable a hear-yourself monitor: a second audio stream that duplicates
    /// your mic audio to a physical output device (speakers/headphones) so you
    /// can hear yourself. Takes an optional device name (substring match, like
    /// --device); with no name the host's default output device is used.
    /// ⚠ Speakers + a live microphone can feed back — headphones recommended.
    /// The monitor can be muted/unmuted at runtime from the phone's settings
    /// panel, but the stream itself only exists when this flag is given.
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "NAME")]
    monitor_device: Option<Option<String>>,

    /// Initial latency-recovery threshold in milliseconds (0 = disabled).
    /// When the output buffer grows past this, the oldest audio is skipped to
    /// catch up. Can be adjusted at runtime via the web UI.
    #[arg(long, default_value = "150")]
    latency_threshold: u32,

    /// List available audio output devices and exit.
    #[arg(long)]
    list_devices: bool,

    /// Disable the startup check for a newer release on GitHub.
    #[arg(long, env = "QUICMIC_NO_UPDATE_CHECK")]
    no_update_check: bool,

    /// Feed the 🔊 Speaker tab a synthetic 440 Hz tone instead of capturing
    /// system audio. Useful for testing the phone-to-earbud path on machines
    /// without WASAPI loopback (non-Windows).
    #[arg(long)]
    speaker_test_tone: bool,

    /// Which PC playback device the 🔊 Speaker tab captures (WASAPI loopback).
    /// A substring of the device name (e.g. "headphones") or the `[n]` index
    /// from `speaker-devices`; omit to capture the Windows default output.
    /// Switchable live from the console with `speaker-device`.
    #[arg(long, value_name = "NAME")]
    speaker_device: Option<String>,

    /// PC console banner theme: neon (cyan/magenta), ghoul
    /// (retro hacker green), or plain (no colors). Switchable live via `theme`.
    #[arg(long, default_value = "neon")]
    theme: String,

    /// Rename the phone-mic capture endpoint so apps list it under a
    /// recognizable name instead of "CABLE Output (VB-Audio Virtual Cable)".
    /// `auto` renames to the paired phone's device name on every pairing
    /// (the default on Windows); `off` disables automatic renames; any other
    /// value renames once at startup to that fixed name. The `mic-name`
    /// console command still works as a manual one-shot in any mode.
    /// Renaming needs one elevated run; the name sticks afterwards.
    #[arg(long, value_name = "MODE|NAME")]
    rename_mic: Option<String>,

    /// Run as a Windows system-tray app instead of a console window: when
    /// double-clicked the console is hidden and a tray icon takes its place
    /// (status line, "Show connection QR", Quit); when launched from a
    /// terminal the console stays and the tray runs alongside it. The console
    /// dashboard itself is unchanged. Windows only — on other platforms this
    /// prints a note and the app runs in console mode as usual.
    #[arg(long)]
    tray: bool,

    /// Force the terminal console UI even when a display is available.
    /// By default the app opens the native GUI window when a display is
    /// detected (always on Windows/macOS; on Linux when DISPLAY or
    /// WAYLAND_DISPLAY is set) and falls back to the console otherwise.
    #[arg(long)]
    console: bool,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        use std::io::IsTerminal;
        // Print the full error chain so the cause is visible even when the app
        // was double-clicked (where the console closes the instant we exit).
        eprintln!("\nError: {e:?}");
        // Pause only when double-clicked AND attached to a real terminal, so we
        // never block in a piped / CI / non-interactive context.
        if launched_by_double_click() && std::io::stdin().is_terminal() {
            pause_before_exit();
        }
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    // Install the default CryptoProvider for rustls (using ring)
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install rustls CryptoProvider"))?;

    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    // The native GUI replaces the terminal console as the default UI whenever
    // a display is available; `--console` (or no display, e.g. SSH without X
    // forwarding) keeps the old terminal UI.
    let use_gui = !cli.console && gui::display_available();
    if use_gui {
        info!("display detected — starting the native GUI (pass --console for the terminal UI)");
    }

    #[cfg(not(windows))]
    if cli.tray {
        // Tray mode needs the Windows shell notification area; everywhere else
        // the console dashboard is the app, so just say so and carry on.
        eprintln!("Note: --tray is Windows-only; continuing in console mode.");
    }

    let theme = console::Theme::parse(&cli.theme).unwrap_or_else(|| {
        eprintln!(
            "Invalid --theme '{}'. Valid themes: ghoul, neon, plain.",
            cli.theme
        );
        std::process::exit(2);
    });

    if cli.list_devices {
        println!("Available audio output devices:");
        for (i, name) in audio::list_output_devices().iter().enumerate() {
            println!("  [{}] {}", i, name);
        }
        return Ok(());
    }

    set_terminal_title("QuicMic");

    // ── Detect LAN IP ───────────────────────────────────────────────────
    let lan_ip: IpAddr = match &cli.ip {
        Some(ip) => parse_ip_arg(ip)?,
        None => detect_lan_ip()?,
    };

    // ── List audio devices ──────────────────────────────────────────────
    let devices = audio::list_output_devices();
    info!("Available audio output devices:");
    for (i, name) in devices.iter().enumerate() {
        info!("  [{}] {}", i, name);
    }

    // ── Create shared ring buffer ───────────────────────────────────────
    let ring = Arc::new(audio::RingBuffer::new(RING_BUFFER_SAMPLES));

    // ── Shared atomics ──────────────────────────────────────────────────
    let is_connected = Arc::new(AtomicBool::new(false));
    let session_token: Arc<parking_lot::Mutex<Option<String>>> =
        Arc::new(parking_lot::Mutex::new(None));
    // --noise-gate is given in dB (-100 = Off) to match the web UI slider; convert
    // it to the linear amplitude the audio thread and HTTP API use. The linear
    // clamp is kept as a final safety net.
    let noise_gate = Arc::new(AtomicU32::new(
        noise_gate_db_to_linear(cli.noise_gate)
            .clamp(server::NOISE_GATE_MIN, server::NOISE_GATE_MAX)
            .to_bits(),
    ));
    let gain = Arc::new(AtomicU32::new(
        cli.gain.clamp(server::GAIN_MIN, server::GAIN_MAX).to_bits(),
    ));
    let output_volume = Arc::new(AtomicU32::new(
        cli.volume
            .clamp(server::OUTPUT_VOLUME_MIN, server::OUTPUT_VOLUME_MAX)
            .to_bits(),
    ));
    // The hear-yourself monitor stream only exists when --monitor-device was
    // given; the phone UI can mute/unmute it at runtime via /api/monitor.
    let monitor_enabled = Arc::new(AtomicBool::new(true));
    let monitor_ring: Option<Arc<audio::RingBuffer>> = cli
        .monitor_device
        .as_ref()
        .map(|_| Arc::new(audio::RingBuffer::new(RING_BUFFER_SAMPLES)));
    let latency_threshold = Arc::new(AtomicU32::new(
        cli.latency_threshold.min(server::LATENCY_THRESHOLD_MAX_MS),
    ));
    let packets_received = Arc::new(AtomicU64::new(0));
    let packets_lost = Arc::new(AtomicU64::new(0));
    let source_sample_rate = Arc::new(AtomicU32::new(48000));
    let is_shutdown = Arc::new(AtomicBool::new(false));
    let device_ok = Arc::new(AtomicBool::new(false));
    // Holds the latest newer release tag once the startup update check finds one
    // (stays None otherwise). Surfaced via `/api/info` for the web UI banner.
    let update_status: Arc<parking_lot::Mutex<Option<String>>> =
        Arc::new(parking_lot::Mutex::new(None));

    // ── Start audio output (supervised: auto-rebuilds if the device drops) ──
    // `device_select` is shared with the PC console: the `device` command swaps
    // the selection and the supervisor rebuilds the stream on the new device.
    let device_select: Arc<parking_lot::Mutex<Option<String>>> =
        Arc::new(parking_lot::Mutex::new(cli.device.clone()));
    audio::spawn_output_supervisor(
        device_select.clone(),
        ring.clone(),
        source_sample_rate.clone(),
        latency_threshold.clone(),
        output_volume.clone(),
        None,
        device_ok.clone(),
    )?;

    // ── Hear-yourself monitor: a second supervised stream to a physical output
    // device, fed by its own ring so the SPSC contract of each ring stays intact.
    if let Some(name_opt) = &cli.monitor_device {
        // `--monitor-device` with no name → the host's default output device;
        // with a name → substring match, like `--device`.
        let monitor_name: Option<String> = name_opt.clone().filter(|n| !n.is_empty());
        let monitor_ok = Arc::new(AtomicBool::new(false));
        audio::spawn_output_supervisor(
            Arc::new(parking_lot::Mutex::new(monitor_name)),
            monitor_ring
                .clone()
                .expect("monitor ring exists when --monitor-device is given"),
            source_sample_rate.clone(),
            latency_threshold.clone(),
            output_volume.clone(),
            Some(monitor_enabled.clone()),
            monitor_ok,
        )?;
        warn!(
            "Monitor enabled: your mic audio now also plays through a physical output \
             device. Speakers + a live mic can feed back — headphones recommended. \
             Mute it anytime from the phone's settings panel."
        );
    }

    // ── Server identity: persistent PIN + TLS certificate ───────────────
    // The identity is what makes auto-connect possible: as long as the same
    // certificate and PIN are served, an already-paired phone reconnects
    // without a fresh QR scan or certificate accept. A `--pin` override is
    // validated here, then remembered by the identity store.
    if let Some(ref p) = cli.pin {
        if p.len() != 6 || !p.bytes().all(|b| b.is_ascii_digit()) {
            anyhow::bail!("--pin must be exactly 6 digits (0-9)");
        }
    }
    let data_dir = identity::data_dir(cli.data_dir.as_deref());
    let (wt_identity, identity, pin, identity_fresh) =
        identity::load_or_create(&data_dir, lan_ip, cli.dump_certs, cli.pin.clone())?;
    // Shared, mutable PIN: the API validates against it and the `newpin`
    // console command rotates it live.
    let pin_shared = std::sync::Arc::new(parking_lot::Mutex::new(pin));
    if identity_fresh {
        info!(
            dir = %data_dir.display(),
            "New server identity created — pair from the QR code below."
        );
    } else {
        info!(
            dir = %data_dir.display(),
            "Loaded saved server identity — already-paired phones reconnect automatically."
        );
    }

    // ── Print startup banner ────────────────────────────────────────────
    // Console mode only — the GUI shows the same information in its own
    // panels (Pair tab for the QR, Diagnostics for the cert hash).
    let url = format!("https://{}:{}", url_host(&lan_ip), cli.port);
    let theme_lock: Arc<parking_lot::Mutex<console::Theme>> =
        Arc::new(parking_lot::Mutex::new(theme));
    if !use_gui {
        if cli.console {
            info!("--console given — using the terminal console UI");
        } else {
            info!("no display detected — using the terminal console UI");
        }
        console::print_banner(
            &theme_lock,
            &url,
            &pin_shared.lock(),
            &identity.cert_hash_base64,
        );

        // Print QR code for easy mobile pairing (URL includes PIN as hash fragment)
        let qr_url = format!("{}#{}", url, pin_shared.lock());
        if let Err(e) = qr2term::print_qr(&qr_url) {
            info!("Could not print QR code: {}", e);
        }
        println!("Type 'help' and press Enter for console commands (status, qr, devices, ...).");
        println!(
            "Drop files phone↔PC: {}  (console: 'drop')",
            console::LOCALDROP_URL
        );
        println!();
    }

    // ── Tray mode (Windows) ─────────────────────────────────────────────
    // The icon and its menu-event channel are kept alive for the whole run.
    // On other platforms --tray is a no-op (noted above) and this stays None.
    // In GUI mode the tray menu gains a "Show / Hide window" item, handled by
    // run_gui_mode; in console mode the main select! loop below handles the
    // channel, exactly as before.
    #[cfg(windows)]
    let (tray_app, mut tray_rx): (
        Option<tray::TrayApp>,
        Option<tokio::sync::mpsc::UnboundedReceiver<tray::TrayAction>>,
    ) = if cli.tray {
        if launched_by_double_click() {
            // The console was created just for us — hide it so the app feels
            // like a real Windows app. Launched from a terminal, the user's
            // console is left alone and the tray runs alongside it.
            tray::hide_own_console();
        }
        let status = format!("QuicMic — {}:{}", url_host(&lan_ip), cli.port);
        // The pairing-QR page: use the LAN IP (bracketed for IPv6) because the
        // self-signed certificate's SAN covers the LAN IP, not 127.0.0.1.
        let tray_qr_url = format!("https://{}:{}/qr", url_host(&lan_ip), cli.port);
        // GUI mode and console mode share the tray icon, but only the GUI has
        // a window to show/hide: GUI mode gets the extra "Show / Hide window"
        // menu item, console mode keeps Show QR + Quit. The icon only changes
        // the presentation, never the server logic.
        let spawned = tray::spawn(&status, &tray_qr_url, use_gui);
        match spawned {
            Ok((app, rx)) => {
                info!("tray mode: running as a system-tray app");
                (Some(app), Some(rx))
            }
            Err(e) => {
                warn!("tray mode failed ({e:#}); continuing without tray");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Background, opt-out check for a newer release. Never blocks startup and stays
    // silent unless a strictly newer version is found. Opt-out sources: the
    // CLI flag / env var, or the GUI Settings checkbox (persisted next to the
    // server identity so it survives restarts).
    let prefs_path = gui::prefs_path(&data_dir);
    let prefs = gui::load_prefs(&prefs_path);
    let no_update_check = cli.no_update_check || prefs.update_check_opt_out;
    let update_check_ran = !no_update_check;
    if !no_update_check {
        let slot = update_status.clone();
        tokio::spawn(async move {
            if let Some(tag) = update_check::latest_if_newer().await {
                info!(
                    "A newer version {} is available (current {}). Releases: {}",
                    tag,
                    env!("CARGO_PKG_VERSION"),
                    update_check::releases_url()
                );
                *slot.lock() = Some(tag);
            }
        });
    }

    let (cancel_tx, _) = tokio::sync::broadcast::channel(16);

    // ── Build shared stream state ───────────────────────────────────────
    let stream_state = server::StreamState {
        ring: ring.clone(),
        is_connected: is_connected.clone(),
        session_token: session_token.clone(),
        noise_gate: noise_gate.clone(),
        gain: gain.clone(),
        latency_threshold: latency_threshold.clone(),
        output_volume: output_volume.clone(),
        monitor_ring: monitor_ring.clone(),
        monitor_enabled: monitor_enabled.clone(),
        test_capture: Arc::new(audio::MicTestBuffer::new()),
        packets_received: packets_received.clone(),
        packets_lost: packets_lost.clone(),
        source_sample_rate: source_sample_rate.clone(),
        cancel_tx,
        is_shutdown: is_shutdown.clone(),
        device_ok: device_ok.clone(),
        mic_peer: Arc::new(parking_lot::Mutex::new(None)),
        speaker_peers: Arc::new(parking_lot::Mutex::new(Vec::new())),
    };

    // ── Speaker (PC → phone) capture ────────────────────────────────────
    // Broadcasts 20 ms stereo PCM frames; the 🔊 Speaker tab serves them over
    // `/speaker-ws`. Capture runs on its own thread and keeps going even with
    // no listeners, so late joiners get audio immediately. A failed capture
    // only disables the Speaker tab — the mic path is unaffected.
    //
    // The capture device is selectable: `--speaker-device` at startup or the
    // `speaker-device` console command at runtime. Switching bumps
    // `speaker_generation`; the old capture thread sees the bump and exits,
    // and the console command starts a fresh thread on the new endpoint.
    let speaker_device: Arc<parking_lot::Mutex<Option<String>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let speaker_generation = Arc::new(AtomicU64::new(0));
    let speaker_running = Arc::new(AtomicBool::new(false));
    // Runtime test-tone state for the GUI's live toggle (Status tab).
    // Initialized from the CLI flag; default behavior is unchanged (tone off
    // unless requested). The GUI flips this at runtime via
    // gui::set_speaker_test_tone.
    let speaker_test_tone = Arc::new(AtomicBool::new(cli.speaker_test_tone));

    // Validate --speaker-device against the real endpoint list (Windows).
    #[cfg(windows)]
    if let Some(ref sel) = cli.speaker_device {
        match speaker::wasapi::list_render_devices() {
            Ok(names) => match speaker::resolve_name(&names, sel) {
                Ok(canonical) => *speaker_device.lock() = canonical,
                Err(e) => warn!("--speaker-device ignored: {e} Using system default."),
            },
            Err(e) => warn!("--speaker-device ignored: cannot list devices: {e:#}"),
        }
    }
    #[cfg(not(windows))]
    if cli.speaker_device.is_some() {
        warn!("--speaker-device needs Windows — ignored");
    }

    let speaker_tx = {
        let (tx, _) = tokio::sync::broadcast::channel::<Vec<f32>>(64);
        let running = if cli.speaker_test_tone {
            speaker::synth::spawn(tx.clone());
            info!("speaker: test-tone mode (440 Hz synthetic)");
            true
        } else {
            #[cfg(windows)]
            {
                match speaker::wasapi::spawn(
                    tx.clone(),
                    speaker_device.lock().clone(),
                    speaker_generation.clone(),
                ) {
                    Ok(()) => {
                        info!("speaker: WASAPI loopback capture started");
                        true
                    }
                    Err(e) => {
                        warn!("speaker: loopback failed to start: {e:#}");
                        false
                    }
                }
            }
            #[cfg(not(windows))]
            {
                warn!(
                    "speaker: system-audio capture needs Windows — re-run with --speaker-test-tone"
                );
                false
            }
        };
        speaker_running.store(running, Ordering::Relaxed);
        running.then_some(tx)
    };

    // ── Build axum app ──────────────────────────────────────────────────
    // Captured before the moves below for the console context.
    let monitor_present = monitor_ring.is_some();
    // Lets /api/update ask the main task to shut down after staging a new
    // exe, so the updater batch can swap and restart.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    // Resolve the mic endpoint naming policy. A fixed `--rename-mic "NAME"`
    // is applied once here at startup; `auto` renames on every phone pairing
    // (see the pair handler). Shared so the GUI can switch the mode at
    // runtime. Needs elevation on Windows; the name sticks afterwards, so one
    // admin run is enough.
    let mic_rename_mode = Arc::new(parking_lot::Mutex::new(mic_name::parse_rename_mic(
        cli.rename_mic.as_deref(),
    )));
    let applied_mic_name = match &*mic_rename_mode.lock() {
        mic_name::MicRenameMode::Fixed(name) => match mic_name::rename_mic(Some(name)) {
            Ok(applied) => {
                info!(applied = %applied, "Mic endpoint renamed (fixed --rename-mic)");
                Some(applied)
            }
            Err(e) => {
                warn!("--rename-mic failed: {e:#}");
                None
            }
        },
        _ => None,
    };

    let app_state = server::AppState {
        stream: stream_state.clone(),
        tls_identity: identity.clone(),
        pairing_pin: pin_shared.clone(),
        wt_port: cli.port,
        lan_ip: lan_ip.to_string(),
        pairing_throttle: Arc::new(parking_lot::Mutex::new(server::PairingThrottle::default())),
        update_status: update_status.clone(),
        speaker_tx: speaker_tx.clone(),
        shutdown_tx,
        phone_device_name: Arc::new(parking_lot::Mutex::new(None)),
        mic_rename_mode: mic_rename_mode.clone(),
        applied_mic_name: Arc::new(parking_lot::Mutex::new(applied_mic_name)),
    };

    // Clone the console-visible state before `app_state` moves into the router.
    let phone_device_name = app_state.phone_device_name.clone();
    let mic_rename_mode = app_state.mic_rename_mode.clone();
    let applied_mic_name = app_state.applied_mic_name.clone();

    let router = server::build_router(app_state);
    let tls_config = tls::build_rustls_config_async(&identity).await?;
    let https_addr = SocketAddr::new(lan_ip, cli.port);

    let axum_handle = axum_server::Handle::new();
    let axum_handle_clone = axum_handle.clone();

    // Launch HTTPS server
    let mut https_task = tokio::spawn(server::run_https_server(
        https_addr,
        router,
        tls_config,
        axum_handle_clone,
    ));

    // Launch WebTransport server
    let mut wt_task = tokio::spawn(server::run_webtransport_server(
        wt_identity,
        cli.port,
        stream_state.clone(),
    ));

    // ── GUI mode ────────────────────────────────────────────────────────
    // The native window replaces the console as the default UI. eframe runs
    // its event loop on this (main) thread; the servers keep running on the
    // tokio workers. When the window closes, the normal graceful shutdown
    // runs below.
    if use_gui {
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let gctx = gui::GuiCtx {
            stream: stream_state.clone(),
            url: url.clone(),
            pin: pin_shared.clone(),
            data_dir: data_dir.clone(),
            cert_hash: identity.cert_hash_base64.clone(),
            device_select: device_select.clone(),
            speaker_running: speaker_running.clone(),
            speaker_test_tone: speaker_test_tone.clone(),
            cli_test_tone: cli.speaker_test_tone,
            speaker_tx: speaker_tx.clone(),
            speaker_generation: speaker_generation.clone(),
            speaker_device: speaker_device.clone(),
            monitor_present,
            mic_test_running: Arc::new(AtomicBool::new(false)),
            mic_test_error: Arc::new(parking_lot::Mutex::new(None)),
            phone_device_name: phone_device_name.clone(),
            mic_rename_mode: mic_rename_mode.clone(),
            applied_mic_name: applied_mic_name.clone(),
            update_status: update_status.clone(),
            port: cli.port,
            lan_ip: lan_ip.to_string(),
            update_check_ran,
            prefs_path: prefs_path.clone(),
            shutdown_requested: shutdown_requested.clone(),
            egui_ctx: Arc::new(parking_lot::Mutex::new(None)),
        };
        return run_gui_mode(
            gctx,
            stream_state,
            axum_handle,
            #[cfg(windows)]
            tray_app,
            #[cfg(windows)]
            tray_rx,
            https_task,
            wt_task,
            shutdown_rx,
        )
        .await;
    }

    // ── Console command loop ────────────────────────────────────────────
    // A blocking stdin reader forwards typed lines to the main task. With
    // stdin closed or piped the iterator ends immediately and the thread exits.
    let ctx = console::Ctx {
        theme: theme_lock.clone(),
        url: url.clone(),
        pin: pin_shared.clone(),
        data_dir: data_dir.clone(),
        cert_hash: identity.cert_hash_base64.clone(),
        device_select: device_select.clone(),
        stream: stream_state.clone(),
        speaker_tx: speaker_tx.clone(),
        speaker_running: speaker_running.clone(),
        speaker_test_tone: cli.speaker_test_tone,
        speaker_device: speaker_device.clone(),
        speaker_generation: speaker_generation.clone(),
        monitor_present,
        phone_device_name,
        mic_rename_mode,
        applied_mic_name,
    };
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::Builder::new()
        .name("console-input".into())
        .spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) => {
                        if cmd_tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();

    // Main event loop: Ctrl+C, console commands, or a server task ending.
    // (A plain select! would exit after the first console command — the loop
    // keeps serving until something actually ends the process.)
    let mut cmds_live = true;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                graceful_shutdown(&stream_state, &axum_handle).await;
                break;
            }
            // --tray (Windows): "Quit" in the tray menu runs the same graceful
            // shutdown as Ctrl+C. select! branches can't carry #[cfg], so the
            // Windows-only work lives inside the cfg'd block and this branch
            // just pends forever on other platforms (or when tray mode is off).
            tray_quit = async {
                #[cfg(windows)]
                {
                    if let Some(rx) = tray_rx.as_mut() {
                        loop {
                            match rx.recv().await {
                                // "Show connection QR" is handled inside the
                                // tray module (opens the browser); keep
                                // listening for the next click.
                                Some(tray::TrayAction::ShowQr) => {}
                                // Console mode spawns the tray without the
                                // window-toggle item, so this never arrives;
                                // the arm is only here for exhaustiveness.
                                Some(tray::TrayAction::ToggleWindow) => {}
                                Some(tray::TrayAction::Quit) => return true,
                                None => return false,
                            }
                        }
                    }
                }
                std::future::pending::<bool>().await
            } => {
                if tray_quit {
                    graceful_shutdown(&stream_state, &axum_handle).await;
                    break;
                }
            }
            // `/api/update` staged a new exe: shut down so the updater batch
            // can swap the file and restart it.
            _ = shutdown_rx.recv() => {
                info!("Update requested via API — shutting down to apply.");
                graceful_shutdown(&stream_state, &axum_handle).await;
                break;
            }
            cmd = async {
                if cmds_live {
                    cmd_rx.recv().await
                } else {
                    std::future::pending::<Option<String>>().await
                }
            } => {
                match cmd {
                    Some(line) => {
                        if console::handle_command(&line, &ctx).await == console::Action::Quit {
                            graceful_shutdown(&stream_state, &axum_handle).await;
                            break;
                        }
                    }
                    // stdin closed or the input thread died: stop polling commands,
                    // keep serving until Ctrl+C or a server task ends.
                    None => cmds_live = false,
                }
            }
            res = &mut https_task => {
                // The HTTPS server returns only on a bind failure or crash — fatal,
                // so surface it instead of exiting silently.
                res.map_err(|e| anyhow::anyhow!("HTTPS server task failed: {e}"))??;
                break;
            }
            res = &mut wt_task => {
                res.map_err(|e| anyhow::anyhow!("WebTransport server task failed: {e}"))??;
                break;
            }
        }
    }

    Ok(())
}

/// GUI mode: background watchers (Ctrl+C, `/api/update`, a dying server task)
/// close the window; eframe itself runs on this (main) thread while the
/// servers keep running on the tokio workers. When the window closes, the
/// same graceful shutdown as the console path runs.
#[allow(clippy::too_many_arguments)]
async fn run_gui_mode(
    g: gui::GuiCtx,
    stream_state: server::StreamState,
    axum_handle: axum_server::Handle<SocketAddr>,
    #[cfg(windows)] tray_app: Option<tray::TrayApp>,
    #[cfg(windows)] mut tray_rx: Option<tokio::sync::mpsc::UnboundedReceiver<tray::TrayAction>>,
    mut https_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    mut wt_task: tokio::task::JoinHandle<anyhow::Result<()>>,
    mut shutdown_rx: tokio::sync::mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    // The tray icon must stay alive for the whole run; dropping it removes
    // the icon from the notification area.
    #[cfg(windows)]
    let _tray_app = tray_app;

    // Holds a fatal server-task error, if one happens, so it can be returned
    // after the window closes instead of being lost.
    let server_error: Arc<parking_lot::Mutex<Option<anyhow::Error>>> =
        Arc::new(parking_lot::Mutex::new(None));

    // Closes the GUI window from any thread. The graceful shutdown itself
    // runs after `gui::run` returns, on this task.
    let shutdown_requested = g.shutdown_requested.clone();
    let egui_ctx = g.egui_ctx.clone();
    let close_window = {
        // The closure takes its own clone so the outer `egui_ctx` stays
        // usable for the Windows tray task below.
        let egui_ctx = egui_ctx.clone();
        move || {
            shutdown_requested.store(true, Ordering::SeqCst);
            if let Some(ctx) = egui_ctx.lock().clone() {
                ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            }
        }
    };

    // Tray menu (Windows `--tray`): "Show / Hide window" flips the viewport
    // visibility, "Quit" closes the window. This runs on its own task rather
    // than in the GUI frame loop so a hidden window (which draws no frames)
    // can still be shown again. "Show connection QR" is handled inside the
    // tray module itself (opens the browser) and never reaches the channel.
    #[cfg(windows)]
    tokio::spawn({
        let close_window = close_window.clone();
        let egui_ctx = egui_ctx.clone();
        let window_visible = Arc::new(AtomicBool::new(true));
        async move {
            let Some(rx) = tray_rx.as_mut() else {
                return;
            };
            while let Some(action) = rx.recv().await {
                match action {
                    tray::TrayAction::ShowQr => {}
                    tray::TrayAction::ToggleWindow => {
                        let visible = !window_visible.load(Ordering::SeqCst);
                        window_visible.store(visible, Ordering::SeqCst);
                        if let Some(ctx) = egui_ctx.lock().clone() {
                            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Visible(visible));
                        }
                    }
                    tray::TrayAction::Quit => close_window(),
                }
            }
        }
    });

    // Ctrl+C — the console path's signal, routed through the window.
    tokio::spawn({
        let close_window = close_window.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                info!("Ctrl+C received — shutting down.");
                close_window();
            }
        }
    });

    // `/api/update` staged a new exe: shut down so the updater can swap it.
    tokio::spawn({
        let close_window = close_window.clone();
        async move {
            if shutdown_rx.recv().await.is_some() {
                info!("Update requested via API — shutting down to apply.");
                close_window();
            }
        }
    });

    // A server task ending is fatal (bind failure, crash) — surface it like
    // the console loop does instead of leaving a dead window up. The watcher
    // is cancelled after `gui::run` returns, *before* graceful shutdown tears
    // the servers down: otherwise the teardown's own task endings would race
    // the watcher and turn a clean Quit into a spurious "ended unexpectedly"
    // error.
    let (watcher_cancel_tx, watcher_cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let watcher = tokio::spawn({
        let close_window = close_window.clone();
        let server_error = server_error.clone();
        async move {
            let err: anyhow::Error = tokio::select! {
                res = &mut https_task => res
                    .map_err(|e| anyhow::anyhow!("HTTPS server task failed: {e}"))
                    .and_then(|r| r)
                    .err()
                    .unwrap_or_else(|| anyhow::anyhow!("HTTPS server task ended unexpectedly")),
                res = &mut wt_task => res
                    .map_err(|e| anyhow::anyhow!("WebTransport server task failed: {e}"))
                    .and_then(|r| r)
                    .err()
                    .unwrap_or_else(|| anyhow::anyhow!("WebTransport server task ended unexpectedly")),
                _ = watcher_cancel_rx => return,
            };
            *server_error.lock() = Some(err);
            close_window();
        }
    });

    // Blocks until the window closes (Quit button, tray, Ctrl+C, updater).
    if let Err(e) = gui::run(g) {
        return Err(anyhow::anyhow!(
            "GUI failed to start: {e:#} (re-run with --console for the terminal UI)"
        ));
    }

    // Normal close: cancel the watcher first. If a server task already ended
    // with a real error, the watcher kept it in `server_error` and this send
    // is a no-op; otherwise dropping the watcher's JoinHandles detaches the
    // still-running server tasks so the teardown below can't trip it.
    let _ = watcher_cancel_tx.send(());
    let _ = watcher.await;

    graceful_shutdown(&stream_state, &axum_handle).await;

    if let Some(err) = server_error.lock().take() {
        return Err(err);
    }
    Ok(())
}

/// Shared graceful-shutdown sequence, used by Ctrl+C, the `quit` console
/// command, and the tray menu's Quit item. See the F5-handover / disconnect-detection
/// notes in AGENTS.md for why the 503-then-wait order matters.
async fn graceful_shutdown(
    stream_state: &server::StreamState,
    axum_handle: &axum_server::Handle<SocketAddr>,
) {
    info!("Graceful shutdown initiated.");

    let had_client = stream_state.is_connected.load(Ordering::SeqCst);

    // Flip the HTTP API to 503 and end the active session. Clients detect
    // the shutdown by polling the API — their transport close event is
    // unreliable/late on iOS Safari — so the only thing that matters is
    // keeping the API up (replying 503) long enough for that poll to land.
    stream_state.is_shutdown.store(true, Ordering::SeqCst);
    let _ = stream_state.cancel_tx.send(());

    if had_client {
        tokio::time::sleep(SHUTDOWN_GRACE_PERIOD).await;
    }

    axum_handle.graceful_shutdown(Some(std::time::Duration::from_millis(200)));
    info!("Graceful shutdown complete. Exiting.");
}

/// Parse the `--ip` argument into an `IpAddr`, tolerating a bracketed IPv6 literal
/// (`[fe80::1]`) since that is how it appears in a URL and how a user is likely to
/// copy it. Brackets are stripped only as a matched pair; anything else is parsed
/// as-is, and a clear error names the offending value.
fn parse_ip_arg(s: &str) -> anyhow::Result<IpAddr> {
    let bare = s
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(s);
    bare.parse::<IpAddr>()
        .map_err(|e| anyhow::anyhow!("invalid --ip value '{s}': {e}"))
}

/// Format an IP address for use in a URL authority, bracketing IPv6 literals
/// (`https://[fe80::1]:8443`) as required by RFC 3986. IPv4 is returned unchanged.
/// The client applies the same bracketing when it builds the WebTransport URL.
fn url_host(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V6(_) => format!("[{ip}]"),
        IpAddr::V4(_) => ip.to_string(),
    }
}

/// Set the terminal window title, best-effort and cross-platform.
fn set_terminal_title(title: &str) {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        // `SetConsoleTitleW` (kernel32, already linked by std) sets the title in
        // cmd, PowerShell, conhost, and Windows Terminal without needing virtual
        // terminal processing to be enabled.
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleTitleW(title: *const u16) -> i32;
        }

        let wide: Vec<u16> = OsStr::new(title)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer that outlives the call.
        unsafe {
            SetConsoleTitleW(wide.as_ptr());
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        // OSC 0 sets the window title in xterm, GNOME Terminal, Terminal.app,
        // iTerm2, and most other terminals.
        print!("\x1b]0;{title}\x07");
        let _ = std::io::stdout().flush();
    }
}

/// Best-effort detection of whether the app was launched by double-clicking
/// (rather than from an existing terminal). Used to keep the window open on a
/// startup error so the user can read it.
#[cfg(windows)]
fn launched_by_double_click() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleProcessList(lpdwProcessList: *mut u32, dwProcessCount: u32) -> u32;
    }
    let mut buffer = [0u32; 2];
    // SAFETY: GetConsoleProcessList writes up to `buffer.len()` entries into the
    // valid stack buffer and returns the number of processes on the console.
    let count = unsafe { GetConsoleProcessList(buffer.as_mut_ptr(), buffer.len() as u32) };
    // Only our own process is attached -> the console was created just for us.
    count <= 1
}

#[cfg(not(windows))]
fn launched_by_double_click() -> bool {
    false
}

/// Wait for the user to press Enter, so a console window created just for this
/// process doesn't vanish before an error message can be read.
fn pause_before_exit() {
    use std::io::Write;
    print!("\nPress Enter to exit...");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
}

#[cfg(test)]
mod tests {
    use super::{
        is_unusable_lan_addr, lan_addr_rank, noise_gate_db_to_linear, parse_ip_arg, pick_lan_ip,
        url_host,
    };
    use std::net::IpAddr;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d))
    }

    fn v6(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn fake_ip_benchmark_range_is_unusable() {
        // Fake-ip proxy TUNs (mihomo/Clash, sing-box, Surge, etc.) use the RFC
        // 2544 benchmark range 198.18/15 as their default fake-ip range.
        assert!(is_unusable_lan_addr(v4(198, 18, 0, 1)));
        assert!(is_unusable_lan_addr(v4(198, 19, 255, 255)));
        // Boundaries outside 198.18.0.0/15 are not caught by this exclusion.
        assert!(!is_unusable_lan_addr(v4(198, 17, 255, 255)));
        assert!(!is_unusable_lan_addr(v4(198, 20, 0, 1)));
        // Real LAN addresses stay usable.
        assert!(!is_unusable_lan_addr(v4(192, 168, 1, 16)));
        assert!(!is_unusable_lan_addr(v4(10, 0, 0, 5)));
    }

    #[test]
    fn loopback_link_local_and_multicast_are_unusable() {
        assert!(is_unusable_lan_addr(v4(0, 0, 0, 0))); // 0.0.0.0/8
        assert!(is_unusable_lan_addr(v4(0, 1, 2, 3)));
        assert!(is_unusable_lan_addr(v4(127, 0, 0, 1)));
        assert!(is_unusable_lan_addr(v4(169, 254, 10, 20)));
        assert!(is_unusable_lan_addr(v4(224, 0, 0, 1)));
        assert!(is_unusable_lan_addr(v4(255, 255, 255, 255))); // reserved / broadcast
        assert!(is_unusable_lan_addr(v6("::"))); // unspecified
        assert!(is_unusable_lan_addr(v6("::1"))); // loopback
        assert!(is_unusable_lan_addr(v6("fe80::1"))); // link-local
        assert!(is_unusable_lan_addr(v6("ff02::1"))); // multicast
    }

    #[test]
    fn ranking_prefers_private_then_cgnat_then_public() {
        assert_eq!(lan_addr_rank(v4(192, 168, 1, 16)), 2);
        assert_eq!(lan_addr_rank(v4(10, 1, 1, 1)), 2);
        assert_eq!(lan_addr_rank(v4(172, 16, 0, 1)), 2);
        assert_eq!(lan_addr_rank(v4(172, 31, 255, 255)), 2);
        assert_eq!(lan_addr_rank(v4(100, 64, 0, 2)), 1); // CGNAT
        assert_eq!(lan_addr_rank(v4(100, 127, 255, 255)), 1); // CGNAT
        assert_eq!(lan_addr_rank(v6("2001:db8::1")), 1); // IPv6
        assert_eq!(lan_addr_rank(v4(172, 32, 0, 1)), 0); // Public
        assert_eq!(lan_addr_rank(v4(8, 8, 8, 8)), 0); // Public
    }

    #[test]
    fn picker_skips_tun_fake_ip_and_prefers_the_real_lan() {
        // The bug pattern: a proxy TUN adapter ("sekai") owns the fake-ip
        // address 198.18.0.1; the real Ethernet is 192.168.1.16. The picker must
        // return the real LAN address, never the fake one.
        let ifas = vec![
            ("sekai".to_string(), v4(198, 18, 0, 1)),
            ("Ethernet".to_string(), v4(192, 168, 1, 16)),
        ];
        assert_eq!(pick_lan_ip(ifas), Some(v4(192, 168, 1, 16)));
    }

    #[test]
    fn picker_deprioritizes_virtual_adapters_on_same_rank_ties() {
        // Same rank (RFC 1918) and same family: a vEthernet (Hyper-V/WSL)
        // holding 172.16.0.1 loses to the real Wi-Fi 192.168.1.42.
        let ifas = vec![
            ("Wi-Fi".to_string(), v4(192, 168, 1, 42)),
            ("vEthernet (Default Switch)".to_string(), v4(172, 16, 0, 1)),
        ];
        assert_eq!(pick_lan_ip(ifas), Some(v4(192, 168, 1, 42)));

        // VirtualBox host-only adapter also yields to real Ethernet.
        let ifas_vbox = vec![
            (
                "VirtualBox Host-Only Ethernet Adapter".to_string(),
                v4(192, 168, 56, 1),
            ),
            ("Ethernet".to_string(), v4(192, 168, 1, 16)),
        ];
        assert_eq!(pick_lan_ip(ifas_vbox), Some(v4(192, 168, 1, 16)));
    }

    #[test]
    fn picker_deprioritizes_virtual_adapters_regardless_of_enumeration_order() {
        let ifas = vec![
            ("vEthernet (WSL)".to_string(), v4(172, 20, 10, 1)),
            ("Ethernet".to_string(), v4(192, 168, 1, 16)),
        ];
        assert_eq!(pick_lan_ip(ifas), Some(v4(192, 168, 1, 16)));
    }

    #[test]
    fn picker_keeps_virtual_adapter_when_nothing_else_is_usable() {
        let ifas = vec![("vEthernet (Default Switch)".to_string(), v4(172, 16, 0, 1))];
        assert_eq!(pick_lan_ip(ifas), Some(v4(172, 16, 0, 1)));
    }

    #[test]
    fn picker_returns_none_when_every_address_is_unusable() {
        let ifas = vec![
            ("tun".to_string(), v4(198, 18, 0, 1)),
            ("lo".to_string(), v4(127, 0, 0, 1)),
        ];
        assert_eq!(pick_lan_ip(ifas), None);
    }

    #[test]
    fn picker_falls_back_to_ipv6_when_only_v6_is_usable() {
        let ifas = vec![
            ("tun".to_string(), v4(198, 18, 0, 1)),
            ("eth".to_string(), v6("fd00::1")),
        ];
        assert_eq!(pick_lan_ip(ifas), Some(v6("fd00::1")));
    }

    #[test]
    fn parse_ip_arg_accepts_bare_and_bracketed() {
        assert_eq!(
            parse_ip_arg("192.168.1.42").unwrap().to_string(),
            "192.168.1.42"
        );
        assert_eq!(parse_ip_arg("fe80::1").unwrap().to_string(), "fe80::1");
        // A bracketed IPv6 literal (as copied from a URL) is accepted.
        assert_eq!(parse_ip_arg("[fe80::1]").unwrap().to_string(), "fe80::1");
        assert_eq!(parse_ip_arg("[::1]").unwrap().to_string(), "::1");
        // Unbalanced brackets / non-addresses are rejected with a clear error.
        assert!(parse_ip_arg("[fe80::1").is_err());
        assert!(parse_ip_arg("not-an-ip").is_err());
    }

    #[test]
    fn url_host_brackets_ipv6_only() {
        // IPv4 is unchanged; IPv6 literals are bracketed for the URL authority.
        assert_eq!(
            url_host(&"192.168.1.42".parse::<IpAddr>().unwrap()),
            "192.168.1.42"
        );
        assert_eq!(url_host(&"::1".parse::<IpAddr>().unwrap()), "[::1]");
        assert_eq!(url_host(&"fe80::1".parse::<IpAddr>().unwrap()), "[fe80::1]");
    }

    #[test]
    fn noise_gate_db_floor_disables_gate() {
        // -100 dB (and anything below it, after clamping) maps to a hard 0.0, i.e.
        // the gate is disabled rather than a tiny-but-nonzero amplitude.
        assert_eq!(noise_gate_db_to_linear(-100.0), 0.0);
        assert_eq!(noise_gate_db_to_linear(-250.0), 0.0);
    }

    #[test]
    fn noise_gate_db_max_is_unity() {
        // 0 dB is full scale; values above are clamped down to it.
        assert!((noise_gate_db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((noise_gate_db_to_linear(12.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn noise_gate_db_converts_linearly() {
        // -6 dB ≈ 0.501 in linear amplitude (10^(-6/20)).
        assert!((noise_gate_db_to_linear(-6.0) - 0.5011872).abs() < 1e-4);
        // -20 dB is exactly 0.1.
        assert!((noise_gate_db_to_linear(-20.0) - 0.1).abs() < 1e-5);
    }
}
