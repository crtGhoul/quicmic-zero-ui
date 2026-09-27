package com.quicmic.android.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.media.AudioManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import com.quicmic.android.StreamEvents
import com.quicmic.android.audio.MicStreamer
import com.quicmic.android.audio.SpeakerPlayer
import com.quicmic.android.net.Api
import com.quicmic.android.net.StatsOutcome
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.net.repairSession
import com.quicmic.android.store.SecureStore

/**
 * Foreground service that owns the mic stream (and optionally the PC-speaker
 * stream) so capture survives screen-off.
 *
 * Must be started while the app is in the foreground
 * (ContextCompat.startForegroundService); the service promotes itself with a
 * `microphone`-type foreground notification within seconds, as the platform
 * requires. The notification carries a Stop action.
 *
 * Besides the audio paths, the service runs the 1 s /api/stats poll — the
 * same liveness signal the web client uses: 503 means the server is shutting
 * down, 401 means our session was taken over (repaired in place via renew or
 * the stored PIN, mirroring web/app.js), and repeated transport failures mean
 * the server is gone.
 */
class QuicMicService : Service() {

    companion object {
        const val ACTION_START = "com.quicmic.android.action.START"
        const val ACTION_STOP = "com.quicmic.android.action.STOP"
        const val ACTION_TOGGLE_SPEAKER = "com.quicmic.android.action.TOGGLE_SPEAKER"
        const val ACTION_TOGGLE_BT = "com.quicmic.android.action.TOGGLE_BT"
        const val ACTION_TOGGLE_MUTE = "com.quicmic.android.action.TOGGLE_MUTE"
        const val ACTION_APPLY_SETTINGS = "com.quicmic.android.action.APPLY_SETTINGS"

        private const val NOTIF_ID = 1
        private const val CHANNEL_ID = "quicmic_stream"

        /** True while the streaming session is up (for UI gating of actions). */
        @Volatile
        var isRunning = false
    }

    private lateinit var store: SecureStore
    private lateinit var audioManager: AudioManager
    private lateinit var wifiLock: WifiManager.WifiLock
    private lateinit var mic: MicStreamer
    private lateinit var speaker: SpeakerPlayer

    private var statsThread: Thread? = null

    @Volatile
    private var streaming = false

    @Volatile
    private var statusText = "Idle"

