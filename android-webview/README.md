# QuicMic WebView (Android)

A thin Android wrapper that loads the QuicMic PC's web UI (mic / speaker /
landing pages) in a WebView, so the app always looks exactly like the browser
UI — including the noise-cancellation toggle and any future web changes.

## How it works

- Single `WebView` pointed at the PC's HTTPS server (default
  `https://192.168.68.104:8443`, editable in the address bar, remembered
  between launches).
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
$BT/aapt2 compile --dir app/src/main/res -o compiled_res.zip
$BT/aapt2 link -o app-base.apk \
  -I $ANDROID_SDK_ROOT/platforms/android-34/android.jar \
  --manifest app/src/main/AndroidManifest.xml compiled_res.zip
javac -encoding UTF-8 -source 17 -target 17 \
  -cp $ANDROID_SDK_ROOT/platforms/android-34/android.jar \
  -d classes $(find app/src/main/java -name '*.java')
mkdir -p dexout
$BT/d8 --lib $ANDROID_SDK_ROOT/platforms/android-34/android.jar \
  --min-api 26 --output dexout $(find classes -name '*.class')
cp app-base.apk app-unsigned.apk
(cd dexout && zip -q ../app-unsigned.apk classes.dex)
$BT/zipalign -f 4 app-unsigned.apk app-aligned.apk
$BT/apksigner sign --ks ~/.android/debug.keystore \
  --ks-pass pass:android --key-pass pass:android \
  --out quicmic-webview.apk app-aligned.apk
```

(Gradle files are included for IDE support; the Gradle daemon had issues in
this sandbox, so the manual build above is the verified path.)
