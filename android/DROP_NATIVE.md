# Native Drop (Android) — protocol, auth, and test notes

Implemented 2026-09-26 on `release/v0.5.0` (workdir `/tmp/apk-exp/work-c`).
Covers task features #3 (native share/file-picker), #8 (background
receive service), #9 (tap-to-connect identity reuse).

## The key finding: there is no Drop upload endpoint on the PC

`src/server/mod.rs::build_router` routes only `/api/*`, `/ws`,
`/speaker-ws`, `/ca`, `/qr`, plus static assets. **`web/drop.html`
never uploads files to the PC over HTTP** — files move over a WebRTC
data channel labeled `localdrop` (ordered), signaled by:

1. **Manual SDP codes** — `base64url(UTF-8 JSON {"t":<offer|answer>,
   "s":<sdp>})`, full-gathered SDP, no trickle (see `encodeSDP` /
   `decodeSDP` / `waitGathering` in `web/drop.js`).
2. **Tap-to-connect** — an EXTERNAL matchmaker WebSocket (NOT the PC).
   Default server: `wss://localdrop-l8ly.onrender.com` (the repo's
   deployed signaling server, per the 2026-09-25 repo audit).
   Wire: out `{"t":"register","id","name"}`, `{"t":"heartbeat"}`,
   `{"t":"signal","to","payload"}`; in `{"t":"roster",...}`,
   `{"t":"signal","from","fromName","payload"}`, `{"t":"error",...}`.
   Payload kinds: `offer {sdp}`, `answer {sdp}`, `ice {candidate}`,
   `declined`. Trickle ICE here (unlike manual codes).

Data-channel protocol (ported 1:1 from `drop.js`):
- text: `{"t":"msg","text":...,"ts":...}`
- file: `{"t":"file-meta","id","name","size","mime"}` → 16 KiB binary
  chunks → `{"t":"file-end","id"}`
- ICE: `stun:stun.l.google.com:19302`, `stun:openrelay.metered.ca:80`,
  `turn:openrelay.metered.ca:80/443` (user `openrelayproject` /
  password `openrelayproject`).

So "upload directly to the PC via the pinned OkHttp client + mic token"
is **not possible without new PC server endpoints**. The native code
speaks the real Drop protocol via `org.webrtc:google-webrtc`
(Maven Central, no Play Services — matches the sideloading stance).

## Auth / identity (#9)

Drop has **no server-side auth at all**: `drop.html` is an open static
asset, pairing codes are trust-on-first-use, and the matchmaker does not
authenticate. There is therefore no mic-token handshake to reuse — and
the token must NEVER be sent to the matchmaker (third-party service;
that would leak the credential off-LAN).

What "identity reuse" means in this implementation:
- Matchmaker display name = `SecureStore.deviceName` — the same name the
  PC sees for the mic (`device_name` on `/api/pair`).
- Matchmaker id = `SecureStore.dropDeviceId` — stable per-install UUID,
  created once, stored encrypted next to the pairing.
- The Drop client refuses to run unless `SecureStore.isPaired` (same
  gate as `DropActivity`).

**Server-side requirement: none today.** If the PC ever gates Drop
behind auth, the sane contract is: static assets + any future Drop HTTP
endpoint accept the existing `X-Session-Token` header (the `/api/stats`
rule), and `Api.kt` gains a `getDropPage(token)`-style call. Do NOT
invent a token query param or send the token to the matchmaker.

## What was built

- `net/DropRtc.kt` — `DropSignalCodec` (manual-code codec),
  `DropPeerFactory` (process-wide PeerConnectionFactory),
  `DropPeer` (offer/answer, trickle, data-channel send/receive with the
  exact chunk protocol), `DropPeerListener`, `DropRtc` constants.
- `net/DropMatchmaker.kt` — matchmaker WebSocket client (register,
  roster, signals, heartbeat, backoff reconnect, payload validation).
