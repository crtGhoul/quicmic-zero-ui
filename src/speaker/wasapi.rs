//! WASAPI loopback capture (Windows only).
//!
//! Opens the default *render* endpoint in loopback mode, which yields exactly
//! what the speakers would play — i.e. all PC audio mixed by the system.
//! Captured audio is converted to 48 kHz stereo f32 and emitted as 20 ms frames.

use std::ffi::c_void;

use tokio::sync::broadcast;
use tracing::{error, info};
use windows::Win32::{Media::Audio::*, System::Com::*};

use super::format::{
    classify_mix_format, i16_to_f32, i32_to_f32, SampleKind, WAVE_FORMAT_EXTENSIBLE,
};
use super::resample::Converter;

pub fn spawn(tx: broadcast::Sender<Vec<f32>>) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("wasapi-loopback".into())
        .spawn(move || {
            if let Err(e) = run(tx) {
                error!("loopback capture failed: {e:#}");
            }
        })
        .map_err(|e| anyhow::anyhow!("spawn capture thread: {e}"))?;
    Ok(())
}

fn run(tx: broadcast::Sender<Vec<f32>>) -> anyhow::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device: IMMDevice = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
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
            Some(std::ptr::addr_of!((*ext).SubFormat).read_unaligned().to_u128())
        } else {
            None
        };
        let kind = classify_mix_format(tag, bits, subformat).ok_or_else(|| {
            anyhow::anyhow!("unsupported mix format: tag={tag} bits={bits} subformat={subformat:?}")
        })?;
        info!("loopback format: {in_ch}ch {in_rate}Hz {kind:?}");

        // 20 ms buffer; shared mode, loopback flag.
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK as u32,
            200_000,
            0,
            pwfx,
            None,
        )?;
        CoTaskMemFree(Some(pwfx as *const c_void));

        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        info!("loopback capture running — play something on the PC");

        let mut conv = Converter::new(in_rate, in_ch);
        let mut frames: Vec<Vec<f32>> = Vec::new();

        loop {
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
