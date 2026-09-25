//! `GET /qr`: a minimal page rendering a large QR code of the phone pairing
//! URL (`https://<lan-ip>:<port>#<pin>`).
//!
//! This is the target of the `--tray` mode's "Show connection QR" menu item,
//! for users who prefer a big scannable code over the terminal one. The server
//! injects the LAN IP, port, and PIN into the page; the QR itself is drawn
//! client-side with the already-embedded `web/qrcode.min.js`, so no new
//! dependencies are needed. The page keeps the strict asset CSP — the QR
//! payload travels in a `data-url` attribute and the init code lives in the
//! static `web/qr.js` (no inline scripts).

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use super::assets::CSP;
use super::state::AppState;

/// Serve the pairing-QR page. The URL (with the PIN in the hash fragment, so
/// it is never sent to the server) is injected into a `data-url` attribute;
/// `web/qr.js` renders it with the embedded `qrcode.min.js`.
pub(super) async fn handle_qr(State(state): State<AppState>) -> Response {
    let host = if state.lan_ip.contains(':') {
        format!("[{}]", state.lan_ip)
    } else {
        state.lan_ip.clone()
    };
    // The PIN is shared with the console's `qr`/`newpin` commands, so read the
    // current value under a short lock (never held across an await).
    let pin = state.pairing_pin.lock().clone();
    let url = format!("https://{}:{}#{}", host, state.wt_port, pin);

    let page = format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>QuicMic — Scan to pair</title>\n\
         <link rel=\"stylesheet\" href=\"/qr.css\">\n\
         </head>\n\
         <body data-url=\"{url_attr}\">\n\
         <main>\n\
         <h1>Scan to pair</h1>\n\
         <div id=\"qr\" role=\"img\" aria-label=\"Pairing QR code\"></div>\n\
         <p class=\"url\"><a id=\"url\" href=\"{url_attr}\"></a></p>\n\
         <p class=\"pin\">PIN: <strong>{pin}</strong></p>\n\
         </main>\n\
         <script src=\"/qrcode.min.js\"></script>\n\
         <script src=\"/qr.js\"></script>\n\
         </body>\n\
         </html>\n",
        url_attr = html_escape(&url),
        pin = html_escape(&pin),
    );

    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        page,
    )
        .into_response()
}

/// Minimal HTML escaping for the injected URL/PIN (attribute + text contexts).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
