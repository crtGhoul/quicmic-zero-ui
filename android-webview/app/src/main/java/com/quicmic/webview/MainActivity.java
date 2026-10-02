package com.quicmic.webview;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.graphics.Typeface;
import android.net.http.SslError;
import android.os.Bundle;
import android.util.Base64;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.PermissionRequest;
import android.webkit.SslErrorHandler;
import android.webkit.WebChromeClient;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import java.security.MessageDigest;
import java.security.cert.X509Certificate;

/**
 * QuicMic for Android: a thin wrapper that loads the QuicMic PC's web UI
 * (mic/speaker/drop pages) in a WebView, so the app always looks exactly
 * like the browser UI — including the noise-cancellation toggle and any
 * future web changes.
 *
 * Flow: start screen → Scan QR code (or Connect to the remembered server,
 * or enter the address manually) → the PC's web UI fills the screen.
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
    private static final int REQ_SCAN = 1002;
    private static final int REQ_CAMERA = 1003;

    private static final int MENU_SCAN = 1;
    private static final int MENU_DISCONNECT = 2;

    private static final int BG = 0xFF101418;
    private static final int CARD = 0xFF1B2129;
    private static final int ACCENT = 0xFF22D3EE;
    private static final int TEXT = 0xFFE8EDF2;
    private static final int DIM = 0xFF9AA7B4;

    private FrameLayout root;
    private LinearLayout startView;
    private TextView statusText;
    private Button connectButton;
    private WebView webView;
    private Button menuFab;
    private PermissionRequest pendingPermissionRequest;

    @SuppressLint("SetJavaScriptEnabled")
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        root = new FrameLayout(this);
        root.setBackgroundColor(BG);
        root.setLayoutParams(new ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        buildStartView();
        root.addView(startView);

        webView = new WebView(this);
        webView.setLayoutParams(new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        webView.setVisibility(View.GONE);
        root.addView(webView);

        // Floating menu: the NoActionBar theme has no action bar and phones
        // have no hardware menu key, so the Scan/Disconnect actions would
        // otherwise be unreachable while the web UI fills the screen.
        menuFab = new Button(this);
        menuFab.setText("⋮");
        menuFab.setTextSize(TypedValue.COMPLEX_UNIT_SP, 22);
        menuFab.setTextColor(TEXT);
        menuFab.setBackgroundColor(0xCC1B2129);
        menuFab.setAllCaps(false);
        int fabSize = dp(56);
        FrameLayout.LayoutParams flp = new FrameLayout.LayoutParams(
                fabSize, fabSize, Gravity.BOTTOM | Gravity.END);
        int m = dp(16);
        flp.setMargins(m, m, m, m);
        menuFab.setLayoutParams(flp);
        menuFab.setVisibility(View.GONE);
        menuFab.setOnClickListener(v -> showWebMenu());
        root.addView(menuFab);

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
    }

    @Override
    protected void onResume() {
        super.onResume();
        refreshStartView();
        if (webView.getVisibility() == View.VISIBLE) {
            webView.onResume();
        }
    }

    @Override
    protected void onPause() {
        if (webView != null) {
            webView.onPause();
        }
        super.onPause();
    }

    /** The app-like start screen: brand, Scan, Connect, manual entry. */
    private void buildStartView() {
        startView = new LinearLayout(this);
        startView.setOrientation(LinearLayout.VERTICAL);
        startView.setGravity(Gravity.CENTER_HORIZONTAL);
        startView.setPadding(dp(32), dp(64), dp(32), dp(32));
        startView.setLayoutParams(new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        TextView mic = new TextView(this);
        mic.setText("\uD83C\uDFA4");
        mic.setTextSize(TypedValue.COMPLEX_UNIT_SP, 64);
        mic.setGravity(Gravity.CENTER);
        startView.addView(mic);

        TextView title = new TextView(this);
        title.setText("QuicMic");
        title.setTextColor(TEXT);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 36);
        title.setTypeface(title.getTypeface(), Typeface.BOLD);
        title.setGravity(Gravity.CENTER);
        title.setPadding(0, dp(16), 0, 0);
        startView.addView(title);

        TextView sub = new TextView(this);
        sub.setText("Wireless microphone for your PC");
        sub.setTextColor(DIM);
        sub.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        sub.setGravity(Gravity.CENTER);
        sub.setPadding(0, dp(4), 0, dp(32));
        startView.addView(sub);

        Button scan = new Button(this);
        scan.setText("\uD83D\uDCF7  Scan QR code");
        stylePrimary(scan);
        scan.setOnClickListener(v -> startScan());
        startView.addView(scan);

        connectButton = new Button(this);
        connectButton.setText("Connect");
        stylePrimary(connectButton);
        connectButton.setOnClickListener(v -> {
            String url = prefs().getString(PREF_URL, null);
            if (url != null) {
                connect(url);
            }
        });
        startView.addView(connectButton);

        Button manual = new Button(this);
        manual.setText("Enter address manually");
        styleGhost(manual);
        manual.setOnClickListener(v -> promptManualAddress());
        startView.addView(manual);

        statusText = new TextView(this);
        statusText.setTextColor(DIM);
        statusText.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        statusText.setGravity(Gravity.CENTER);
        statusText.setPadding(0, dp(24), 0, 0);
        startView.addView(statusText);
    }

    private void refreshStartView() {
        String url = prefs().getString(PREF_URL, null);
        if (url != null) {
            connectButton.setVisibility(View.VISIBLE);
            connectButton.setText("Connect to " + shortHost(url));
            statusText.setText("Point the camera at the QR code in the PC app's Pair tab,\nor reconnect to your last server.");
        } else {
            connectButton.setVisibility(View.GONE);
            statusText.setText("Point the camera at the QR code in the PC app's Pair tab to get started.");
        }
    }

    private static String shortHost(String url) {
        String s = url.replaceFirst("^https?://", "");
        int slash = s.indexOf('/');
        if (slash > 0) {
            s = s.substring(0, slash);
        }
        int hash = s.indexOf('#');
        if (hash > 0) {
            s = s.substring(0, hash);
        }
        return s;
    }

    private void stylePrimary(Button b) {
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.topMargin = dp(8);
        b.setLayoutParams(lp);
        b.setBackgroundColor(ACCENT);
        b.setTextColor(Color.BLACK);
        b.setTextSize(TypedValue.COMPLEX_UNIT_SP, 17);
        b.setPadding(0, dp(14), 0, dp(14));
        b.setAllCaps(false);
    }

    private void styleGhost(Button b) {
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.topMargin = dp(8);
        b.setLayoutParams(lp);
        b.setBackgroundColor(CARD);
        b.setTextColor(TEXT);
        b.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        b.setPadding(0, dp(12), 0, dp(12));
        b.setAllCaps(false);
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    // ── Scan flow ────────────────────────────────────────────────────

    private void startScan() {
        if (checkSelfPermission(Manifest.permission.CAMERA)
                == PackageManager.PERMISSION_GRANTED) {
            startActivityForResult(new Intent(this, ScanActivity.class), REQ_SCAN);
        } else {
            requestPermissions(new String[]{Manifest.permission.CAMERA}, REQ_CAMERA);
        }
    }

    private void promptManualAddress() {
        final EditText input = new EditText(this);
        input.setSingleLine();
        input.setHint("https://<pc-ip>:8443");
        input.setText(prefs().getString(PREF_URL, "https://"));
        input.setTextColor(TEXT);
        input.setHintTextColor(DIM);
        int pad = dp(16);
        input.setPadding(pad, pad, pad, pad);
        new AlertDialog.Builder(this)
                .setTitle("Server address")
                .setMessage("Find it in the PC app's Pair tab.")
                .setView(input)
                .setPositiveButton("Connect", (d, w) -> connect(input.getText().toString()))
                .setNegativeButton("Cancel", null)
                .show();
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == REQ_SCAN && resultCode == RESULT_OK && data != null) {
            String text = data.getStringExtra(ScanActivity.EXTRA_RESULT);
            if (text != null) {
                text = text.trim();
                if (text.contains("://")) {
                    connect(text);
                } else {
                    Toast.makeText(this,
                            "That QR code doesn't look like a QuicMic address.",
                            Toast.LENGTH_LONG).show();
                }
            }
        }
    }

    // ── Connection ───────────────────────────────────────────────────

    private SharedPreferences prefs() {
        return getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    private void connect(String rawUrl) {
        String url = rawUrl.trim();
        if (!url.contains("://")) {
            url = "https://" + url;
        }
        prefs().edit().putString(PREF_URL, url).apply();
        showWeb();
        webView.loadUrl(url);
    }

    private void showWeb() {
        startView.setVisibility(View.GONE);
        webView.setVisibility(View.VISIBLE);
        menuFab.setVisibility(View.VISIBLE);
        webView.onResume();
        invalidateOptionsMenu();
    }

    private void showStart() {
        webView.onPause();
        webView.loadUrl("about:blank");
        webView.setVisibility(View.GONE);
        menuFab.setVisibility(View.GONE);
        startView.setVisibility(View.VISIBLE);
        refreshStartView();
        invalidateOptionsMenu();
    }

    /** Menu for the connected state: rescan a QR or disconnect. */
    private void showWebMenu() {
        final String[] items = {"\uD83D\uDCF7 Scan QR code", "Disconnect"};
        new AlertDialog.Builder(this)
                .setItems(items, (d, which) -> {
                    if (which == 0) {
                        startScan();
                    } else {
                        showStart();
                    }
                })
                .show();
    }

    private boolean isWebShown() {
        return webView.getVisibility() == View.VISIBLE;
    }

    @Override
    public boolean onCreateOptionsMenu(Menu menu) {
        if (isWebShown()) {
            menu.add(0, MENU_SCAN, 0, "Scan QR code");
            menu.add(0, MENU_DISCONNECT, 0, "Disconnect");
        }
        return true;
    }

    @Override
    public boolean onOptionsItemSelected(MenuItem item) {
        if (item.getItemId() == MENU_SCAN) {
            startScan();
            return true;
        } else if (item.getItemId() == MENU_DISCONNECT) {
            showStart();
            return true;
        }
        return super.onOptionsItemSelected(item);
    }

    // ── Mic permission forwarding (unchanged) ─────────────────────────

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
            // Grant audio capture only, even if the page asked for more.
            request.grant(new String[]{PermissionRequest.RESOURCE_AUDIO_CAPTURE});
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
                    req.grant(new String[]{PermissionRequest.RESOURCE_AUDIO_CAPTURE});
                } else {
                    req.deny();
                }
            }
        } else if (requestCode == REQ_CAMERA) {
            if (grantResults.length > 0
                    && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
                startActivityForResult(new Intent(this, ScanActivity.class), REQ_SCAN);
            } else {
                Toast.makeText(this,
                        "Camera access is needed to scan the QR code.",
                        Toast.LENGTH_LONG).show();
            }
        }
    }

    // ── Self-signed certificate trust + pinning (unchanged) ───────────

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
        // The PC app shows the fingerprint as base64 (Diagnostics tab,
        // "Cert SHA-256") — show the same encoding so the two can actually be
        // compared character by character.
        String fpB64 = Base64.encodeToString(sha256(der), Base64.NO_WRAP);
        final byte[] derFinal = der;
        String title = pinned != null ? "Certificate changed!" : "Trust this QuicMic server?";
        String msg = pinned != null
                ? "The server's certificate changed. This can mean the PC regenerated "
                + "its identity — or someone intercepting you. Only trust it if you "
                + "just reset QuicMic on your PC.\n\nNew fingerprint (SHA-256, base64):\n" + fpB64
                : "QuicMic uses a self-signed certificate. Compare this fingerprint "
                + "with the one in the PC app's Diagnostics tab (\"Cert SHA-256\") — "
                + "trust it only if they match exactly.\n\nFingerprint (SHA-256, base64):\n"
                + fpB64;
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
        if (isWebShown()) {
            if (webView.canGoBack()) {
                webView.goBack();
            } else {
                showStart();
            }
        } else {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        if (webView != null) {
            // Detach before destroying: destroying an attached WebView leaks
            // its render thread on some devices.
            ViewGroup parent = (ViewGroup) webView.getParent();
            if (parent != null) {
                parent.removeView(webView);
            }
            webView.destroy();
        }
        super.onDestroy();
    }
}
