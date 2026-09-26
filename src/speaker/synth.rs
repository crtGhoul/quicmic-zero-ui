//! Synthetic 440 Hz sine source: used for `--test-tone` mode and for the
//! automated end-to-end test (no audio hardware required).

use tokio::sync::broadcast;

use super::{FRAME_SAMPLES, SAMPLE_RATE};

/// Render one 20 ms stereo frame of the 440 Hz test tone, advancing `phase`.
/// Pure math (no I/O) so the frame layout is unit-testable.
fn render_frame(phase: &mut f64) -> Vec<f32> {
    let step = 2.0 * std::f64::consts::PI * 440.0 / SAMPLE_RATE as f64;
    let mut frame = Vec::with_capacity(FRAME_SAMPLES * 2);
    for _ in 0..FRAME_SAMPLES {
        let s = (phase.sin() * 0.5) as f32;
        frame.push(s);
        frame.push(s);
        *phase += step;
    }
    frame
}

pub fn spawn(tx: broadcast::Sender<Vec<f32>>) {
    std::thread::Builder::new()
        .name("synth-tone".into())
        .spawn(move || {
            let mut phase: f64 = 0.0;
            let interval = std::time::Duration::from_millis(20);
            loop {
                let start = std::time::Instant::now();
                let frame = render_frame(&mut phase);
                // Err = no listeners right now (phone hasn't connected yet) —
                // keep the tone running anyway so late joiners get audio.
                // NOTE: no `continue` here — skipping the sleep below would
                // busy-spin this thread at 100% CPU whenever nobody is
                // listening.
                let _ = tx.send(frame);
                let elapsed = start.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
            }
        })
        .expect("spawn synth thread");
}

#[cfg(test)]
mod tests {
    use super::super::FRAME_BYTES;
    use super::*;

    #[test]
    fn frame_matches_wire_layout() {
        let mut phase = 0.0;
        let frame = render_frame(&mut phase);
        // 960 samples/channel * 2 channels, f32: exactly one wire frame.
        assert_eq!(frame.len(), FRAME_SAMPLES * 2);
        assert_eq!(frame.len() * 4, FRAME_BYTES);
    }

    #[test]
    fn stereo_channels_match_and_amplitude_bounded() {
        let mut phase = 0.0;
        let frame = render_frame(&mut phase);
        for pair in frame.as_chunks::<2>().0 {
            assert_eq!(pair[0], pair[1]);
            assert!(pair[0].abs() <= 0.5);
        }
        // The tone actually oscillates — not silence, not DC.
        assert!(frame.iter().any(|&s| s > 0.4));
        assert!(frame.iter().any(|&s| s < -0.4));
    }

    #[test]
    fn phase_is_continuous_across_frames() {
        let mut phase = 0.0;
        let a = render_frame(&mut phase);
        let b = render_frame(&mut phase);
        // Sample k of the stream must equal sin(k * step) * 0.5 wherever the
        // frame boundary falls — no phase jump, no click every 20 ms.
        let step = 2.0 * std::f64::consts::PI * 440.0 / SAMPLE_RATE as f64;
        for (k, pair) in a.as_chunks::<2>().0.iter().enumerate() {
            let expected = ((k as f64 * step).sin() * 0.5) as f32;
            assert!((pair[0] - expected).abs() < 1e-6, "frame a sample {k}");
        }
        for (k, pair) in b.as_chunks::<2>().0.iter().enumerate() {
            let expected = (((FRAME_SAMPLES + k) as f64 * step).sin() * 0.5) as f32;
            assert!((pair[0] - expected).abs() < 1e-6, "frame b sample {k}");
        }
    }

    #[test]
    fn tone_is_440hz() {
        let mut phase = 0.0;
        let mut prev = 0.0f32;
        let mut crossings = 0u32;
        let mut total = 0u32;
        // One second of frames, left channel only.
        for _ in 0..50 {
            for s in render_frame(&mut phase).into_iter().step_by(2) {
                if prev <= 0.0 && s > 0.0 {
                    crossings += 1;
                }
                prev = s;
                total += 1;
            }
        }
        assert_eq!(total, SAMPLE_RATE);
        let measured = crossings as f64 / (total as f64 / SAMPLE_RATE as f64);
        assert!((measured - 440.0).abs() < 2.0, "measured {measured} Hz");
    }
}
