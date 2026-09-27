package com.quicmic.android.ui

import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.CompoundButton
import android.widget.SeekBar
import android.widget.Spinner
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import com.quicmic.android.R
import com.quicmic.android.net.Api
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread
import kotlin.math.roundToInt

/**
 * Wires the `settings_section_audio.xml` fragment. The integrator <include>s
 * the fragment into activity_settings.xml and calls `bind(activity)` from
 * SettingsActivity.onCreate (after setContentView).
 *
 * Every control loads from [SecureStore] and saves on change. Server-backed
 * knobs are best-effort POSTed to the PC off the main thread; the server is
 * authoritative where the web UI says so (monitor).
 */
object AudioSettingsBinder {

    fun bind(activity: SettingsActivity) {
        val store = SecureStore(activity)

        val tvVol = activity.findViewById<TextView>(R.id.au_tv_output_volume)
        val sbVol = activity.findViewById<SeekBar>(R.id.au_sb_output_volume)
        val swEcho = activity.findViewById<Switch>(R.id.au_sw_echo_cancel)
        val swMonitor = activity.findViewById<Switch>(R.id.au_sw_monitor)
        val tvLatency = activity.findViewById<TextView>(R.id.au_tv_latency)
        val sbLatency = activity.findViewById<SeekBar>(R.id.au_sb_latency)
        val spMic = activity.findViewById<Spinner>(R.id.au_sp_mic_input)
        val swPower = activity.findViewById<Switch>(R.id.au_sw_power_save)

        // --- Output volume: progress 0..50 -> 0.0..5.0 (mirrors web slider) ---
        sbVol.max = 50
        sbVol.progress = (store.outputVolume * 10f).roundToInt().coerceIn(0, 50)
        fun renderVol() {
            tvVol.text = "%.1fx".format(sbVol.progress / 10f)
        }
        renderVol()
        sbVol.setOnSeekBarChangeListener(simpleListener({ renderVol() }, onStop = {
            val v = sbVol.progress / 10f
            store.outputVolume = v
            postSettings(activity, store, mapOf("output_volume" to v.toDouble()))
        }))
        activity.findViewById<Button>(R.id.au_btn_output_volume_reset).setOnClickListener {
            sbVol.progress = 10 // 1.0x
            renderVol()
            store.outputVolume = 1.0f
            postSettings(activity, store, mapOf("output_volume" to 1.0))
        }

        // --- Echo cancellation: phone-local capture pref. No server key
        // exists (the web client keeps it in localStorage too), so this also
        // best-effort POSTs {"echo_cancel": …} for a future server endpoint.
        // Applied to the capture graph by MicStreamer on the next stream start
        // (integrator hook: openAudio() reads store.echoCancel). ---
        swEcho.isChecked = store.echoCancel
        swEcho.setOnCheckedChangeListener { _, checked ->
            store.echoCancel = checked
            postSettings(activity, store, mapOf("echo_cancel" to checked))
        }

        // --- Monitor (hear yourself): POST /api/monitor; server is
        // authoritative, so the switch reverts on failure (mirrors web). ---
        // lateinit because the listener references itself for silent reverts.
        lateinit var monitorListener: CompoundButton.OnCheckedChangeListener
        monitorListener = CompoundButton.OnCheckedChangeListener { _, checked ->
            if (store.powerSave) {
                // Power-save forces the monitor off — revert the user's tap.
                setCheckedSilent(swMonitor, false, monitorListener)
                return@OnCheckedChangeListener
            }
            setMonitor(activity, store, swMonitor, monitorListener, checked)
        }
        setCheckedSilent(swMonitor, store.monitorEnabled, monitorListener)
        applyMonitorAvailability(activity, store, swMonitor, monitorListener)

        // --- Latency recovery: progress 0..50 -> 0..500 ms, step 10 ---
        sbLatency.max = 50
        sbLatency.progress = (store.latencyThreshold / 10).coerceIn(0, 50)
        fun renderLatency() {
            val ms = sbLatency.progress * 10
            tvLatency.text = if (ms == 0) activity.getString(R.string.au_latency_off) else "$ms ms"
        }
        renderLatency()
        sbLatency.setOnSeekBarChangeListener(simpleListener({ renderLatency() }, onStop = {
            val ms = sbLatency.progress * 10
            store.latencyThreshold = ms
            postSettings(activity, store, mapOf("latency_threshold" to ms))
        }))
        activity.findViewById<Button>(R.id.au_btn_latency_reset).setOnClickListener {
            sbLatency.progress = 15 // 150 ms
            renderLatency()
            store.latencyThreshold = 150
            postSettings(activity, store, mapOf("latency_threshold" to 150))
        }

        // --- Mic input picker: enumerate Android inputs; -1 = default.
        // Applied on the next stream start (integrator hook: MicStreamer). ---
        val inputs = inputDevices(activity)
        val names = mutableListOf(activity.getString(R.string.au_mic_input_default))
        val ids = mutableListOf(-1)
        for (d in inputs) {
            names.add(d.productName?.toString()?.takeIf { it.isNotBlank() } ?: "Mic ${d.id}")
            ids.add(d.id)
        }
        spMic.adapter = ArrayAdapter(activity, android.R.layout.simple_spinner_dropdown_item, names)
        val savedIdx = ids.indexOf(store.micInputDevice).takeIf { it >= 0 } ?: 0
        spMic.setSelection(savedIdx, false)
        spMic.onItemSelectedListener = selectingListener { pos ->
            store.micInputDevice = ids.getOrElse(pos) { -1 }
        }

        // --- Power-save: forces the monitor off and asks the main screen to
        // throttle its VU-meter refresh (integrator hook: MainActivity reads
        // store.powerSave in the Level event handler). ---
        swPower.isChecked = store.powerSave
        swPower.setOnCheckedChangeListener { _, checked ->
            store.powerSave = checked
            if (checked) {
                setCheckedSilent(swMonitor, false, monitorListener)
                setMonitor(activity, store, swMonitor, monitorListener, false)
            } else {
                applyMonitorAvailability(activity, store, swMonitor, monitorListener)
            }
        }
    }

