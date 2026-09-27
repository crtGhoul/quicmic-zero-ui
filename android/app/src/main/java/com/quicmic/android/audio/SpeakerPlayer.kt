package com.quicmic.android.audio

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import com.quicmic.android.net.SessionExpiredException
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.net.wsBaseUrl
import com.quicmic.android.store.SecureStore
import okio.ByteString
import java.io.IOException
import java.nio.ByteOrder
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/**
 * PC-speaker stream player.
 *
 * Connects to wss://<host>:<port>/speaker-ws?token=... ; the server pushes
 * 7680-byte binary frames (20 ms stereo f32-LE PCM @ 48 kHz, 960 samples per
 * channel — mirrors src/server/speaker.rs and web/speaker-worklet.js).
 * Frames are decoded on a handoff queue and played through an AudioTrack in
 * PCM-float stereo; a full queue drops (rather than blocking the socket) the
 * way the server skips lagging broadcast receivers.
 *
 * Unlike the mic path, any number of phones may listen at once (no slot).
 * HTTP 503 on the upgrade means speaker capture is not running on the PC;
 * 401 means the token is dead.
 *
 * Bluetooth routing: [setBluetoothSco] toggles a SCO audio connection
 * (MODE_IN_COMMUNICATION) so earbuds can be targeted. Note SCO is narrowband
 * on most devices — a Bluetooth hardware limitation, not an app bug — and the
 * SCO connection takes ~1 s to come up.
 */
