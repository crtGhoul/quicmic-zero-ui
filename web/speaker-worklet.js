'use strict';
/* AudioWorklet: pulls stereo-interleaved f32 frames from a queue and plays them.
   Reports RMS level + underrun counts back to the main thread. */

class PcmPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];   // Float32Array frames, stereo interleaved
    this.cur = null;
    this.ci = 0;
    this.blockUnderruns = 0;  // process() calls that had any gap
    this.totalUnderruns = 0;
    this.rmsAcc = 0;
    this.rmsN = 0;
    this.msgTick = 0;
    this.port.onmessage = (e) => {
      this.queue.push(new Float32Array(e.data));
      // bound memory if the network outruns playback absurdly
      if (this.queue.length > 250) this.queue.splice(0, this.queue.length - 250);
    };
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
    let gap = false;
    for (let i = 0; i < n; i++) {
      const [l, r, g] = this.nextPair();
      if (g) gap = true;
      out[0][i] = l;
      if (stereo) out[1][i] = r;
      this.rmsAcc += l * l + r * r;
      this.rmsN += 2;
    }
    if (gap) this.blockUnderruns++;
    if (++this.msgTick >= 20) {
      this.port.postMessage({
        rms: Math.sqrt(this.rmsAcc / Math.max(1, this.rmsN)),
        under: this.blockUnderruns,
      });
      this.totalUnderruns += this.blockUnderruns;
      this.blockUnderruns = 0;
      this.rmsAcc = 0; this.rmsN = 0; this.msgTick = 0;
    }
    return true;
  }
}

registerProcessor('pcm-player', PcmPlayer);
