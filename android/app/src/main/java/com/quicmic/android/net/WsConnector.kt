package com.quicmic.android.net

import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * A WebSocket whose open handshake has completed, plus a liveness flag.
 *
 * [dead] is set when the socket closes or fails mid-stream. It is NOT set for
 * the initial open failure — that throws from [openWebSocket] instead, so the
 * caller can tell "rejected" apart from "died later".
 */
class ManagedSocket(val socket: WebSocket, val dead: AtomicBoolean) {
    fun close() {
        try {
            socket.close(1000, "close")
        } catch (_: Exception) {
            // Already gone.
        }
    }
}

/**
 * Open a WebSocket and block (up to 15 s) until the upgrade completes.
 *
 * HTTP status mapping (mirrors src/server/websocket.rs + speaker.rs):
 *  - 401: bad/missing token -> [SessionExpiredException] (renew or re-pair).
 *  - 409: another client holds the single-connection slot. Per the protocol a
 *    new connection kicks the old one server-side (via /api/renew's cancel
 *    broadcast), so callers renew before connecting and treat a lingering 409
 *    as transient.
 *  - 503: the speaker endpoint reports this when PC capture is not running.
 *
 * NOTE: there is no 4096 anywhere in the protocol — the close code is
 * deliberately never inspected (see the repo's AGENTS.md: iOS reports junk
 * codes). Mid-stream death is observed via [ManagedSocket.dead], and intent
 * (server gone vs transient) is decided with an HTTP probe, never the code.
 */
fun openWebSocket(client: OkHttpClient, url: String): ManagedSocket {
    val latch = CountDownLatch(1)
    val dead = AtomicBoolean(false)
    val opened = AtomicBoolean(false)
    @Volatile
    var failure: Throwable? = null

    val listener = object : WebSocketListener() {
        override fun onOpen(ws: WebSocket, response: Response) {
            opened.set(true)
            latch.countDown()
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
                failure = when (response?.code) {
                    401 -> SessionExpiredException("server rejected the token (401)")
                    409 -> IOException("another client is already connected (409)")
                    503 -> IOException("service unavailable (503)")
                    else -> IOException("websocket failed: ${t.message} (http=${response?.code})")
                }
                latch.countDown()
            }
        }
    }

    val ws = client.newWebSocket(Request.Builder().url(url).build(), listener)
    if (!latch.await(15, TimeUnit.SECONDS)) {
        try {
            ws.cancel()
        } catch (_: Exception) {
            // Ignore.
        }
        throw IOException("websocket open timed out: $url")
    }
    failure?.let { throw it }
    return ManagedSocket(ws, dead)
}
