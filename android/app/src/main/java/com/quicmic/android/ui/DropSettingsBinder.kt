package com.quicmic.android.ui

import android.content.Intent
import android.text.Editable
import android.text.TextWatcher
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.ScrollView
import android.widget.Switch
import android.widget.Toast
import com.quicmic.android.R
import com.quicmic.android.net.DropMatchmaker
import com.quicmic.android.service.DropReceiveService
import com.quicmic.android.store.SecureStore

/**
 * Wires the Drop settings section (res/layout/settings_section_drop.xml)
 * into SettingsActivity.
 *
 * INTEGRATOR NOTE: the section is now <include>d directly in
 * activity_settings.xml (id section_drop), so this binder finds the
 * already-inflated view instead of inflating a second copy.
 */
object DropSettingsBinder {

    fun bind(activity: SettingsActivity) {
        val store = SecureStore(activity)

        // The section is <include>d in activity_settings.xml; find it.
        val section = activity.findViewById<View>(R.id.section_drop) ?: return

        // "Send file to PC": system picker, then the native send flow.
        section.findViewById<Button>(R.id.btn_drop_send_file).setOnClickListener {
            if (!store.isPaired) {
                Toast.makeText(
                    activity,
                    activity.getString(R.string.drop_pair_first),
                    Toast.LENGTH_SHORT,
                ).show()
                return@setOnClickListener
            }
            activity.startActivity(
                Intent(activity, DropShareActivity::class.java)
                    .putExtra(DropShareActivity.EXTRA_PICK_FILE, true),
            )
        }

        // Background receive toggle <-> DropReceiveService.
        val swReceive = section.findViewById<Switch>(R.id.sw_drop_receive)
        swReceive.isChecked = store.dropReceiveEnabled || DropReceiveService.isRunning
        swReceive.setOnCheckedChangeListener { _, checked ->
            store.dropReceiveEnabled = checked
            if (checked) {
                if (!store.isPaired) {
                    Toast.makeText(
                        activity,
                        activity.getString(R.string.drop_pair_first),
                        Toast.LENGTH_SHORT,
                    ).show()
                    swReceive.isChecked = false
                    store.dropReceiveEnabled = false
                    return@setOnCheckedChangeListener
                }
                DropReceiveService.start(activity)
            } else {
                DropReceiveService.stop(activity)
            }
        }

        // Auto-accept toggle.
        val swAuto = section.findViewById<Switch>(R.id.sw_drop_auto_accept)
        swAuto.isChecked = store.dropAutoAccept
        swAuto.setOnCheckedChangeListener { _, checked ->
            store.dropAutoAccept = checked
        }

        // Matchmaker server URL.
        val etServer = section.findViewById<EditText>(R.id.et_drop_server)
        etServer.setText(store.dropSignalUrl)
        etServer.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: Editable?) {
                val normalized = DropMatchmaker.normalizeUrl(s.toString())
                if (normalized.isNotEmpty()) store.dropSignalUrl = normalized
            }
        })
    }
}
