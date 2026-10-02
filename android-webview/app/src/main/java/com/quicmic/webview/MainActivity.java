package com.quicmic.webview;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.net.http.SslError;
import android.os.Bundle;
import android.util.Base64;
import android.view.ViewGroup;
import android.webkit.PermissionRequest;
import android.webkit.SslErrorHandler;
import android.webkit.WebChromeClient;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;

import java.security.MessageDigest;
import java.security.cert.X509Certificate;

/**
 * Thin wrapper that loads the QuicMic PC's web UI (mic/speaker/landing pages)
 * in a WebView, so the app always looks exactly like the browser UI.
 *
 * The PC uses a self-signed certificate: the first connection shows the
 * SHA-256 fingerprint for the user to verify against the PC app's Pair tab,
 * then pins it. Later connections proceed silently unless the cert changes.
 */
public class MainActivity extends Activity {

    private static final String PREFS = "quicmic_webview";
    private static final String PREF_URL = "server_url";
    private static final String PREF_CERT_DER = "pinned_cert_der";
    private static final int REQ_MIC = 1001;
    private static final String DEFAULT_URL = "https://192.168.68.104:8443";

    private WebView webView;
    private EditText urlInput;
    private PermissionRequest pendingPermissionRequest;

    @SuppressLint("SetJavaScriptEnabled")
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setLayoutParams(new ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setLayoutParams(new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT));
        bar.setPadding(8, 8, 8, 8);

        urlInput = new EditText(this);
        urlInput.setLayoutParams(new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        urlInput.setHint("https://<pc-ip>:8443");
        urlInput.setText(prefs().getString(PREF_URL, DEFAULT_URL));
        urlInput.setSingleLine();

        Button go = new Button(this);
        go.setLayoutParams(new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT));
        go.setText("Go");
        go.setOnClickListener(v -> connect());

        bar.addView(urlInput);
        bar.addView(go);
        root.addView(bar);

        webView = new WebView(this);
        webView.setLayoutParams(new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));
        root.addView(webView);
        setContentView(root);

        webView.getSettings().setJavaScriptEnabled(true);
        webView.getSettings().setDomStorageEnabled(true);
        webView.getSettings().setMediaPlaybackRequiresUserGesture(false);

        webView.setWebViewClient(new WebViewClient() {
            @Override
            public void onReceivedSslError(WebView view, SslErrorHandler handler, SslError error) {
                handleSslError(handler, error);
            }
        });
        webView.setWebChromeClient(new WebChromeClient() {
            @Override
            public void onPermissionRequest(PermissionRequest request) {
                runOnUiThread(() -> handlePermissionRequest(request));
            }
        });

        if (prefs().contains(PREF_URL)) {
            connect();
        }
    }

    private SharedPreferences prefs() {
        return getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    private void connect() {
        String url = urlInput.getText().toString().trim();
        if (!url.startsWith("http")) {
            url = "https://" + url;
        }
        prefs().edit().putString(PREF_URL, url).apply();
        webView.loadUrl(url);
    }

    private void handlePermissionRequest(PermissionRequest request) {
        boolean wantsMic = false;
        for (String r : request.getResources()) {
            if (PermissionRequest.RESOURCE_AUDIO_CAPTURE.equals(r)) {
                wantsMic = true;
                break;
            }
        }
        if (!wantsMic) {
            request.deny();
            return;
        }
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO)
                == PackageManager.PERMISSION_GRANTED) {
            request.grant(request.getResources());
        } else {
            pendingPermissionRequest = request;
            requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, REQ_MIC);
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions,
                                           int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == REQ_MIC) {
            PermissionRequest req = pendingPermissionRequest;
            pendingPermissionRequest = null;
            if (req != null) {
                if (grantResults.length > 0
                        && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
                    req.grant(req.getResources());
                } else {
                    req.deny();
                }
            }
        }
    }

    private void handleSslError(SslErrorHandler handler, SslError error) {
        byte[] der = null;
        try {
            X509Certificate cert = error.getCertificate() != null
                    ? error.getCertificate().getX509Certificate() : null;
            if (cert != null) {
                der = cert.getEncoded();
            }
        } catch (Exception ignored) {
        }
        if (der == null) {
            handler.cancel();
            return;
        }
        byte[] pinned = null;
        String pinnedB64 = prefs().getString(PREF_CERT_DER, null);
        if (pinnedB64 != null) {
            try {
                pinned = Base64.decode(pinnedB64, Base64.DEFAULT);
            } catch (Exception ignored) {
            }
        }
        if (pinned != null && java.util.Arrays.equals(pinned, der)) {
            handler.proceed();
            return;
        }
        StringBuilder fp = new StringBuilder();
        for (byte b : sha256(der)) {
            if (fp.length() > 0) {
                fp.append(':');
            }
            fp.append(String.format("%02X", b));
        }
        final byte[] derFinal = der;
        String title = pinned != null ? "Certificate changed!" : "Trust this QuicMic server?";
        String msg = pinned != null
                ? "The server's certificate changed. This can mean the PC regenerated "
                + "its identity — or someone intercepting you. Only trust it if you "
                + "just reset QuicMic on your PC.\n\nNew fingerprint (SHA-256):\n" + fp
                : "QuicMic uses a self-signed certificate. Compare this fingerprint "
                + "with the one shown in the PC app's Pair tab — trust it only if "
                + "they match.\n\nFingerprint (SHA-256):\n" + fp;
        new AlertDialog.Builder(this)
                .setTitle(title)
                .setMessage(msg)
                .setPositiveButton("Trust", (d, w) -> {
                    prefs().edit()
                            .putString(PREF_CERT_DER,
                                    Base64.encodeToString(derFinal, Base64.DEFAULT))
                            .apply();
                    handler.proceed();
                })
                .setNegativeButton("Cancel", (d, w) -> handler.cancel())
                .setCancelable(false)
                .show();
    }

    private static byte[] sha256(byte[] data) {
        try {
            return MessageDigest.getInstance("SHA-256").digest(data);
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        if (webView != null && webView.canGoBack()) {
            webView.goBack();
        } else {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        if (webView != null) {
            webView.destroy();
        }
        super.onDestroy();
    }
}
