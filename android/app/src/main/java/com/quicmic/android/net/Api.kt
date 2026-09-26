package com.quicmic.android.net

import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * REST client for the QuicMic PC server.
 *
 * Wire contract (mirrors src/server/api.rs — the server is the source of truth):
 *  - GET  /api/info      (no auth) -> {"cert_hash","wt_port","lan_ip",...};
 *    cert_hash is the base64 SHA-256 of the server certificate DER.
 *  - POST /api/pair      {"pin","device_name"} ->
 *    {"success","token","error","mic_name"}; a wrong PIN is HTTP 200 with
 *    success=false BY DESIGN (not an error status); 429 = per-IP lockout.
 *  - POST /api/renew     {"token": old} -> {"success","token": new}; the old
 *    token is invalidated immediately and old connections are kicked.
 *  - GET  /api/stats     header X-Session-Token (NOT Authorization: Bearer);
 *    401 = session taken over, 503 = server shutting down.
 *  - GET  /api/settings  (open, read-only).
 *  - POST /api/settings  with the token IN THE BODY (server rule).
 *
 * All calls are synchronous — callers must run them off the main thread.
 */

private val JSON = "application/json; charset=utf-8".toMediaType()

data class ServerInfo(val certHashBase64: String, val lanIp: String, val wtPort: Int)
data class PairResult(
    val success: Boolean,
    val token: String?,
    val error: String?,
    val micName: String?,
)
data class RenewResult(val success: Boolean, val token: String?)

sealed class StatsOutcome {
    data class Ok(val json: JSONObject) : StatsOutcome()
    object Unauthorized : StatsOutcome() // server alive, our session was taken over
    object ServerGone : StatsOutcome() // 503: server is shutting down
    data class Error(val msg: String) : StatsOutcome()
}

class ApiException(message: String, cause: Throwable? = null) : Exception(message, cause)

/** The session token is dead: renew it or re-pair. */
class SessionExpiredException(message: String) : Exception(message)

/** The server could not be reached at all (network down, wrong host, ...). */
class ServerUnreachableException(message: String, cause: Throwable? = null) :
    Exception(message, cause)

/**
 * Repair a dead session: try /api/renew with the stored token first, else
 * re-pair in place with the stored PIN (mirrors the web client's in-place
 * re-pair). Returns true when the store now holds a live token.
 */
fun Api.repairSession(store: com.quicmic.android.store.SecureStore): Boolean {
    store.getToken()?.let { t ->
        try {
            val r = renew(t)
            if (r.success && !r.token.isNullOrEmpty()) {
                store.setToken(r.token)
                return true
            }
        } catch (_: Exception) {
            // Fall through to re-pairing.
        }
    }
    val pin = store.getPin() ?: return false
    return try {
        val p = pair(pin, store.deviceName)
        if (p.success && !p.token.isNullOrEmpty()) {
            store.setToken(p.token)
            try {
                pushSettings(p.token, gateLinear(store.gateDb), store.gain)
            } catch (_: Exception) {
                // Settings sync is best-effort; the token is what matters.
            }
            true
        } else {
            false
        }
    } catch (_: Exception) {
        false
    }
}

/** dB slider value -> linear amplitude the server expects (0 = gate off). */
fun gateLinear(db: Float): Float =
    if (db <= -100f) 0f else Math.pow(10.0, (db / 20f).toDouble()).toFloat()

/** OkHttpClient that pins exactly [pinnedDer] and trusts nothing else. */
fun pinnedClient(pinnedDer: ByteArray): OkHttpClient {
    val tm = PinningTrustManager(pinnedDer)
    return OkHttpClient.Builder()
        .sslSocketFactory(sslContextFor(tm).socketFactory, tm)
        .hostnameVerifier(PinningHostnameVerifier)
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(10, TimeUnit.SECONDS)
        .writeTimeout(10, TimeUnit.SECONDS)
        .build()
}

/**
 * Capture-only client for first pairing: records the server certificate via
 * [CapturingTrustManager] and validates nothing. The caller MUST verify the
 * fingerprint out-of-band before trusting it.
 */
fun capturingClient(): Pair<OkHttpClient, CapturingTrustManager> {
    val tm = CapturingTrustManager()
    val client = OkHttpClient.Builder()
        .sslSocketFactory(sslContextFor(tm).socketFactory, tm)
        .hostnameVerifier(PinningHostnameVerifier)
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(10, TimeUnit.SECONDS)
        .build()
    return client to tm
}

class Api(private val client: OkHttpClient, private val baseUrl: String) {

    fun getInfo(): ServerInfo {
        val req = Request.Builder().url("$baseUrl/api/info").get().build()
        client.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) throw ApiException("GET /api/info: HTTP ${resp.code}")
            val j = JSONObject(resp.body!!.string())
            return ServerInfo(
                certHashBase64 = j.getString("cert_hash"),
                lanIp = j.optString("lan_ip"),
                wtPort = j.optInt("wt_port", 8443),
            )
        }
    }

    fun pair(pin: String, deviceName: String): PairResult {
        val body = JSONObject()
            .put("pin", pin)
            .put("device_name", deviceName)
            .toString()
            .toRequestBody(JSON)
        val req = Request.Builder().url("$baseUrl/api/pair").post(body).build()
        client.newCall(req).execute().use { resp ->
            // Note: wrong PIN and lockout both come back as HTTP 200/429 with a
            // JSON body — parse it either way instead of treating non-200 as fatal.
            val j = JSONObject(resp.body!!.string())
            return PairResult(
                success = j.optBoolean("success", false),
                token = j.optString("token").takeIf { it.isNotEmpty() },
                error = j.optString("error").takeIf { it.isNotEmpty() },
                micName = j.optString("mic_name").takeIf { it.isNotEmpty() },
            )
        }
    }

    fun renew(token: String): RenewResult {
        val body = JSONObject().put("token", token).toString().toRequestBody(JSON)
        val req = Request.Builder().url("$baseUrl/api/renew").post(body).build()
        client.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) return RenewResult(false, null)
            val j = JSONObject(resp.body!!.string())
            return RenewResult(
                success = j.optBoolean("success", false),
                token = j.optString("token").takeIf { it.isNotEmpty() },
            )
        }
    }

    /** Returns (noiseGateLinear, gain), or null on failure. */
    fun getSettings(): Pair<Float, Float>? {
        val req = Request.Builder().url("$baseUrl/api/settings").get().build()
        client.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) return null
            val j = JSONObject(resp.body!!.string())
            return j.getDouble("noise_gate").toFloat() to j.getDouble("gain").toFloat()
        }
    }

    /** Token goes in the body — that is the server's rule for this endpoint. */
    fun pushSettings(token: String, noiseGateLinear: Float, gain: Float): Boolean {
        val body = JSONObject()
            .put("token", token)
            .put("noise_gate", noiseGateLinear.toDouble())
            .put("gain", gain.toDouble())
            .toString()
            .toRequestBody(JSON)
        val req = Request.Builder().url("$baseUrl/api/settings").post(body).build()
        client.newCall(req).execute().use { resp ->
            return resp.isSuccessful
        }
    }

    fun getStats(token: String): StatsOutcome {
        val req = Request.Builder()
            .url("$baseUrl/api/stats")
            .header("X-Session-Token", token)
            .get()
            .build()
        client.newCall(req).execute().use { resp ->
            return when (resp.code) {
                200 -> StatsOutcome.Ok(JSONObject(resp.body!!.string()))
                401 -> StatsOutcome.Unauthorized
                503 -> StatsOutcome.ServerGone
                else -> StatsOutcome.Error("HTTP ${resp.code}")
            }
        }
    }
}
