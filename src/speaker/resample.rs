//! Minimal linear resampler: arbitrary input rate/channels -> 48 kHz stereo.
//!
//! Prototype-grade (linear interpolation, not a polyphase filter), but
//! phase-correct: a fractional carry keeps long-term rate exact, so a
//! continuous tone comes out at the right pitch with no drift.

use super::{FRAME_SAMPLES, SAMPLE_RATE, CHANNELS};

/// Converts interleaved f32 source samples to 20 ms stereo frames at 48 kHz.
pub struct Converter {
    in_rate: u32,
    in_ch: usize,
    /// Buffered source samples (interleaved); `acc[0]` is absolute frame `base`.
    acc: Vec<f32>,
    base: u64,
    /// Absolute fractional input-frame position where the next output frame starts.
    pos: f64,
}

impl Converter {
    pub fn new(in_rate: u32, in_ch: usize) -> Self {
        assert!(in_ch >= 1);
        Self {
            in_rate,
            in_ch,
            acc: Vec::new(),
            base: 0,
            pos: 0.0,
        }
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        debug_assert_eq!(interleaved.len() % self.in_ch, 0);
        self.acc.extend_from_slice(interleaved);
    }

    /// Drain every complete 20 ms stereo output frame currently buffered.
    pub fn drain(&mut self, out: &mut Vec<Vec<f32>>) {
        let step = self.in_rate as f64 / SAMPLE_RATE as f64;
        loop {
            // Need one extra input frame past the last interpolated position.
            let need_abs = self.pos + (FRAME_SAMPLES - 1) as f64 * step + 1.0;
            let have_abs = self.base as f64 + (self.acc.len() / self.in_ch) as f64;
            if need_abs > have_abs {
                break;
            }
            let mut frame = Vec::with_capacity(FRAME_SAMPLES * CHANNELS);
            for i in 0..FRAME_SAMPLES {
                let rel = self.pos + i as f64 * step - self.base as f64;
                let i0 = rel.floor() as usize;
                let frac = (rel - i0 as f64) as f32;
                for c in 0..CHANNELS {
                    let a = self.at(i0, c);
                    let b = self.at(i0 + 1, c);
                    frame.push(a + (b - a) * frac);
                }
            }
            out.push(frame);
            self.pos += FRAME_SAMPLES as f64 * step;
            // Drop input frames strictly before floor(pos), keeping one for overlap.
            let first_keep = (self.pos.floor() as u64).saturating_sub(1);
            let drop_frames = (first_keep.saturating_sub(self.base) as usize)
                .min(self.acc.len() / self.in_ch);
            if drop_frames > 0 {
                self.acc.drain(..drop_frames * self.in_ch);
                self.base += drop_frames as u64;
            }
        }
    }

    /// Source sample at relative input-frame `rel`, mapped to stereo output.
    fn at(&self, rel: usize, out_ch: usize) -> f32 {
        let idx = rel * self.in_ch;
        if idx >= self.acc.len() {
            return 0.0;
        }
        if self.in_ch == 1 {
            return self.acc[idx];
        }
        if out_ch < self.in_ch {
            return self.acc[idx + out_ch];
        }
        // More than 2 input channels: fold down to mono for extra outputs.
        let mut s = 0.0;
        for c in 0..self.in_ch {
            s += self.acc[idx + c];
        }
        s / self.in_ch as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 44.1 kHz mono sine in -> 48 kHz stereo out: pitch must survive resampling.
    #[test]
    fn resample_keeps_pitch() {
        let in_rate = 44_100u32;
        let freq = 1000.0f64;
        let mut conv = Converter::new(in_rate, 1);
        // Feed 1 second of source audio in small chunks (like WASAPI packets).
        let mut phase: f64 = 0.0;
        let step = 2.0 * std::f64::consts::PI * freq / in_rate as f64;
        for _ in 0..100 {
            let chunk: Vec<f32> = (0..441).map(|_| {
                let s = phase.sin() as f32;
                phase += step;
                s
            }).collect();
            conv.push(&chunk);
        }
        let mut frames = Vec::new();
        conv.drain(&mut frames);
        assert!(!frames.is_empty());
        let left: Vec<f32> = frames.iter().flat_map(|f| f.iter().step_by(2).copied()).collect();
        // Right channel must equal left (mono duplicated).
        for f in &frames {
            for (l, r) in f.iter().step_by(2).zip(f.iter().skip(1).step_by(2)) {
                assert!((l - r).abs() < 1e-6);
            }
        }
        let mut crossings = 0;
        for w in left.windows(2) {
            if w[0] <= 0.0 && w[1] > 0.0 {
                crossings += 1;
            }
        }
        let dur = left.len() as f64 / SAMPLE_RATE as f64;
        let measured = crossings as f64 / dur;
        assert!((measured - freq).abs() < 15.0, "measured {measured} Hz");
    }
}
