# QuicMic — native Android app

Native Android client for the QuicMic private PC server (`crtGhoul/quicmic-zero-ui`).
Sideloaded (no Play Services, no Google dependencies): Kotlin, minSdk 26,
OkHttp WebSocket transport, ZXing QR pairing, certificate pinning.

## What it does

- **Pairing screen** — scans the QR from the PC app's Pair tab
  (`https://<lan-ip>:8443#<pin>`, PIN in the hash fragment). On first pair the
  app TLS-handshakes the server, shows the certificate's SHA-256 fingerprint
  next to the PIN for the user to confirm against the PC display, cross-checks
  it against `GET /api/info` → `cert_hash` (base64 SHA-256), then pins exactly
  that certificate. Never falls back to the system trust store.
- **Mic** — `AudioRecord` @ 48 kHz mono PCM16 (`VOICE_COMMUNICATION`), with
  `AcousticEchoCanceler` + `NoiseSuppressor` when the device offers them. The
  noise-gate algorithm mirrors `web/worklet.js` (per-sample² threshold, 250 ms
  hold, onset look-ahead so word attacks are not clipped, gain in the float
  domain). Sends 964-byte binary frames over
  `wss://<host>:8443/ws?token=…&sr=48000` — 4-byte u32 LE sequence + 480 ×
  Int16 LE. A 401/409 on the upgrade (or a dead token mid-stream) triggers
  `POST /api/renew`; if renewal fails it re-pairs in place with the stored PIN.
- **Speaker** — `wss://<host>:8443/speaker-ws?token=…`; the server pushes
  7680-byte frames (20 ms stereo f32-LE @ 48 kHz), played via `AudioTrack`
  (PCM float). Bluetooth routing toggle (`startBluetoothSco` /
  `stopBluetoothSco`, `MODE_IN_COMMUNICATION`) so earbuds can be targeted.
- **Foreground service** (`microphone` type) with a persistent notification
  and a Stop action — start it while the app is in the foreground, it keeps
  streaming with the screen off.
- **Settings** — device name, noise-gate threshold (dB), gain; persisted with
  the token in `EncryptedSharedPreferences`.
- Authenticated REST calls use the `X-Session-Token` header (never
  `Authorization: Bearer`).

## Protocol reference (server is the source of truth)

| Item | Value |
|---|---|
| Base | `https://<lan-ip>:8443` (default port 8443) |
| QR payload | `https://<host>:<port>#<pin>` — 6-digit PIN in the hash fragment |
| Pair | `POST /api/pair` `{"pin","device_name"}` → `{"success","token","error","mic_name"}`; wrong PIN = HTTP 200 + `success:false` (by design); 429 after 5 bad PINs per IP (30 s lockout) |
| Renew | `POST /api/renew` `{"token": old}` → `{"success","token": new}`; old token dies immediately, old connections are kicked |
| Info | `GET /api/info` → `{"cert_hash","wt_port","lan_ip",…}`; `cert_hash` = base64 SHA-256 of the server cert DER |
| Stats | `GET /api/stats` with `X-Session-Token` → packets/loss/buffer; 401 = session taken over, 503 = server shutting down |
| Settings | `GET /api/settings` (open); `POST /api/settings` with the token **in the body** |
| Mic WS | `GET /ws?token=…&sr=48000` → upgrade; 401 bad token, 409 slot busy; binary frames = 964 B |
| Speaker WS | `GET /speaker-ws?token=…` → upgrade; 401 bad token, 503 speaker capture not running on the PC; server pushes 7680 B frames |

No QUIC/WebTransport on Android: Cronet exposes no stable public WebTransport
API, so the WebSocket path (TCP) is used on LAN.

## Build

Requirements: JDK 17, Android SDK with `platforms;android-34` and
`build-tools;34.0.0` (see "CI requirements" in the mission findings log for
the exact setup).

```sh
cd android
./gradlew assembleDebug
# APK: app/build/outputs/apk/debug/app-debug.apk — sideload it.
```

A release build needs a signing key (`assembleRelease`); the debug APK is
signed with the debug key and installs fine for sideloading.

## Project layout

```
app/src/main/java/com/quicmic/android/
  ui/MainActivity.kt        status / start-stop / speaker + BT toggles
  ui/PairActivity.kt        QR scan → fingerprint confirm → pair
  ui/SettingsActivity.kt    device name, noise gate (dB), gain
  service/QuicMicService.kt foreground service (microphone type), stats poller
  audio/MicStreamer.kt      AudioRecord + noise gate + WS sender
  audio/SpeakerPlayer.kt    WS receiver + AudioTrack + Bluetooth SCO toggle
  net/TlsPinning.kt         QR parse, pinning/capturing trust managers
  net/Api.kt                REST client (info/pair/renew/settings/stats)
  store/SecureStore.kt      EncryptedSharedPreferences (token, PIN, cert, settings)
  StreamEvents.kt           service → UI event bus
```

## Status

Written 2026-09-26. **Not run on a real device** — no Android SDK on the
build VM, so this has never been compiled; the first real build happens in CI
/ on a dev machine. Pairing, mic audio, earbuds routing and the background
service all need the user's OnePlus 13 + Windows PC on the same LAN to verify.
See the mission findings log (`../FINDINGS.md`, "### android") for the full
verified-vs-pending list and the CI requirements spec.