    override fun onCreate() {
        super.onCreate()
        store = SecureStore(this)
        audioManager = getSystemService(AudioManager::class.java)
        val wifi = applicationContext.getSystemService(WifiManager::class.java)
        wifiLock = wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "QuicMic:stream")
        wifiLock.setReferenceCounted(false)
        mic = MicStreamer(store)
        speaker = SpeakerPlayer(store, audioManager)
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> startStreaming()
            ACTION_STOP -> stopStreaming()
            ACTION_TOGGLE_SPEAKER -> toggleSpeaker()
            ACTION_TOGGLE_BT -> speaker.setBluetoothSco(!speaker.bluetoothSco).also {
                StreamEvents.post(StreamEvents.Event.Bluetooth(speaker.bluetoothSco))
            }
            ACTION_TOGGLE_MUTE -> toggleMute()
            ACTION_APPLY_SETTINGS -> applySettings()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        stopStreaming()
        super.onDestroy()
    }

    // --- Actions ---

    private fun startStreaming() {
        if (streaming) return
        if (!store.isPaired) {
            setStatus("Not paired — scan the QR first")
            stopSelf()
            return
        }
        streaming = true
        isRunning = true
        startForegroundNotification("Connecting…")
        try {
            wifiLock.acquire()
        } catch (_: Exception) {
            // Lock unavailable; streaming continues without it.
        }
        mic.start(micListener)
        if (store.speakerEnabled) {
            speaker.start(speakerListener)
            StreamEvents.post(StreamEvents.Event.Speaker(true))
        }
        startStatsPoller()
    }

    private fun stopStreaming() {
        if (!streaming) {
            // Still make sure a stray start left nothing behind.
            stopSelf()
            return
        }
        streaming = false
        isRunning = false
        statsThread?.interrupt()
        statsThread = null
        try {
            mic.stop()
        } catch (_: Exception) {
            // Ignore.
        }
        try {
            speaker.stop()
        } catch (_: Exception) {
            // Ignore.
        }
        if (speaker.bluetoothSco) speaker.setBluetoothSco(false)
        try {
            if (wifiLock.isHeld) wifiLock.release()
        } catch (_: Exception) {
            // Ignore.
        }
        StreamEvents.post(StreamEvents.Event.Speaker(false))
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun toggleSpeaker() {
        if (!streaming) return
        if (speaker.running) {
            speaker.stop()
            StreamEvents.post(StreamEvents.Event.Speaker(false))
        } else {
            store.speakerEnabled = true
            speaker.start(speakerListener)
            StreamEvents.post(StreamEvents.Event.Speaker(true))
        }
    }

    private fun toggleMute() {
        if (!streaming) return
        mic.muted = !mic.muted
        setStatus(if (mic.muted) "Muted" else "Streaming")
    }

    private fun applySettings() {
        mic.setGateDb(store.gateDb)
        mic.setGain(store.gain)
    }

    // --- Listeners ---

    private val micListener = object : MicStreamer.Listener {
        override fun onLevel(level: Float) {
            StreamEvents.post(StreamEvents.Event.Level(level))
        }

        override fun onStatus(text: String) = setStatus(text)
        override fun onStopped() {
            StreamEvents.post(StreamEvents.Event.StreamingStopped)
        }

        override fun onPairingLost() {
            StreamEvents.post(StreamEvents.Event.PairingLost)
            stopStreaming()
        }

        override fun onServerGone() {
            StreamEvents.post(StreamEvents.Event.ServerGone)
            stopStreaming()
        }
    }

    private val speakerListener = object : SpeakerPlayer.Listener {
        override fun onStatus(text: String) = setStatus(text)
        override fun onStopped(reason: String) {
            StreamEvents.post(StreamEvents.Event.Speaker(false))
            setStatus(reason)
        }
    }

    // --- Stats poller (liveness, mirrors the web client's 1 s tick) ---

    private fun startStatsPoller() {
        statsThread?.interrupt()
        statsThread = Thread({
            val host = store.getHost() ?: return@Thread
            val certDer = store.getCertDer() ?: return@Thread
            val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
            var failures = 0
            while (streaming) {
                try {
                    Thread.sleep(1000)
                } catch (_: InterruptedException) {
                    break
                }
                if (!streaming) break
                val token = store.getToken() ?: break
                val outcome: StatsOutcome = try {
                    api.getStats(token)
                } catch (_: Exception) {
                    StatsOutcome.Error("unreachable")
                }
                when (outcome) {
                    is StatsOutcome.Ok -> {
                        failures = 0
                        StreamEvents.post(StreamEvents.statsFrom(outcome.json))
                    }
                    is StatsOutcome.Unauthorized -> {
                        // Server is alive but our session was taken over —
                        // repair in place, then drop the stale socket so the
                        // streamer reconnects immediately.
                        if (!api.repairSession(store)) {
                            StreamEvents.post(StreamEvents.Event.PairingLost)
                            stopStreaming()
                            break
                        }
                        failures = 0
                        mic.poke()
                    }
                    is StatsOutcome.ServerGone -> {
                        StreamEvents.post(StreamEvents.Event.ServerGone)
                        stopStreaming()
                        break
                    }
                    is StatsOutcome.Error -> {
                        failures++
                        if (failures >= 5) {
                            StreamEvents.post(StreamEvents.Event.ServerGone)
                            stopStreaming()
                            break
                        }
                    }
                }
            }
        }, "quicmic-stats").apply {
            isDaemon = true
            start()
        }
    }

    // --- Foreground notification ---

    private fun setStatus(text: String) {
        statusText = text
        updateNotification(text)
        StreamEvents.post(StreamEvents.Event.Status(text))
    }

    private fun notificationManager(): NotificationManager =
        getSystemService(NotificationManager::class.java)

    private fun buildNotification(text: String): Notification {
        val nm = notificationManager()
        // NotificationChannel exists since API 26, our minSdk — no guard needed.
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "QuicMic streaming", NotificationManager.IMPORTANCE_LOW),
        )
        val stopIntent = Intent(this, QuicMicService::class.java).setAction(ACTION_STOP)
        val stopPi = PendingIntent.getService(
            this, 0, stopIntent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("QuicMic")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                    "Stop",
                    stopPi,
                ).build(),
            )
            .build()
    }

    private fun startForegroundNotification(text: String) {
        val notif = buildNotification(text)
        // The 3-arg overload (foreground service type) exists since API 29;
        // the manifest already declares foregroundServiceType="microphone".
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIF_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        } else {
            @Suppress("DEPRECATION")
            startForeground(NOTIF_ID, notif)
        }
    }

    private fun updateNotification(text: String) {
        if (!streaming) return
        try {
            notificationManager().notify(NOTIF_ID, buildNotification(text))
        } catch (_: Exception) {
            // Notification updates are cosmetic; never crash the stream for them.
        }
    }
}
