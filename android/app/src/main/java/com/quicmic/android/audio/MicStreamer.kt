package com.quicmic.android.audio

import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import android.os.SystemClock
import com.quicmic.android.net.Api
import com.quicmic.android.net.ManagedSocket
import com.quicmic.android.net.SessionExpiredException
import com.quicmic.android.net.gateLinear
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.openWebSocket
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.net.repairSession
import com.quicmic.android.net.wsBaseUrl
import com.quicmic.android.store.SecureStore
import okio.ByteString
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.pow
import kotlin.math.sqrt

/**
 * Microphone capture -> WebSocket sender.
 *
 * Capture: AudioRecord @ 48 kHz mono PCM16, source VOICE_COMMUNICATION, with
 * AcousticEchoCanceler + NoiseSuppressor enabled when the device offers them.
 *
 * DSP: the noise gate and gain run here, mirroring web/worklet.js exactly —
 * per-sample-squared threshold, 250 ms hold, onset look-ahead (the last gated
 * packet is prepended on a closed->open edge so a word's soft attack is not
 * clipped), gain applied in the float domain before Int16 conversion, and the
 * sequence number incremented ONLY for sent packets so the server's
 * LossTracker sees a contiguous stream. Gated-out silence sends nothing, so
 * the radio idles (same battery win as the web client).
 *
 * Wire: wss://<host>:<port>/ws?token=...&sr=48000, binary frames of 964 bytes
 * (4-byte u32 LE sequence + 480 x Int16 LE). A 401 on the upgrade means the
 * token is dead -> renew via /api/renew, else re-pair in place with the stored
 * PIN. A mid-stream death retries with backoff (0/1/2/3/4 s, 5 attempts);
 * intent is decided by the service's HTTP stats probe, never by close codes.
 *
 * Runs on its own thread; all DSP state is confined to it.
 */
class MicStreamer(private val store: SecureStore) {

    interface Listener {
        fun onLevel(level: Float) // VU meter, 0..100
        fun onStatus(text: String)
        fun onStopped()
        /** Token is dead and renew + re-pair both failed: the user must pair again. */
        fun onPairingLost()

        /** Server unreachable after retries. Pairing is kept. */
        fun onServerGone()
    }

    companion object {
        const val SAMPLE_RATE = 48000
        const val SAMPLES_PER_PACKET = 480
        private const val PACKET_BYTES = 4 + SAMPLES_PER_PACKET * 2 // 964
        private const val HOLD_NS = 250_000_000L // gate hold, mirrors worklet.js
        private val BACKOFF_MS = longArrayOf(0L, 1000L, 2000L, 3000L, 4000L)
    }

    @Volatile
    var muted = false

    @Volatile
    private var running = false
    private var thread: Thread? = null
    private var listener: Listener? = null

    // Audio + socket handles, set on the worker thread, torn down in stop().
    @Volatile
    private var recordRef: AudioRecord? = null
    private var aecRef: AcousticEchoCanceler? = null
    private var nsRef: NoiseSuppressor? = null

    @Volatile
    private var socketRef: ManagedSocket? = null

    // DSP state (worker thread only, except the two volatile setters).
    @Volatile
    private var gain = 1.0f
    private var perSampleSq = 0.0
    private var lastActiveNs = Long.MIN_VALUE / 4
    private var wasOpen = false
    private var prevFrame: ByteArray? = null
    private var seq = 0 // wraps as u32 on the wire via putInt

    @Volatile
    private var lastLevel = 0f

    /** dB slider value (-100 = off) -> per-sample-squared threshold. */
    fun setGateDb(db: Float) {
        perSampleSq = if (db <= -100f) {
            0.0
        } else {
            val linear = 10.0.pow(db / 20.0)
            (linear * 32768.0).pow(2.0)
        }
    }

    fun setGain(g: Float) {
        gain = g.coerceIn(0.2f, 3.0f)
    }

    @Synchronized
    fun start(l: Listener) {
        if (running) return
        listener = l
        running = true
        setGateDb(store.gateDb)
        setGain(store.gain)
        // Fresh gate state per stream (mirrors the worklet's mute-reset rule:
        // never leak a stale onset packet or hold window across a boundary).
        wasOpen = false
        prevFrame = null
        lastActiveNs = Long.MIN_VALUE / 4
        thread = Thread(::runLoop, "quicmic-mic").apply {
            isDaemon = true
            start()
        }
    }

