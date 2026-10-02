//! Speech-focused noise cancellation for the mic receive path.
//!
//! Wraps RNNoise (via the pure-Rust `nnnoiseless` port) plus a voice-activity
//! gate. RNNoise suppresses non-speech noise (fans, hum, keyboard clatter,
//! TV in another room) inside each 10 ms frame, and the gate additionally
//! fades frames with no detected speech toward near-silence, so between
//! sentences the PC output does not leak residual room noise.
//!
//! This detects *human speech in general* — it does not identify any specific
//! speaker. There is no voiceprint, enrollment, or biometrics.
//!
//! Placement: this runs in `processor::decode_into_rings` on the network
//! receive task, *not* in the cpal output callback, so its cost (tens of µs
//! per 10 ms frame) and its mutex never sit in the real-time path. All
//! buffers are fixed-size stack arrays; `process` allocates nothing.
//!
//! RNNoise is a 48 kHz / 480-sample (10 ms) model. Added latency: a partial
//! frame is held until it completes (at most one frame, ~10 ms) plus
//! RNNoise's own one-frame algorithmic delay, whose first (all-zero) output
//! frame is dropped at startup. Capture at other sample rates still works;
//! the model simply sees the audio slightly pitch/time-shifted.

use nnnoiseless::DenoiseState;

/// RNNoise frame size: 480 samples = 10 ms at 48 kHz.
pub const FRAME_SAMPLES: usize = 480;
/// Frames the gate stays open after the last speech frame (~250 ms at
/// 10 ms/frame), so word endings and short pauses are not clipped.
const HANGOVER_FRAMES: u32 = 25;
/// Residual gain when the gate is closed (~-34 dB): near-silence, but not a
/// hard mute, which avoids an unnatural gated on/off feel.
const GATE_FLOOR: f32 = 0.02;
/// Per-frame gain smoothing: fast attack when speech starts, slower release.
const GAIN_ATTACK: f32 = 0.4;
const GAIN_RELEASE: f32 = 0.10;
/// Default voice-probability threshold for the gate.
pub const DEFAULT_VAD_THRESHOLD: f32 = 0.5;

/// Pure voice-activity gate policy: maps RNNoise's per-frame voice
/// probability to a smoothed gain, with hangover. Kept separate from the
/// neural model so the policy is unit-testable on its own.
#[derive(Debug)]
pub struct VoiceGate {
    threshold: f32,
    hangover_left: u32,
    gain: f32,
}

impl VoiceGate {
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold: threshold.clamp(0.0, 1.0),
            hangover_left: 0,
            gain: GATE_FLOOR,
        }
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.clamp(0.0, 1.0);
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Current smoothed gain (after the most recent `step`). Test-only: the
    /// production path consumes `step`'s returned ramp directly.
    #[cfg(test)]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Advance one frame with the model's voice probability. Returns the
    /// (start, end) gain for the frame so the caller can ramp linearly
    /// across the frame's samples instead of stepping (which would click).
    pub fn step(&mut self, voice_prob: f32) -> (f32, f32) {
        let start = self.gain;
        if voice_prob >= self.threshold {
            self.hangover_left = HANGOVER_FRAMES;
        }
        let open = self.hangover_left > 0;
        if open {
            self.hangover_left -= 1;
        }
        let target = if open { 1.0 } else { GATE_FLOOR };
        let rate = if target > self.gain {
            GAIN_ATTACK
        } else {
            GAIN_RELEASE
        };
        self.gain += (target - self.gain) * rate;
        (start, self.gain)
    }
}

/// Stateful speech denoiser for the single mic stream.
///
/// One instance lives in `StreamState` behind a mutex. The receive path is
/// the only producer, so the lock is uncontended in practice; settings
/// changes take the same lock from the HTTP handler (never on the audio
/// callback).
pub struct SpeechDenoiser {
    state: Box<DenoiseState<'static>>,
    gate: VoiceGate,
    enabled: bool,
    in_f32: [f32; FRAME_SAMPLES],
    out_f32: [f32; FRAME_SAMPLES],
    pending: [i16; FRAME_SAMPLES],
    pending_len: usize,
    ready: [i16; FRAME_SAMPLES],
    ready_len: usize,
    ready_pos: usize,
    /// RNNoise's first output frame corresponds to no input (one-frame
    /// algorithmic delay) and is conventionally discarded.
    primed: bool,
}

