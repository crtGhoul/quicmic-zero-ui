# Signing the QuicMic iOS app (unsigned IPA → your iPhone)

The CI build produces an **unsigned** `.ipa`. iOS refuses to install unsigned
apps, so you sign it yourself with your own free Apple ID, then import the
signed app into LiveContainer. No paid developer account needed.

## What you need

- The unsigned IPA (see step 1)
- A free Apple ID (any iCloud account works)
- One of: a Windows PC (Sideloadly), or your iPhone (Feather / AltStore)

## 1. Get the unsigned IPA

- **From the GitHub release:** open the v0.5.0 release page
  (`https://github.com/crtGhoul/quicmic-zero-ui/releases/tag/v0.5.0`),
  download `QuicMic-unsigned.ipa` from the Assets list.
- **From CI artifacts (if no release asset):** open the `ios.yml` workflow run
  on GitHub → Artifacts → download `QuicMic-unsigned-ipa` and unzip it.
  The file you want ends in `.ipa`.

## 2. Sign it with your free Apple ID

### Option A — Sideloadly (Windows PC, easiest)

1. Install Sideloadly from sideloadly.io and open it.
2. Connect your iPhone with a USB cable. Tap **Trust** on the iPhone if asked.
3. In Sideloadly: drag the `.ipa` into the window (or click the IPA icon).
4. Enter your Apple ID email + password. (Sideloadly sends these to Apple only,
   for the signing certificate — use an app-specific password if your Apple ID
   has two-factor on: appleid.apple.com → Sign-In and Security → App-Specific
   Passwords.)
5. Click **Start**. Wait for "Done".
6. On the iPhone: Settings → General → VPN & Device Management → tap your
   Apple ID → **Trust**.

### Option B — Feather (on the iPhone itself)

1. Install Feather (get it from its official site or via your sideloading
   source of choice).
2. In Feather: import the `.ipa` (Files app → share → Feather, or Feather's
   import button).
3. Feather → Settings → sign in with your Apple ID.
4. Tap the imported QuicMic app → **Sign**.
5. Trust the profile as in step A6 above.

### Option C — AltStore

1. Install AltStore (altstore.io) on your PC + the AltStore app on your iPhone.
2. On the iPhone, open AltStore → My Apps → tap **+** → pick the `.ipa`
   (save it in the Files app first).
3. Sign in with your Apple ID when asked. AltStore installs it directly.

## 3. Import the signed app into LiveContainer

1. Open **LiveContainer** on your iPhone.
2. Tap **+** (add app) → choose the **signed** `.ipa` file.
3. Wait for the import to finish, then launch QuicMic from the LiveContainer
   app list.
4. First launch: allow **Microphone** (required) and **Camera** (only needed
   for QR scanning — you can also type the address + PIN by hand).

## The 7-day free Apple ID limit

Apps signed with a **free** Apple ID expire after **7 days**. After that the
app icon greys out and it won't launch until you re-sign:

- With Sideloadly: plug the phone back in and hit **Start** again.
- With AltStore: it can **refresh automatically** over your own Wi-Fi
  (AltStore → Settings → enable background refresh; keep AltServer running
  on the PC on the same network).
- With Feather: re-sign in the app.

Paid Apple Developer accounts ($99/year) get 1-year signatures instead, but
re-signing weekly with a free ID works fine for personal use.

## Pairing the app (once it's running)

1. On your PC, start the QuicMic server (the GUI shows a **Pair** tab with a
   QR code, or the console prints one).
2. In the iOS app: tap **Scan QR code** and point at the PC's QR code —
   or type the LAN address (e.g. `192.168.1.42`), port (`8443`), and the
   6-digit PIN by hand.
3. **Certificate check:** the app shows the server's certificate fingerprint
   (e.g. `AA:BB:CC:...`). Compare it with the fingerprint shown on the PC
   (GUI → Diagnostics tab, or the console output). Only tap
   **Confirm and pair** if they match. This pins the PC's self-signed
   certificate — from then on the app trusts exactly that certificate and
   nothing else.
4. Tap the big mic button to start streaming. Long list of details (packet
   loss, buffer depth) appears under it while streaming.

## Troubleshooting

- **"Could not reach the server":** iPhone and PC must be on the **same
  Wi-Fi network**. Windows Firewall may need to allow the QuicMic port
  (8443) on first run.
- **"Server is gone (restarted?)":** the PC app restarted and its certificate
  changed — pair again (the fingerprint screen appears again).
- **No speaker audio on the phone:** the Speaker tab must be running on the
  PC app; otherwise the phone streams mic-only by design.
- **App greyed out after a week:** the 7-day free-ID signature expired —
  re-sign (see above).
