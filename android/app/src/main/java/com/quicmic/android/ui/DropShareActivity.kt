package com.quicmic.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Parcelable
import android.provider.OpenableColumns
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity
import com.google.zxing.BarcodeFormat
import com.journeyapps.barcodescanner.BarcodeEncoder
import com.quicmic.android.R
import com.quicmic.android.net.DropMatchmaker
import com.quicmic.android.net.DropPeer
import com.quicmic.android.net.DropPeerListener
import com.quicmic.android.net.DropSignalCodec
import com.quicmic.android.store.SecureStore
import org.json.JSONObject
import org.webrtc.IceCandidate
import org.webrtc.SessionDescription

/**
 * Native Drop sender: handles ACTION_SEND / ACTION_SEND_MULTIPLE (files and
 * text) and uploads straight to the PC over a native WebRTC data channel —
 * no WebView, no second pairing.
 *
 * Endpoint/auth finding: the PC server exposes NO Drop upload endpoint
 * (src/server/mod.rs has only the /api/ endpoints, /ws, /speaker-ws, /ca, /qr + static
 * assets), so "upload via the pinned OkHttp client + mic token" is not
 * possible without new server endpoints. The transport is therefore the
 * real Drop protocol from web/drop.js (data channel "localdrop"), spoken
 * natively via org.webrtc. Identity is the mic pairing identity:
 * device_name + stable drop device id from SecureStore — the PC sees the
 * same name it sees for the mic.
 *
 * Two ways to reach the PC:
 *  1. Tap-to-connect: join the matchmaker, tap the PC in the roster, its
 *     drop.html rings, it accepts, files send automatically.
 *  2. Pair with code: the PC opens drop.html → Share → shows a code; paste
 *     it here, hand the reply code back (copy or QR).
 */
class DropShareActivity : AppCompatActivity() {

    companion object {
        /** Start with a system file picker, then send what was picked. */
        const val EXTRA_PICK_FILE = "com.quicmic.android.drop.PICK_FILE"
        private const val REQ_PICK = 41
        private const val RING_TIMEOUT_MS = 20_000L
        private const val MAX_FILE_BYTES = 256L * 1024 * 1024
    }

    private sealed interface ShareItem {
        data class Text(val text: String) : ShareItem
        data class File(val name: String, val mime: String, val bytes: ByteArray) : ShareItem
    }

    private lateinit var store: SecureStore
    private val ui = Handler(Looper.getMainLooper())
    private val items = ArrayList<ShareItem>()
    private var matchmaker: DropMatchmaker? = null
    private var peer: DropPeer? = null
    private var ringTo: String? = null
    private var ringTimer: Runnable? = null
    private var sending = false

    private lateinit var llItems: LinearLayout
    private lateinit var tvStatus: TextView
    private lateinit var progress: ProgressBar
    private lateinit var btnFindPc: Button
    private lateinit var llRoster: LinearLayout
    private lateinit var btnPairCode: Button
    private lateinit var llCode: LinearLayout
    private lateinit var etCode: EditText

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = SecureStore(this)
        if (!store.isPaired) {
            Toast.makeText(this, getString(R.string.drop_pair_first), Toast.LENGTH_SHORT).show()
            finish()
            return
        }
        setContentView(R.layout.activity_drop_share)
        llItems = findViewById(R.id.ll_items)
        tvStatus = findViewById(R.id.tv_status)
        progress = findViewById(R.id.progress)
        btnFindPc = findViewById(R.id.btn_find_pc)
        llRoster = findViewById(R.id.ll_roster)
        btnPairCode = findViewById(R.id.btn_pair_code)
        llCode = findViewById(R.id.ll_code)
        etCode = findViewById(R.id.et_code)

        btnFindPc.setOnClickListener { findPc() }
        btnPairCode.setOnClickListener {
            llCode.visibility = if (llCode.visibility == View.VISIBLE) View.GONE else View.VISIBLE
        }
        findViewById<Button>(R.id.btn_make_reply).setOnClickListener { joinWithCode() }

