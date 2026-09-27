# Keep the networking and QR-scanning stacks intact under R8.
-keep class com.google.zxing.** { *; }
-keep class okhttp3.** { *; }
-keep class okio.** { *; }
# WebRTC is driven via its Java API surface (PeerConnectionFactory etc.).
-keep class org.webrtc.** { *; }
# Our own model classes are only used via direct references; keep the entry points.
-keep class com.quicmic.android.ui.** { *; }
-keep class com.quicmic.android.service.** { *; }
# Tink (via security-crypto) references error-prone annotations that are
# compile-time only; safe to ignore under R8.
-dontwarn com.google.errorprone.annotations.**
