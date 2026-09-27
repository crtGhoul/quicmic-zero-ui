package com.quicmic.android.net

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Base64
import org.json.JSONObject
import org.webrtc.DataChannel
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.charset.StandardCharsets
import java.util.UUID

/**
 * Native WebRTC transport for Drop file sharing.
 *
 * Protocol finding (see web/drop.js, the source of truth): the PC server has
 * NO HTTP upload endpoint for Drop — `src/server/mod.rs` only routes
 * the /api/ endpoints, /ws, /speaker-ws, /ca, /qr and static assets. drop.html moves files
 * over a WebRTC data channel labeled "localdrop" (ordered), signaled either
 * by manual SDP codes or by the external tap-to-connect matchmaker. This file
 * is a byte-faithful port of that protocol to org.webrtc so the phone can
 * send/receive Drops natively, with no WebView and no second pairing.
 *
 * Signaling codec (manual codes): base64url(UTF-8 JSON {"t":<offer|answer>,
 * "s":<sdp>}), full-gathered SDP, no trickle — identical to drop.js'
 * encodeSDP/decodeSDP/parseCode.
 *
 * Data-channel messages:
 *  - text:  {"t":"msg","text":...,"ts":...}
 *  - file:  {"t":"file-meta","id","name","size","mime"} then 16 KiB binary
 *           chunks, then {"t":"file-end","id"}.
 * ICE: STUN stun.l.google.com:19302 + stun:openrelay.metered.ca:80, TURN
 * openrelay.metered.ca:80/443 (openrelayproject/openrelayproject) — the same
 * RTC_CFG as drop.js.
 *
 * Auth note: Drop has no server-side auth at all — drop.html is an open
 * static asset and pairing codes are trust-on-first-use. The native client
 * therefore does NOT send the mic session token anywhere here (and must
 * never send it to the matchmaker, which is a third-party service). Identity
 * reuse = the mic pairing identity: device_name from SecureStore plus a
 * stable per-install drop device id stored alongside it.
 */

/** Manual-code signaling codec — exact port of drop.js encodeSDP/decodeSDP. */
object DropSignalCodec {
    /** Encode a local description as a share/reply code. */
    fun encode(desc: SessionDescription): String {
        val json = JSONObject()
            .put("t", desc.type.canonicalForm())
            .put("s", desc.description)
            .toString()
        return Base64.encodeToString(
            json.toByteArray(StandardCharsets.UTF_8),
            Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP,
        )
    }

    /** Decode a pasted code; null when it is not a well-formed offer/answer. */
    fun decode(code: String): SessionDescription? {
        return try {
            val json = JSONObject(
                String(Base64.decode(code.trim(), Base64.URL_SAFE), StandardCharsets.UTF_8),
            )
            val type = SessionDescription.Type.fromCanonicalForm(json.getString("t"))
            val sdp = json.getString("s")
            if (!sdp.startsWith("v=0")) null else SessionDescription(type, sdp)
        } catch (_: Exception) {
            null
        }
    }
}

/** Shared Drop WebRTC constants (mirror web/drop.js RTC_CFG). */
object DropRtc {
    const val DC_LABEL = "localdrop"
    const val CHUNK = 16384
    const val BUFFERED_LIMIT = 512 * 1024

    fun iceServers(): List<PeerConnection.IceServer> = listOf(
        PeerConnection.IceServer.builder("stun:stun.l.google.com:19302").createIceServer(),
        PeerConnection.IceServer.builder("stun:openrelay.metered.ca:80").createIceServer(),
        PeerConnection.IceServer.builder("turn:openrelay.metered.ca:80")
            .setUsername("openrelayproject").setPassword("openrelayproject").createIceServer(),
        PeerConnection.IceServer.builder("turn:openrelay.metered.ca:443")
            .setUsername("openrelayproject").setPassword("openrelayproject").createIceServer(),
    )

    fun rtcConfig(): PeerConnection.RTCConfiguration =
        PeerConnection.RTCConfiguration(iceServers()).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
        }
}

