package com.quicmic.android.ui

import android.os.Bundle
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.appcompat.app.AppCompatActivity
import com.quicmic.android.net.httpsBaseUrl
import com.quicmic.android.net.pinnedClient
import com.quicmic.android.store.SecureStore
import okhttp3.Request
import java.io.ByteArrayInputStream

/**
 * In-app browser for the Drop file-sharing page (`drop.html`) served by the
 * paired PC.
 *
 * Why a WebView instead of an external browser intent: the PC server uses a
 * self-signed certificate, so a plain browser would land on a "your connection
 * is not private" interstitial. This activity instead routes every request to
 * the PC through the app's existing pinned OkHttp client
 * ([pinnedClient], same trust as the mic stream), so the page loads with no
 * cert warning and no trust downgrade.
 *
 * Only traffic to the paired PC is intercepted. The Drop page's matchmaker
 * WebSocket and any other third-party traffic use the WebView's normal stack
 * (the matchmaker has a real public certificate).
 */
class DropActivity : AppCompatActivity() {

    private lateinit var webView: WebView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val store = SecureStore(this)
        val host = store.getHost()
        val certDer = store.getCertDer()
        if (host == null || certDer == null) {
            Toast.makeText(this, "Pair with the PC first", Toast.LENGTH_SHORT).show()
            finish()
            return
        }
        val baseUrl = httpsBaseUrl(host, store.getPort())
        val client = pinnedClient(certDer)

        webView = WebView(this)
        setContentView(webView)
        webView.settings.javaScriptEnabled = true
        webView.settings.domStorageEnabled = true
        webView.webViewClient = object : WebViewClient() {
            override fun shouldInterceptRequest(
                view: WebView,
                request: WebResourceRequest,
            ): WebResourceResponse? {
                val url = request.url.toString()
                if (!url.startsWith(baseUrl)) return null
                return try {
                    val req = Request.Builder().url(url).apply {
                        request.requestHeaders.forEach { (k, v) -> addHeader(k, v) }
                    }.build()
                    client.newCall(req).execute().use { resp ->
                        val body = resp.body?.bytes() ?: ByteArray(0)
                        val contentType = resp.header("Content-Type")
                        val mime = contentType?.substringBefore(";")?.trim()
                        val encoding = contentType
                            ?.substringAfter("charset=", "")
                            ?.substringBefore(";")
                            ?.trim()
                            ?.takeIf { it.isNotEmpty() }
                        WebResourceResponse(
                            mime,
                            encoding,
                            resp.code,
                            resp.message.ifEmpty { "OK" },
                            // Single value per header; keep the last on duplicates.
                            resp.headers.toMultimap()
                                .mapValues { it.value.lastOrNull().orEmpty() },
                            ByteArrayInputStream(body),
                        )
                    }
                } catch (_: Exception) {
                    // Let the WebView handle it normally (shows its error page).
                    null
                }
            }
        }
        webView.loadUrl("$baseUrl/drop.html")
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (webView.canGoBack()) webView.goBack() else finish()
            }
        })
    }

    override fun onDestroy() {
        if (::webView.isInitialized) webView.destroy()
        super.onDestroy()
    }
}
