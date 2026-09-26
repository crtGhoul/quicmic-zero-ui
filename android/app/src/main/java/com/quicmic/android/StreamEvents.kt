package com.quicmic.android

import org.json.JSONObject
import java.util.concurrent.CopyOnWriteArrayList

/**
 * Minimal service -> UI event bus. The foreground service posts state changes
 * here; activities subscribe in onResume and unsubscribe in onPause.
 * Callbacks arrive on the poster's thread — activities must hop to the main
 * thread before touching views.
 */
object StreamEvents {

    sealed class Event {
        data class Status(val text: String) : Event()
        data class Level(val level: Float) : Event() // VU meter, 0..100
        data class Stats(
            val packetsReceived: Long,
            val lossPercent: Double,
            val bufferMs: Long,
        ) : Event()

        data class Speaker(val on: Boolean) : Event()
        data class Bluetooth(val on: Boolean) : Event()
        object StreamingStopped : Event()
        /** Token is dead and re-pair failed: the user must pair again. */
        object PairingLost : Event()

        /** Server unreachable after retries. Pairing is kept. */
        object ServerGone : Event()
    }

    fun interface Listener {
        fun onEvent(e: Event)
    }

    private val listeners = CopyOnWriteArrayList<Listener>()

    fun add(l: Listener) = listeners.add(l)
    fun remove(l: Listener) = listeners.remove(l)

    fun post(e: Event) {
        for (l in listeners) {
            try {
                l.onEvent(e)
            } catch (_: Exception) {
                // A dead activity must not break the service.
            }
        }
    }

    fun statsFrom(json: JSONObject): Event.Stats = Event.Stats(
        packetsReceived = json.optLong("packets_received"),
        lossPercent = json.optDouble("loss_percent"),
        bufferMs = json.optLong("buffer_ms"),
    )
}
