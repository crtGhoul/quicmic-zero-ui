//! WASAPI loopback capture (Windows only).
//!
//! Opens a *render* endpoint in loopback mode, which yields exactly
//! what the speakers would play — i.e. all PC audio mixed by the system.
//! Captured audio is converted to 48 kHz stereo f32 and emitted as 20 ms frames.
//!
//! The capture device is selectable: [`list_render_devices`] enumerates the
//! active playback endpoints by friendly name, and [`spawn`] takes an optional
//! selection (index or name substring, resolved by
//! [`super::resolve_name`]). A generation counter lets the capture thread exit
//! cleanly so a supervisor can restart it on a newly chosen device.

use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use tokio::sync::broadcast;
use tracing::{info, warn};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName, Media::Audio::*, System::Com::*,
    UI::Shell::PropertiesSystem::IPropertyStore,
};

use super::format::{
    classify_mix_format, i16_to_f32, i32_to_f32, SampleKind, WAVE_FORMAT_EXTENSIBLE,
};
use super::resample::Converter;

/// Start loopback capture on the chosen render endpoint, in the background.
///
/// `device`: `None` → the system default render endpoint; `Some(name)` → a
/// friendly name previously validated by [`super::resolve_name`] (canonical
/// form, so it matches exactly here).
///
/// `generation`: the capture thread exits as soon as this counter no longer
/// equals the value it started with — the device-switch path bumps it, waits
/// a beat, then calls `spawn` again with the new selection.
///
/// The endpoint is opened and the mix format validated on the capture thread,
/// but the outcome is relayed back through a rendezvous channel that `spawn`
/// blocks on: a bad device or an unsupported format still returns `Err` to the
/// caller, so the Speaker tab is disabled with the real reason instead of
/// accepting phone clients onto a stream that can never produce audio.
/// Keeping the COM interfaces on the single thread that created them also
/// avoids `Send` issues (`windows` interface types are not `Send`).
///
/// If the stream dies later — e.g. the endpoint is unplugged and WASAPI
/// reports `AUDCLNT_E_DEVICE_INVALIDATED` — the thread reopens it once per
/// second until the generation bumps (the same "retry until back" policy as
/// the mic output supervisor). Only the initial open is rendezvoused; later
/// restarts never surface to the caller.
pub fn spawn(
    tx: broadcast::Sender<Vec<f32>>,
    device: Option<String>,
    generation: Arc<AtomicU64>,
) -> anyhow::Result<()> {
    let my_generation = generation.load(Ordering::Relaxed);
    // Rendezvous: the capture thread opens and validates the endpoint, then
    // reports the outcome through this channel before entering the pump loop.
    // `spawn` blocks on it (bounded by a timeout) so a bad device or an
    // unsupported mix format returns `Err` here, synchronously: the Speaker
    // tab is then disabled with the real reason instead of accepting phone
    // clients onto a stream that can never produce audio.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new()
        .name("wasapi-loopback".into())
        .spawn(move || {
            // Note: `start_capture` initializes COM (MTA) itself via
            // `open_render_device`; everything COM stays on this thread.
            let mut ready_tx = Some(ready_tx);
            // Consecutive failed (re)opens; reset on success. Drives the
            // sparse retry heartbeat so a long outage doesn't spam the log
            // (same policy as the mic output supervisor).
            let mut attempts: u32 = 0;
            loop {
                match start_capture(device.as_deref()) {
                    Ok(cap) => {
                        if let Some(tx) = ready_tx.take() {
                            let _ = tx.send(Ok(()));
                        }
                        if attempts > 0 {
                            info!("loopback capture rebuilt; streaming resumed");
                        }
                        attempts = 0;
                        match pump(tx.clone(), cap, my_generation, &generation) {
                            Ok(()) => return, // generation bumped: the fresh thread takes over
                            Err(e) => warn!("loopback capture failed: {e:#}; retrying"),
                        }
                    }
                    Err(e) => {
                        if let Some(tx) = ready_tx.take() {
                            let _ = tx.send(Err(anyhow::anyhow!("{e:#}")));
                            return; // startup failure: the caller reports it
                        }
                        attempts += 1;
                        if attempts.is_multiple_of(30) {
                            warn!(
                                attempts,
                                "speaker capture device still unavailable; retrying"
                            );
                        }
                    }
                }
                // 1 s backoff, waking early when a device switch bumps the
                // generation so the replacement thread takes over promptly.
                for _ in 0..20 {
                    if generation.load(Ordering::Relaxed) != my_generation {
                        info!("loopback capture stopping for device switch");
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("spawn capture thread: {e}"))?;
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|_| anyhow::anyhow!("capture thread died during startup"))?
}

/// An opened, started loopback stream. Created and driven on the capture
/// thread (see [`spawn`]): the COM interfaces never cross a thread boundary.
/// `device`/`client` are kept alive for the stream's whole lifetime.
struct Capture {
    name: String,
    #[allow(dead_code)]
    device: IMMDevice,
    #[allow(dead_code)]
    client: IAudioClient,
    capture: IAudioCaptureClient,
    conv: Converter,
    in_ch: usize,
    kind: SampleKind,
}

/// Open the chosen render endpoint in loopback mode, validate its mix format,
/// and start the capture stream. Runs on the capture thread; [`spawn`]
/// relays the result back so startup still fails fast.
fn start_capture(wanted: Option<&str>) -> anyhow::Result<Capture> {
    unsafe {
        let (dev_name, device) = open_render_device(wanted)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        let pwfx = client.GetMixFormat()?;
        let wf = &*pwfx;
        // WAVEFORMATEX is packed: copy fields to locals before use.
        let in_ch = wf.nChannels as usize;
        let in_rate = wf.nSamplesPerSec;
        let tag = wf.wFormatTag as u32;
        let bits = wf.wBitsPerSample;
        // WAVE_FORMAT_EXTENSIBLE wraps the real encoding in a SubFormat GUID
        // (this is what most real devices report, e.g. tag=65534 bits=32).
        let subformat: Option<u128> = if tag == WAVE_FORMAT_EXTENSIBLE {
            let ext = pwfx as *const WAVEFORMATEXTENSIBLE;
            // addr_of! + read_unaligned: safe regardless of struct packing.
            Some(
                std::ptr::addr_of!((*ext).SubFormat)
                    .read_unaligned()
                    .to_u128(),
            )
        } else {
            None
        };
        let kind = classify_mix_format(tag, bits, subformat).ok_or_else(|| {
            anyhow::anyhow!("unsupported mix format: tag={tag} bits={bits} subformat={subformat:?}")
        })?;
        info!("loopback format on '{dev_name}': {in_ch}ch {in_rate}Hz {kind:?}");

        // 20 ms buffer; shared mode, loopback flag. `Initialize` copies what it
        // needs, so free the mix format on both paths — a `?` here would
        // otherwise leak the CoTaskMemAlloc'd block.
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            200_000,
            0,
            pwfx,
            None,
        );
        CoTaskMemFree(Some(pwfx as *const c_void));
        init?;

        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        info!("loopback capture running on '{dev_name}' — play something on the PC");

        Ok(Capture {
            name: dev_name,
            device,
            client,
            capture,
            conv: Converter::new(in_rate, in_ch),
            in_ch,
            kind,
        })
    }
}

/// Friendly names of all active render (playback) endpoints, e.g.
/// `"Speakers (Realtek Audio)"`, `"Headphones (OnePlus Buds)"`.
/// Order is stable per call and doubles as the `[n]` index for selection.
pub fn list_render_devices() -> anyhow::Result<Vec<String>> {
    Ok(enum_render_endpoints()?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// Open the selected render endpoint: `None` → system default,
/// `Some(name)` → exact friendly-name match (validated up front).
fn open_render_device(wanted: Option<&str>) -> anyhow::Result<(String, IMMDevice)> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        match wanted {
            None => {
                let dev = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let name = friendly_name(&dev).unwrap_or_else(|_| "system default".into());
                Ok((name, dev))
            }
            Some(name) => {
                let endpoints = enum_render_endpoints()?;
                endpoints
                    .into_iter()
                    .find(|(n, _)| n == name)
                    .ok_or_else(|| {
                        anyhow::anyhow!("speaker device '{name}' disappeared — unplugged?")
                    })
            }
        }
    }
}

fn enum_render_endpoints() -> anyhow::Result<Vec<(String, IMMDevice)>> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let dev: IMMDevice = collection.Item(i)?;
            let name = friendly_name(&dev).unwrap_or_else(|_| format!("(unknown endpoint {i})"));
            out.push((name, dev));
        }
        Ok(out)
    }
}

