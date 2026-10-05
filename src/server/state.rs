//! Shared server state and single-connection lifecycle management.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::RingBuffer;
use crate::mic_name::MicRenameMode;
use crate::tls::TlsIdentity;

/// State shared between both transport handlers and the HTTP API.
/// Extracted as a separate struct to keep the transport layer decoupled
/// from HTTP-specific fields.
#[derive(Clone)]
pub struct StreamState {
    pub ring: Arc<RingBuffer>,
    pub is_connected: Arc<AtomicBool>,
    pub session_token: Arc<parking_lot::Mutex<Option<String>>>,
    pub noise_gate: Arc<AtomicU32>, // f32::to_bits(), 0.0 = disabled
    pub gain: Arc<AtomicU32>,       // f32::to_bits(), 1.0 = unity
    pub latency_threshold: Arc<AtomicU32>, // u32 milliseconds
    /// PC-side output volume multiplier (f32 bits, 1.0 = unity). Applied in the
    /// output stage after resampling, so it scales both the virtual-device
    /// stream and the hear-yourself monitor stream. Adjustable at runtime via
    /// `POST /api/settings` (and the phone's settings panel).
    pub output_volume: Arc<AtomicU32>, // f32::to_bits(), 1.0 = unity
    /// Second ring feeding the hear-yourself monitor stream. Always present
    /// (cheap buffer); the monitor *stream* itself is created at startup via
    /// `--monitor-device` or on demand from the PC UI. The decode hot path
    /// pushes to both rings, keeping each ring's SPSC contract intact.
    /// Speech-focused noise cancellation state (RNNoise + voice gate),
    /// applied on the network receive path before samples enter the ring.
    /// `None`-free by design: the denoiser itself carries the on/off setting
    /// (default on) so there is a single source of truth, adjustable at
    /// runtime via `POST /api/settings`. The receive path is the only
    /// producer, so this lock is uncontended in practice.
    pub denoiser: Arc<parking_lot::Mutex<crate::audio::SpeechDenoiser>>,
    pub monitor_ring: Option<Arc<RingBuffer>>,
    /// Runtime mute for the monitor stream, toggled from the phone UI via
    /// `POST /api/monitor`. Checked in the monitor output callback: `true` =
    /// audible. Defaults to `true` when the monitor stream is created.
    pub monitor_enabled: Arc<AtomicBool>,
    /// Whether the hear-yourself monitor supervisor thread is running. Set at
    /// startup when `--monitor-device` is given, or later when the PC UI's
    /// "Hear how I sound" button spawns it on demand.
    pub monitor_spawned: Arc<AtomicBool>,
    pub packets_received: Arc<AtomicU64>,
    pub packets_lost: Arc<AtomicU64>,
    pub source_sample_rate: Arc<AtomicU32>, // Client's actual capture rate
    pub cancel_tx: tokio::sync::broadcast::Sender<()>,
    pub is_shutdown: Arc<AtomicBool>,
    /// Whether a working audio output stream is currently active. Cleared while the
    /// output device is lost (disabled/removed) and the supervisor is rebuilding it;
    /// surfaced to the client via `/api/stats` so the UI can warn the user.
    pub device_ok: Arc<AtomicBool>,
    /// IP of the currently connected Mic client (`None` when idle). Set by both
    /// transports on connect/disconnect; read by the PC console status panel.
    pub mic_peer: Arc<parking_lot::Mutex<Option<String>>>,
    /// IPs of the currently connected Speaker clients. Updated on
    /// connect/disconnect; read by the PC console status panel.
    pub speaker_peers: Arc<parking_lot::Mutex<Vec<String>>>,
}

/// Consecutive failed PIN attempts (per client IP) before a lockout kicks in.
const MAX_FAILED_ATTEMPTS: u32 = 5;
/// How long a client IP stays locked out after hitting the failure threshold.
const LOCKOUT_DURATION: Duration = Duration::from_secs(30);
/// A client IP's failure count is forgotten after this long with no new failed
/// attempt. This both decays the counter for a legitimate user who simply
/// mistyped and bounds the map's memory: idle entries are pruned away.
const FAILURE_DECAY: Duration = Duration::from_secs(60);

