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
}
