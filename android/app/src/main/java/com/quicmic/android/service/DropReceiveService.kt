package com.quicmic.android.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.graphics.drawable.Icon
import android.os.Build
import android.os.Environment
import android.os.IBinder
import android.provider.MediaStore
import androidx.core.content.ContextCompat
import com.quicmic.android.R
import com.quicmic.android.net.DropMatchmaker
import com.quicmic.android.net.DropPeer
import com.quicmic.android.net.DropPeerListener
import com.quicmic.android.store.SecureStore
import org.json.JSONObject
import org.webrtc.IceCandidate
import org.webrtc.SessionDescription
import java.io.File

/**
 * Background Drop receiver.
 *
 * How incoming Drops reach the phone: the PC has NO push endpoint for Drop
 * (see net/DropRtc.kt — the PC only serves the drop.html page; transfers
 * are WebRTC data channels). A device that wants to send to this phone
 * rings it through the tap-to-connect matchmaker. This service keeps that
 * matchmaker socket alive in the background and raises an Accept/Decline
 * notification for each incoming offer. No server-side change was needed —
 * the signaling path already exists; the service is the plumbing the web
 * page's call modal (drop.html #callModal) is on desktop.
 *
 * Foreground service type: dataSync — the service exists to transfer files
 * to the device. Android 14+ requires the declared type plus
 * FOREGROUND_SERVICE_DATA_SYNC (both in the manifest). Note for testers:
 * Android 14 caps dataSync foreground services at roughly 6h per 24h.
 *
 * Files land in Downloads/QuicMic via MediaStore (scoped storage, API 29+;
 * pre-Q falls back to the public Downloads dir).
 */
class DropReceiveService : Service() {

    companion object {
        const val ACTION_START = "com.quicmic.android.drop.RECEIVE_START"
        const val ACTION_STOP = "com.quicmic.android.drop.RECEIVE_STOP"
        const val ACTION_ACCEPT = "com.quicmic.android.drop.RECEIVE_ACCEPT"
        const val ACTION_DECLINE = "com.quicmic.android.drop.RECEIVE_DECLINE"

        private const val CHANNEL_LISTEN = "drop_listen"
        private const val CHANNEL_INCOMING = "drop_incoming"
        private const val NOTIF_LISTEN = 2001
        private const val NOTIF_INCOMING = 2002
        private const val NOTIF_TRANSFER = 2003

        @Volatile
        var isRunning = false
            private set

        fun start(context: Context) {
            ContextCompat.startForegroundService(
                context,
                Intent(context, DropReceiveService::class.java).setAction(ACTION_START),
            )
        }

        fun stop(context: Context) {
            context.startService(
                Intent(context, DropReceiveService::class.java).setAction(ACTION_STOP),
            )
        }
    }

    private lateinit var store: SecureStore
    private lateinit var matchmaker: DropMatchmaker
    private var peer: DropPeer? = null
    private var peerFrom: String? = null

    private data class PendingOffer(val from: String, val fromName: String, val sdp: String)
    private var pendingOffer: PendingOffer? = null

    override fun onCreate() {
        super.onCreate()
        store = SecureStore(this)
        matchmaker = DropMatchmaker(object : DropMatchmaker.Listener {
            override fun onRoster(devices: List<DropMatchmaker.DropDevice>) {}
            override fun onSignal(from: String, fromName: String, payload: JSONObject) =
                handleSignal(from, fromName, payload)
            override fun onConnectionChange(connected: Boolean) =
                updateListenNotification(connected)
            override fun onError(msg: String) {}
        })
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> startListening()
            ACTION_STOP -> stopListening()
            ACTION_ACCEPT -> acceptPending()
            ACTION_DECLINE -> declinePending()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        isRunning = false
        matchmaker.close()
        peer?.close()
        super.onDestroy()
    }

    // --- lifecycle ---

    private fun startListening() {
        if (!store.isPaired) {
            // No PC paired — nothing to receive for; don't linger.
            stopSelf()
            return
        }
        startForegroundNotification()
        isRunning = true
        matchmaker.connect(store.dropSignalUrl, store.dropDeviceId, store.deviceName)
    }

