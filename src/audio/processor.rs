//! Incoming-packet decoding: parse little-endian i16 PCM and push it to the ring.
//!
//! Audio *input* processing (noise gate and gain) runs client-side in the
//! AudioWorklet (`web/worklet.js`), so the server is a pure passthrough on the
//! hot path — it just decodes the bytes and hands them to the output stage.

use super::ring_buffer::RingBuffer;

/// Maximum samples per packet (480 = 10ms at 48kHz).
pub const MAX_SAMPLES_PER_PACKET: usize = 480;

/// Decode little-endian i16 PCM from `pcm_bytes` and push it into `ring`, plus
/// into `monitor_ring` when a hear-yourself monitor stream is active, plus
/// into `test_capture` for the PC-side "Test mic" button.
///
/// The packet is decoded once into a stack buffer and the same samples are
/// pushed to every consumer, so the extra taps cost no additional decode work
/// and no allocation on the hot path. Each ring keeps its own SPSC contract —
/// the single producer just pushes to all of them.
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
    test_capture: Option<&MicTestBuffer>,
) {
    let mut samples = [0i16; MAX_SAMPLES_PER_PACKET];
    let mut count = 0;
    let (chunks, _) = pcm_bytes.as_chunks::<2>();
    for chunk in chunks.iter().take(MAX_SAMPLES_PER_PACKET) {
        samples[count] = i16::from_le_bytes(*chunk);
        count += 1;
    }
    if count > 0 {
        ring.push(&samples[..count]);
        if let Some(monitor) = monitor_ring {
            monitor.push(&samples[..count]);
        }
        if let Some(capture) = test_capture {
            capture.push(&samples[..count]);
        }
    }
}

/// Seconds of incoming mic audio retained for the PC-side "Test mic" button.
const TEST_CAPTURE_SECS: usize = 5;
/// Sizing rate for the capture buffer: 5 s at 48 kHz mono 16-bit = 240,000
/// samples ≈ 480 KB. The playback resampler follows the live
/// `source_sample_rate`, so other capture rates still play back correctly —
/// they just fill a slightly shorter/longer window of time.
const TEST_CAPTURE_CAPACITY: usize = TEST_CAPTURE_SECS * 48_000;

/// Rolling capture of the most recent incoming mic audio, tapped on the
/// decode hot path and snapshotted by the GUI when the user hits "Test mic".
///
/// Written only by the transport threads (the single producer, inside
/// `decode_into_rings`) and read only by the GUI-triggered snapshot — a
/// short `parking_lot` lock on each side, never held across an `.await`.
/// Fixed-size: once full, the oldest samples are overwritten, so memory stays
/// flat at ~480 KB no matter how long the stream runs.
pub struct MicTestBuffer {
    inner: parking_lot::Mutex<MicTestInner>,
}

struct MicTestInner {
    buf: Box<[i16]>,
    /// Index the next pushed sample overwrites (also the oldest sample once full).
    write_pos: usize,
    /// Valid samples currently held, capped at the buffer capacity.
    filled: usize,
}

impl MicTestBuffer {
    pub fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(MicTestInner {
                buf: vec![0i16; TEST_CAPTURE_CAPACITY].into_boxed_slice(),
                write_pos: 0,
                filled: 0,
            }),
        }
    }

    /// Append decoded mic samples. Cheap: one uncontended lock plus a memcpy.
    pub fn push(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        let cap = inner.buf.len();
        let write_pos = inner.write_pos;
        // Two contiguous segments handle the wrap-around without per-sample modulo.
        let first = samples.len().min(cap - write_pos);
        inner.buf[write_pos..write_pos + first].copy_from_slice(&samples[..first]);
        let second = samples.len() - first;
        if second > 0 {
            // A single packet can never exceed the capacity (capped upstream
            // at MAX_SAMPLES_PER_PACKET), so one wrap is always enough.
            inner.buf[..second].copy_from_slice(&samples[first..]);
        }
        inner.write_pos = (write_pos + samples.len()) % cap;
        inner.filled = (inner.filled + samples.len()).min(cap);
    }

    /// True once at least one packet has been captured.
    pub fn has_samples(&self) -> bool {
        self.inner.lock().filled > 0
    }

    /// Copy the buffered audio oldest-first. One short lock, no allocation on
    /// the hot path (the Vec is the snapshot itself).
    pub fn snapshot(&self) -> Vec<i16> {
        let inner = self.inner.lock();
        let mut out = Vec::with_capacity(inner.filled);
        if inner.filled == 0 {
            return out;
        }
        let cap = inner.buf.len();
        // Before the first wrap the data starts at zero; once full the oldest
        // sample sits at the write cursor.
        let start = if inner.filled < cap {
            0
        } else {
            inner.write_pos
        };
        let first = (cap - start).min(inner.filled);
        out.extend_from_slice(&inner.buf[start..start + first]);
        out.extend_from_slice(&inner.buf[..inner.filled - first]);
        out
    }
}

impl Default for MicTestBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_into_rings, MicTestBuffer, MAX_SAMPLES_PER_PACKET};
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

    #[test]
    fn test_capture_tap_receives_decoded_samples() {
        let ring = RingBuffer::new(4096);
        let capture = MicTestBuffer::new();
        assert!(!capture.has_samples());
        let bytes = [0x01, 0x00, 0xff, 0xff];
        decode_into_rings(&bytes, &ring, None, Some(&capture));
        assert!(capture.has_samples());
        assert_eq!(capture.snapshot(), vec![1, -1]);
        // The main ring is unaffected by the extra tap.
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn test_capture_snapshot_is_oldest_first() {
        let capture = MicTestBuffer::new();
        capture.push(&[1, 2, 3]);
        capture.push(&[4, 5]);
        assert_eq!(capture.snapshot(), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_capture_empty_snapshot_is_empty() {
        let capture = MicTestBuffer::new();
        assert!(capture.snapshot().is_empty());
    }

    #[test]
    fn test_capture_overwrites_oldest_when_full() {
        let capture = MicTestBuffer::new();
        // Fill the buffer exactly, then push one more packet: the oldest
        // samples must fall off and the snapshot stays at capacity.
        let cap = super::TEST_CAPTURE_CAPACITY;
        let chunk = vec![7i16; 480];
        for _ in 0..cap / 480 {
            capture.push(&chunk);
        }
        assert_eq!(capture.snapshot().len(), cap);
        capture.push(&[1, 2, 3]);
        let snap = capture.snapshot();
        assert_eq!(snap.len(), cap);
        // The three oldest 7s were evicted; the new samples are at the tail.
        assert_eq!(&snap[cap - 3..], &[1, 2, 3]);
        assert!(snap[..cap - 3].iter().all(|&s| s == 7));
    }
}
