# Keep the networking and QR-scanning stacks intact under R8.
-keep class com.google.zxing.** { *; }
-keep class okhttp3.** { *; }
-keep class okio.** { *; }
# Our own model classes are only used via direct references; keep the entry points.
-keep class com.quicmic.android.ui.** { *; }
-keep class com.quicmic.android.service.** { *; }
