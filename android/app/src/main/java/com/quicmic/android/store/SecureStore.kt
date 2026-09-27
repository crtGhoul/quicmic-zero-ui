package com.quicmic.android.store

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Encrypted storage for everything sensitive: the session token, the pairing
 * PIN, the pinned server certificate, and the audio settings.
 *
 * Uses EncryptedSharedPreferences (AES256-SIV keys / AES256-GCM values) so a
 * rooted-device file read does not hand over the session token.
 */
class SecureStore(context: Context) {

    private val prefs: SharedPreferences = run {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context,
            "quicmic_secure",
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    // --- Pairing identity ---

    fun savePairing(host: String, port: Int, pin: String, token: String, certDer: ByteArray) {
        prefs.edit()
            .putString("host", host)
            .putInt("port", port)
            .putString("pin", pin)
            .putString("token", token)
            .putString("cert_der", Base64.encodeToString(certDer, Base64.NO_WRAP))
            .apply()
    }

    /** True when we have a token AND a pinned certificate to talk to the server. */
    val isPaired: Boolean
        get() = getToken() != null && getCertDer() != null && getHost() != null

    fun clearPairing() {
        prefs.edit()
            .remove("host").remove("port").remove("pin")
            .remove("token").remove("cert_der")
            .apply()
    }

    fun getHost(): String? = prefs.getString("host", null)
    fun getPort(): Int = prefs.getInt("port", 8443)
    fun getPin(): String? = prefs.getString("pin", null)
    fun getToken(): String? = prefs.getString("token", null)
    fun setToken(token: String) = prefs.edit().putString("token", token).apply()

    fun getCertDer(): ByteArray? =
        prefs.getString("cert_der", null)?.let { Base64.decode(it, Base64.NO_WRAP) }

    // --- Settings ---

    /** Self-reported device name sent as `device_name` on /api/pair. */
    var deviceName: String
        get() = prefs.getString("device_name", null) ?: Build.MODEL ?: "Android"
        set(v) = prefs.edit().putString("device_name", v).apply()

    /** Noise-gate threshold in dB, -100 = off (matches the web UI slider). */
    var gateDb: Float
        get() = prefs.getFloat("gate_db", -50f)
        set(v) = prefs.edit().putFloat("gate_db", v).apply()

    /** Linear output gain, 0.2..3.0 (matches the web UI slider). */
    var gain: Float
        get() = prefs.getFloat("gain", 1.0f)
        set(v) = prefs.edit().putFloat("gain", v).apply()

    /** Whether the PC-speaker stream starts with the mic stream. */
    var speakerEnabled: Boolean
        get() = prefs.getBoolean("speaker_enabled", false)
        set(v) = prefs.edit().putBoolean("speaker_enabled", v).apply()

    /** Whether PC-speaker audio routes over Bluetooth SCO (earbuds). */
    var btEnabled: Boolean
        get() = prefs.getBoolean("bt_enabled", false)
        set(v) = prefs.edit().putBoolean("bt_enabled", v).apply()

    // --- Audio knobs (SettingsActivity audio/speaker sections) ---

    /**
     * PC-side output volume multiplier, 0..5 (mirrors web/mic.html's Output
     * Volume slider). The PC server is authoritative: POST /api/settings
     * `output_volume` (verified in src/server/api.rs).
     */
    var outputVolume: Float
        get() = prefs.getFloat("output_volume", 1.0f)
        set(v) = prefs.edit().putFloat("output_volume", v).apply()

    /**
     * Latency-recovery threshold in ms, 0 = off, default 150 (mirrors
     * web/mic.html's Latency Recovery slider). Server key: POST /api/settings
     * `latency_threshold` (verified in src/server/api.rs).
     */
    var latencyThreshold: Int
        get() = prefs.getInt("latency_threshold", 150)
        set(v) = prefs.edit().putInt("latency_threshold", v).apply()

    /**
     * Phone-mic echo cancellation. This is a LOCAL capture preference
     * (mirrors the web client's getUserMedia echoCancellation pref, which is
     * also client-side): the server has no key for it, so the toggle is
     * persisted here and must be applied by MicStreamer at stream start (read
     * store.echoCancel in openAudio() and skip AcousticEchoCanceler when off).
     * The binder also best-effort POSTs {"echo_cancel": …} to /api/settings so
     * a future server endpoint can pick it up.
     */
    var echoCancel: Boolean
        get() = prefs.getBoolean("echo_cancel", true)
        set(v) = prefs.edit().putBoolean("echo_cancel", v).apply()

    /**
     * Hear-yourself monitor preference. The monitor stream itself is owned by
     * the PC server (POST /api/monitor, verified in src/server/api.rs); the
     * server's reply is authoritative.
     */
    var monitorEnabled: Boolean
        get() = prefs.getBoolean("monitor_enabled", false)
        set(v) = prefs.edit().putBoolean("monitor_enabled", v).apply()

    /**
     * Power-save mode. What it does (documented here so the UI and the
     * streaming paths agree):
     *  - the hear-yourself monitor switch is forced off and disabled (less
     *    server work, no feedback loop);
     *  - the VU-meter UI refresh should be throttled (integrator hook:
     *    MainActivity's Level handler reads this and uses a ~500 ms throttle
     *    instead of 100 ms).
     */
    var powerSave: Boolean
        get() = prefs.getBoolean("power_save", false)
        set(v) = prefs.edit().putBoolean("power_save", v).apply()

    /**
     * Chosen phone mic input, as an AudioDeviceInfo id; -1 = system default.
     * Enumerated at bind time via AudioManager.getDevices(TYPE_INPUT) and
     * applied by MicStreamer on the NEXT stream start (integrator hook:
     * openAudio() reads this and calls AudioRecord.Builder.setInputDevice).
     */
    var micInputDevice: Int
        get() = prefs.getInt("mic_input_device", -1)
        set(v) = prefs.edit().putInt("mic_input_device", v).apply()

    /**
     * PC-speaker stream volume on THIS phone, 0..1, default 0.8 (mirrors the
     * web speaker page's local spk_volume). Local only — applied to the
     * SpeakerPlayer AudioTrack, not sent to the server.
     */
    var speakerVolume: Float
        get() = prefs.getFloat("speaker_volume", 0.8f)
        set(v) = prefs.edit().putFloat("speaker_volume", v).apply()

    /**
     * PC-speaker jitter-buffer preset: "low" | "balanced" | "smooth" (mirrors
     * the web speaker page's local spk_latency). Local only; applied by
     * SpeakerPlayer on the next speaker start (integrator hook: pre-buffer
     * 2 / 5 / 12 frames before play(), matching web/speaker.js).
     */
    var speakerLatency: String
        get() = prefs.getString("speaker_latency", null) ?: "balanced"
        set(v) = prefs.edit().putString("speaker_latency", v).apply()

    /**
     * Chosen phone output device for the PC-speaker stream, as an
     * AudioDeviceInfo id; -1 = system default. Enumerated at bind time via
     * AudioManager.getDevices(TYPE_OUTPUT); applied on the next speaker start
     * (integrator hook: AudioTrack.setPreferredDevice in SpeakerPlayer).
     */
    var speakerSink: Int
        get() = prefs.getInt("speaker_sink", -1)
        set(v) = prefs.edit().putInt("speaker_sink", v).apply()

    /**
     * Phone UI theme: 0 = dark, 1 = light, 2 = follow system. Dark is the
     * default (matches the PC). Read by ThemeManager before any activity is
     * created (see ui/QuicMicApp).
     */
    var themeMode: Int
        get() = prefs.getInt("theme_mode", 0).coerceIn(0, 2)
        set(v) = prefs.edit().putInt("theme_mode", v.coerceIn(0, 2)).apply()

    /**
     * Opt-out for the phone's PC-update check (GET /api/info's
     * update_available). When false, the System section's "Check now" row is
     * disabled and no update status is fetched.
     */
    var updateCheckEnabled: Boolean
        get() = prefs.getBoolean("update_check_enabled", true)
        set(v) = prefs.edit().putBoolean("update_check_enabled", v).apply()

    // --- Drop identity (reuses the mic pairing; no second pairing) ---

    /**
     * Stable per-install Drop device id, used as the tap-to-connect
     * matchmaker identity alongside [deviceName]. Created once and kept in
     * the same encrypted store as the mic pairing.
     */
    val dropDeviceId: String
        get() = prefs.getString("drop_device_id", null)
            ?: java.util.UUID.randomUUID().toString().also {
                prefs.edit().putString("drop_device_id", it).apply()
            }

    /**
     * Tap-to-connect matchmaker server URL. This is an EXTERNAL signaling
     * service, not the paired PC — the pinned mic client must never talk
     * to it, and the mic session token must never be sent to it.
     */
    var dropSignalUrl: String
        get() = prefs.getString(
            "drop_signal_url",
            null,
        ) ?: com.quicmic.android.net.DropMatchmaker.DEFAULT_URL
        set(v) = prefs.edit().putString("drop_signal_url", v).apply()

    /** Background Drop receive service on/off (Settings toggle). */
    var dropReceiveEnabled: Boolean
        get() = prefs.getBoolean("drop_receive_enabled", false)
        set(v) = prefs.edit().putBoolean("drop_receive_enabled", v).apply()

    /**
     * Auto-accept incoming Drop offers without asking. Off by default:
     * anyone on the same matchmaker server could otherwise push files to
     * this phone unprompted.
     */
    var dropAutoAccept: Boolean
        get() = prefs.getBoolean("drop_auto_accept", false)
        set(v) = prefs.edit().putBoolean("drop_auto_accept", v).apply()

    /**
     * Desired mic-rename mode the PC should apply: "auto", "off" or "fixed".
     * Mirrors the PC GUI's mic-rename mode selector (src/gui/panels.rs).
     * NOTE: as of server v0.5.0 there is no phone-facing rename API, so this
     * is persisted locally and pushed best-effort — see RenameSettingsBinder.
     */
    var micRenameMode: String
        get() = prefs.getString("mic_rename_mode", "off") ?: "off"
        set(v) = prefs.edit().putString("mic_rename_mode", v).apply()

    /** Fixed mic name used by Fixed rename mode and the "Rename now" button. */
    var micRenameFixedName: String
        get() = prefs.getString("mic_rename_fixed_name", "") ?: ""
        set(v) = prefs.edit().putString("mic_rename_fixed_name", v).apply()
}
