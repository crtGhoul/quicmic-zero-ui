package com.quicmic.android.net

import java.net.URI
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

/**
 * TLS pinning for the QuicMic PC server's self-signed certificate.
 *
 * The server generates a persistent self-signed cert (see the repo's
 * src/tls.rs / src/identity.rs). There is no CA to trust, so the app pins the
 * exact certificate instead of using the system trust store — this is what
 * kills the "connection not secure" UX instead of papering over it.
 *
 * First-pair flow (see PairActivity):
 *  1. Scan the QR: https://<host>:<port>#<pin> (PIN in the hash fragment).
 *  2. TLS-handshake the server with [CapturingTrustManager] (validates
 *     nothing) and read the presented leaf certificate.
 *  3. Cross-check: SHA-256(leaf DER), base64, must equal GET /api/info's
 *     `cert_hash` — binds the advertisement to the same TLS endpoint.
 *  4. Show the SHA-256 fingerprint next to the PIN; the user confirms it
 *     matches the PC's Pair tab, then the cert DER is stored.
 *  5. All later traffic uses [PinningTrustManager], which trusts ONLY the
 *     stored certificate and never falls back to system trust.
 */

private val PIN_RE = Regex("\\d{6}")

/** Parsed form of the pairing QR payload. */
data class PairingInfo(val host: String, val port: Int, val pin: String)

/** Parse the QR payload the PC Pair tab shows. Null when malformed. */
fun parsePairingUrl(text: String): PairingInfo? {
    val uri = try {
        URI(text.trim())
    } catch (_: Exception) {
        return null
    }
    if (!uri.scheme.equals("https", ignoreCase = true)) return null
    val host = uri.host ?: return null
    val port = if (uri.port > 0) uri.port else 8443
    val pin = uri.fragment?.takeIf { PIN_RE.matches(it) } ?: return null
    return PairingInfo(host, port, pin)
}

/** wss:// base URL; brackets IPv6 literals per RFC 3986 (mirrors the server). */
fun wsBaseUrl(host: String, port: Int): String {
    val h = if (':' in host && !host.startsWith("[")) "[$host]" else host
    return "wss://$h:$port"
}

/** https:// base URL; brackets IPv6 literals per RFC 3986. */
fun httpsBaseUrl(host: String, port: Int): String {
    val h = if (':' in host && !host.startsWith("[")) "[$host]" else host
    return "https://$h:$port"
}

fun sha256(bytes: ByteArray): ByteArray =
    MessageDigest.getInstance("SHA-256").digest(bytes)

/** Base64 SHA-256 — the same encoding as the server's /api/info cert_hash. */
fun sha256Base64(bytes: ByteArray): String =
    android.util.Base64.encodeToString(sha256(bytes), android.util.Base64.NO_WRAP)

/** Uppercase colon-separated hex fingerprint, for showing to the user. */
fun fingerprintHex(der: ByteArray): String =
    sha256(der).joinToString(":") { "%02X".format(it) }

/**
 * Trust manager used ONLY during first pairing: records the server's leaf
 * certificate without validating it, so the UI can display the fingerprint.
 * Never used for real traffic.
 */
class CapturingTrustManager : X509TrustManager {
    @Volatile
    var leafDer: ByteArray? = null
        private set

    override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {
        // Not used.
    }

    override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
        leafDer = chain.firstOrNull()?.encoded
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

/**
 * The only trust manager used for real traffic: trusts EXACTLY the pinned
 * server certificate (matched by SHA-256 of the DER). A replaced certificate
 * is a hard failure — there is deliberately no fallback to the system trust
 * store.
 */
class PinningTrustManager(pinnedDer: ByteArray) : X509TrustManager {
    private val pinnedHash = sha256(pinnedDer)

    override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) {
        throw CertificateException("client authentication is not used")
    }

    override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
        val leaf = chain.firstOrNull()
            ?: throw CertificateException("empty certificate chain")
        // Cheap sanity check; the pin is what really authenticates the server.
        try {
            leaf.checkValidity()
        } catch (e: Exception) {
            throw CertificateException("pinned certificate is not currently valid: ${e.message}")
        }
        if (!MessageDigest.isEqual(sha256(leaf.encoded), pinnedHash)) {
            throw CertificateException("server certificate does not match the pinned certificate")
        }
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

/**
 * Hostname verification is subsumed by pinning: the pinned certificate IS the
 * server's identity, so there is nothing left for a name check to add.
 */
val PinningHostnameVerifier = HostnameVerifier { _, _ -> true }

fun sslContextFor(tm: X509TrustManager): SSLContext {
    val ctx = SSLContext.getInstance("TLS")
    ctx.init(null, arrayOf(tm), SecureRandom())
    return ctx
}