/// Per-IP brute-force counter for the pairing endpoint.
struct ThrottleEntry {
    failed_attempts: u32,
    locked_until: Option<Instant>,
    last_seen: Instant,
}

/// Brute-force protection state for the pairing endpoint, keyed by client IP so
/// one misbehaving host cannot lock everyone else out (the previous single global
/// counter did). Kept behind one lock (in `AppState`) so each IP's counter and
/// lockout deadline are read and updated together. Idle/expired entries are
/// pruned on every check, so the map stays bounded — and a completed HTTPS pair
/// request requires a real TCP+TLS handshake, so the key cannot be spoofed to
/// flood it.
#[derive(Default)]
pub struct PairingThrottle {
    entries: HashMap<IpAddr, ThrottleEntry>,
}

impl PairingThrottle {
    /// Remaining lockout seconds for `ip` if it is currently locked out. Clears an
    /// expired lockout and prunes stale entries as a side effect.
    pub(super) fn locked_remaining(&mut self, ip: IpAddr, now: Instant) -> Option<u64> {
        self.prune(now);
        let entry = self.entries.get_mut(&ip)?;
        match entry.locked_until {
            Some(until) if until > now => Some((until - now).as_secs()),
            _ => {
                entry.locked_until = None;
                None
            }
        }
    }

    /// Record a failed attempt for `ip`. Returns `(attempts, locked)` where
    /// `attempts` is the running failure count and `locked` is true if this
    /// attempt tripped the lockout (after which the count resets).
    pub(super) fn register_failure(&mut self, ip: IpAddr, now: Instant) -> (u32, bool) {
        let entry = self.entries.entry(ip).or_insert(ThrottleEntry {
            failed_attempts: 0,
            locked_until: None,
            last_seen: now,
        });
        entry.last_seen = now;
        entry.failed_attempts += 1;
        let attempts = entry.failed_attempts;
        let locked = attempts >= MAX_FAILED_ATTEMPTS;
        if locked {
            entry.locked_until = Some(now + LOCKOUT_DURATION);
            entry.failed_attempts = 0;
        }
        (attempts, locked)
    }

    /// Clear all failure state for `ip` after a successful pairing.
    pub(super) fn clear(&mut self, ip: IpAddr) {
        self.entries.remove(&ip);
    }

    /// Drop entries that are neither actively locked out nor recently active, so
    /// the map only ever holds IPs with live brute-force state.
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, e| {
            matches!(e.locked_until, Some(until) if until > now)
                || now.duration_since(e.last_seen) < FAILURE_DECAY
        });
    }
}

/// Full application state accessible from all HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    pub stream: StreamState,
    pub tls_identity: TlsIdentity,
    pub pairing_pin: Arc<parking_lot::Mutex<String>>,
    pub wt_port: u16,
    pub lan_ip: String,
    pub pairing_throttle: Arc<parking_lot::Mutex<PairingThrottle>>,
    /// Latest newer release tag found by the startup update check, if any. Read by
    /// `/api/info` so the web UI can show a small "update available" banner.
    pub update_status: Arc<parking_lot::Mutex<Option<String>>>,
    /// Broadcasts 20 ms stereo PCM frames from the PC's loopback capture to
    /// `/speaker-ws` listeners. `None` when speaker capture isn't running
    /// (non-Windows without `--speaker-test-tone`, or capture failed to start).
    pub speaker_tx: Option<tokio::sync::broadcast::Sender<Vec<f32>>>,
    /// Lets `/api/update` ask the main task to shut down after it stages a new
    /// exe, so the updater batch can swap and restart. Buffered (1) so the
    /// handler never blocks on a main loop that's already exiting.
    pub shutdown_tx: tokio::sync::mpsc::Sender<()>,
    /// The phone's self-reported device name from the last pairing (`None`
    /// until the first pair). Shown in the console status panel and used for
    /// automatic mic renames.
    pub phone_device_name: Arc<parking_lot::Mutex<Option<String>>>,
    /// How the capture endpoint's display name is managed (`--rename-mic`).
    /// Shared so the GUI can switch the mode at runtime; the pair handler
    /// reads it on every pairing.
    pub mic_rename_mode: Arc<parking_lot::Mutex<MicRenameMode>>,
    /// The endpoint name this server actually applied (`None` when it never
    /// renamed anything). Reported to the phone so its UI can show what apps
    /// like Discord will list as the mic input.
    pub applied_mic_name: Arc<parking_lot::Mutex<Option<String>>>,
    /// The last mic-rename failure, if any (`None` when the last rename
    /// succeeded or none was attempted). Surfaced in the GUI so a failed
    /// auto-rename (usually: not running as administrator) is visible
    /// instead of only being a `warn!` log line.
    pub mic_rename_error: Arc<parking_lot::Mutex<Option<String>>>,
    /// Where the server identity, GUI prefs, and update markers live. Used by
    /// `/api/update` so the staged-update marker lands next to the identity.
    pub data_dir: std::path::PathBuf,
}