    /** Set a switch without firing its change listener. */
    private fun setCheckedSilent(
        sw: Switch,
        checked: Boolean,
        listener: CompoundButton.OnCheckedChangeListener?,
    ) {
        sw.setOnCheckedChangeListener(null)
        sw.isChecked = checked
        sw.setOnCheckedChangeListener(listener)
    }

    // --- Monitor ---

    /** Learn monitor_available / monitor_enabled from the open GET /api/settings. */
    private fun applyMonitorAvailability(
        activity: SettingsActivity,
        store: SecureStore,
        sw: Switch,
        listener: CompoundButton.OnCheckedChangeListener?,
    ) {
        thread(isDaemon = true) {
            val (available, enabled) = monitorState(store) ?: (true to store.monitorEnabled)
            activity.runOnUiThread {
                sw.isEnabled = available && !store.powerSave
                setCheckedSilent(sw, enabled && !store.powerSave, listener)
                if (!available && !store.powerSave) {
                    Toast.makeText(
                        activity, R.string.au_monitor_unavailable, Toast.LENGTH_LONG,
                    ).show()
                }
            }
        }
    }

    /** Returns (available, enabled) or null when the server is unreachable. */
    private fun monitorState(store: SecureStore): Pair<Boolean, Boolean>? {
        val host = store.getHost() ?: return null
        val certDer = store.getCertDer() ?: return null
        return try {
            val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
            val j = api.getSettingsJson() ?: return null
            j.optBoolean("monitor_available", false) to j.optBoolean("monitor_enabled", false)
        } catch (_: Exception) {
            null
        }
    }

    /** POST /api/monitor; the server's reply wins — revert the switch on failure. */
    private fun setMonitor(
        activity: SettingsActivity,
        store: SecureStore,
        sw: Switch,
        listener: CompoundButton.OnCheckedChangeListener?,
        enabled: Boolean,
    ) {
        thread(isDaemon = true) {
            val result = postMonitor(store, enabled)
            activity.runOnUiThread {
                if (result == null) {
                    setCheckedSilent(sw, !enabled, listener) // revert
                    Toast.makeText(activity, R.string.au_monitor_failed, Toast.LENGTH_SHORT).show()
                } else {
                    store.monitorEnabled = result.enabled
                    setCheckedSilent(sw, result.enabled, listener)
                    if (!result.available) {
                        Toast.makeText(
                            activity, R.string.au_monitor_unavailable, Toast.LENGTH_LONG,
                        ).show()
                    }
                }
            }
        }
    }

    private fun postMonitor(store: SecureStore, enabled: Boolean): Api.MonitorResult? {
        val host = store.getHost() ?: return null
        val certDer = store.getCertDer() ?: return null
        val token = store.getToken() ?: return null
        return try {
            Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort())).setMonitor(token, enabled)
        } catch (_: Exception) {
            null
        }
    }

    // --- Generic settings POST ---

    /** Best-effort POST /api/settings with token-in-body (the server's rule). */
    private fun postSettings(activity: SettingsActivity, store: SecureStore, values: Map<String, Any>) {
        thread(isDaemon = true) {
            val host = store.getHost() ?: return@thread
            val certDer = store.getCertDer() ?: return@thread
            val token = store.getToken() ?: return@thread
            try {
                Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
                    .pushSettingsExtended(token, values)
            } catch (_: Exception) {
                // Best-effort; the local store is what the app uses.
            }
        }
    }

    // --- Input enumeration ---

    private fun inputDevices(activity: SettingsActivity): List<AudioDeviceInfo> {
        return try {
            val am = activity.getSystemService(AudioManager::class.java) ?: return emptyList()
            am.getDevices(AudioManager.GET_DEVICES_INPUTS)
                .filter { it.type == AudioDeviceInfo.TYPE_BUILTIN_MIC || it.isSource }
                .sortedBy { it.id }
        } catch (_: Exception) {
            emptyList()
        }
    }

    // --- Small listener helpers ---

    private fun simpleListener(
        onChange: () -> Unit,
        onStop: () -> Unit = {},
    ) = object : SeekBar.OnSeekBarChangeListener {
        override fun onProgressChanged(sb: SeekBar?, progress: Int, fromUser: Boolean) = onChange()
        override fun onStartTrackingTouch(sb: SeekBar?) {}
        override fun onStopTrackingTouch(sb: SeekBar?) = onStop()
    }

    private fun selectingListener(onSelect: (Int) -> Unit) =
        object : android.widget.AdapterView.OnItemSelectedListener {
            override fun onItemSelected(
                parent: android.widget.AdapterView<*>?, view: android.view.View?,
                position: Int, id: Long,
            ) = onSelect(position)

            override fun onNothingSelected(parent: android.widget.AdapterView<*>?) {}
        }
}
