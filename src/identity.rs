//! Persistent server identity: the pairing PIN and the TLS certificate are kept
//! on disk (in the per-user app data directory) so a phone that paired once can
//! reconnect automatically after the PC app restarts. Previously both were
//! regenerated on every launch, which forced a fresh QR scan and a new
//! certificate-accept on the phone each time — the reason "auto-connect" could
//! never work.
//!
//! Layout inside the data dir:
//!   identity.json  — { pin, lan_ip, created_at, expires_at } (unix seconds)
//!   cert.der       — the self-signed leaf certificate (DER)
//!   key.der        — the ECDSA P-256 private key (PKCS#8 DER, 0600 on unix)
//!
//! The stored identity is reused only while it is still valid: the LAN IP must
//! match (a new DHCP lease means a new certificate SAN), and the certificate
//! must not be within a day of expiry (self-signed certs live 14 days, the
//! WebTransport maximum for `serverCertificateHashes`). Otherwise a fresh
//! identity is generated and persisted, and the phone is told (via the cert
//! hash in `/api/info`) that it must pair again.

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::tls::{self, TlsIdentity};

/// Self-signed certificates are generated with a 14-day lifetime (the
/// WebTransport maximum for `serverCertificateHashes`).
const CERT_LIFETIME_SECS: u64 = 14 * 24 * 3600;
/// Regenerate this far before actual expiry so an in-use identity never dies
/// mid-stream.
const CERT_RENEW_MARGIN_SECS: u64 = 24 * 3600;

const IDENTITY_FILE: &str = "identity.json";
const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

#[derive(Serialize, Deserialize)]
struct IdentityMeta {
    pin: String,
    lan_ip: String,
    created_at: u64,
    expires_at: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve the per-user data directory for the persistent identity.
/// `--data-dir` wins; otherwise the platform convention; last resort: the
/// directory containing the executable.
pub fn data_dir(cli_override: Option<&str>) -> PathBuf {
    if let Some(dir) = cli_override {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata).join("QuicMic");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home).join("Library/Application Support/QuicMic");
            }
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("quicmic");
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return PathBuf::from(home).join(".local/share/quicmic");
            }
        }
    }
    // Last resort: next to the executable.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn valid_pin(pin: &str) -> bool {
    pin.len() == 6 && pin.bytes().all(|b| b.is_ascii_digit())
}

