package com.quicmic.android.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.widget.Button
import android.widget.ProgressBar
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.quicmic.android.R
import com.quicmic.android.StreamEvents
import com.quicmic.android.service.QuicMicService
import com.quicmic.android.store.SecureStore

/**
 * Main screen: streaming status, VU meter, stats, start/stop, mute, pairing,
 * settings, speaker + Bluetooth toggles.
 *
 * The actual streaming lives in [QuicMicService] (foreground service); this
 * activity only sends it actions and renders [StreamEvents].
 */
class MainActivity : AppCompatActivity() {

    companion object {
        private const val REQ_MIC = 1
        private const val REQ_NOTIF = 2
    }

    private lateinit var store: SecureStore
    private lateinit var tvStatus: TextView
    private lateinit var tvStats: TextView
    private lateinit var levelBar: ProgressBar
    private lateinit var btnStartStop: Button
    private lateinit var btnMute: Button
    private lateinit var btnFiles: Button
    private lateinit var swSpeaker: Switch
    private lateinit var swBt: Switch

    private val mainHandler = Handler(Looper.getMainLooper())
    private var lastLevelUi = 0L
    private var streaming = false
    private var muted = false

    private val events = StreamEvents.Listener { e ->
        mainHandler.post { handleEvent(e) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = SecureStore(this)
        setContentView(R.layout.activity_main)

        tvStatus = findViewById(R.id.tv_status)
        tvStats = findViewById(R.id.tv_stats)
        levelBar = findViewById(R.id.level_bar)
        btnStartStop = findViewById(R.id.btn_start_stop)
        btnMute = findViewById(R.id.btn_mute)
        btnFiles = findViewById(R.id.btn_files)
        swSpeaker = findViewById(R.id.sw_speaker)
        swBt = findViewById(R.id.sw_bt)

        btnStartStop.setOnClickListener { toggleStreaming() }
        btnMute.setOnClickListener { sendServiceAction(QuicMicService.ACTION_TOGGLE_MUTE) }
        btnFiles.setOnClickListener { openDrop() }
        findViewById<Button>(R.id.btn_pair).setOnClickListener {
            startActivity(Intent(this, PairActivity::class.java))
        }
        findViewById<Button>(R.id.btn_settings).setOnClickListener {
            startActivity(Intent(this, SettingsActivity::class.java))
        }
        findViewById<Button>(R.id.btn_unpair).setOnClickListener { confirmUnpair() }

        swSpeaker.setOnCheckedChangeListener { _, on ->
            store.speakerEnabled = on
            if (streaming) sendServiceAction(QuicMicService.ACTION_TOGGLE_SPEAKER)
        }
        swBt.setOnCheckedChangeListener { _, on ->
            // Persist immediately; the service applies it live when streaming.
            store.btEnabled = on
            if (streaming) sendServiceAction(QuicMicService.ACTION_TOGGLE_BT)
        }

        swSpeaker.isChecked = store.speakerEnabled
        swBt.isChecked = store.btEnabled
        refreshUi()
    }

    override fun onResume() {
        super.onResume()
        StreamEvents.add(events)
        refreshUi()
    }

    override fun onPause() {
        StreamEvents.remove(events)
        super.onPause()
    }

    private fun refreshUi() {
        btnStartStop.text = if (streaming) "Stop" else "Start"
        btnMute.text = if (muted) "Unmute" else "Mute"
        btnMute.isEnabled = streaming
        btnFiles.isEnabled = store.isPaired
        if (!store.isPaired) {
            tvStatus.text = "Not paired — tap Pair and scan the PC's QR code"
        }
    }

    /** Open the PC's Drop file-sharing page in the pinned in-app browser. */
    private fun openDrop() {
        if (!store.isPaired) {
            toast("Pair with the PC first")
            startActivity(Intent(this, PairActivity::class.java))
            return
        }
        startActivity(Intent(this, DropActivity::class.java))
    }

    private fun toggleStreaming() {
        if (streaming) {
            sendServiceAction(QuicMicService.ACTION_STOP)
            return
        }
        if (!store.isPaired) {
            toast("Pair with the PC first")
            startActivity(Intent(this, PairActivity::class.java))
            return
        }
        if (!hasPermission(Manifest.permission.RECORD_AUDIO)) {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.RECORD_AUDIO), REQ_MIC)
            return
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            !hasPermission(Manifest.permission.POST_NOTIFICATIONS)
        ) {
            ActivityCompat.requestPermissions(
                this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIF,
            )
            // Continue anyway: the notification permission only affects whether
            // the persistent notification is shown, not the stream itself.
        }
        streaming = true
        muted = false
        refreshUi()
        val intent = Intent(this, QuicMicService::class.java)
            .setAction(QuicMicService.ACTION_START)
        ContextCompat.startForegroundService(this, intent)
    }

    private fun sendServiceAction(action: String) {
        if (!streaming) return
        startService(Intent(this, QuicMicService::class.java).setAction(action))
    }

    private fun confirmUnpair() {
        if (!store.isPaired) {
            toast("Not paired")
            return
        }
        AlertDialog.Builder(this)
            .setTitle("Unpair")
            .setMessage("Forget this PC? The session token and pinned certificate are deleted.")
            .setPositiveButton("Unpair") { _, _ ->
                sendServiceAction(QuicMicService.ACTION_STOP)
                store.clearPairing()
                streaming = false
                refreshUi()
                tvStatus.text = "Not paired — tap Pair and scan the PC's QR code"
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun handleEvent(e: StreamEvents.Event) {
        when (e) {
            is StreamEvents.Event.Status -> tvStatus.text = e.text
            is StreamEvents.Event.Level -> {
                val now = SystemClock.uptimeMillis()
                if (now - lastLevelUi > 100) {
                    lastLevelUi = now
                    levelBar.progress = e.level.toInt()
                }
            }
            is StreamEvents.Event.Stats -> tvStats.text =
                "↑ ${e.packetsReceived} pkts · loss ${"%.2f".format(e.lossPercent)}% · buf ${e.bufferMs} ms"
            is StreamEvents.Event.Speaker -> {
                if (swSpeaker.isChecked != e.on) swSpeaker.isChecked = e.on
            }
            is StreamEvents.Event.Bluetooth -> {
                if (swBt.isChecked != e.on) swBt.isChecked = e.on
            }
            StreamEvents.Event.StreamingStopped -> {
                streaming = false
                muted = false
                refreshUi()
            }
            StreamEvents.Event.PairingLost -> {
                streaming = false
                toast("Session expired — please pair again")
                startActivity(Intent(this, PairActivity::class.java))
                refreshUi()
            }
            StreamEvents.Event.ServerGone -> {
                streaming = false
                toast("Server unreachable")
                refreshUi()
            }
        }
        if (e is StreamEvents.Event.Status) {
            muted = e.text == "Muted"
            btnMute.text = if (muted) "Unmute" else "Mute"
        }
    }

    private fun hasPermission(perm: String): Boolean =
        ContextCompat.checkSelfPermission(this, perm) == PackageManager.PERMISSION_GRANTED

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_MIC &&
            grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED
        ) {
            toggleStreaming()
        } else if (requestCode == REQ_MIC) {
            toast("Microphone permission is required to stream")
        }
    }

    private fun toast(msg: String) =
        Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
}