        if (intent?.getBooleanExtra(EXTRA_PICK_FILE, false) == true) {
            pickFile()
        } else {
            collectIntent(intent)
        }
        renderItems()
    }

    // --- collecting the shared content ---

    private fun pickFile() {
        val i = Intent(Intent.ACTION_OPEN_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE)
            .setType("*/*")
            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        startActivityForResult(i, REQ_PICK)
    }

    @Suppress("DEPRECATION") // startActivityForResult for the document picker
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_PICK) return
        if (resultCode != RESULT_OK || data == null) {
            if (items.isEmpty()) finish()
            return
        }
        val uris = ArrayList<Uri>()
        data.clipData?.let { clip ->
            for (i in 0 until clip.itemCount) uris += clip.getItemAt(i).uri
        }
        data.data?.let { uris += it }
        uris.forEach { addUri(it) }
        renderItems()
    }

    private fun collectIntent(intent: Intent?) {
        if (intent == null) return
        when (intent.action) {
            Intent.ACTION_SEND -> {
                intent.getStringExtra(Intent.EXTRA_TEXT)?.takeIf { it.isNotBlank() }?.let {
                    items += ShareItem.Text(it)
                }
                streamExtra(intent)?.let { addUri(it) }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                streamListExtra(intent).forEach { addUri(it) }
                intent.getStringExtra(Intent.EXTRA_TEXT)?.takeIf { it.isNotBlank() }?.let {
                    items += ShareItem.Text(it)
                }
            }
        }
    }

    @Suppress("DEPRECATION")
    private fun streamExtra(intent: Intent): Uri? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Parcelable::class.java) as? Uri
        } else {
            intent.getParcelableExtra(Intent.EXTRA_STREAM) as? Uri
        }

    @Suppress("DEPRECATION")
    private fun streamListExtra(intent: Intent): List<Uri> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Parcelable::class.java)
                .orEmpty().filterIsInstance<Uri>()
        } else {
            intent.getParcelableArrayListExtra<Parcelable>(Intent.EXTRA_STREAM)
                .orEmpty().filterIsInstance<Uri>()
        }

    private fun addUri(uri: Uri) {
        try {
            var name = "file"
            contentResolver.query(
                uri,
                arrayOf(OpenableColumns.DISPLAY_NAME),
                null, null, null,
            )?.use { c ->
                if (c.moveToFirst()) name = c.getString(0) ?: name
            }
            val mime = contentResolver.getType(uri) ?: "application/octet-stream"
            val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() }
                ?: return
            if (bytes.size > MAX_FILE_BYTES) {
                Toast.makeText(
                    this,
                    getString(R.string.drop_file_too_big, name),
                    Toast.LENGTH_LONG,
                ).show()
                return
            }
            items += ShareItem.File(name, mime, bytes)
        } catch (_: Exception) {
            Toast.makeText(this, getString(R.string.drop_cant_read), Toast.LENGTH_SHORT).show()
        }
    }

    private fun renderItems() {
        llItems.removeAllViews()
        if (items.isEmpty()) {
            setStatus(getString(R.string.drop_nothing_to_send))
            btnFindPc.isEnabled = false
            btnPairCode.isEnabled = false
            return
        }
        for (item in items) {
            val tv = TextView(this).apply {
                text = when (item) {
                    is ShareItem.Text -> "✉️ " + item.text.take(80)
                    is ShareItem.File -> "📎 ${item.name} (${fmtSize(item.bytes.size.toLong())})"
                }
                setTextColor(getColor(R.color.text_primary))
                textSize = 14f
                setPadding(0, 6, 0, 6)
            }
            llItems.addView(tv)
        }
        setStatus(getString(R.string.drop_ready, items.size))
    }

    // --- tap-to-connect flow ---

    private fun findPc() {
        if (items.isEmpty() || sending) return
        setStatus(getString(R.string.drop_connecting_matchmaker))
        btnFindPc.isEnabled = false
        matchmaker?.close()
        matchmaker = DropMatchmaker(object : DropMatchmaker.Listener {
            override fun onRoster(devices: List<DropMatchmaker.DropDevice>): Unit =
                renderRoster(devices)
            override fun onSignal(from: String, fromName: String, payload: JSONObject): Unit =
                onSignal(from, fromName, payload)
            override fun onConnectionChange(connected: Boolean) {
                if (!connected) setStatus(getString(R.string.drop_matchmaker_lost))
            }
            override fun onError(msg: String) {
                setStatus(getString(R.string.drop_matchmaker_error, msg))
                btnFindPc.isEnabled = true
            }
        }).also {
            it.connect(store.dropSignalUrl, store.dropDeviceId, store.deviceName)
        }
    }

    private fun renderRoster(devices: List<DropMatchmaker.DropDevice>) {
        llRoster.removeAllViews()
        llRoster.visibility = View.VISIBLE
        if (devices.isEmpty()) {
            setStatus(getString(R.string.drop_no_devices))
            return
        }
        setStatus(getString(R.string.drop_tap_pc))
        for (d in devices) {
            val b = Button(this, null, 0, R.style.QuicMicButton).apply {
                text = (if (d.nearby) "📶 " else "") + d.name
                setOnClickListener { ring(d.id, d.name) }
            }
            llRoster.addView(b)
        }
    }

    private fun ring(id: String, name: String) {
        if (sending) return
        ringTo = id
        setStatus(getString(R.string.drop_ringing, name))
        llRoster.visibility = View.GONE
        peer?.close()
        val listener = peerListener()
        try {
            peer = DropPeer(this, listener).also { it.createAsCaller() }
        } catch (e: Exception) {
            setStatus(getString(R.string.drop_webrtc_failed))
            return
        }
        ringTimer?.let { ui.removeCallbacks(it) }
        ringTimer = Runnable {
            if (!sending) {
                setStatus(getString(R.string.drop_no_answer, name))
                cleanupCall()
            }
        }.also { ui.postDelayed(it, RING_TIMEOUT_MS) }
    }

    private fun onSignal(from: String, fromName: String, p: JSONObject) {
        when (p.optString("kind")) {
            "answer" -> {
                if (from == ringTo) {
                    ringTimer?.let { ui.removeCallbacks(it) }
                    peer?.setRemoteAnswer(p.optString("sdp"))
                }
            }
            "ice" -> {
                // Trickle ICE only happens in the tap-to-connect ring flow
                // (manual codes carry full-gathered SDP, no trickle).
                if (from == ringTo) {
                    val c = p.optJSONObject("candidate") ?: return
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
                if (from == ringTo) {
                    ringTimer?.let { ui.removeCallbacks(it) }
                    setStatus(getString(R.string.drop_declined, fromName))
                    cleanupCall()
                }
            }
        }
    }

    private fun cleanupCall() {
        ringTimer?.let { ui.removeCallbacks(it) }
        ringTimer = null
        ringTo = null
        peer?.close()
        peer = null
        btnFindPc.isEnabled = true
    }

    // --- manual code flow ---

    private fun joinWithCode() {
        val code = etCode.text.toString()
        if (code.isBlank() || sending) return
        val desc = DropSignalCodec.decode(code)
        if (desc == null || desc.type != SessionDescription.Type.OFFER) {
            Toast.makeText(this, getString(R.string.drop_bad_code), Toast.LENGTH_LONG).show()
            return
        }
        setStatus(getString(R.string.drop_generating_reply))
        peer?.close()
        try {
            peer = DropPeer(this, peerListener()).also { it.answerOffer(desc.description) }
        } catch (e: Exception) {
            setStatus(getString(R.string.drop_webrtc_failed))
        }
    }

    private fun showReplyCode(answer: SessionDescription) {
        val code = DropSignalCodec.encode(answer)
        val qr: Bitmap? = try {
            BarcodeEncoder().encodeBitmap(code, BarcodeFormat.QR_CODE, 512, 512)
        } catch (_: Exception) {
            null
        }
        val view = layoutInflater.inflate(R.layout.dialog_drop_reply, null)
        view.findViewById<TextView>(R.id.tv_reply_code).text = code
        val iv = view.findViewById<ImageView>(R.id.iv_reply_qr)
        if (qr != null) iv.setImageBitmap(qr) else iv.visibility = View.GONE
        view.findViewById<Button>(R.id.btn_copy_reply).setOnClickListener {
            (getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager)
                .setPrimaryClip(ClipData.newPlainText("drop-reply", code))
            Toast.makeText(this, getString(R.string.drop_copied), Toast.LENGTH_SHORT).show()
        }
        AlertDialog.Builder(this)
            .setTitle(getString(R.string.drop_reply_title))
            .setView(view)
            .setMessage(getString(R.string.drop_reply_hint))
            .setPositiveButton(android.R.string.ok, null)
            .show()
        setStatus(getString(R.string.drop_waiting_host))
    }

    // --- peer events / sending ---

    private inner class SharePeerListener : DropPeerListener {
        /** Manual-code join: hold the answer until ICE gathering completes. */
        var pendingAnswer: SessionDescription? = null
        private var gatheringDone = false

        private fun flushAnswer() {
            pendingAnswer?.let {
                pendingAnswer = null
                showReplyCode(it)
            }
        }

        override fun onLocalDescription(desc: SessionDescription) {
            if (desc.type == SessionDescription.Type.OFFER) {
                // Trickle: send the offer immediately, ICE follows.
                val to = ringTo ?: return
                matchmaker?.sendSignal(
                    to,
                    JSONObject().put("kind", "offer").put("sdp", desc.description),
                )
            } else if (ringTo != null) {
                // Answer in the tap-to-connect flow (shouldn't happen — we
                // only ever call out — but route it sanely if it does).
                matchmaker?.sendSignal(
                    ringTo!!,
                    JSONObject().put("kind", "answer").put("sdp", desc.description),
                )
            } else {
                // Manual-code join: the reply code must carry full-gathered
                // SDP (drop.js waitGathering), so wait for onGatheringComplete.
                pendingAnswer = desc
                if (gatheringDone) flushAnswer()
                // Safety net: gathering occasionally stalls — don't hang the
                // user forever (drop.js uses a 6s cap for the same reason).
                ui.postDelayed({ flushAnswer() }, 8_000)
            }
        }
        override fun onGatheringComplete() {
            gatheringDone = true
            flushAnswer()
        }
        override fun onLocalIce(candidate: IceCandidate) {
            val to = ringTo ?: return
            matchmaker?.sendSignal(
                to,
                JSONObject().put("kind", "ice").put(
                    "candidate",
                    JSONObject()
                        .put("candidate", candidate.sdp)
                        .put("sdpMid", candidate.sdpMid)
                        .put("sdpMLineIndex", candidate.sdpMLineIndex),
                ),
            )
        }
        override fun onConnected() {
            ringTimer?.let { ui.removeCallbacks(it) }
            setStatus(getString(R.string.drop_connected_sending))
            sendQueue()
        }
        override fun onDisconnected() {
            if (sending) {
                setStatus(getString(R.string.drop_send_interrupted))
                sending = false
            }
        }
        override fun onError(msg: String) {
            setStatus(msg)
            sending = false
            btnFindPc.isEnabled = true
        }
    }

    private fun peerListener() = SharePeerListener()

    private fun sendQueue() {
        if (sending || items.isEmpty()) return
        sending = true
        progress.visibility = View.VISIBLE
        sendNext(0)
    }

    private fun sendNext(i: Int) {
        val peer = this.peer
        if (peer == null) {
            sending = false
            return
        }
        if (i >= items.size) {
            sending = false
            progress.visibility = View.GONE
            setStatus(getString(R.string.drop_sent))
            ui.postDelayed({ finish() }, 1500)
            return
        }
        when (val item = items[i]) {
            is ShareItem.Text -> {
                peer.sendText(item.text)
                sendNext(i + 1)
            }
            is ShareItem.File -> {
                setStatus(getString(R.string.drop_sending_file, item.name, i + 1, items.size))
                peer.sendFile(item.name, item.mime, item.bytes) { sent, total ->
                    progress.max = 100
                    progress.progress =
                        if (total > 0) (sent * 100 / total).toInt().coerceIn(0, 100) else 0
                    if (sent >= total) sendNext(i + 1)
                }
            }
        }
    }

    // --- misc ---

    private fun setStatus(t: String) {
        ui.post { tvStatus.text = t }
    }

    private fun fmtSize(b: Long): String = when {
        b < 1024 -> "$b B"
        b < 1048576 -> "%.1f KB".format(b / 1024.0)
        b < 1073741824 -> "%.1f MB".format(b / 1048576.0)
        else -> "%.2f GB".format(b / 1073741824.0)
    }

    override fun onDestroy() {
        ringTimer?.let { ui.removeCallbacks(it) }
        peer?.close()
        matchmaker?.close()
        super.onDestroy()
    }
}