/// Try to atomically claim the single-connection slot, retrying briefly to
/// absorb a fast F5 handover. Returns `true` once acquired. Shared by both
/// transports so the CAS policy lives in one place.
pub(super) async fn acquire_connection_slot(is_connected: &AtomicBool) -> bool {
    for _ in 0..10 {
        if is_connected
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// RAII guard that releases the single-connection slot when dropped, however
/// the session task ends (completion, cancellation, or panic).
pub(super) struct ConnectionGuard {
    is_connected: Arc<AtomicBool>,
}

impl ConnectionGuard {
    pub(super) fn new(is_connected: Arc<AtomicBool>) -> Self {
        Self { is_connected }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.is_connected.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::{acquire_connection_slot, ConnectionGuard};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn slot_acquires_and_guard_releases() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(acquire_connection_slot(&flag).await);
        assert!(flag.load(Ordering::SeqCst));
        {
            let _guard = ConnectionGuard::new(flag.clone());
            assert!(flag.load(Ordering::SeqCst));
        }
        assert!(
            !flag.load(Ordering::SeqCst),
            "dropping the guard must release the slot"
        );
    }

    #[tokio::test]
    async fn slot_acquisition_fails_when_already_taken() {
        let flag = Arc::new(AtomicBool::new(true));
        assert!(!acquire_connection_slot(&flag).await);
    }

    #[test]
    fn throttle_locks_out_per_ip_independently() {
        use super::{PairingThrottle, MAX_FAILED_ATTEMPTS};
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::Instant;

        let mut throttle = PairingThrottle::default();
        let attacker = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        let victim = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 51));
        let now = Instant::now();

        // The attacker burns through the failure budget and gets locked out.
        for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
            assert!(!throttle.register_failure(attacker, now).1);
        }
        assert!(
            throttle.register_failure(attacker, now).1,
            "5th failure locks"
        );
        assert!(throttle.locked_remaining(attacker, now).is_some());

        // A different IP is unaffected — the old global counter would have locked
        // it too.
        assert!(throttle.locked_remaining(victim, now).is_none());

        // A successful pair clears the attacker's state.
        throttle.clear(attacker);
        assert!(throttle.locked_remaining(attacker, now).is_none());
    }

    #[test]
    fn throttle_prunes_idle_entries() {
        use super::{PairingThrottle, FAILURE_DECAY};
        use std::net::{IpAddr, Ipv4Addr};
        use std::time::Instant;

        let mut throttle = PairingThrottle::default();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let t0 = Instant::now();

        // One failure, then no activity for longer than the decay window.
        throttle.register_failure(ip, t0);
        let later = t0 + FAILURE_DECAY + std::time::Duration::from_secs(1);

        // The stale entry is pruned, so the counter has effectively reset.
        assert!(throttle.locked_remaining(ip, later).is_none());
        assert_eq!(
            throttle.register_failure(ip, later).0,
            1,
            "decayed entry must restart the count at 1"
        );
    }
}