impl Default for SpeechDenoiser {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechDenoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            gate: VoiceGate::new(DEFAULT_VAD_THRESHOLD),
            enabled: true,
            in_f32: [0.0; FRAME_SAMPLES],
            out_f32: [0.0; FRAME_SAMPLES],
            pending: [0; FRAME_SAMPLES],
            pending_len: 0,
            ready: [0; FRAME_SAMPLES],
            ready_len: 0,
            ready_pos: 0,
            primed: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            // Drop any buffered frames so re-enabling never plays stale audio.
            self.pending_len = 0;
            self.ready_len = 0;
            self.ready_pos = 0;
        }
    }

    pub fn vad_threshold(&self) -> f32 {
        self.gate.threshold()
    }

    pub fn set_vad_threshold(&mut self, threshold: f32) {
        self.gate.set_threshold(threshold);
    }

    /// Process `input` samples, writing up to `out.len()` processed samples
    /// and returning how many were written. When disabled this is a plain
    /// copy. When enabled, output lags input by less than one frame (plus
    /// the model's own one-frame delay) because partial frames are buffered
    /// until they complete a 480-sample RNNoise frame. No allocation.
    pub fn process(&mut self, input: &[i16], out: &mut [i16]) -> usize {
        if !self.enabled {
            let n = input.len().min(out.len());
            out[..n].copy_from_slice(&input[..n]);
            return n;
        }
        let mut read = 0;
        let mut written = 0;
        while written < out.len() {
            if self.ready_pos < self.ready_len {
                let k = (self.ready_len - self.ready_pos).min(out.len() - written);
                out[written..written + k]
                    .copy_from_slice(&self.ready[self.ready_pos..self.ready_pos + k]);
                self.ready_pos += k;
                written += k;
                if self.ready_pos == self.ready_len {
                    self.ready_pos = 0;
                    self.ready_len = 0;
                }
                continue;
            }
            if read >= input.len() {
                break;
            }
            let take = (FRAME_SAMPLES - self.pending_len).min(input.len() - read);
            self.pending[self.pending_len..self.pending_len + take]
                .copy_from_slice(&input[read..read + take]);
            self.pending_len += take;
            read += take;
            if self.pending_len == FRAME_SAMPLES {
                self.run_frame();
            }
        }
        written
    }

    /// Run one full 480-sample frame through RNNoise + the gate into `ready`.
    fn run_frame(&mut self) {
        for (dst, src) in self.in_f32.iter_mut().zip(self.pending.iter()) {
            *dst = *src as f32;
        }
        // RNNoise convention: f32 samples in i16 range, and the returned
        // value is the voice-activity probability for the frame.
        let voice_prob = self.state.process_frame(&mut self.out_f32, &self.in_f32);
        if !self.primed {
            self.primed = true;
            self.ready = [0; FRAME_SAMPLES];
        } else {
            let (g0, g1) = self.gate.step(voice_prob.clamp(0.0, 1.0));
            for (i, dst) in self.ready.iter_mut().enumerate() {
                let ramp = (i + 1) as f32 / FRAME_SAMPLES as f32;
                let gain = g0 + (g1 - g0) * ramp;
                *dst = (self.out_f32[i] * gain).clamp(-32_768.0, 32_767.0) as i16;
            }
        }
        self.ready_len = FRAME_SAMPLES;
        self.ready_pos = 0;
        self.pending_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{SpeechDenoiser, VoiceGate, FRAME_SAMPLES, GATE_FLOOR};

    #[test]
    fn gate_opens_for_speech_and_holds_over_pauses() {
        let mut gate = VoiceGate::new(0.5);
        // A speech frame opens the gate.
        gate.step(0.9);
        assert!(gate.gain() > GATE_FLOOR);
        // ~200 ms of non-speech right after still passes (hangover).
        for _ in 0..20 {
            gate.step(0.0);
        }
        assert!(gate.gain() > 0.5, "hangover should keep the gate open");
        // Long after the hangover expires the gate fades to the floor.
        for _ in 0..200 {
            gate.step(0.0);
        }
        assert!(gate.gain() < 0.05, "gate should close, got {}", gate.gain());
    }

    #[test]
    fn gate_stays_closed_for_noise_only() {
        let mut gate = VoiceGate::new(0.5);
        for _ in 0..100 {
            gate.step(0.1);
        }
        assert!(
            gate.gain() <= GATE_FLOOR + 0.001,
            "noise-only frames must stay attenuated, got {}",
            gate.gain()
        );
    }

    #[test]
    fn disabled_denoiser_is_passthrough() {
        let mut d = SpeechDenoiser::new();
        d.set_enabled(false);
        let input = [100i16, -200, 300, -400];
        let mut out = [0i16; 4];
        assert_eq!(d.process(&input, &mut out), 4);
        assert_eq!(out, input);
    }

    #[test]
    fn partial_frame_is_buffered_until_complete() {
        let mut d = SpeechDenoiser::new();
        let mut out = [0i16; FRAME_SAMPLES];
        // Half a frame in: nothing can come out yet.
        assert_eq!(d.process(&[0i16; 240], &mut out), 0);
        // Completing the frame flushes the (discarded) first RNNoise frame.
        let n = d.process(&[0i16; 240], &mut out);
        assert_eq!(n, FRAME_SAMPLES);
    }

    #[test]
    fn silence_stays_silent() {
        let mut d = SpeechDenoiser::new();
        let silence = [0i16; FRAME_SAMPLES];
        let mut out = [0i16; FRAME_SAMPLES];
        for _ in 0..10 {
            d.process(&silence, &mut out);
        }
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn loud_harmonic_signal_is_not_erased() {
        // Best-effort smoke test (perceptual quality needs the real-Windows
        // smoke test): a loud voice-like stack of harmonics must survive at
        // clearly non-silent energy once the gate has opened. This also
        // catches a swapped input/output argument to process_frame, which
        // would leave the gate permanently closed on real audio.
        let mut d = SpeechDenoiser::new();
        let mut out = [0i16; FRAME_SAMPLES];
        let mut out_energy = 0f64;
        let mut in_energy = 0f64;
        for frame in 0..30 {
            let mut input = [0i16; FRAME_SAMPLES];
            for (i, s) in input.iter_mut().enumerate() {
                let t = (frame * FRAME_SAMPLES + i) as f32 / 48_000.0;
                let v = 0.5 * (2.0 * std::f32::consts::PI * 120.0 * t).sin()
                    + 0.3 * (2.0 * std::f32::consts::PI * 240.0 * t).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * 360.0 * t).sin();
                *s = (v * 20_000.0) as i16;
            }
            in_energy += input.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
            d.process(&input, &mut out);
            out_energy += out.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
        }
        assert!(
            out_energy > 0.01 * in_energy,
            "voiced-like signal was erased: out={out_energy} in={in_energy}"
        );
    }
}
