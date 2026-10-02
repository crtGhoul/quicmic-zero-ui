# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] - 2026-10-01

### Added
- **Speech-focused noise cancellation (PC-side, on by default):** RNNoise (pure-Rust `nnnoiseless`) plus a voice-activity gate now clean the mic audio on the PC before it reaches apps — speech-focused noise cancellation — passes human voice, suppresses background noise; it does not identify a specific person. Frames with no detected speech fade to near-silence with a ~250 ms hangover so word endings aren't clipped; added latency is at most ~20 ms. Toggle/threshold via `/api/settings` (`noise_cancellation`, `nc_vad_threshold`) and the phone's Mic settings drawer. Previously the only noise handling was the phone-side amplitude noise gate in the AudioWorklet — there was no server-side suppression.
- Windows MSI installer via cargo-dist (Start Menu shortcut + uninstaller); the portable `.zip` remains available. The build embeds `assets/quicmic.ico` into the exe when that asset is present.

### Changed
- Self-updater now works against the public GitHub Releases with **no token required**; a token (`QUICMIC_GITHUB_TOKEN` / `github_token.txt`) is optional and only raises API rate limits. It also picks the right asset for the machine architecture.
- `POST /api/update` only accepts requests from the PC itself (loopback); phones now get a clear "start updates from the PC app/console" message.

### Security
- `.gitignore` now covers local token files and key material (`github_token.txt`, `.env*`, `*.pem`, `*.key`, …).

## [0.4.2] - 2026-09-26

### Added
- **Native app window (no more command-line only):** the app now opens a real GUI by default when a display is available (`--console` keeps the old terminal UI). Five tabs — **Status** (live mic/audio state), **Pair** (native QR code, large pairing PIN with show/mask, copy, rotate, open-QR-page), **Devices** (mic output-device picker with virtual-cable recommendations), **Settings** (gain, noise gate, latency threshold, volume, monitor mute, mic rename mode, update-check opt-out), **Diagnostics** (cert hash, transport, update status). Background state polling ~3 Hz; preferences in `gui_prefs.json`.
- **Mic rename mode switch in the GUI:** auto / off / fixed is now changeable at runtime from Settings (previously `--rename-mic` at startup only). Auto renames the capture endpoint to the paired phone's name on every pairing, so Discord lists e.g. "iPhone" instead of "CABLE Output".
- Tray **Show / Hide window** menu item in GUI mode (Windows `--tray`).

### Fixed
- **Mic rename wrote the wrong registry property:** the code used a mistyped property-set GUID and PID 2 (the device description) instead of the real `PKEY_Device_FriendlyName` (PID 14), so Discord kept showing "CABLE Output" and repeat renames broke. Now derived from Windows' own constant, with a regression test.
- **Speaker died silently on device unplug:** a lost/invalidated WASAPI endpoint killed capture permanently while the app looked alive. Capture now retries once per second until the endpoint returns. Also fixed a `CoTaskMemFree` leak on the WASAPI init-error path.
- **Drop file sharing:** early ICE candidates were discarded before the peer connection existed (breaking tap-to-connect); the Connect button stayed dead after a host timeout; the receive flow could wait forever; overlapping file metadata could corrupt a transfer; double-tapping Share/Receive let stale attempts overwrite the UI.
- **Self-updater pointed at the wrong repo** (`Fix3dll/QuicMic`) — now checks `crtGhoul/quicmic-zero-ui`. Also: non-Windows no longer downloads before the platform check, chunked downloads can't silently truncate, downloaded assets are validated as Windows executables (MZ/PE check), and zip-only releases report cleanly instead of failing obscurely.
- Added a regression test: a restarted server keeps serving the persisted certificate (pairing/PIN/token survive restarts).

### Notes
- The GUI was verified under Xvfb (all five tabs render, PIN rotation works); the tray Show/Hide item and the Windows registry rename still need real Windows verification, and pairing/audio still need the iPhone + Discord end-to-end check.

## [0.4.1] - 2026-09-25

### Fixed
- **Connection / auto-connect overhaul (the big one):** the PC used to regenerate its TLS certificate and pairing PIN on every launch, so restarting the app invalidated the phone's session, remembered server, and accepted certificate all at once — auto-connect could never survive a restart. The PC now keeps a persistent identity (certificate, key, PIN, LAN IP) in `%APPDATA%\QuicMic`, reused while the LAN IP is unchanged and the certificate is fresh. The phone remembers the paired server, PIN, and certificate hash and silently re-pairs after a PC restart. QR pairing still works and clears stale state.
- `newpin` now actually revokes the active session token (it claimed old phones had to re-pair but left their token valid).
- Installed phone PWAs no longer serve stale app files forever after a PC update: the service worker now revalidates instead of cache-first-permanent.
- Pairing no longer gets stuck on "Connecting..." if it fails, and storage failures (e.g. private browsing) no longer turn a successful pairing into a fake "Connection error".
- WebTransport now connects to the page's own hostname instead of a possibly stale server-reported LAN IP, and `/api/info` is refreshed after pairing so the certificate hash can't go stale.
- Drop: selecting/pasting files while disconnected now says so instead of failing silently; a malformed matchmaker URL shows a clear error instead of retrying forever.
- Speaker: removed the synthetic-tone busy-spin that could pin a CPU core at 100%; WASAPI capture failures now disable Speaker with the real reason instead of streaming silence; added an iOS-friendly timeout around audio startup and cleanup of orphaned audio contexts/sockets.

