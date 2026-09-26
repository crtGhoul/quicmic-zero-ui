package com.quicmic.android.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.google.zxing.integration.android.IntentIntegrator
import com.quicmic.android.R
import com.quicmic.android.net.Api
import com.quicmic.android.net.PairingInfo
import com.quicmic.android.net.capturingClient
import com.quicmic.android.net.fingerprintHex
import com.quicmic.android.net.gateLinear
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.parsePairingUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.net.sha256Base64
import com.quicmic.android.store.SecureStore
import kotlin.concurrent.thread

/**
 * Pairing screen.
 *
 * 1. Scan the QR from the PC app's Pair tab (or type host/port/PIN manually).
 *    The payload is https://<host>:<port>#<pin> — the PIN lives in the hash
 *    fragment, so it is never sent to the server in a URL.
 * 2. "Fetch server identity" TLS-handshakes the server with a capture-only
 *    trust manager (validates nothing), then cross-checks
 *    SHA-256(leaf DER) against GET /api/info's cert_hash (base64).
 * 3. The certificate SHA-256 fingerprint is shown next to the PIN; the user
 *    confirms it matches the PC's Pair tab, then "Confirm & Pair" issues
 *    POST /api/pair. A wrong PIN is HTTP 200 with success=false BY DESIGN.
 * 4. Host, PIN, token and the certificate DER are stored encrypted; all later
 *    traffic pins exactly that certificate (never the system trust store).
 */
class PairActivity : AppCompatActivity() {

    companion object {
        private const val REQ_CAMERA = 10
    }

    private lateinit var store: SecureStore
    private lateinit var etHost: EditText
    private lateinit var etPort: EditText
    private lateinit var etPin: EditText
    private lateinit var tvFingerprint: TextView
    private lateinit var tvPinConfirm: TextView
    private lateinit var btnConfirm: Button
    private lateinit var tvStatus: TextView

    private var pendingInfo: PairingInfo? = null
    private var pendingDer: ByteArray? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = SecureStore(this)
        setContentView(R.layout.activity_pair)

        etHost = findViewById(R.id.et_host)
        etPort = findViewById(R.id.et_port)
        etPin = findViewById(R.id.et_pin)
        tvFingerprint = findViewById(R.id.tv_fingerprint)
        tvPinConfirm = findViewById(R.id.tv_pin_confirm)
        btnConfirm = findViewById(R.id.btn_confirm)
        tvStatus = findViewById(R.id.tv_pair_status)

        etPort.setText("8443")