- `ui/DropShareActivity.kt` — `ACTION_SEND`/`ACTION_SEND_MULTIPLE`
  (files + text) and `EXTRA_PICK_FILE` (system file picker). Tap a PC in
  the roster to ring it (trickle offer via matchmaker), or paste the
  PC's Share code for the manual flow (reply code shown as text + QR).
  Sends the queue automatically on connect with progress.
- `service/DropReceiveService.kt` — foreground service (`dataSync`
  type) keeping the matchmaker alive; persistent "listening"
  notification with Stop; incoming offer → high-priority
  Accept/Decline notification (+vibration) or auto-accept if the
  toggle is on; accepted transfers save to `Downloads/QuicMic` with a
  progress + saved/failed notification.
- `ui/DropSettingsBinder.kt` — `fun bind(activity: SettingsActivity)`
  inflates `settings_section_drop.xml` into the settings form above the
  Save button (no edits to `activity_settings.xml`/`SettingsActivity`).
  **Integrator: call `DropSettingsBinder.bind(this)` in
  `SettingsActivity.onCreate()` after `setContentView()`.**
- `res/layout/settings_section_drop.xml` — Send-file button, background-
  receive toggle, auto-accept toggle, matchmaker URL field.
- `res/layout/activity_drop_share.xml`, `res/layout/dialog_drop_reply.xml`.
- `res/values/strings_drop.xml` — all new strings (strings.xml untouched).
- `AndroidManifest.xml` — `FOREGROUND_SERVICE_DATA_SYNC`, `VIBRATE`,
  `WRITE_EXTERNAL_STORAGE (maxSdk 28)`; `DropShareActivity`
  (exported, SEND/SEND_MULTIPLE `*/*`); `DropReceiveService`
  (`foregroundServiceType="dataSync"`).
- `store/SecureStore.kt` — `dropDeviceId`, `dropSignalUrl`,
  `dropReceiveEnabled`, `dropAutoAccept` (minimal, scoped additions).
- `app/build.gradle.kts` — `org.webrtc:google-webrtc:1.0.32006`
  (verify resolution at build time; fallback `1.0.30039`).
- `app/proguard-rules.pro` — keep `org.webrtc.**`.
- `Api.kt` — **not modified**: no Drop HTTP surface exists to attach
  auth to; see above.

## Verified vs flagged

- **Verified**: protocol analysis against `web/drop.js` + `src/server`
  (endpoints, codec, chunk protocol, ICE config, matchmaker wire).
  Code written against stable `org.webrtc` APIs; style matches the
  existing Kotlin.
- **NOT verified**: no Java/Android SDK on this machine, so the code
  **did not compile** and was never run — no emulator, no device.
  Treat the first build as the verification step.
- **Needs server endpoint: NO** — the signaling path (matchmaker) and
  transfer path (WebRTC) both already exist; nothing was stubbed.

## Human tester notes (Android 14+)

1. Pair with the PC first (mic pairing) — Drop share/receive refuse to
   run without it.
2. Grant **Notifications** when prompted (Android 13+): the incoming-
   Drop Accept/Decline arrives as a notification.
3. To SEND: share a photo/file/text to QuicMic, or Settings → Drop →
   "Send file to PC". Tap "Find PC nearby" — **the PC must have
   `drop.html` open and joined to the same matchmaker server** (enter
   the server URL in its Settings; default is the Render server both
   sides use). Tap the PC → accept on the PC → files send.
   Fallback: PC → Share → copy code → "Pair with code instead" here →
   hand the reply code back to the PC.
4. To RECEIVE: Settings → Drop → enable "Background receive". A
   persistent "Drop receive — Listening" notification appears. From the
   PC's Drop page, tap this phone in its roster → Accept on the phone →
   file lands in `Downloads/QuicMic`.
5. Auto-accept is OFF by default for a reason: anyone on the matchmaker
   can push files when it's on.
6. Android 14 caps `dataSync` foreground services (~6h/24h); if the
   listen notification disappears overnight, that's the OS, not a bug.
7. Large-file caveat: received files are assembled in memory (same as
   the web page's Blob-of-parts); stay under a few hundred MB for now.
   Send side caps files at 256 MB.
