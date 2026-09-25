//! Synthetic 440 Hz sine source: used for `--test-tone` mode and for the
//! automated end-to-end test (no audio hardware required).

use tokio::sync::broadcast;

use super::{FRAME_SAMPLES, SAMPLE_RATE};

pub fn spawn(tx: broadcast::Sender<Vec<f32>>) {
    std::thread::Builder::new()
        .name("synth-tone".into())
        .spawn(move || {
            let mut phase: f64 = 0.0;
            let step = 2.0 * std::f64::consts::PI * 440.0 / SAMPLE_RATE as f64;
            let interval = std::time::Duration::from_millis(20);
            loop {
                let start = std::time::Instant::now();
                let mut frame = Vec::with_capacity(FRAME_SAMPLES * 2);
                for _ in 0..FRAME_SAMPLES {
                    let s = (phase.sin() * 0.5) as f32;
                    frame.push(s);
                    frame.push(s);
                    phase += step;
                }
                if tx.send(frame).is_err() {
                    // No listeners right now (phone hasn't connected yet) —
                    // keep the tone running so late joiners get audio.
                    continue;
                }
                let elapsed = start.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
            }
        })
        .expect("spawn synth thread");
}
