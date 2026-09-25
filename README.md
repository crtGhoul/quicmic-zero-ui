# QuicMic Zero UI

One Windows app, three screens. Your phone becomes a **wireless mic**, a **file-drop station**, and a **wireless speaker** for your PC — all over your local network. No accounts, no cloud, no app install on the phone.

**Download:** grab `quicmic-zero-ui-drop.exe` from the [private release](https://github.com/crtGhoul/quicmic-zero-ui/releases/tag/v0.3.0-zero-ui) and double-click it.

## The PC window

This is what you see when you start the exe. It shows a QR code, the phone URL, and a 6-digit pairing PIN (values below are illustrative — yours will differ):

![PC console window](docs/screenshots/pc-console.png)

## The phone screens

Switch between the three tabs at the top of any page.

| 🎤 Mic | 📦 Drop | 🔊 Speaker |
|---|---|---|
| ![Mic tab](docs/screenshots/phone-mic.png) | ![Drop tab](docs/screenshots/phone-drop.png) | ![Speaker tab](docs/screenshots/phone-speaker.png) |

## Quick start

1. **PC:** run `quicmic-zero-ui-drop.exe`. The console window prints a QR code, a URL like `https://192.168.x.x:8443`, and a 6-digit PIN.
2. **Phone** (on the same Wi-Fi): scan the QR with your camera, **or** open the URL in your browser and type the PIN.
3. Done — pairing sticks, you only do it once.

### 🎤 Mic — phone as wireless PC microphone

Tap the big button to mute/unmute. Gestures:

- **Tap** — mute / unmute
- **Long-press** — Eco mode on/off
- **Swipe up/down** — PC output volume (with a HUD readout)
- **Swipe up from the bottom edge** — settings drawer (devices, volume, hear-yourself monitor)

Tip: run the exe with `--monitor-device` if you want the hear-yourself toggle in the drawer — wear headphones when testing it.

### 📦 Drop — send anything to nearby devices

Photos, files, text & links straight over your local network, peer-to-peer and encrypted (WebRTC).

- **Share** — host a session and get a code
- **Receive** — join with a code
- **Nearby & ready** — set a matchmaker server URL in ⚙ Settings and devices on your network show up automatically. No matchmaker? The Share/Receive codes work fine without one.
- The status dot turns green when Drop is connected.

### 🔊 Speaker — hear your PC through your earbuds

1. Pair your Bluetooth earbuds to your **phone**.
2. Open the **Mic** tab once and pair (Speaker reuses that pairing).
3. Open the **Speaker** tab, tap **Connect**, then play anything on the PC — music, video, games.

The page shows a live level meter, a volume slider, and frame stats. ⚙ Settings has auto-connect and an 880 Hz test tone to check the phone→earbuds path without the PC.

> Heads-up: the three tabs are separate pages, so switching tabs unloads the current one. To run Mic and Speaker **at the same time**, open them in two browser tabs side by side.

## iPhone: trusting the certificate

The PC uses a self-signed certificate, so Safari shows a warning on first visit. That's expected:

1. Open the URL in Safari, tap *Show Details* → *visit this website*.
2. The PC window shows a **Cert SHA-256** fingerprint — check it matches, so you know it's your PC.
3. Trust it permanently in **Settings → General → About → Certificate Trust Settings**.

## Troubleshooting

- **Drop dot stays gray** — check the matchmaker URL in Drop's ⚙ Settings, or skip discovery entirely and use Share/Receive codes (pure LAN).
- **Speaker says "pair on the Mic tab first"** — open Mic and complete pairing once.
- **Speaker connects but no sound** — it captures the PC's *default output* device; check that's the right one, and try the 880 Hz test tone to verify the phone→earbuds path.
- **Underruns keep climbing** — Wi-Fi congestion; move closer to the router.
- **Mic page asks for the PIN again** — your phone's saved pairing expired; re-pair from the PC window.

## Privacy

Everything stays on your LAN. Mic and Speaker stream directly between your PC and phone. Drop transfers are peer-to-peer; the matchmaker server only helps devices find each other.

---

*Built by Strider.*
