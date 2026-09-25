//! Windows system-tray mode (`--tray`): the app runs as a background tray icon
//! instead of a console window.
//!
//! The console dashboard stays fully functional — `--tray` only changes the
//! presentation: when the exe was launched by double-click the (freshly created)
//! console window is hidden, a tray icon with a small menu takes its place, and
//! "Quit" in the menu runs the same graceful shutdown as Ctrl+C. When launched
//! from an existing terminal the console is left alone and the tray icon simply
//! runs alongside it.

use std::os::raw::c_void;

use tokio::sync::mpsc;
use tracing::{info, warn};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

// Raw RGBA bytes + dimensions produced by build.rs from web/icons/icon-192.png.
include!(concat!(env!("OUT_DIR"), "/tray_icon.rs"));
const TRAY_ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tray_icon.rgba"));

const MENU_ID_SHOW_QR: &str = "show-qr";
const MENU_ID_QUIT: &str = "quit";

/// Actions the tray menu can request. They are forwarded to the main event loop
/// in `main.rs`, which owns the shutdown path and the server state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Open the connection-QR page in the default browser.
    ShowQr,
    /// Run the graceful shutdown (same as Ctrl+C / the `quit` console command).
    Quit,
}

/// Hide this process's console window via Win32. Only call when the app was
/// launched by double-click — i.e. the console exists solely for this process.
/// Never call it when the user launched from their own terminal.
pub fn hide_own_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut c_void;
        fn ShowWindow(hWnd: *mut c_void, nCmdShow: i32) -> i32;
    }
    const SW_HIDE: i32 = 0;
    // SAFETY: both functions come from kernel32 (already linked by std) and take
    // no owned resources; a null HWND (no console attached) is checked.
    unsafe {
        let hwnd = GetConsoleWindow();
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// The running tray icon. Keep it alive for the whole process lifetime —
/// dropping it removes the icon from the tray.
pub struct TrayApp {
    _tray: TrayIcon,
}

/// Build the tray icon and menu, and spawn a bridge thread that forwards menu
/// clicks to the main task as [`TrayAction`]s.
///
/// `status_text` is shown as the (disabled) first menu item, e.g.
/// "QuicMic — 192.168.1.42:8443". `qr_url` is opened in the default browser
/// when "Show connection QR" is clicked.
pub fn spawn(
    status_text: &str,
    qr_url: &str,
) -> anyhow::Result<(TrayApp, mpsc::UnboundedReceiver<TrayAction>)> {
    let icon = Icon::from_rgba(TRAY_ICON_RGBA.to_vec(), TRAY_ICON_WIDTH, TRAY_ICON_HEIGHT)
        .map_err(|e| anyhow::anyhow!("invalid tray icon image: {e}"))?;

    let menu = Menu::new();
    let status = MenuItem::with_id("status", status_text, false, None);
    let show_qr = MenuItem::with_id(MENU_ID_SHOW_QR, "Show connection QR", true, None);
    let quit = MenuItem::with_id(MENU_ID_QUIT, "Quit", true, None);
    menu.append(&status)?;
    menu.append(&show_qr)?;
    menu.append(&quit)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("QuicMic")
        .with_icon(icon)
        .build()?;
    info!("tray icon running");

    // MenuEvent::receiver() is a blocking crossbeam channel; bridge it onto a
    // tokio channel so the main select! loop can await it like everything else.
    let (tx, rx) = mpsc::unbounded_channel::<TrayAction>();
    let qr_url = qr_url.to_string();
    std::thread::Builder::new()
        .name("tray-menu".into())
        .spawn(move || {
            for event in MenuEvent::receiver().iter() {
                let action = match event.id.as_ref() {
                    MENU_ID_SHOW_QR => TrayAction::ShowQr,
                    MENU_ID_QUIT => TrayAction::Quit,
                    _ => continue,
                };
                if action == TrayAction::ShowQr {
                    // Best-effort: opening the browser must never kill the app.
                    if let Err(e) = open::that_detached(&qr_url) {
                        warn!("could not open QR page in browser: {e}");
                    }
                    continue;
                }
                if tx.send(action).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("could not spawn tray menu thread: {e}"))?;

    Ok((TrayApp { _tray: tray }, rx))
}
