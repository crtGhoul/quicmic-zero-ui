package com.quicmic.android.ui

import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import com.quicmic.android.R
import com.quicmic.android.net.Api
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread

/**
 * Mic-rename settings (feature #6): phone-side mirror of the PC GUI's
 * mic-rename controls in src/gui/panels.rs — mode selector (Auto / Off /
 * Fixed name), fixed-name text field, and a "Rename now" button.
 *
 * - The mode + fixed name are persisted in SecureStore and pushed to the PC
 *   server with the same best-effort pattern as gate/gain (Api.pushRename,
 *   POST /api/settings with the token in the body).
 * - "Rename now" sends the name in the fixed-name field as `mic_rename_now`.
 *
 * "NEEDS SERVER ENDPOINT": the PC server (src/server/api.rs, v0.5.0) has NO
 * phone-facing rename API. The PC GUI renames locally via
 * crate::mic_name::rename_mic() (Windows registry, not an HTTP API), and
 * POST /api/settings only accepts noise_gate / gain / latency_threshold /
 * output_volume — serde ignores the extra rename keys, so the push is a
 * no-op today. Until a server endpoint exists that applies them
 * (state.mic_rename_mode + rename_mic on the PC), only the local SecureStore
 * persistence works. The layout's rename_note string says this in the UI.
 */
object RenameSettingsBinder {

    private const val MODE_AUTO = "auto"
    private const val MODE_OFF = "off"
    private const val MODE_FIXED = "fixed"

    fun bind(activity: AppCompatActivity) {
        val store = SecureStore(activity)

        val rgMode = activity.findViewById<RadioGroup>(R.id.rg_rename_mode)
        val rbAuto = activity.findViewById<RadioButton>(R.id.rb_rename_auto)
        val rbOff = activity.findViewById<RadioButton>(R.id.rb_rename_off)
        val rbFixed = activity.findViewById<RadioButton>(R.id.rb_rename_fixed)
        val etFixedName = activity.findViewById<EditText>(R.id.et_rename_fixed_name)
        val tvFixedHint = activity.findViewById<TextView>(R.id.tv_rename_fixed_hint)
        val btnRenameNow = activity.findViewById<Button>(R.id.btn_rename_now)

        fun updateFixedVisibility() {
            val fixed = rgMode.checkedRadioButtonId == R.id.rb_rename_fixed
            etFixedName.visibility = if (fixed) View.VISIBLE else View.GONE
            tvFixedHint.visibility =
                if (fixed && etFixedName.text.toString().trim().isEmpty()) View.VISIBLE else View.GONE
        }

        fun persistAndPush(renameNow: String? = null) {
            val mode = when (rgMode.checkedRadioButtonId) {
                R.id.rb_rename_auto -> MODE_AUTO
                R.id.rb_rename_fixed -> MODE_FIXED
                else -> MODE_OFF
            }
            val fixed = etFixedName.text.toString().trim().take(64)
            store.micRenameMode = mode
            store.micRenameFixedName = fixed

            // Push to the server the same way gate/gain are pushed:
            // best-effort, off the main thread, failures swallowed.
            thread(isDaemon = true) {
                val host = store.getHost()
                val certDer = store.getCertDer()
                val token = store.getToken()
                if (host != null && certDer != null && token != null) {
                    try {
                        val api = Api(pinnedClient(certDer), httpsBaseUrl(host, store.getPort()))
                        api.pushRename(token, mode, fixed, renameNow)
                    } catch (_: Exception) {
                        // Best-effort; the local values are what the app uses.
                    }
                }
            }
        }

        // Restore persisted state.
        when (store.micRenameMode) {
            MODE_AUTO -> rbAuto.isChecked = true
            MODE_FIXED -> rbFixed.isChecked = true
            else -> rbOff.isChecked = true
        }
        etFixedName.setText(store.micRenameFixedName)
        updateFixedVisibility()

        rgMode.setOnCheckedChangeListener { _, _ ->
            updateFixedVisibility()
            persistAndPush()
            activity.runOnUiThread {
                Toast.makeText(activity, R.string.rename_saved, Toast.LENGTH_SHORT).show()
            }
        }

        etFixedName.setOnFocusChangeListener { _, hasFocus ->
            // Persist the fixed name when the user leaves the field (only if
            // Fixed mode is selected — like the PC GUI, an empty fixed name
            // never switches the mode).
            if (!hasFocus && rgMode.checkedRadioButtonId == R.id.rb_rename_fixed &&
                etFixedName.text.toString().trim().isNotEmpty()
            ) {
                persistAndPush()
            }
            updateFixedVisibility()
        }

        btnRenameNow.setOnClickListener {
            val wanted = etFixedName.text.toString().trim().take(64)
            if (wanted.isEmpty()) {
                Toast.makeText(activity, R.string.rename_needs_name, Toast.LENGTH_SHORT).show()
                return@setOnClickListener
            }
            persistAndPush(renameNow = wanted)
            Toast.makeText(activity, R.string.rename_requested, Toast.LENGTH_SHORT).show()
        }
    }
}
