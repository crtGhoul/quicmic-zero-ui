# QuicMic WebView (Android)

A thin Android wrapper that loads the QuicMic PC's web UI (mic / speaker /
landing pages) in a WebView, so the app always looks exactly like the browser
UI — including the noise-cancellation toggle and any future web changes.

## How it works

- Start screen with **Scan QR code** (camera + bundled ZXing core, no
  dependencies), Connect to the remembered server, or manual address entry.
  Scanning the pairing QR in the PC app's Pair tab loads the page directly —
  no typing.
- The PC uses a self-signed certificate: on first connect the app shows the
  SHA-256 fingerprint for you to verify against the PC app's Pair tab, then
  pins it. Later connections proceed silently; a changed cert triggers a
  loud warning.
- Microphone permission is requested at runtime and forwarded to the page
  (`WebChromeClient.onPermissionRequest`), so the web mic works.
- No native audio pipeline: background streaming (screen off) is not
  supported — keep the app in the foreground during calls.

## Building

The project is plain Java with zero external dependencies, so it can be
built without Gradle:

```sh
export ANDROID_SDK_ROOT=/path/to/android-sdk
BT=$ANDROID_SDK_ROOT/build-tools/34.0.0
# One-time: fetch ZXing core (QR decoding) — no other dependencies.
curl -sL -o $ANDROID_SDK_ROOT/extras/zxing-core-3.5.3.jar \
  https://repo1.maven.org/maven2/com/google/zxing/core/3.5.3/core-3.5.3.jar
ZX=$ANDROID_SDK_ROOT/extras/zxing-core-3.5.3.jar
$BT/aapt2 compile --dir app/src/main/res -o compiled_res.zip
$BT/aapt2 link -o app-base.apk \
  -I $ANDROID_SDK_ROOT/platforms/android-34/android.jar \
  --manifest app/src/main/AndroidManifest.xml compiled_res.zip
javac -encoding UTF-8 -source 17 -target 17 \
  -cp $ANDROID_SDK_ROOT/platforms/android-34/android.jar:$ZX \
  -d classes $(find app/src/main/java -name '*.java')
mkdir -p dexout
$BT/d8 --lib $ANDROID_SDK_ROOT/platforms/android-34/android.jar \
  --min-api 26 --output dexout $(find classes -name '*.class') $ZX
cp app-base.apk app-unsigned.apk
(cd dexout && zip -q ../app-unsigned.apk classes.dex)
$BT/zipalign -f 4 app-unsigned.apk app-aligned.apk
$BT/apksigner sign --ks ~/workspace/quicmic-release-keys/quicmic-release.keystore \
  --out quicmic-webview.apk app-aligned.apk
```

The release keystore at `~/workspace/quicmic-release-keys/` is the signing
identity for every QuicMic WebView APK — keep it (and its password file)
forever. All future APK updates must be signed with the same key, or Android
will refuse to install them as updates (users would have to uninstall first).
Never commit the keystore to the repo. (An earlier debug-signed APK used the
throwaway `~/.android/debug.keystore`; it was never installed anywhere and is
superseded by the release-signed build.)

(Gradle files are included for IDE support; the Gradle daemon had issues in
this sandbox, so the manual build above is the verified path.)