        findViewById<Button>(R.id.btn_scan).setOnClickListener { ensureCameraThenScan() }
        findViewById<Button>(R.id.btn_fetch).setOnClickListener { fetchIdentity() }
        btnConfirm.setOnClickListener { confirmAndPair() }
    }

    // --- QR scanning ---

    private fun ensureCameraThenScan() {
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            startScan()
        } else {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.CAMERA), REQ_CAMERA)
        }
    }

    private fun startScan() {
        IntentIntegrator(this)
            .setDesiredBarcodeFormats(IntentIntegrator.QR_CODE)
            .setPrompt("Scan the QR code on the PC Pair tab")
            .setBeepEnabled(false)
            .setOrientationLocked(true)
            .initiateScan()
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_CAMERA &&
            grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED
        ) {
            startScan()
        }
    }

    @Deprecated("IntentIntegrator still uses the legacy activity-result API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        val result = IntentIntegrator.parseActivityResult(requestCode, resultCode, data)
        if (result != null) {
            val contents = result.contents
            if (contents == null) {
                toast("No QR code found")
            } else {
                fillFromQr(contents)
            }
        } else {
            super.onActivityResult(requestCode, resultCode, data)
        }
    }

    private fun fillFromQr(contents: String) {
        val info = parsePairingUrl(contents)
        if (info == null) {
            toast("That QR code is not a QuicMic pairing code")
            return
        }
        etHost.setText(info.host)
        etPort.setText(info.port.toString())
        etPin.setText(info.pin)
        hideConfirmation()
        toast("QR read — now fetch the server identity")
    }

    // --- Identity fetch + fingerprint confirmation ---

    private fun readFields(): PairingInfo? {
        val host = etHost.text.toString().trim()
        val port = etPort.text.toString().trim().toIntOrNull() ?: 8443
        val pin = etPin.text.toString().trim()
        if (host.isEmpty() || !Regex("\\d{6}").matches(pin) || port !in 1..65535) {
            return null
        }
        return PairingInfo(host, port, pin)
    }

    private fun fetchIdentity() {
        val info = readFields()
        if (info == null) {
            tvStatus.text = "Enter the host, port and 6-digit PIN (or scan the QR)."
            return
        }
        hideConfirmation()
        tvStatus.text = "Contacting server…"
        thread(isDaemon = true) {
            try {
                val (client, capturing) = capturingClient()
                val api = Api(client, httpsBaseUrl(info.host, info.port))
                val serverInfo = api.getInfo()
                val der = capturing.leafDer
                    ?: throw IllegalStateException("server presented no certificate")
                // Cross-check: the handshake certificate must be the one the
                // server advertises — binds /api/info to the same TLS endpoint.
                if (sha256Base64(der) != serverInfo.certHashBase64.trim()) {
                    throw IllegalStateException(
                        "server identity mismatch: the TLS certificate does not " +
                            "match /api/info's cert_hash. Aborting.",
                    )
                }
                pendingInfo = info
                pendingDer = der
                runOnUiThread {
                    tvFingerprint.text = fingerprintHex(der)
                    tvPinConfirm.text = "PIN from QR: ${info.pin}"
                    tvFingerprint.visibility = View.VISIBLE
                    tvPinConfirm.visibility = View.VISIBLE
                    btnConfirm.visibility = View.VISIBLE
                    tvStatus.text =
                        "Does this fingerprint match the PC's Pair tab? " +
                            "Confirm only if it does."
                }
            } catch (e: Exception) {
                runOnUiThread { tvStatus.text = "Could not verify server: ${e.message}" }
            }
        }
    }

    private fun hideConfirmation() {
        pendingInfo = null
        pendingDer = null
        tvFingerprint.visibility = View.GONE
        tvPinConfirm.visibility = View.GONE
        btnConfirm.visibility = View.GONE
    }

    // --- Pair ---

    private fun confirmAndPair() {
        val info = pendingInfo ?: return
        val der = pendingDer ?: return
        btnConfirm.isEnabled = false
        tvStatus.text = "Pairing…"
        thread(isDaemon = true) {
            try {
                // From here on the pinned certificate is the ONLY trust root.
                val api = Api(pinnedClient(der), httpsBaseUrl(info.host, info.port))
                val res = api.pair(info.pin, store.deviceName)
                if (!res.success || res.token.isNullOrEmpty()) {
                    // Wrong PIN arrives as HTTP 200 + success:false (by design).
                    runOnUiThread {
                        tvStatus.text = res.error ?: "Pairing failed"
                        btnConfirm.isEnabled = true
                    }
                    return@thread
                }
                store.savePairing(info.host, info.port, info.pin, res.token, der)
                // Push our settings — the client is the source of truth.
                try {
                    api.pushSettings(res.token, gateLinear(store.gateDb), store.gain)
                } catch (_: Exception) {
                    // Best-effort.
                }
                runOnUiThread {
                    toast("Paired${res.micName?.let { " as $it" } ?: ""}")
                    setResult(RESULT_OK)
                    finish()
                }
            } catch (e: Exception) {
                runOnUiThread {
                    tvStatus.text = "Pairing failed: ${e.message}"
                    btnConfirm.isEnabled = true
                }
            }
        }
    }

    private fun toast(msg: String) =
        Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
}