class SpeakerPlayer(
    private val store: SecureStore,
    private val audioManager: AudioManager,
) {

    interface Listener {
        fun onStatus(text: String)
        fun onStopped(reason: String)
    }

    companion object {
        const val SAMPLE_RATE = 48000
        private const val FRAME_SAMPLES = 960 // per channel, 20 ms @ 48 kHz
        private const val CHANNELS = 2
        private const val FRAME_BYTES = FRAME_SAMPLES * CHANNELS * 4 // 7680
    }

    @Volatile
    var running = false
        private set

    @Volatile
    var bluetoothSco = false
        private set

    /**
     * Phone-local speaker volume (mirrors the web speaker page's spk_volume
     * slider). Persists to the store and applies live when a track is up.
     */
    fun setVolume(v: Float) {
        val clamped = v.coerceIn(0f, 1f)
        store.speakerVolume = clamped
        try {
            trackRef?.setVolume(clamped)
        } catch (_: Exception) {
            // No live track; the stored value applies at the next start.
        }
    }

    private var thread: Thread? = null
    private var listener: Listener? = null

    @Volatile
    private var trackRef: AudioTrack? = null

    @Volatile
    private var socketDead: java.util.concurrent.atomic.AtomicBoolean? = null
    private var socket: WebSocket? = null

    fun setBluetoothSco(enabled: Boolean) {
        bluetoothSco = enabled
        store.btEnabled = enabled
        if (enabled) {
            audioManager.mode = AudioManager.MODE_IN_COMMUNICATION
            audioManager.isBluetoothScoOn = true
            try {
                audioManager.startBluetoothSco()
            } catch (_: Exception) {
                // No Bluetooth audio available; playback continues on speaker.
            }
        } else {
            audioManager.isBluetoothScoOn = false
            try {
                audioManager.stopBluetoothSco()
            } catch (_: Exception) {
                // Already stopped.
            }
            audioManager.mode = AudioManager.MODE_NORMAL
        }
        listener?.onStatus(if (enabled) "Bluetooth earbuds on" else "Bluetooth earbuds off")
    }

    @Synchronized
    fun start(l: Listener) {
        if (running) return
        listener = l
        running = true
        thread = Thread(::runLoop, "quicmic-speaker").apply {
            isDaemon = true
            start()
        }
    }

    @Synchronized
    fun stop() {
        running = false
        try {
            socket?.close(1000, "stop")
        } catch (_: Exception) {
            // Already gone.
        }
        val t = thread
        thread = null
        t?.interrupt()
        // Same teardown-race guard as MicStreamer.stop(): never join ourselves.
        if (t != null && t != Thread.currentThread()) {
            try {
                t.join(3000)
            } catch (_: Exception) {
                // Timed out; the daemon thread dies on its own.
            }
        }
    }

    private fun runLoop() {
        var reason = "Stopped"
        try {
            streamLoop()
            reason = "Stopped"
        } catch (_: InterruptedException) {
            reason = "Stopped"
        } catch (e: SessionExpiredException) {
            reason = "Speaker session expired"
        } catch (e: Exception) {
            if (running) reason = e.message ?: "Speaker error"
        } finally {
            running = false
            try {
                socket?.cancel()
            } catch (_: Exception) {
                // Ignore.
            }
            socket = null
            try {
                trackRef?.stop()
            } catch (_: Exception) {
                // Ignore.
            }
            try {
                trackRef?.release()
            } catch (_: Exception) {
                // Ignore.
            }
            trackRef = null
            if (reason != "Stopped") listener?.onStopped(reason)
        }
    }

    private fun streamLoop() {
        val host = store.getHost() ?: throw SessionExpiredException("not paired")
        val certDer = store.getCertDer() ?: throw SessionExpiredException("not paired")
        val token = store.getToken() ?: throw SessionExpiredException("not paired")
        val client = pinnedClient(certDer)

        val minBuf = AudioTrack.getMinBufferSize(
            SAMPLE_RATE,
            AudioFormat.CHANNEL_OUT_STEREO,
            AudioFormat.ENCODING_PCM_FLOAT,
        )
        if (minBuf <= 0) throw IOException("float stereo playback not supported")
        val track = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build(),
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .build(),
            )
            .setBufferSizeInBytes(minBuf * 4)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
        if (track.state != AudioTrack.STATE_INITIALIZED) {
            try {
                track.release()
            } catch (_: Exception) {
                // Ignore.
            }
            throw IOException("AudioTrack not initialized")
        }
        trackRef = track

        // Handoff queue: the OkHttp reader thread must never block on
        // AudioTrack, so a slow consumer drops frames (like the server's
        // broadcast lag handling) instead of stalling the socket.
        val queue = ArrayBlockingQueue<ByteString>(16)
        val url = "${wsBaseUrl(host, store.getPort())}/speaker-ws?token=$token"
        val dead = java.util.concurrent.atomic.AtomicBoolean(false)
        // The speaker path needs a queueing listener, so it does its own open
        // with the same semantics as openWebSocket (401 -> expired, 503 ->
        // PC capture not running).
        openQueueingSocket(client, url, queue, dead)
        socketDead = dead

        if (store.btEnabled) setBluetoothSco(true)
        listener?.onStatus("Listening to PC")
        try {
            track.setVolume(store.speakerVolume)
        } catch (_: Exception) {
            // Volume is best-effort on some devices; playback continues.
        }
        track.play()

        val floats = FloatArray(FRAME_SAMPLES * CHANNELS)
        while (running && !dead.get()) {
            val frame = queue.poll(200, TimeUnit.MILLISECONDS) ?: continue
            frame.asByteBuffer().order(ByteOrder.LITTLE_ENDIAN).asFloatBuffer().get(floats)
            var off = 0
            while (off < floats.size && running) {
                val written = track.write(floats, off, floats.size - off, AudioTrack.WRITE_BLOCKING)
                if (written < 0) throw IOException("AudioTrack.write error=$written")
                off += written
            }
        }
        if (dead.get() && running) throw IOException("speaker socket closed")
    }

    /**
     * Open the speaker socket with a listener that enqueues valid frames.
     * (openWebSocket's listener only tracks liveness; the speaker path needs
     * the queue, so it does its own open here with identical semantics.)
     */
    private fun openQueueingSocket(
        client: okhttp3.OkHttpClient,
        url: String,
        queue: ArrayBlockingQueue<ByteString>,
        dead: java.util.concurrent.atomic.AtomicBoolean,
    ) {
        val latch = java.util.concurrent.CountDownLatch(1)
        val opened = java.util.concurrent.atomic.AtomicBoolean(false)
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>(null)
        val listener = object : WebSocketListener() {
            override fun onOpen(ws: WebSocket, response: Response) {
                opened.set(true)
                latch.countDown()
            }

            override fun onMessage(ws: WebSocket, bytes: ByteString) {
                // Exactly one frame size is valid; anything else is a bug or
                // garbage — drop it rather than desyncing the float stream.
                if (bytes.size == FRAME_BYTES) queue.offer(bytes)
            }

            override fun onClosing(ws: WebSocket, code: Int, reason: String) {
                dead.set(true)
            }

            override fun onClosed(ws: WebSocket, code: Int, reason: String) {
                dead.set(true)
            }

            override fun onFailure(ws: WebSocket, t: Throwable, response: Response?) {
                if (opened.get()) {
                    dead.set(true)
                } else {
                    failure.set(when (response?.code) {
                        401 -> SessionExpiredException("server rejected the token (401)")
                        503 -> IOException("speaker capture is not running on this PC (503)")
                        else -> IOException("speaker socket failed: ${t.message} (http=${response?.code})")
                    })
                    latch.countDown()
                }
            }
        }
        val ws = client.newWebSocket(okhttp3.Request.Builder().url(url).build(), listener)
        if (!latch.await(15, TimeUnit.SECONDS)) {
            try {
                ws.cancel()
            } catch (_: Exception) {
                // Ignore.
            }
            throw IOException("speaker socket open timed out")
        }
        failure.get()?.let { throw it }
        socket = ws
        socketDead = dead
    }
}