### Added
- `--tray` flag (Windows): double-click launch hides the console and parks the app in the notification area with a status line, **Show connection QR**, and **Quit** (graceful shutdown). Terminal launches keep the console visible.
- `/qr` page: a large scannable pairing QR served by the PC itself.
- **Diagnose connection** button on the phone pairing page: checks server reachability, HTTPS/certificate status, WebTransport support, and flags common Wi-Fi/VPN/firewall causes.

## [0.4.0] - 2026-09-25

### Added
- Hear-yourself monitoring: `monitor <on|off>` console command and `--monitor-device` flag play the incoming mic audio back through a PC output device (wear headphones to avoid feedback); the phone settings drawer has a monitor toggle when enabled.
- PC-side output volume: swipe up/down on the phone's Mic screen adjusts the PC output volume with a HUD readout, and `status` shows the current volume.
- Phone device name: the Mic screen's settings now have a **Device name** field (pre-filled from the phone type, editable). The phone reports it at pairing (`device_name` in the pair request), the PC console shows it in `status`, and the phone's Diagnostics section shows what apps list as the mic input.
- Automatic mic endpoint rename on pairing: `--rename-mic auto` (default on Windows, `off` elsewhere) renames the virtual-cable capture endpoint to the paired phone's device name, so Discord/Windows list "iPhone" instead of "CABLE Output (VB-Audio Virtual Cable)". `--rename-mic off` disables it; `--rename-mic "NAME"` uses one fixed name. The applied name is reported back in the pair and renew responses. One elevated run is needed (the name lives in HKLM); it sticks afterwards.
- Repeat-safe endpoint matching: the rename now matches on friendly name *or* the driver-set device description, so it keeps finding the endpoint after a previous rename changed its friendly name.
- Pairing screen improvements: "✓ PIN filled in from QR code" badge when the PIN came from a scan, "Pairing with \<host\>" line, and numbered setup steps.
- Transport memory: the phone remembers which connection type worked last (WebTransport UDP vs WebSocket TCP) and tries it first, so reconnects on UDP-blocked networks skip the doomed WebTransport attempt.

### Fixed
- CI: gate Windows-only helpers so macOS/Ubuntu Clippy stays green.
- Windows build: import `PROPERTYKEY` from `Win32::Foundation` (its home in windows crate 0.61).

## [0.2.4] - 2026-09-19

### Added
- Add Linux setup with autostart and separate troubleshooting items by @Fix3dll

### Changed
- Bump dependencies in Cargo.lock by @Fix3dll
- Polish lan detection and add vbox adapter filter by @Fix3dll
- Merge pull request #2 from hu3rror/fix/safari-wt by @hu3rror in [#2](https://github.com/Fix3dll/QuicMic/pull/2)

### Fixed
- Filter proxy-TUN fake-ip from LAN IP detection by @hu3rror in [#3](https://github.com/Fix3dll/QuicMic/pull/3)
- Satisfy clippy chunks_exact_to_as_chunks with as_chunks by @Fix3dll

### New Contributors
* @hu3rror made their first contribution in [#3](https://github.com/Fix3dll/QuicMic/pull/3)

## [0.2.3] - 2026-07-12

### Changed
- V0.2.3 by @Fix3dll
- Publish to crates.io via Trusted Publishing by @Fix3dll

### Fixed
- Recover from microphone interruptions by @Fix3dll

## [0.2.2] - 2026-06-28

### Changed
- V0.2.2 by @Fix3dll
- Prepare crate for crates.io publishing by @Fix3dll
- Skip docs-only changes via paths-ignore and cancel superseded runs by @Fix3dll

## [0.2.1] - 2026-06-27

### Changed
- V0.2.1 changelog by @Fix3dll

### Fixed
- Reliable WebSocket fallback + surface it to the user by @Fix3dll

## [0.2.0] - 2026-06-27

### Changed
- Trigger releases on tag push instead of manual dispatch by @Fix3dll
- Migrate releases to cargo-dist with native x86_64 + arm64 builds by @Fix3dll
- Initial commit by @Fix3dll

### New Contributors
* @Fix3dll made their first contribution

[0.2.4]: https://github.com/Fix3dll/QuicMic/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/Fix3dll/QuicMic/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/Fix3dll/QuicMic/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/Fix3dll/QuicMic/compare/v0.2.0...v0.2.1

<!-- generated by git-cliff -->
