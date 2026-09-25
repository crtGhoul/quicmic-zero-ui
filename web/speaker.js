'use strict';
/* Speaker tab: PC audio -> this phone -> Bluetooth earbuds.
   Same-origin pages share localStorage, so the pairing token the Mic tab
   saved is reused here; the server gates /speaker-ws on it. */

const $ = (id) => document.getElementById(id);
const LS_VOL = 'spk_volume', LS_AUTOCONN = 'spk_autoconnect';

const meterCtx = $('meter').getContext('2d');

let ctx = null, player = null, gainNode = null, ws = null;
let frames = 0, underruns = 0, startedAt = 0, live = false;
let volume = 0.8, autoConn = true;
let toneNodes = null;

function setStatus(cls, text) {
  $('status').className = 'status' + (cls ? ' ' + cls : '');
  $('statusText').textContent = text;
}

function token() {
  try { return localStorage.getItem('sessionToken') || ''; } catch (e) { return ''; }
}

function drawMeter(rms) {
  const w = 340, h = 26;
  meterCtx.fillStyle = '#0b0b12';
  meterCtx.fillRect(0, 0, w, h);
  const bw = Math.floor(Math.min(1, rms * 3) * w);
  const grad = meterCtx.createLinearGradient(0, 0, w, 0);
  grad.addColorStop(0, '#00e5ff');
  grad.addColorStop(0.7, '#3d7bff');
  grad.addColorStop(1, '#ff5d5d');
  meterCtx.fillStyle = grad;
  meterCtx.fillRect(0, 0, bw, h);
}

function teardown() {
  live = false;
  try { if (ws) ws.close(); } catch (e) { /* noop */ }
  ws = null;
  try { if (ctx) ctx.close(); } catch (e) { /* noop */ }
  ctx = null; player = null; gainNode = null;
  const b = $('btnConnect');
  b.classList.remove('live');
  b.innerHTML = 'TAP TO<br>CONNECT';
  drawMeter(0);
}

async function connect() {
  if (live) {
    teardown();
    setStatus('', 'not connected');
    return;
  }
  const tok = token();
  if (!tok) {
    setStatus('bad', 'pair on the Mic tab first');
    return;
  }
  try {
    setStatus('', 'starting audio…');
    ctx = new (window.AudioContext || window.webkitAudioContext)({
      sampleRate: 48000, latencyHint: 'interactive',
    });
    await ctx.resume();
    await ctx.audioWorklet.addModule('speaker-worklet.js');
    player = new AudioWorkletNode(ctx, 'pcm-player', { outputChannelCount: [2] });
    gainNode = ctx.createGain();
    gainNode.gain.value = volume;
    player.connect(gainNode);
    gainNode.connect(ctx.destination);
    player.port.onmessage = (e) => {
      const d = e.data || {};
      if (typeof d.rms === 'number') drawMeter(d.rms);
      if (d.under) underruns += d.under;
    };

    const proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
    ws = new WebSocket(proto + location.host + '/speaker-ws?token=' + encodeURIComponent(tok));
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      live = true;
      startedAt = Date.now();
      frames = 0; underruns = 0;
      const b = $('btnConnect');
      b.classList.add('live');
      b.innerHTML = 'LIVE<br><span class="btnsub">tap to stop</span>';
      setStatus('ok', 'streaming — play audio on the PC');
    };
    ws.onmessage = (ev) => {
      frames++;
      // Hand the buffer to the worklet without copying.
      player.port.postMessage(ev.data, [ev.data]);
    };
    ws.onclose = () => {
      if (live) {
        setStatus('bad', 'disconnected');
        teardown();
      }
    };
    ws.onerror = () => {
      setStatus('bad', 'connection error — is the PC app running?');
      teardown();
    };
  } catch (err) {
    setStatus('bad', 'audio init failed: ' + err.message);
    teardown();
  }
}

function applyVolume(v) {
  volume = Math.min(1, Math.max(0, v));
  if (gainNode) gainNode.gain.value = volume;
  try { localStorage.setItem(LS_VOL, String(Math.round(volume * 100))); } catch (e) { /* noop */ }
}

function toggleTone() {
  if (toneNodes) {
    try { toneNodes.osc.stop(); } catch (e) { /* noop */ }
    toneNodes = null;
    $('btnTone').textContent = 'Play local test tone';
    return;
  }
  const start = async () => {
    const c = ctx || new (window.AudioContext || window.webkitAudioContext)();
    if (c.state === 'suspended') await c.resume();
    const osc = c.createOscillator();
    const g = c.createGain();
    osc.frequency.value = 880;
    g.gain.value = 0.2;
    osc.connect(g);
    g.connect(c.destination);
    osc.start();
    toneNodes = { osc };
    if (!ctx) ctx = c;
    $('btnTone').textContent = 'Stop test tone';
    setStatus('', 'local 880 Hz tone — should come from your earbuds');
  };
  start().catch((err) => setStatus('bad', 'tone failed: ' + err.message));
}

(function init() {
  try {
    const v = parseInt(localStorage.getItem(LS_VOL), 10);
    if (!isNaN(v)) volume = Math.min(100, Math.max(0, v)) / 100;
    autoConn = localStorage.getItem(LS_AUTOCONN) !== '0';
  } catch (e) { /* noop */ }
  $('volSlider').value = Math.round(volume * 100);
  $('autoConnChk').checked = autoConn;
  $('volSlider').addEventListener('input', (e) => applyVolume(e.target.value / 100));
  $('autoConnChk').addEventListener('change', (e) => {
    autoConn = e.target.checked;
    try { localStorage.setItem(LS_AUTOCONN, autoConn ? '1' : '0'); } catch (e2) { /* noop */ }
    if (autoConn && !live) connect();
  });
  $('btnConnect').addEventListener('click', connect);
  $('btnTone').addEventListener('click', toggleTone);
  setStatus('', token() ? 'not connected' : 'pair on the Mic tab first');
  if (autoConn && token()) connect();
})();

setInterval(() => {
  if (!live || !startedAt) return;
  const secs = (Date.now() - startedAt) / 1000;
  $('stats').textContent =
    frames + ' frames · ' + (frames / Math.max(1, secs)).toFixed(1) + '/s · ' +
    underruns + ' underruns';
}, 1000);

drawMeter(0);
