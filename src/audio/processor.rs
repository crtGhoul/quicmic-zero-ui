//! Incoming-packet decoding: parse little-endian i16 PCM and push it to the ring.
//!
//! Basic input processing (noise gate and gain) runs client-side in the
//! AudioWorklet (`web/worklet.js`). Speech-focused noise cancellation runs
//! here on the PC when the caller passes the shared `SpeechDenoiser`: the
//! decoded samples are denoised on this network-receive task (never in the
//! cpal output callback) before they enter the ring.

use super::denoise::SpeechDenoiser;
use super::ring_buffer::RingBuffer;

/// Maximum samples per packet (480 = 10ms at 48kHz).
pub const MAX_SAMPLES_PER_PACKET: usize = 480;

/// Decode little-endian i16 PCM from `pcm_bytes` and push it into `ring`, plus
/// into `monitor_ring` when a hear-yourself monitor stream is active.
///
/// The packet is decoded once into a stack buffer and the same samples are
/// pushed to both rings, so the dual-consumer case costs no extra decode work
/// and no allocation on the hot path. Each ring keeps its own SPSC contract —
/// the single producer just pushes to both.
///
/// `MAX_SAMPLES_PER_PACKET` caps how much a single frame can contribute, so a
/// malformed or oversize frame can never write more than one packet's worth (the
/// WebTransport path also rejects oversize datagrams up front). A well-formed
/// frame is always within the cap. A trailing odd byte, if any, is ignored by
/// `as_chunks`.
pub fn decode_into_rings(
    pcm_bytes: &[u8],
    ring: &RingBuffer,
    monitor_ring: Option<&RingBuffer>,
    denoiser: Option<&parking_lot::Mutex<SpeechDenoiser>>,
) {
    let mut samples = [0i16; MAX_SAMPLES_PER_PACKET];
    let mut count = 0;
    let (chunks, _) = pcm_bytes.as_chunks::<2>();
    for chunk in chunks.iter().take(MAX_SAMPLES_PER_PACKET) {
        samples[count] = i16::from_le_bytes(*chunk);
        count += 1;
    }
    if count == 0 {
        return;
    }
    // Speech-focused noise cancellation (Gate 5): when a denoiser is attached
    // and enabled, push the cleaned samples instead. The monitor ring hears
    // exactly what apps on the PC will hear.
    if let Some(denoiser) = denoiser {
        let mut denoiser = denoiser.lock();
        if denoiser.enabled() {
            let mut cleaned = [0i16; MAX_SAMPLES_PER_PACKET];
            let n = denoiser.process(&samples[..count], &mut cleaned);
            if n > 0 {
                ring.push(&cleaned[..n]);
                if let Some(monitor) = monitor_ring {
                    monitor.push(&cleaned[..n]);
                }
            }
            return;
        }
    }
    ring.push(&samples[..count]);
    if let Some(monitor) = monitor_ring {
        monitor.push(&samples[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_into_rings, MAX_SAMPLES_PER_PACKET};
    use crate::audio::RingBuffer;

    #[test]
    fn decodes_le_i16_into_ring() {
        let ring = RingBuffer::new(64);
        // Two little-endian i16 samples: 1 and -1.
        let bytes = [0x01, 0x00, 0xff, 0xff];
        decode_into_rings(&bytes, &ring, None, None);
        assert_eq!(ring.len(), 2);
        let mut out = [0i16; 2];
        ring.pop(&mut out);
        assert_eq!(out, [1, -1]);
    }

    #[test]
    fn caps_at_max_samples_per_packet() {
        let ring = RingBuffer::new(4096);
        let bytes = vec![0u8; (MAX_SAMPLES_PER_PACKET + 50) * 2];
        decode_into_rings(&bytes, &ring, None, None);
        assert_eq!(ring.len(), MAX_SAMPLES_PER_PACKET);
    }

    #[test]
    fn dual_decode_pushes_identical_samples_to_both_rings() {
        let ring = RingBuffer::new(4096);
        let monitor = RingBuffer::new(4096);
        // Two little-endian i16 samples: 1 and -1.
        let bytes = [0x01, 0x00, 0xff, 0xff];
        decode_into_rings(&bytes, &ring, Some(&monitor), None);
        assert_eq!(ring.len(), 2);
        assert_eq!(monitor.len(), 2);
        let mut out = [0i16; 2];
        ring.pop(&mut out);
        assert_eq!(out, [1, -1]);
        monitor.pop(&mut out);
        assert_eq!(out, [1, -1]);
    }

    #[test]
    fn dual_decode_without_monitor_matches_single_decode() {
        let ring = RingBuffer::new(4096);
        let bytes = [0x01, 0x00, 0xff, 0xff];
        decode_into_rings(&bytes, &ring, None, None);
        assert_eq!(ring.len(), 2);
    }
}