    private fun stopListening() {
        isRunning = false
        matchmaker.close()
        peer?.close()
        peer = null
        peerFrom = null
        pendingOffer = null
        notificationManager().cancel(NOTIF_INCOMING)
        notificationManager().cancel(NOTIF_TRANSFER)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    // --- incoming signals ---

    private fun handleSignal(from: String, fromName: String, p: JSONObject) {
        when (p.optString("kind")) {
            "offer" -> {
                val sdp = p.optString("sdp")
                if (sdp.isEmpty()) return
                // Busy with another transfer — decline like drop.js does.
                if (pendingOffer != null || (peer != null && peerFrom != null)) {
                    sendDeclined(from)
                    return
                }
                pendingOffer = PendingOffer(from, fromName, sdp)
                if (store.dropAutoAccept) {
                    acceptPending()
                } else {
                    showIncomingNotification(fromName)
                }
            }
            "answer" -> {
                // We never ring out from this service; ignore.
            }
            "ice" -> {
                val c = p.optJSONObject("candidate") ?: return
                if (peerFrom == from) {
                    peer?.addRemoteIce(
                        IceCandidate(
                            c.optString("sdpMid"),
                            c.optInt("sdpMLineIndex"),
                            c.optString("candidate"),
                        ),
                    )
                }
            }
            "declined" -> {
                // Caller hung up while we were deciding.
                if (pendingOffer?.from == from) {
                    pendingOffer = null
                    notificationManager().cancel(NOTIF_INCOMING)
                }
            }
        }
    }

    private fun acceptPending() {
        val offer = pendingOffer ?: return
        pendingOffer = null
        notificationManager().cancel(NOTIF_INCOMING)
        peerFrom = offer.from
        peer?.close()
        val listener = object : DropPeerListener {
            override fun onLocalDescription(desc: SessionDescription) {
                if (desc.type != SessionDescription.Type.ANSWER) return
                matchmaker.sendSignal(
                    offer.from,
                    JSONObject().put("kind", "answer").put("sdp", desc.description),
                )
            }
            override fun onLocalIce(candidate: IceCandidate) {
                matchmaker.sendSignal(
                    offer.from,
                    JSONObject().put("kind", "ice").put(
                        "candidate",
                        JSONObject()
                            .put("candidate", candidate.sdp)
                            .put("sdpMid", candidate.sdpMid)
                            .put("sdpMLineIndex", candidate.sdpMLineIndex),
                    ),
                )
            }
            override fun onFileStart(name: String, size: Long, mime: String) {
                showTransferNotification(name, 0, size)
            }
            override fun onFileProgress(received: Long, total: Long) {
                showTransferNotification(null, received, total)
            }
            override fun onFileComplete(name: String, mime: String, data: ByteArray) {
                val ok = saveToDownloads(name, mime, data)
                notificationManager().cancel(NOTIF_TRANSFER)
                showSavedNotification(name, ok)
                endTransfer()
            }
            override fun onDisconnected() = endTransfer()
            override fun onError(msg: String) = endTransfer()
        }
        try {
            peer = DropPeer(this, listener).also { it.answerOffer(offer.sdp) }
        } catch (e: Exception) {
            peer = null
            peerFrom = null
        }
    }

    private fun declinePending() {
        val offer = pendingOffer ?: return
        pendingOffer = null
        notificationManager().cancel(NOTIF_INCOMING)
        sendDeclined(offer.from)
    }

    private fun sendDeclined(to: String) {
        matchmaker.sendSignal(to, JSONObject().put("kind", "declined"))
    }

    private fun endTransfer() {
        peer?.close()
        peer = null
        peerFrom = null
    }

    // --- storage ---

    private fun saveToDownloads(name: String, mime: String, data: ByteArray): Boolean {
        val safeName = name.replace(Regex("[/\\\\]"), "_").take(128).ifEmpty { "drop-file" }
        return try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                val values = ContentValues().apply {
                    put(MediaStore.Downloads.DISPLAY_NAME, safeName)
                    put(
                        MediaStore.Downloads.MIME_TYPE,
                        mime.ifEmpty { "application/octet-stream" },
                    )
                    put(
                        MediaStore.Downloads.RELATIVE_PATH,
                        Environment.DIRECTORY_DOWNLOADS + "/QuicMic",
                    )
                }
                val uri = contentResolver.insert(
                    MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                    values,
                ) ?: return false
                contentResolver.openOutputStream(uri)?.use { it.write(data) } ?: return false
                true
            } else {
                @Suppress("DEPRECATION")
                val dir = Environment.getExternalStoragePublicDirectory(
                    Environment.DIRECTORY_DOWNLOADS,
                )
                File(dir, safeName).writeBytes(data)
                true
            }
        } catch (_: Exception) {
            false
        }
    }

    // --- notifications ---

    private fun notificationManager(): NotificationManager =
        getSystemService(NotificationManager::class.java)

    private fun serviceIntent(action: String, requestCode: Int): PendingIntent =
        PendingIntent.getService(
            this,
            requestCode,
            Intent(this, DropReceiveService::class.java).setAction(action),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )

    private fun startForegroundNotification() {
        val nm = notificationManager()
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL_LISTEN,
                getString(R.string.drop_notif_listen_channel),
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
        // Created up-front too: auto-accept posts "saved" notifications here
        // without ever showing the incoming-call notification.
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL_INCOMING,
                getString(R.string.drop_notif_incoming_channel),
                NotificationManager.IMPORTANCE_HIGH,
            ),
        )
        val notif = listenNotification(connected = false)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIF_LISTEN,
                notif,
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } else {
            @Suppress("DEPRECATION")
            startForeground(NOTIF_LISTEN, notif)
        }
    }

    private fun listenNotification(connected: Boolean): Notification {
        val text = if (connected) {
            getString(R.string.drop_notif_listening)
        } else {
            getString(R.string.drop_notif_connecting)
        }
        return Notification.Builder(this, CHANNEL_LISTEN)
            .setContentTitle(getString(R.string.drop_notif_listen_title))
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(
                        this,
                        android.R.drawable.ic_menu_close_clear_cancel,
                    ),
                    getString(R.string.drop_stop),
                    serviceIntent(ACTION_STOP, 10),
                ).build(),
            )
            .build()
    }

    private fun updateListenNotification(connected: Boolean) {
        if (!isRunning) return
        try {
            notificationManager().notify(NOTIF_LISTEN, listenNotification(connected))
        } catch (_: Exception) {
        }
    }

    private fun showIncomingNotification(fromName: String) {
        // Channel is created in startForegroundNotification; create again
        // harmlessly in case this path is ever reached first.
        notificationManager().createNotificationChannel(
            NotificationChannel(
                CHANNEL_INCOMING,
                getString(R.string.drop_notif_incoming_channel),
                NotificationManager.IMPORTANCE_HIGH,
            ),
        )
        val notif = Notification.Builder(this, CHANNEL_INCOMING)
            .setContentTitle(getString(R.string.drop_incoming_title, fromName))
            .setContentText(getString(R.string.drop_incoming_text))
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setVibrate(longArrayOf(0, 120, 60, 120))
            .setAutoCancel(true)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, android.R.drawable.ic_menu_call),
                    getString(R.string.drop_accept),
                    serviceIntent(ACTION_ACCEPT, 11),
                ).build(),
            )
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(
                        this,
                        android.R.drawable.ic_menu_close_clear_cancel,
                    ),
                    getString(R.string.drop_decline),
                    serviceIntent(ACTION_DECLINE, 12),
                ).build(),
            )
            .build()
        try {
            notificationManager().notify(NOTIF_INCOMING, notif)
        } catch (_: Exception) {
        }
    }

    private fun showTransferNotification(name: String?, received: Long, total: Long) {
        val builder = Notification.Builder(this, CHANNEL_LISTEN)
            .setContentTitle(getString(R.string.drop_receiving_title))
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setOngoing(true)
            .setProgress(
                100,
                if (total > 0) (received * 100 / total).toInt().coerceIn(0, 100) else 0,
                total <= 0,
            )
        if (name != null) builder.setContentText(name)
        try {
            notificationManager().notify(NOTIF_TRANSFER, builder.build())
        } catch (_: Exception) {
        }
    }

    private fun showSavedNotification(name: String, ok: Boolean) {
        val notif = Notification.Builder(this, CHANNEL_INCOMING)
            .setContentTitle(
                if (ok) getString(R.string.drop_saved_title) else getString(R.string.drop_save_failed_title),
            )
            .setContentText(name)
            .setSmallIcon(
                if (ok) android.R.drawable.stat_sys_download_done
                else android.R.drawable.stat_notify_error,
            )
            .setAutoCancel(true)
            .build()
        try {
            notificationManager().notify(NOTIF_TRANSFER, notif)
        } catch (_: Exception) {
        }
    }
}
