package com.quicmic.android.ui

import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.SeekBar
import android.widget.Spinner
import android.widget.TextView
import com.quicmic.android.R
import com.quicmic.android.service.QuicMicService
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread
import kotlin.math.roundToInt
import kotlin.math.sin

/**
 * Wires the `settings_section_speaker.xml` fragment. The integrator <include>s
 * the fragment into activity_settings.xml and calls `bind(activity)` from
 * SettingsActivity.onCreate (after setContentView).
 *
 * Everything here is phone-local (mirrors web/speaker.html — the web speaker
 * page keeps volume/latency/sink in localStorage and plays the test tone on
 * the phone with no network): nothing is POSTed to the server.
 */
object SpeakerSettingsBinder {

    fun bind(activity: SettingsActivity) {
        val store = SecureStore(activity)

        val tvVol = activity.findViewById<TextView>(R.id.sp_tv_volume)
        val sbVol = activity.findViewById<SeekBar>(R.id.sp_sb_volume)
        val spLatency = activity.findViewById<Spinner>(R.id.sp_sp_latency)
        val spSink = activity.findViewById<Spinner>(R.id.sp_sp_sink)
        val btnTone = activity.findViewById<Button>(R.id.sp_btn_tone)

        // --- Speaker volume: progress 0..100 (web default 80). Saved on
        // change; applied live to a running speaker stream via the service's
        // existing ACTION_APPLY_SETTINGS path. ---
        sbVol.max = 100
        sbVol.progress = (store.speakerVolume * 100f).roundToInt().coerceIn(0, 100)
        fun renderVol() {
            tvVol.text = "${sbVol.progress}%"
        }
        renderVol()
        sbVol.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(sb: SeekBar?, progress: Int, fromUser: Boolean) =
                renderVol()

            override fun onStartTrackingTouch(sb: SeekBar?) {}
            override fun onStopTrackingTouch(sb: SeekBar?) {
                store.speakerVolume = sbVol.progress / 100f
                if (QuicMicService.isRunning) {
                    activity.startService(
                        Intent(activity, QuicMicService::class.java)
                            .setAction(QuicMicService.ACTION_APPLY_SETTINGS),
                    )
                }
            }
        })

        // --- Latency preset: persisted; applied on the next speaker start
        // (integrator hook: SpeakerPlayer.streamLoop pre-buffers 2 / 5 / 12
        // frames for low / balanced / smooth, matching web/speaker.js). ---
        val latValues = activity.resources.getStringArray(R.array.sp_latency_values)
        val latEntries = activity.resources.getStringArray(R.array.sp_latency_entries)
        spLatency.adapter =
            ArrayAdapter(activity, android.R.layout.simple_spinner_dropdown_item, latEntries.toList())
        spLatency.setSelection(latValues.indexOf(store.speakerLatency).takeIf { it >= 0 } ?: 1, false)
        spLatency.onItemSelectedListener = selectingListener { pos ->
            store.speakerLatency = latValues.getOrElse(pos) { "balanced" }
        }

        // --- Output device picker: enumerate this phone's outputs; the phone
        // CAN enumerate sinks natively, so no server list is needed. Persisted;
        // applied on the next speaker start (integrator hook:
        // AudioTrack.setPreferredDevice in SpeakerPlayer.streamLoop). ---
        val outputs = outputDevices(activity)
        val names = mutableListOf(activity.getString(R.string.sp_sink_default))
        val ids = mutableListOf(-1)
        for (d in outputs) {
            names.add(d.productName?.toString()?.takeIf { it.isNotBlank() } ?: "Output ${d.id}")
            ids.add(d.id)
        }
        spSink.adapter =
            ArrayAdapter(activity, android.R.layout.simple_spinner_dropdown_item, names)
        spSink.setSelection(ids.indexOf(store.speakerSink).takeIf { it >= 0 } ?: 0, false)
        spSink.onItemSelectedListener = selectingListener { pos ->
            store.speakerSink = ids.getOrElse(pos) { -1 }
        }

        // --- Local test tone (mirrors the web page: 880 Hz on this phone
        // only, no network). No server endpoint exists for this, and none is
        // needed — the point is proving the phone's routing before streaming. ---
        btnTone.setOnClickListener {
            if (TestTone.playing) {
                TestTone.stop()
                btnTone.text = activity.getString(R.string.sp_test_tone)
            } else {
                if (TestTone.play(activity)) {
                    btnTone.text = activity.getString(R.string.sp_test_tone_stop)
                    thread(isDaemon = true) {
                        while (TestTone.playing) Thread.sleep(200)
                        activity.runOnUiThread {
                            btnTone.text = activity.getString(R.string.sp_test_tone)
                        }
                    }
                }
            }
        }
    }

    private fun outputDevices(activity: SettingsActivity): List<AudioDeviceInfo> {
        return try {
            val am = activity.getSystemService(AudioManager::class.java) ?: return emptyList()
            am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
                .filter { it.isSink }
                .sortedBy { it.id }
        } catch (_: Exception) {
            emptyList()
        }
    }

    private fun selectingListener(onSelect: (Int) -> Unit) =
        object : android.widget.AdapterView.OnItemSelectedListener {
            override fun onItemSelected(
                parent: android.widget.AdapterView<*>?, view: android.view.View?,
                position: Int, id: Long,
            ) = onSelect(position)

            override fun onNothingSelected(parent: android.widget.AdapterView<*>?) {}
        }

    /**
     * 880 Hz sine test tone on this phone only (mirrors the web speaker
     * page's local tone). Loops until stopped with a second tap.
     */
    private object TestTone {
        private const val SAMPLE_RATE = 48000
        private const val FREQ = 880.0
        private const val MAX_SECONDS = 5

        @Volatile
        var playing = false
            private set

        private var track: AudioTrack? = null

        @Synchronized
        fun play(activity: SettingsActivity): Boolean {
            if (playing) return false
            val frames = SAMPLE_RATE * MAX_SECONDS
            val samples = FloatArray(frames) { i ->
                (0.4f * sin(2 * Math.PI * FREQ * i / SAMPLE_RATE)).toFloat()
            }
            val t = try {
                AudioTrack.Builder()
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
                            .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                            .build(),
                    )
                    .setBufferSizeInBytes(frames * 4)
                    .setTransferMode(AudioTrack.MODE_STATIC)
                    .build()
            } catch (_: Exception) {
                return false
            }
            if (t.state != AudioTrack.STATE_INITIALIZED) {
                try {
                    t.release()
                } catch (_: Exception) {
                    // Ignore.
                }
                return false
            }
            t.write(samples, 0, frames, AudioTrack.WRITE_BLOCKING)
            t.setLoopPoints(0, frames, -1)
            t.play()
            track = t
            playing = true
            return true
        }

        @Synchronized
        fun stop() {
            playing = false
            val t = track
            track = null
            thread(isDaemon = true) {
                try {
                    t?.stop()
                } catch (_: Exception) {
                    // Ignore.
                }
                try {
                    t?.release()
                } catch (_: Exception) {
                    // Ignore.
                }
            }
        }
    }
}
