package com.quicmic.android.ui

import android.content.Context
import androidx.appcompat.app.AppCompatDelegate
import com.quicmic.android.store.SecureStore

/**
 * Phone UI theme driver. The app ships dark-first (matches the PC): the
 * [SecureStore.themeMode] pref holds 0 = dark, 1 = light, 2 = follow system,
 * defaulting to dark.
 *
 * Resource trick: res/values/colors.xml holds the DARK palette and
 * res/values-night/colors.xml holds the LIGHT palette — inverted from the
 * usual convention on purpose, because the app forces MODE_NIGHT_NO by
 * default (not-night -> default `values/` resources -> dark).
 */
object ThemeManager {

    const val MODE_DARK = 0
    const val MODE_LIGHT = 1
    const val MODE_SYSTEM = 2

    private fun toNightMode(mode: Int): Int = when (mode) {
        MODE_LIGHT -> AppCompatDelegate.MODE_NIGHT_YES
        MODE_SYSTEM -> AppCompatDelegate.MODE_NIGHT_FOLLOW_SYSTEM
        else -> AppCompatDelegate.MODE_NIGHT_NO // dark, the default
    }

    /**
     * Apply the stored theme. Call before any activity is created — QuicMicApp
     * does this in onCreate() (registered as android:name on <application>).
     */
    fun apply(context: Context) {
        AppCompatDelegate.setDefaultNightMode(toNightMode(SecureStore(context).themeMode))
    }

    /**
     * Persist a new theme and apply it. The caller should recreate() the
     * current activity afterwards so already-inflated views re-resolve.
     */
    fun setMode(context: Context, mode: Int) {
        val store = SecureStore(context)
        store.themeMode = mode
        AppCompatDelegate.setDefaultNightMode(toNightMode(store.themeMode))
    }
}
