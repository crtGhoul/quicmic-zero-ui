//! PC-to-phone speaker streaming: WASAPI loopback capture (Windows) or a
//! synthetic test tone, resampled to 48 kHz stereo and fanned out as 20 ms
//! PCM frames over the `/speaker-ws` WebSocket.
//!
//! Wire format: every binary WS message is exactly one frame =
//! 960 samples/channel * 2 channels * 4 bytes = 7680 bytes, little-endian f32,
//! interleaved stereo. No headers, no negotiation.

pub mod format;
pub mod resample;
pub mod synth;
#[cfg(windows)]
pub mod wasapi;

/// Samples per channel in one frame (20 ms at 48 kHz).
pub const FRAME_SAMPLES: usize = 960;
pub const CHANNELS: usize = 2;
pub const SAMPLE_RATE: u32 = 48_000;
/// Exact byte length of one binary WS message.
pub const FRAME_BYTES: usize = FRAME_SAMPLES * CHANNELS * 4;
