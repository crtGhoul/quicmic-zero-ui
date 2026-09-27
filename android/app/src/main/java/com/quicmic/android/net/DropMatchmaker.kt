package com.quicmic.android.net

import android.os.Handler
import android.os.Looper
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * Tap-to-connect matchmaker client — port of the signaling half of
 * web/drop.js (sigConnect/handleSigMsg/onRemoteSignal/callDevice).
 *
 * This talks to the EXTERNAL matchmaker service (default
 * wss://localdrop-l8ly.onrender.com — the repo's deployed signaling server),
 * NOT to the paired PC. It uses a plain OkHttp client with the platform
 * trust store (the matchmaker has a real public certificate, like the
 * DropActivity WebView comment notes) — never the pinned PC client.
 *
 * Wire protocol (from drop.js):
 *  - out: {"t":"register","id":...,"name":...}, {"t":"heartbeat"},
 *         {"t":"signal","to":<id>,"payload":{...}}
 *  - in:  {"t":"roster","devices":[{id,name,nearby}]},
 *         {"t":"signal","from":...,"fromName":...,"payload":{...}},
 *         {"t":"error","msg":...}
 *  - payload kinds: offer {sdp}, answer {sdp}, ice {candidate|null},
 *    declined. Trickle ICE is used here (unlike manual codes).
 *
 * AUTH WARNING: never put the mic session token in any of these messages —
 * the matchmaker is a third-party service and the token would leak to it.
 * The Drop "identity" is just the mic device name (SecureStore.deviceName)
 * plus a stable per-install id, so the PC shows the same name it sees for
 * the mic. Drop itself has no server-side auth to reuse.
 */
class DropMatchmaker(private val listener: Listener) {

    interface Listener {
        fun onRoster(devices: List<DropDevice>)
        fun onSignal(from: String, fromName: String, payload: JSONObject)
        fun onConnectionChange(connected: Boolean)
        fun onError(msg: String)
    }

    data class DropDevice(val id: String, val name: String, val nearby: Boolean)

    companion object {
        /** Deployed signaling server (see repo audit notes); user-overridable. */
        const val DEFAULT_URL = "wss://localdrop-l8ly.onrender.com"
        private const val HEARTBEAT_MS = 25_000L
        private const val BACKOFF_START_MS = 2_000L
        private const val BACKOFF_MAX_MS = 30_000L

        /** Port of drop.js normalizeServerUrl: bare host -> wss://, http(s) -> ws(s). */
        fun normalizeUrl(u: String): String {
            val t = u.trim()
            if (t.isEmpty()) return ""
            if (t.startsWith("wss://", true) || t.startsWith("ws://", true)) return t
            if (t.startsWith("https://", true)) return "wss://" + t.substring(8)
            if (t.startsWith("http://", true)) return "ws://" + t.substring(7)
            return "wss://$t"
        }
    }

    private val ui = Handler(Looper.getMainLooper())
    private val client = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.SECONDS) // long-lived socket; heartbeats keep it alive
        .build()

    private var ws: WebSocket? = null
    private var wanted = false
    private var lastUrl = ""
    private var deviceId = ""
    private var deviceName = ""
    private var backoffMs = BACKOFF_START_MS
    private var heartbeat: Runnable? = null

    private fun post(fn: () -> Unit) = ui.post(fn)

    /** Connect (and stay connected with backoff) as [deviceId]/[deviceName]. */
    @Synchronized
    fun connect(url: String, deviceId: String, deviceName: String) {
        close()
        val normalized = normalizeUrl(url)
        if (normalized.isEmpty()) {
            post { listener.onError("no matchmaker server configured") }
            return
        }
        wanted = true
        lastUrl = normalized
        this.deviceId = deviceId
        this.deviceName = deviceName.trim().take(24).ifEmpty { "Android" }
        backoffMs = BACKOFF_START_MS
        dial(normalized)
    }

    @Synchronized
    fun close() {
        wanted = false
        heartbeat?.let { ui.removeCallbacks(it) }
        heartbeat = null
        try {
            ws?.close(1000, "bye")
        } catch (_: Exception) {
        }
        ws = null
    }

    fun sendSignal(to: String, payload: JSONObject) {
        val ws = this.ws ?: return
        val msg = JSONObject()
            .put("t", "signal")
            .put("to", to)
            .put("payload", payload)
            .toString()
        try {
            ws.send(msg)
        } catch (_: Exception) {
        }
    }

    private fun dial(url: String) {
        val req = Request.Builder().url(url).build()
        ws = client.newWebSocket(req, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                backoffMs = BACKOFF_START_MS
                webSocket.send(
                    JSONObject()
                        .put("t", "register")
                        .put("id", deviceId)
                        .put("name", deviceName)
                        .toString(),
                )
                startHeartbeat()
                post { listener.onConnectionChange(true) }
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                handleMessage(text)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                onGone()
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                onGone()
            }
        })
    }

    private fun startHeartbeat() {
        heartbeat?.let { ui.removeCallbacks(it) }
        val r = object : Runnable {
            override fun run() {
                try {
                    ws?.send(JSONObject().put("t", "heartbeat").toString())
                } catch (_: Exception) {
                }
                heartbeat = this
                ui.postDelayed(this, HEARTBEAT_MS)
            }
        }
        heartbeat = r
        ui.postDelayed(r, HEARTBEAT_MS)
    }

    @Synchronized
    private fun onGone() {
        heartbeat?.let { ui.removeCallbacks(it) }
        heartbeat = null
        ws = null
        post { listener.onConnectionChange(false) }
        if (!wanted || lastUrl.isEmpty()) return
        val delay = backoffMs
        backoffMs = minOf(backoffMs * 2, BACKOFF_MAX_MS)
        ui.postDelayed(
            {
                synchronized(this) {
                    if (wanted && ws == null && lastUrl.isNotEmpty()) dial(lastUrl)
                }
            },
            delay,
        )
    }

    private fun handleMessage(raw: String) {
        val m = try {
            JSONObject(raw)
        } catch (_: Exception) {
            return
        }
        when (m.optString("t")) {
            "roster" -> {
                val arr = m.optJSONArray("devices") ?: return
                val devices = ArrayList<DropDevice>(arr.length())
                for (i in 0 until arr.length()) {
                    val d = arr.optJSONObject(i) ?: continue
                    devices += DropDevice(
                        id = d.optString("id"),
                        name = d.optString("name", "Unknown device"),
                        nearby = d.optBoolean("nearby", false),
                    )
                }
                // Nearby first, then alphabetical — like drop.js sortRoster.
                devices.sortWith { a, b ->
                    if (a.nearby != b.nearby) {
                        if (a.nearby) -1 else 1
                    } else {
                        a.name.compareTo(b.name)
                    }
                }
                post { listener.onRoster(devices) }
            }
            "signal" -> {
                val payload = m.optJSONObject("payload") ?: return
                if (!validPayload(payload)) return
                val from = m.optString("from")
                val fromName = m.optString("fromName", "Unknown device")
                post { listener.onSignal(from, fromName, payload) }
            }
            "error" -> {
                val msg = m.optString("msg", "matchmaker error")
                post { listener.onError(msg) }
            }
        }
    }

    /** Port of drop.js validSignalPayload — sanity-check inbound signals. */
    private fun validPayload(p: JSONObject): Boolean {
        return when (p.optString("kind")) {
            "declined" -> true
            "ice" -> p.isNull("candidate") || p.optJSONObject("candidate") != null
            "offer", "answer" -> {
                val sdp = p.optString("sdp")
                sdp.startsWith("v=0")
            }
            else -> false
        }
    }
}