    @Synchronized
    fun stop() {
        running = false
        // Unblock a blocking AudioRecord.read() so the thread can exit.
        try {
            recordRef?.stop()
        } catch (_: Exception) {
            // Already stopped.
        }
        socketRef?.close()
        val t = thread
        thread = null
        t?.interrupt()
        // Wait for the worker to finish teardown so a later start() cannot
        // race its releaseAudio()/socket-close against the new session.
        // Never join ourselves (stop() is also called from the worker thread
        // via onPairingLost/onServerGone).
        if (t != null && t != Thread.currentThread()) {
            try {
                t.join(3000)
            } catch (_: Exception) {
                // Timed out; the daemon thread dies on its own.
            }
        }
    }

    /** Drop the current socket so the worker reconnects immediately. */
    fun poke() {
        socketRef?.close()
    }

    private fun status(text: String) {
        listener?.onStatus(text)
    }

    private fun runLoop() {
        var attempt = 0
        var cleanStop = false
        while (running) {
            try {
                streamSession()
                cleanStop = true
                break // streamSession only returns when stop() was called
            } catch (_: InterruptedException) {
                break
            } catch (e: SessionExpiredException) {
                status("Session expired — repairing…")
                if (!repairNow()) {
                    listener?.onPairingLost()
                    break
                }
                attempt = 0
            } catch (e: Exception) {
                if (!running) break
                if (attempt >= BACKOFF_MS.size - 1) {
                    listener?.onServerGone()
                    break
                }
                val waitMs = BACKOFF_MS[attempt++]
                status("Connection lost — retrying…")
                try {
                    Thread.sleep(waitMs)
                } catch (_: InterruptedException) {
                    break
                }
            } finally {
                releaseAudio()
                socketRef?.close()
                socketRef = null
            }
        }
        running = false
        if (cleanStop) listener?.onStopped()
    }

    /** Renew with the stored token, else re-pair in place with the stored PIN. */
    private fun repairNow(): Boolean {
        val host = store.getHost() ?: return false
        val certDer = store.getCertDer() ?: return false
        val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
        return api.repairSession(store)
    }

