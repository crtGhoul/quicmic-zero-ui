package com.quicmic.android.ui

import android.content.Intent
import android.graphics.Typeface
import android.widget.Button
import android.widget.Switch
import android.widget.TextView
import androidx.core.content.ContextCompat
import com.quicmic.android.R
import com.quicmic.android.net.Api
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread

/**
 * Wires the System settings fragment (res/layout/settings_section_system.xml):
 * the theme selector, the PC update "check now" row + opt-out toggle, and the
 * Diagnostics entry row.
 *
 * INTEGRATOR: <include layout="@layout/settings_section_system" /> at the end
 * of activity_settings.xml's inner LinearLayout, then call
 * SystemSettingsBinder.bind(this) from SettingsActivity.onCreate AFTER
 * setContentView.
 */
object SystemSettingsBinder {

    fun bind(activity: SettingsActivity) {
        val store = SecureStore(activity)
        bindTheme(activity, store)
        bindUpdateCheck(activity, store)
        activity.findViewById<Button>(R.id.sys_btn_diagnostics).setOnClickListener {
            activity.startActivity(Intent(activity, DiagnosticsActivity::class.java))
        }
        activity.findViewById<Button>(R.id.sys_btn_guide).setOnClickListener {
            activity.startActivity(Intent(activity, GuideActivity::class.java))
        }
    }

    // --- Theme ---

    private fun bindTheme(activity: SettingsActivity, store: SecureStore) {
        val buttons = listOf(
            activity.findViewById<Button>(R.id.sys_theme_dark) to ThemeManager.MODE_DARK,
            activity.findViewById<Button>(R.id.sys_theme_light) to ThemeManager.MODE_LIGHT,
            activity.findViewById<Button>(R.id.sys_theme_system) to ThemeManager.MODE_SYSTEM,
        )
        fun render() {
            val current = store.themeMode
            val selected = ContextCompat.getColor(activity, R.color.quicmic_green)
            val idle = ContextCompat.getColor(activity, R.color.text_muted)
            for ((btn, mode) in buttons) {
                val on = mode == current
                btn.setTextColor(if (on) selected else idle)
                btn.setTypeface(btn.typeface, if (on) Typeface.BOLD else Typeface.NORMAL)
            }
        }
        for ((btn, mode) in buttons) {
            btn.setOnClickListener {
                if (store.themeMode != mode) {
                    ThemeManager.setMode(activity, mode)
                    activity.recreate() // re-inflate with the new palette
                }
            }
        }
        render()
    }

    // --- PC update check ---

    private fun bindUpdateCheck(activity: SettingsActivity, store: SecureStore) {
        val status = activity.findViewById<TextView>(R.id.sys_update_status)
        val checkBtn = activity.findViewById<Button>(R.id.sys_btn_update_check)
        val optOut = activity.findViewById<Switch>(R.id.sys_sw_update_optout)

        fun renderEnabled() {
            val enabled = store.updateCheckEnabled
            checkBtn.isEnabled = enabled
            if (!enabled) status.text = activity.getString(R.string.update_disabled)
        }
        optOut.isChecked = store.updateCheckEnabled
        optOut.setOnCheckedChangeListener { _, checked ->
            store.updateCheckEnabled = checked
            renderEnabled()
        }
        renderEnabled()

        checkBtn.setOnClickListener {
            if (!store.updateCheckEnabled) return@setOnClickListener
            status.text = activity.getString(R.string.update_checking)
            checkBtn.isEnabled = false
            thread(isDaemon = true) {
                val result: String = when {
                    !store.isPaired ->
                        activity.getString(R.string.update_not_paired)
                    else -> try {
                        val host = store.getHost()!!
                        val api = Api(
                            pinnedClient(store.getCertDer()!!),
                            httpsBaseUrl(host, store.getPort()),
                        )
                        val info = api.getInfo()
                        if (info.updateAvailable && info.latestVersion != null) {
                            activity.getString(R.string.update_available_fmt, info.latestVersion)
                        } else {
                            activity.getString(R.string.update_up_to_date)
                        }
                    } catch (e: Exception) {
                        activity.getString(
                            R.string.update_check_failed_fmt,
                            e.message ?: e.javaClass.simpleName,
                        )
                    }
                }
                activity.runOnUiThread {
                    status.text = result
                    checkBtn.isEnabled = store.updateCheckEnabled
                }
            }
        }
    }
}