fn friendly_name(device: &IMMDevice) -> anyhow::Result<String> {
    unsafe {
        let store: IPropertyStore = device.OpenPropertyStore(STGM_READ)?;
        let mut pv = store.GetValue(&PKEY_Device_FriendlyName)?;
        let name = pv
            .Anonymous
            .Anonymous
            .Anonymous
            .pwszVal
            .to_string()
            .unwrap_or_default();
        PropVariantClear(&mut pv)?;
        if name.is_empty() {
            anyhow::bail!("empty friendly name");
        }
        Ok(name)
    }
}

/// Drain capture packets into the resampler and broadcast 20 ms stereo
/// frames until the device-switch generation bumps (the supervisor then
/// starts a fresh capture thread on the new endpoint).
fn pump(
    tx: broadcast::Sender<Vec<f32>>,
    cap: Capture,
    my_generation: u64,
    generation: &AtomicU64,
) -> anyhow::Result<()> {
    let Capture {
        name: dev_name,
        device: _device,
        client: _client,
        capture,
        mut conv,
        in_ch,
        kind,
    } = cap;
    let mut frames: Vec<Vec<f32>> = Vec::new();
    unsafe {
        loop {
            // A device switch bumps the generation: exit so the supervisor's
            // fresh thread takes over on the new endpoint.
            if generation.load(Ordering::Relaxed) != my_generation {
                info!("loopback capture on '{dev_name}' stopping for device switch");
                return Ok(());
            }
            let mut packet_frames: u32 = capture.GetNextPacketSize()?;
            if packet_frames == 0 {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            while packet_frames > 0 {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut n: u32 = 0;
                let mut flags: u32 = 0;
                capture.GetBuffer(&mut data, &mut n, &mut flags, None, None)?;
                let n = n as usize;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                    conv.push(&vec![0.0; n * in_ch]);
                } else {
                    match kind {
                        SampleKind::F32 => {
                            let s = std::slice::from_raw_parts(data as *const f32, n * in_ch);
                            conv.push(s);
                        }
                        SampleKind::I16 => {
                            let s = std::slice::from_raw_parts(data as *const i16, n * in_ch);
                            conv.push(&s.iter().map(|v| i16_to_f32(*v)).collect::<Vec<_>>());
                        }
                        SampleKind::I32 => {
                            let s = std::slice::from_raw_parts(data as *const i32, n * in_ch);
                            conv.push(&s.iter().map(|v| i32_to_f32(*v)).collect::<Vec<_>>());
                        }
                    }
                }
                capture.ReleaseBuffer(n as u32)?;
                packet_frames = capture.GetNextPacketSize()?;
            }
            conv.drain(&mut frames);
            for f in frames.drain(..) {
                // If nobody is listening (phone not connected yet), drop the
                // frame but keep capturing so late joiners get audio.
                let _ = tx.send(f);
            }
        }
    }
}
