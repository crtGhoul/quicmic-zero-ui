package com.quicmic.android.ui

import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity
import com.quicmic.android.R

/**
 * Built-in 101 guide: what the app does, how to set up the PC, how to pair,
 * the same-WiFi requirement, the wrong-IP and rotating-PIN gotchas, a tour
 * of the Settings sections, and quick troubleshooting.
 *
 * Read-only screen — no network, no state. Opened from the Pair screen and
 * from the System section in Settings.
 */
class GuideActivity : AppCompatActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        ThemeManager.apply(this) // belt-and-braces; QuicMicApp covers this too
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_guide)
    }
}
