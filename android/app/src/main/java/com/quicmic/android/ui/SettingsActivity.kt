package com.quicmic.android.ui

import android.content.Intent
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.SeekBar
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import com.quicmic.android.R
import com.quicmic.android.net.Api
import com.quicmic.android.net.gateLinear
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.service.QuicMicService
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread
import kotlin.math.roundToInt

/**
 * Settings: device name (sent as `device_name` on /api/pair), noise-gate
 * threshold in dB (-100 = off, matches the web UI slider), and linear gain
 * (0.2x–3.0x). Stored encrypted; applied live to a running stream and pushed
 * to the server (the client is the source of truth for these settings).
 */
class SettingsActivity : AppCompatActivity() {

    private lateinit var store: SecureStore
    private lateinit var etDeviceName: EditText
    private lateinit var sbGate: SeekBar
    private lateinit var tvGateVal: TextView
    private lateinit var sbGain: SeekBar
    private lateinit var tvGainVal: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = SecureStore(this)
        setContentView(R.layout.activity_settings)

        etDeviceName = findViewById(R.id.et_device_name)
        sbGate = findViewById(R.id.sb_gate)
        tvGateVal = findViewById(R.id.tv_gate_val)
        sbGain = findViewById(R.id.sb_gain)
        tvGainVal = findViewById(R.id.tv_gain_val)

        etDeviceName.setText(store.deviceName)

        // Gate: progress 0..100 -> dB -100..0.
        sbGate.max = 100
        sbGate.progress = (store.gateDb + 100f).roundToInt().coerceIn(0, 100)
        sbGate.setOnSeekBarChangeListener(simpleListener { renderGate() })

        // Gain: progress 0..280 -> 0.20x..3.00x.
        sbGain.max = 280
        sbGain.progress = ((store.gain * 100f).roundToInt() - 20).coerceIn(0, 280)
        sbGain.setOnSeekBarChangeListener(simpleListener { renderGain() })

        renderGate()
        renderGain()

        AudioSettingsBinder.bind(this)
        SpeakerSettingsBinder.bind(this)
        DropSettingsBinder.bind(this)
        SystemSettingsBinder.bind(this)
        RenameSettingsBinder.bind(this)

        findViewById<Button>(R.id.btn_save).setOnClickListener { save() }
    }

    private fun gateDb(): Float = sbGate.progress - 100f

    private fun gainVal(): Float = (sbGain.progress + 20) / 100f

    private fun renderGate() {
        val db = gateDb()
        tvGateVal.text = if (db <= -100f) "Off" else "$db dB"
    }

    private fun renderGain() {
        tvGainVal.text = "%.2fx".format(gainVal())
    }

    private fun save() {
        val name = etDeviceName.text.toString().trim().take(64)
        if (name.isNotEmpty()) store.deviceName = name
        store.gateDb = gateDb()
        store.gain = gainVal()

        // Apply live to a running stream…
        if (QuicMicService.isRunning) {
            startService(
                Intent(this, QuicMicService::class.java)
                    .setAction(QuicMicService.ACTION_APPLY_SETTINGS),
            )
        }
        // …and push to the server so the values survive a PC restart.
        thread(isDaemon = true) {
            val host = store.getHost()
            val certDer = store.getCertDer()
            val token = store.getToken()
            if (host != null && certDer != null && token != null) {
                try {
                    val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
                    api.pushSettings(token, gateLinear(store.gateDb), store.gain)
                } catch (_: Exception) {
                    // Best-effort; the local values are what the app uses.
                }
            }
            runOnUiThread {
                Toast.makeText(this, "Settings saved", Toast.LENGTH_SHORT).show()
                finish()
            }
        }
    }

    private fun simpleListener(onChange: () -> Unit) = object : SeekBar.OnSeekBarChangeListener {
        override fun onProgressChanged(sb: SeekBar?, progress: Int, fromUser: Boolean) = onChange()
        override fun onStartTrackingTouch(sb: SeekBar?) {}
        override fun onStopTrackingTouch(sb: SeekBar?) {}
    }
}
