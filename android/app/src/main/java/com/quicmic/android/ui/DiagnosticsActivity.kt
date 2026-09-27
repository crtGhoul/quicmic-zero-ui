package com.quicmic.android.ui

import android.os.Bundle
import android.os.SystemClock
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import com.quicmic.android.R
import com.quicmic.android.net.Api
import com.quicmic.android.net.StatsOutcome
import com.quicmic.android.net.fingerprintHex
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.service.QuicMicService
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread

/**
 * Read-only diagnostics screen: transport, server reachability, the live
 * stream health, server-side stats, and the pinned certificate fingerprint.
 * Opened from the System section in Settings.
 *
 * Data-source honesty notes (see the task brief):
 *  - Transport: the Android client only ever opens a WebSocket (MicStreamer
 *    has no WebTransport path), so "WebSocket (TCP)" is shown while the
 *    streaming service is up and "Not streaming" otherwise — never invented.
 *  - Ping: measured as the round-trip time of the GET /api/stats call itself
 *    (labeled "Ping (HTTPS RTT)"); there is no dedicated ping endpoint.
 *  - Audio health: MicStreamer's `running` flag is private to the service, so
 *    this reports the service-level state (streaming / idle).
 */
class DiagnosticsActivity : AppCompatActivity() {

    private lateinit var store: SecureStore
    private lateinit var tvTransport: TextView
    private lateinit var tvServer: TextView
    private lateinit var tvLanIp: TextView
    private lateinit var tvPing: TextView
    private lateinit var tvDevice: TextView
    private lateinit var tvMic: TextView
    private lateinit var tvPackets: TextView
    private lateinit var tvLoss: TextView
    private lateinit var tvBuffer: TextView
    private lateinit var tvFingerprint: TextView
    private lateinit var btnRefresh: Button

    override fun onCreate(savedInstanceState: Bundle?) {
        ThemeManager.apply(this) // belt-and-braces; QuicMicApp covers this too
        super.onCreate(savedInstanceState)
        store = SecureStore(this)
        setContentView(R.layout.activity_diagnostics)

        tvTransport = findViewById(R.id.diag_transport)
        tvServer = findViewById(R.id.diag_server)
        tvLanIp = findViewById(R.id.diag_lan_ip)
        tvPing = findViewById(R.id.diag_ping)
        tvDevice = findViewById(R.id.diag_device)
        tvMic = findViewById(R.id.diag_mic)
        tvPackets = findViewById(R.id.diag_packets)
        tvLoss = findViewById(R.id.diag_loss)
        tvBuffer = findViewById(R.id.diag_buffer)
        tvFingerprint = findViewById(R.id.diag_fingerprint)
        btnRefresh = findViewById(R.id.diag_btn_refresh)

        renderLocal()
        btnRefresh.setOnClickListener { refresh() }
        refresh()
    }

    /** Everything derivable without touching the network. */
    private fun renderLocal() {
        val host = store.getHost()
        tvServer.text = if (host != null) "$host:${store.getPort()}" else na()
        tvDevice.text = store.deviceName
        tvFingerprint.text = store.getCertDer()?.let { fingerprintHex(it) } ?: na()

        if (QuicMicService.isRunning) {
            // MicStreamer only ever opens a wss:// WebSocket — no WT path exists
            // on Android, so this is observed, not assumed.
            tvTransport.text = getString(R.string.diag_transport_ws)
            tvMic.text = getString(R.string.diag_streaming)
        } else {
            tvTransport.text = getString(R.string.diag_not_streaming)
            tvMic.text = getString(R.string.diag_idle)
            clearLiveRows()
        }
    }

    private fun clearLiveRows() {
        tvPing.text = na()
        tvLanIp.text = na()
        tvPackets.text = na()
        tvLoss.text = na()
        tvBuffer.text = na()
    }

    private fun na(): String = getString(R.string.diag_unavailable)

    /** Best-effort network refresh: /api/info + /api/stats. */
    private fun refresh() {
        renderLocal()
        btnRefresh.isEnabled = false
        btnRefresh.text = getString(R.string.diag_refreshing)
        val host = store.getHost()
        val certDer = store.getCertDer()
        val token = store.getToken()
        if (host == null || certDer == null) {
            btnRefresh.isEnabled = true
            btnRefresh.text = getString(R.string.diag_refresh)
            return
        }
        thread(isDaemon = true) {
            var lanIp: String? = null
            var pingMs: Long? = null
            var packets: Long? = null
            var loss: Double? = null
            var buffer: Long? = null
            try {
                val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
                lanIp = try {
                    api.getInfo().lanIp.takeIf { it.isNotEmpty() }
                } catch (_: Exception) {
                    null
                }
                if (token != null) {
                    val t0 = SystemClock.elapsedRealtime()
                    when (val outcome = api.getStats(token)) {
                        is StatsOutcome.Ok -> {
                            pingMs = SystemClock.elapsedRealtime() - t0
                            val j = outcome.json
                            packets = j.optLong("packets_received")
                            loss = j.optDouble("loss_percent")
                            buffer = j.optLong("buffer_ms")
                        }
                        else -> { /* server alive but no stats for us; leave "—" */ }
                    }
                }
            } catch (_: Exception) {
                // Best-effort screen: local values stay, live rows stay "—".
            }
            runOnUiThread {
                tvLanIp.text = lanIp ?: na()
                tvPing.text = pingMs?.let { "$it ms" } ?: na()
                tvPackets.text = packets?.toString() ?: na()
                tvLoss.text = loss?.let { "%.1f %%".format(it) } ?: na()
                tvBuffer.text = buffer?.let { "$it ms" } ?: na()
                btnRefresh.isEnabled = true
                btnRefresh.text = getString(R.string.diag_refresh)
            }
        }
    }
}