/// Try to load a still-valid identity from `dir`. Returns `Ok(None)` when there
/// is nothing usable stored (first run, corrupt files, IP change, or an expired
/// certificate) — the caller then generates a fresh one.
fn load(
    dir: &Path,
    lan_ip: &str,
) -> anyhow::Result<Option<(wtransport::Identity, TlsIdentity, String)>> {
    let meta_path = dir.join(IDENTITY_FILE);
    let meta_bytes = match fs::read(&meta_path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let meta: IdentityMeta = match serde_json::from_slice(&meta_bytes) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if !valid_pin(&meta.pin) || meta.lan_ip != lan_ip {
        return Ok(None);
    }
    if now_unix() + CERT_RENEW_MARGIN_SECS >= meta.expires_at {
        return Ok(None);
    }
    let cert_der =
        fs::read(dir.join(CERT_FILE)).map_err(|e| anyhow::anyhow!("read stored cert: {e}"))?;
    let key_der =
        fs::read(dir.join(KEY_FILE)).map_err(|e| anyhow::anyhow!("read stored key: {e}"))?;
    if cert_der.is_empty() || key_der.is_empty() {
        return Ok(None);
    }
    match tls::identity_from_der(&cert_der, &key_der) {
        Ok((wt_identity, tls_identity)) => Ok(Some((wt_identity, tls_identity, meta.pin))),
        Err(_) => Ok(None),
    }
}

fn write_secure(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    fs::write(path, bytes).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Persist a freshly generated (or `--pin`-overridden) identity.
fn persist(dir: &Path, pin: &str, lan_ip: &str, tls_identity: &TlsIdentity) -> anyhow::Result<()> {
    fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("create data dir {}: {e}", dir.display()))?;
    let now = now_unix();
    let meta = IdentityMeta {
        pin: pin.to_string(),
        lan_ip: lan_ip.to_string(),
        created_at: now,
        expires_at: now + CERT_LIFETIME_SECS,
    };
    let meta_json = serde_json::to_vec_pretty(&meta)?;
    write_secure(&dir.join(IDENTITY_FILE), &meta_json)?;
    write_secure(&dir.join(CERT_FILE), &tls_identity.cert_der)?;
    write_secure(&dir.join(KEY_FILE), &tls_identity.key_der)?;
    Ok(())
}

/// Replace just the stored PIN (the `newpin` console command). The certificate
/// is untouched, so already-accepted certs stay valid — only the PIN changes.
pub fn save_pin(dir: &Path, pin: &str) -> anyhow::Result<()> {
    if !valid_pin(pin) {
        anyhow::bail!("PIN must be exactly 6 digits");
    }
    let meta_path = dir.join(IDENTITY_FILE);
    let meta_bytes = fs::read(&meta_path).map_err(|e| anyhow::anyhow!("no saved identity: {e}"))?;
    let mut meta: IdentityMeta = serde_json::from_slice(&meta_bytes)
        .map_err(|e| anyhow::anyhow!("saved identity is corrupt: {e}"))?;
    meta.pin = pin.to_string();
    let meta_json = serde_json::to_vec_pretty(&meta)?;
    write_secure(&meta_path, &meta_json)?;
    Ok(())
}

/// Load the persisted server identity, or generate and persist a fresh one.
/// `pin_override` (the `--pin` flag) always wins and is remembered.
/// Returns the wtransport identity, the TLS identity, the PIN, and whether the
/// identity is fresh (fresh = phones must pair again from the QR code).
pub fn load_or_create(
    dir: &Path,
    lan_ip: IpAddr,
    dump_certs: bool,
    pin_override: Option<String>,
) -> anyhow::Result<(wtransport::Identity, TlsIdentity, String, bool)> {
    let lan_ip_str = lan_ip.to_string();
    if let Some((wt_identity, tls_identity, stored_pin)) = load(dir, &lan_ip_str)? {
        let pin = match pin_override {
            Some(p) => {
                save_pin(dir, &p)?;
                p
            }
            None => stored_pin,
        };
        return Ok((wt_identity, tls_identity, pin, false));
    }
    // Nothing usable stored: generate fresh.
    let (wt_identity, tls_identity) = tls::generate_identity(lan_ip, dump_certs)?;
    let pin = match pin_override {
        Some(p) => p,
        None => format!("{:06}", rand::random_range(0..1_000_000u32)),
    };
    persist(dir, &pin, &lan_ip_str, &tls_identity)?;
    Ok((wt_identity, tls_identity, pin, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_pin_accepts_six_digits() {
        assert!(valid_pin("123456"));
        assert!(!valid_pin("12345"));
        assert!(!valid_pin("1234567"));
        assert!(!valid_pin("12345a"));
        assert!(!valid_pin(""));
    }

    #[test]
    fn data_dir_override_wins() {
        assert_eq!(
            data_dir(Some("/tmp/quicmic-test")),
            PathBuf::from("/tmp/quicmic-test")
        );
    }

    #[test]
    fn persist_round_trip_reuses_identity() {
        let dir = std::env::temp_dir().join(format!("quicmic-ident-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lan_ip: std::net::IpAddr = "192.168.1.99".parse().unwrap();

        let (_id1, tls1, pin1, fresh1) = load_or_create(&dir, lan_ip, false, None).unwrap();
        assert!(fresh1, "first load should generate a fresh identity");
        assert!(valid_pin(&pin1));

        let (_id2, tls2, pin2, fresh2) = load_or_create(&dir, lan_ip, false, None).unwrap();
        assert!(!fresh2, "second load should reuse the saved identity");
        assert_eq!(pin1, pin2, "PIN must persist across restarts");
        assert_eq!(tls1.cert_der, tls2.cert_der, "certificate must persist");
        assert_eq!(tls1.key_der, tls2.key_der, "private key must persist");
        assert_eq!(
            tls1.cert_hash_base64, tls2.cert_hash_base64,
            "cert hash must persist"
        );

        // A different LAN IP must NOT reuse the old identity (its SANs would
        // be wrong), so the app regenerates instead of serving a bad cert.
        let other_ip: std::net::IpAddr = "192.168.1.100".parse().unwrap();
        let (_id3, tls3, _pin3, fresh3) = load_or_create(&dir, other_ip, false, None).unwrap();
        assert!(fresh3, "IP change must trigger a fresh identity");
        assert_ne!(tls1.cert_der, tls3.cert_der);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn meta_round_trip() {
        let dir = std::env::temp_dir().join(format!("quicmic-ident-test-{}", now_unix()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // save_pin on a missing identity must fail cleanly, not panic.
        assert!(save_pin(&dir, "654321").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression test for the v0.4.1 pairing/connection path: after an app
    /// restart, the server must present the exact certificate the phone
    /// pinned (via the `cert_hash` in `/api/info`) together with its matching
    /// private key. If either diverged, the phone's WebTransport
    /// `serverCertificateHashes` check would fail and it could never connect.
    ///
    /// Rebuilds the wtransport identity from the persisted files exactly like
    /// `load_or_create` does and compares bytes -- no network involved (a live
    /// QUIC handshake needs UDP, which CI sandboxes typically block).
    #[test]
    fn persisted_identity_reload_serves_pinned_cert() {
        use base64::Engine as _;
        use wtransport::tls::Sha256Digest;

        let dir = std::env::temp_dir().join(format!("quicmic-ident-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lan_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();

        // First run: fresh identity, persisted to disk.
        let (_wt1, tls1, pin1, fresh1) = load_or_create(&dir, lan_ip, false, None).unwrap();
        assert!(fresh1, "first load should generate a fresh identity");

        // Second run: simulated restart -- must reuse the persisted identity.
        let (wt2, tls2, pin2, fresh2) = load_or_create(&dir, lan_ip, false, None).unwrap();
        assert!(!fresh2, "restart must reuse the persisted identity");
        assert_eq!(pin1, pin2, "PIN must survive a restart");
        assert_eq!(
            tls1.cert_hash_base64, tls2.cert_hash_base64,
            "advertised cert hash must survive a restart"
        );

        // The wtransport identity handed to the real servers after a restart
        // must serve the persisted certificate byte-for-byte.
        let chain = wt2.certificate_chain().as_slice();
        assert_eq!(
            chain.len(),
            1,
            "expected the single self-signed certificate"
        );
        assert_eq!(
            chain[0].der(),
            tls2.cert_der.as_slice(),
            "served certificate must match the persisted one"
        );

        // ...and its SHA-256 fingerprint must equal the advertised `/api/info`
        // hash. This is exactly what the phone's `serverCertificateHashes`
        // check verifies: base64 -> raw bytes, compared against the TLS-layer
        // fingerprint of the served certificate.
        let pinned: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(&tls2.cert_hash_base64)
            .expect("advertised hash must be valid base64")
            .try_into()
            .expect("advertised hash must be 32 bytes");
        assert_eq!(
            chain[0].hash(),
            Sha256Digest::new(pinned),
            "served cert fingerprint must match the pinned hash"
        );

        // The private key must round-trip too, or the TLS handshake fails.
        assert_eq!(
            wt2.private_key().secret_der(),
            tls2.key_der.as_slice(),
            "private key must survive a restart"
        );

        // Rebuilding straight from the raw files on disk (the other restart
        // path) must agree as well.
        let cert_der = std::fs::read(dir.join("cert.der")).unwrap();
        let key_der = std::fs::read(dir.join("key.der")).unwrap();
        let (rebuilt, _) = crate::tls::identity_from_der(&cert_der, &key_der).unwrap();
        assert_eq!(
            rebuilt.certificate_chain().as_slice()[0].der(),
            cert_der.as_slice(),
            "file-rebuilt identity must serve the persisted cert"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
