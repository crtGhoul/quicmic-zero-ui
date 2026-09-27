package com.quicmic.android.ui

import android.app.Application

/**
 * Applies the stored UI theme (dark by default) before any activity is
 * created, so the first frame already uses the right palette.
 *
 * INTEGRATOR: register on the <application> element in AndroidManifest.xml:
 *   android:name=".ui.QuicMicApp"
 */
class QuicMicApp : Application() {
    override fun onCreate() {
        super.onCreate()
        ThemeManager.apply(this)
    }
}