    private fun streamSession() {
        val host = store.getHost() ?: throw SessionExpiredException("not paired")
        val certDer = store.getCertDer() ?: throw SessionExpiredException("not paired")
        val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))

        // A fresh token kicks any stale session server-side (the renew
        // broadcasts on the server's cancel channel), so the single-connection
        // slot is ours before the socket opens.
        val token = renewToken(api)
        // The client is the source of truth for settings (mirrors web/app.js):
        // push them on every (re)connect so a restarted server cannot clobber
        // the user's choices.
        try {
            api.pushSettings(token, gateLinear(store.gateDb), store.gain)
        } catch (_: Exception) {
            // Best-effort; the stream is what matters.
        }

        openAudio()
        val url = "${wsBaseUrl(host, store.getPort())}/ws?token=$token&sr=$SAMPLE_RATE"
        status("Connecting…")
        socketRef = openWebSocket(pinnedClient(certDer), url)
        status("Streaming")
        pumpLoop()
    }

    private fun renewToken(api: Api): String {
        val old = store.getToken() ?: throw SessionExpiredException("no token")
        val res = try {
            api.renew(old)
        } catch (e: IOException) {
            throw IOException("renew failed: ${e.message}", e)
        }
        if (!res.success || res.token.isNullOrEmpty()) {
            throw SessionExpiredException("renew rejected")
        }
        store.setToken(res.token)
        return res.token
    }

    private fun openAudio() {
        val minBuf = AudioRecord.getMinBufferSize(
            SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        if (minBuf <= 0) throw IOException("48 kHz mono PCM16 capture not supported")
        val record = try {
            AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(SAMPLE_RATE)
                        .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                        .build(),
                )
                .setBufferSizeInBytes(minBuf * 4)
                .build()
        } catch (e: Exception) {
            throw IOException("AudioRecord init failed: ${e.message}", e)
        }
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            try {
                record.release()
            } catch (_: Exception) {
                // Ignore.
            }
            throw IOException("AudioRecord not initialized")
        }
        // Hardware echo cancellation / noise suppression when offered.
        if (AcousticEchoCanceler.isAvailable()) {
            aecRef = AcousticEchoCanceler.create(record.audioSessionId)?.apply {
                try {
                    enabled = true
                } catch (_: Exception) {
                    // Effect rejected the enable; capture continues without it.
                }
            }
        }
        if (NoiseSuppressor.isAvailable()) {
            nsRef = NoiseSuppressor.create(record.audioSessionId)?.apply {
                try {
                    enabled = true
                } catch (_: Exception) {
                    // Effect rejected the enable; capture continues without it.
                }
            }
        }
        recordRef = record
        record.startRecording()
        if (record.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
            throw IOException("AudioRecord did not start recording")
        }
    }

    private fun releaseAudio() {
        try {
            recordRef?.stop()
        } catch (_: Exception) {
            // Already stopped.
        }
        try {
            recordRef?.release()
        } catch (_: Exception) {
            // Ignore.
        }
        recordRef = null
        try {
            aecRef?.release()
        } catch (_: Exception) {
            // Ignore.
        }
        aecRef = null
        try {
            nsRef?.release()
        } catch (_: Exception) {
            // Ignore.
        }
        nsRef = null
    }

    private fun pumpLoop() {
        val record = recordRef ?: throw IllegalStateException("audio not open")
        val managed = socketRef ?: throw IllegalStateException("socket not open")
        val ws = managed.socket
        val acc = ShortArray(SAMPLES_PER_PACKET)
        var accPos = 0
        val chunk = ShortArray(2048)
        var lastLevelPostNs = 0L

        while (running && !managed.dead.get()) {
            val n = try {
                record.read(chunk, 0, chunk.size) // blocking
            } catch (e: IllegalStateException) {
                if (!running) return else throw IOException("audio read failed", e)
            }
            if (n < 0) {
                if (!running) return
                throw IOException("AudioRecord.read error=$n")
            }
            if (n == 0) continue
            if (muted) {
                // Drain the mic so it does not overrun, but send nothing. The
                // throttled zero level keeps the UI honest ("muted", not dead).
                val now = SystemClock.elapsedRealtimeNanos()
                if (now - lastLevelPostNs > 200_000_000L) {
                    lastLevelPostNs = now
                    listener?.onLevel(0f)
                }
                continue
            }
            var off = 0
            while (off < n) {
                val take = minOf(SAMPLES_PER_PACKET - accPos, n - off)
                System.arraycopy(chunk, off, acc, accPos, take)
                accPos += take
                off += take
                if (accPos == SAMPLES_PER_PACKET) {
                    accPos = 0
                    for (frame in processPacket(acc)) {
                        if (!ws.send(ByteString.of(*frame))) {
                            throw IOException("websocket send failed")
                        }
                    }
                    val now = SystemClock.elapsedRealtimeNanos()
                    if (now - lastLevelPostNs > 100_000_000L) {
                        lastLevelPostNs = now
                        listener?.onLevel(lastLevel)
                    }
                }
            }
        }
        if (managed.dead.get() && running) throw IOException("websocket closed")
    }

    /**
     * One 480-sample packet through the gate. Returns the wire frames to send:
     * 0 when gated, 1 when open, 2 on a closed->open edge (the retained onset
     * packet first). Mirrors web/worklet.js.
     */
    private fun processPacket(pcm: ShortArray): List<ByteArray> {
        // Gate and VU decide on the raw pre-gain energy (server-side semantics).
        var sumSq = 0.0
        for (s in pcm) {
            val v = s.toDouble()
            sumSq += v * v
        }
        val rms = sqrt(sumSq / SAMPLES_PER_PACKET) / 32768.0
        lastLevel = (rms * 300.0).coerceIn(0.0, 100.0).toFloat()

        val gateOff = perSampleSq <= 0.0
        val now = SystemClock.elapsedRealtimeNanos()
        if (gateOff || sumSq >= perSampleSq * SAMPLES_PER_PACKET) {
            lastActiveNs = now
        }
        val open = gateOff || (now - lastActiveNs) < HOLD_NS

        // Gain in the float domain (no double quantization), clamped to [-1, 1].
        val frame = ByteBuffer.allocate(PACKET_BYTES).order(ByteOrder.LITTLE_ENDIAN)
        frame.putInt(seq++) // u32 LE; Int wrap is exactly the u32 wrap
        for (s in pcm) {
            val g = (s / 32768f * gain).coerceIn(-1f, 1f)
            frame.putShort(
                if (g < 0) (g * 0x8000).toInt().toShort()
                else (g * 0x7FFF).toInt().toShort(),
            )
        }

        return if (open) {
            val out = ArrayList<ByteArray>(2)
            if (!wasOpen) {
                // Onset look-ahead: prepend the last gated packet so a word's
                // soft attack is not clipped. It takes the next sequence
                // number, keeping the stream contiguous.
                prevFrame?.let { out.add(it) }
                prevFrame = null
            }
            out.add(frame.array())
            wasOpen = true
            out
        } else {
            // Gated: retain this packet as the potential onset for the next
            // open edge.
            prevFrame = frame.array()
            wasOpen = false
            emptyList()
        }
    }
}