/** Process-wide PeerConnectionFactory (init once; data-channel only, no EGL). */
object DropPeerFactory {
    @Volatile
    private var factory: PeerConnectionFactory? = null

    fun get(context: Context): PeerConnectionFactory = synchronized(this) {
        factory ?: run {
            PeerConnectionFactory.initialize(
                PeerConnectionFactory.InitializationOptions.builder(context.applicationContext)
                    .createInitializationOptions(),
            )
            PeerConnectionFactory.builder()
                .setOptions(PeerConnectionFactory.Options())
                .createPeerConnectionFactory()
                .also { factory = it }
        }
    }
}

/** SdpObserver with no-op defaults so call sites only override what they need. */
open class SimpleSdpObserver : SdpObserver {
    override fun onCreateSuccess(desc: SessionDescription?) {}
    override fun onSetSuccess() {}
    override fun onCreateFailure(msg: String?) {}
    override fun onSetFailure(msg: String?) {}
}

/** Callbacks from a Drop peer connection. All run on the main thread. */
interface DropPeerListener {
    /** Local description ready (offer for trickle callers, answer for callees). */
    fun onLocalDescription(desc: SessionDescription) {}
    /** Trickle ICE candidate to forward over the signaling transport. */
    fun onLocalIce(candidate: IceCandidate) {}
    fun onConnected() {}
    fun onDisconnected() {}
    fun onText(text: String) {}
    fun onFileStart(name: String, size: Long, mime: String) {}
    fun onFileProgress(received: Long, total: Long) {}
    fun onFileComplete(name: String, mime: String, data: ByteArray) {}
    /** Full ICE gathering finished — manual codes must wait for this. */
    fun onGatheringComplete() {}
    fun onError(msg: String) {}
}

/**
 * One Drop peer connection. Either side creates it; the caller calls
 * [createAsCaller] (creates the data channel + offer), the callee calls
 * [answerOffer] (waits for onLocalDescription with the answer).
 *
 * Sending files runs on a worker thread with the same bufferedAmount
 * backpressure as drop.js' pumpQueue. Incoming files are assembled in
 * memory (same as drop.js' Blob-of-parts); very large files can OOM —
 * keep an eye on this for >500 MB transfers.
 */
