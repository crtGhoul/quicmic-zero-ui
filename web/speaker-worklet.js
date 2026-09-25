'use strict';
/* AudioWorklet: pulls stereo-interleaved f32 frames from a queue and plays them.
   Reports RMS level + underrun counts back to the main thread. */

class PcmPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];   // Float32Array frames, stereo interleaved
    this.cur = null;
    this.ci = 0;
    // Jitter buffer: don't start (or resume) playback until this many frames
    // are queued. Absorbs Wi-Fi jitter at the cost of a little latency.
    // Set from the main thread via { prebuffer: n }; 20 ms per frame.
    this.prebuffer = 5;
    this.primed = false;
    this.blockUnderruns = 0;  // process() calls that had any gap
    this.totalUnderruns = 0;
    this.rmsAcc = 0;
    this.rmsN = 0;
    this.msgTick = 0;
    this.port.onmessage = (e) => {
      const d = e.data;
      if (d instanceof ArrayBuffer) {
        this.queue.push(new Float32Array(d));
        // bound memory if the network outruns playback absurdly
        if (this.queue.length > 250) this.queue.splice(0, this.queue.length - 250);
        return;
      }
      // Control message from the main thread.
      if (d && typeof d === 'object' && typeof d.prebuffer === 'number') {
        const p = Math.max(0, Math.min(25, d.prebuffer | 0));
        if (p !== this.prebuffer) {
          this.prebuffer = p;
          this.unprime(); // re-buffer at the new depth: brief pause, then smooth
        }
      }
    };
  }

  // Drop any partial frame and wait for the queue to refill to the prebuffer
  // depth before resuming. Called on underrun and on latency-setting changes.
  unprime() {
    this.primed = false;
    this.cur = null;
    this.ci = 0;
  }

  // One stereo pair; [0,0] + gap flag when the queue is dry.
  nextPair() {
    for (;;) {
      if (this.cur && this.ci + 2 <= this.cur.length) {
        const l = this.cur[this.ci], r = this.cur[this.ci + 1];
        this.ci += 2;
        if (this.ci >= this.cur.length) { this.cur = null; this.ci = 0; }
        return [l, r, false];
      }
      this.cur = this.queue.shift() || null;
      this.ci = 0;
      if (!this.cur) return [0, 0, true];
    }
  }

  process(inputs, outputs) {
    const out = outputs[0];
    const n = out[0].length;
    const stereo = out.length > 1;
    // Not enough buffered yet: hold silence until the jitter buffer fills.
    if (!this.primed) {
      if (this.queue.length >= this.prebuffer) {
        this.primed = true;
      } else {
        for (let i = 0; i < n; i++) {
          out[0][i] = 0;
          if (stereo) out[1][i] = 0;
        }
        this.report();
        return true;
      }
    }
    let gap = false;
    for (let i = 0; i < n; i++) {
      const [l, r, g] = this.nextPair();
      if (g) gap = true;
      out[0][i] = l;
      if (stereo) out[1][i] = r;
      this.rmsAcc += l * l + r * r;
      this.rmsN += 2;
    }
    if (gap) {
      this.blockUnderruns++;
      this.unprime(); // drained mid-stream: pause and re-buffer instead of stuttering
    }
    this.report();
    return true;
  }

  // Throttled stats back to the main thread (every 20 blocks).
  report() {
    if (++this.msgTick >= 20) {
      this.port.postMessage({
        rms: Math.sqrt(this.rmsAcc / Math.max(1, this.rmsN)),
        under: this.blockUnderruns,
      });
      this.totalUnderruns += this.blockUnderruns;
      this.blockUnderruns = 0;
      this.rmsAcc = 0; this.rmsN = 0; this.msgTick = 0;
    }
  }
}

registerProcessor('pcm-player', PcmPlayer);