class DropPeer(
    context: Context,
    private val listener: DropPeerListener,
) {
    private val ui = Handler(Looper.getMainLooper())
    private val factory = DropPeerFactory.get(context)
    private var pc: PeerConnection? = null
    private var dc: DataChannel? = null
    private var incoming: IncomingFile? = null
    private var closed = false

    private data class IncomingFile(
        val id: String,
        val name: String,
        val size: Long,
        val mime: String,
        val out: ByteArrayOutputStream,
    )

    private fun post(fn: () -> Unit) {
        ui.post { if (!closed) fn() }
    }

    private fun newPc() {
        close()
        closed = false
        val observer = object : PeerConnection.Observer {
            override fun onSignalingChange(state: PeerConnection.SignalingState?) {}
            override fun onIceConnectionChange(state: PeerConnection.IceConnectionState?) {
                if (state == PeerConnection.IceConnectionState.CONNECTED) post { listener.onConnected() }
                if (state == PeerConnection.IceConnectionState.DISCONNECTED ||
                    state == PeerConnection.IceConnectionState.CLOSED ||
                    state == PeerConnection.IceConnectionState.FAILED
                ) post { listener.onDisconnected() }
            }
            override fun onIceConnectionReceivingChange(receiving: Boolean) {}
            override fun onIceGatheringChange(state: PeerConnection.IceGatheringState?) {
                if (state == PeerConnection.IceGatheringState.COMPLETE) {
                    post { listener.onGatheringComplete() }
                }
            }
            override fun onIceCandidate(candidate: IceCandidate?) {
                if (candidate != null) post { listener.onLocalIce(candidate) }
            }
            override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>?) {}
            override fun onAddStream(stream: org.webrtc.MediaStream?) {}
            override fun onRemoveStream(stream: org.webrtc.MediaStream?) {}
            override fun onDataChannel(channel: DataChannel?) {
                if (channel != null) wireDataChannel(channel)
            }
            override fun onRenegotiationNeeded() {}
            override fun onAddTrack(receiver: org.webrtc.RtpReceiver?, streams: Array<out org.webrtc.MediaStream>?) {}
        }
        pc = factory.createPeerConnection(DropRtc.rtcConfig(), observer)
            ?: throw IllegalStateException("createPeerConnection failed")
    }

    private fun wireDataChannel(channel: DataChannel) {
        dc = channel
        channel.registerObserver(object : DataChannel.Observer {
            override fun onBufferedAmountChange(previousAmount: Long) {}
            override fun onStateChange() {
                if (channel.state() == DataChannel.State.OPEN) post { listener.onConnected() }
                if (channel.state() == DataChannel.State.CLOSED) post { listener.onDisconnected() }
            }
            override fun onMessage(buffer: DataChannel.Buffer) {
                val bytes = ByteArray(buffer.data.remaining())
                buffer.data.get(bytes)
                if (buffer.binary) onBinaryChunk(bytes) else onControlMessage(bytes)
            }
        })
    }

    private fun onControlMessage(bytes: ByteArray) {
        val m = try {
            JSONObject(String(bytes, StandardCharsets.UTF_8))
        } catch (_: Exception) {
            return
        }
        when (m.optString("t")) {
            "msg" -> post { listener.onText(m.optString("text")) }
            "file-meta" -> {
                val id = m.optString("id")
                val name = m.optString("name", "file")
                val size = m.optLong("size", 0)
                val mime = m.optString("mime", "application/octet-stream")
                incoming = IncomingFile(id, name, size, mime, ByteArrayOutputStream())
                post { listener.onFileStart(name, size, mime) }
            }
            "file-end" -> {
                val cur = incoming
                if (cur != null && cur.id == m.optString("id")) {
                    incoming = null
                    val data = cur.out.toByteArray()
                    post { listener.onFileComplete(cur.name, cur.mime, data) }
                }
            }
        }
    }

    private fun onBinaryChunk(bytes: ByteArray) {
        val cur = incoming ?: return
        cur.out.write(bytes)
        val got = cur.out.size().toLong()
        post { listener.onFileProgress(got, cur.size) }
    }

    /** Caller side: create the data channel and mint an offer. */
    fun createAsCaller() {
        newPc()
        val pc = this.pc ?: return
        val channel = pc.createDataChannel(
            DropRtc.DC_LABEL,
            DataChannel.Init().apply { ordered = true },
        ) ?: throw IllegalStateException("createDataChannel failed")
        wireDataChannel(channel)
        @Suppress("DEPRECATION")
        pc.createOffer(object : SimpleSdpObserver() {
            override fun onCreateSuccess(desc: SessionDescription?) {
                if (desc == null) return
                @Suppress("DEPRECATION")
                pc.setLocalDescription(object : SimpleSdpObserver() {
                    override fun onSetSuccess() = post { listener.onLocalDescription(desc) }
                    override fun onSetFailure(msg: String?) =
                        post { listener.onError("setLocalDescription failed") }
                }, desc)
            }
            override fun onCreateFailure(msg: String?) =
                post { listener.onError("createOffer failed") }
        }, MediaConstraints())
    }

    /** Callee side: answer a remote offer; the answer arrives via onLocalDescription. */
    fun answerOffer(offerSdp: String) {
        newPc()
        val pc = this.pc ?: return
        val offer = SessionDescription(SessionDescription.Type.OFFER, offerSdp)
        @Suppress("DEPRECATION")
        pc.setRemoteDescription(object : SimpleSdpObserver() {
            override fun onSetSuccess() {
                @Suppress("DEPRECATION")
                pc.createAnswer(object : SimpleSdpObserver() {
                    override fun onCreateSuccess(desc: SessionDescription?) {
                        if (desc == null) return
                        @Suppress("DEPRECATION")
                        pc.setLocalDescription(object : SimpleSdpObserver() {
                            override fun onSetSuccess() = post { listener.onLocalDescription(desc) }
                            override fun onSetFailure(msg: String?) =
                                post { listener.onError("setLocalDescription failed") }
                        }, desc)
                    }
                    override fun onCreateFailure(msg: String?) =
                        post { listener.onError("createAnswer failed") }
                }, MediaConstraints())
            }
            override fun onSetFailure(msg: String?) =
                post { listener.onError("bad offer") }
        }, offer)
    }

    fun setRemoteAnswer(answerSdp: String) {
        val pc = this.pc ?: return
        @Suppress("DEPRECATION")
        pc.setRemoteDescription(
            object : SimpleSdpObserver() {
                override fun onSetFailure(msg: String?) =
                    post { listener.onError("bad answer") }
            },
            SessionDescription(SessionDescription.Type.ANSWER, answerSdp),
        )
    }

    fun addRemoteIce(candidate: IceCandidate) {
        try {
            pc?.addIceCandidate(candidate)
        } catch (_: Exception) {
        }
    }

    fun sendText(text: String) {
        val dc = this.dc ?: return
        if (dc.state() != DataChannel.State.OPEN) return
        val payload = JSONObject()
            .put("t", "msg")
            .put("text", text)
            .put("ts", System.currentTimeMillis())
            .toString()
            .toByteArray(StandardCharsets.UTF_8)
        dc.send(DataChannel.Buffer(ByteBuffer.wrap(payload), false))
    }

    /**
     * Send a file: file-meta, 16 KiB chunks with bufferedAmount backpressure,
     * file-end. Runs on a worker thread; [progress] is called on the main
     * thread with (sentBytes, totalBytes).
     */
    fun sendFile(
        name: String,
        mime: String,
        data: ByteArray,
        progress: (sent: Long, total: Long) -> Unit,
    ) {
        Thread({
            try {
                val dc = this.dc ?: return@Thread
                if (dc.state() != DataChannel.State.OPEN) {
                    post { listener.onError("not connected") }
                    return@Thread
                }
                val id = UUID.randomUUID().toString().replace("-", "")
                val meta = JSONObject()
                    .put("t", "file-meta")
                    .put("id", id)
                    .put("name", name)
                    .put("size", data.size.toLong())
                    .put("mime", mime.ifEmpty { "application/octet-stream" })
                    .toString()
                    .toByteArray(StandardCharsets.UTF_8)
                dc.send(DataChannel.Buffer(ByteBuffer.wrap(meta), false))
                var off = 0
                while (off < data.size) {
                    while (dc.bufferedAmount() > DropRtc.BUFFERED_LIMIT) {
                        try {
                            Thread.sleep(60)
                        } catch (_: InterruptedException) {
                            return@Thread
                        }
                    }
                    val n = minOf(DropRtc.CHUNK, data.size - off)
                    dc.send(DataChannel.Buffer(ByteBuffer.wrap(data, off, n), true))
                    off += n
                    val sent = off.toLong()
                    val total = data.size.toLong()
                    post { progress(sent, total) }
                }
                val end = JSONObject()
                    .put("t", "file-end")
                    .put("id", id)
                    .toString()
                    .toByteArray(StandardCharsets.UTF_8)
                dc.send(DataChannel.Buffer(ByteBuffer.wrap(end), false))
            } catch (e: Exception) {
                post { listener.onError("send failed: ${e.message}") }
            }
        }, "drop-send").apply { isDaemon = true }.start()
    }

    fun close() {
        closed = true
        try {
            dc?.close()
        } catch (_: Exception) {
        }
        try {
            pc?.close()
        } catch (_: Exception) {
        }
        dc = null
        pc = null
        incoming = null
    }
}
