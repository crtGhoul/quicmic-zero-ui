'use strict';
/* LocalDrop — LocalSend-style sharing for the web.
   Manual-signaling WebRTC: no server, no account. Open this page on two devices. */

const $ = (id) => document.getElementById(id);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const uid = () => Math.random().toString(36).slice(2, 10);

function fmtSize(b) {
  if (b < 1024) return b + ' B';
  if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
  if (b < 1073741824) return (b / 1048576).toFixed(1) + ' MB';
  return (b / 1073741824).toFixed(2) + ' GB';
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[c]));
}
function linkify(s) {
  // Note: the match `u` comes from the already-escaped string, so it is safe
  // to use verbatim in both the href and the link text (no double-escaping).
  return esc(s).replace(/(https?:\/\/[^\s<]+|www\.[^\s<]+)/gi, (u) => {
    const href = /^https?:\/\//i.test(u) ? u : 'https://' + u;
    return '<a href="' + href + '" target="_blank" rel="noopener">' + u + '</a>';
  });
}
function copyText(t) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(t).catch(() => fallbackCopy(t));
  } else {
    fallbackCopy(t);
  }
}
function fallbackCopy(t) {
  const ta = document.createElement('textarea');
  ta.value = t;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand('copy'); } catch (e) { /* noop */ }
  ta.remove();
}

/* ---------- signaling codec (SDP <-> short text code) ---------- */
function encodeSDP(desc) {
  const json = JSON.stringify({ t: desc.type, s: desc.sdp });
  return btoa(unescape(encodeURIComponent(json)))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
function decodeSDP(code) {
  let b64 = code.trim().replace(/\s+/g, '').replace(/-/g, '+').replace(/_/g, '/');
  while (b64.length % 4) b64 += '=';
  const o = JSON.parse(decodeURIComponent(escape(atob(b64))));
  return { type: o.t, sdp: o.s };
}

/* Validate a pasted code: right shape AND right kind (offer vs answer). */
function parseCode(code, wantType) {
  let d;
  try { d = decodeSDP(code); } catch (e) { return { error: 'unreadable' }; }
  if (!d || d.type !== wantType || typeof d.sdp !== 'string' || d.sdp.slice(0, 3) !== 'v=0')
    return { error: 'wrong-kind' };
  return { desc: d };
}

/* If the connection isn't up within `ms`, say so plainly instead of
   hanging on "connecting…" forever — and release the call state so a
   later tap doesn't get auto-declined as "busy". */
let connectTimer = null;
function watchConnect(ms) {
  clearTimeout(connectTimer);
  connectTimer = setTimeout(() => {
    const open = dc && dc.readyState === 'open';
    if (!open && pc && pc.connectionState !== 'connected') {
      const wasSignal = viaSignal;
      endCallAttempt("couldn't connect");
      alert(wasSignal
        ? "Couldn't establish the connection.\n\n• Make sure both devices are online\n• Try tapping the device again in a moment"
        : "Couldn't establish the connection.\n\n" +
          "• Keep both devices on the same Wi-Fi\n" +
          "• Codes are single-use: go back and generate fresh codes\n" +
          "• Make sure the FULL code was copied (they're long)");
    }
  }, ms);
}

/* ---------- views ---------- */
const VIEWS = ['home', 'host', 'join', 'chat'];
function go(name) {
  VIEWS.forEach((v) => $('view-' + v).classList.toggle('hidden', v !== name));
  window.scrollTo(0, 0);
}

/* ---------- webrtc state ---------- */
const APP_V = 9; // protocol version: hello handshake, file-ask approval, file-cancel, clip
const RTC_CFG = {
  iceServers: [
    { urls: 'stun:stun.l.google.com:19302' },
    { urls: 'stun:openrelay.metered.ca:80' },
    // Fallback TURN (Open Relay Project — free public relay) for networks
    // where a direct peer-to-peer path can't be established. The data
    // channel stays end-to-end encrypted; the relay can't read it.
    { urls: 'turn:openrelay.metered.ca:80', username: 'openrelayproject', credential: 'openrelayproject' },
    { urls: 'turn:openrelay.metered.ca:443', username: 'openrelayproject', credential: 'openrelayproject' },
  ],
};
let pc = null;
let dc = null;
let peerV = 0;            // protocol version of the connected peer (0 = unknown/legacy)
let incoming = null;          // file currently being received
const sendQueue = [];
let sending = false;
let activeSend = null;        // {id, cancelled} — transfer currently on the wire
const approvalWaiters = {};   // id -> resolve fn for file-ask approval
const pendingAsks = {};       // id -> {meta, el} — incoming files awaiting our verdict

function setStatus(state, text) {
  $('status').className = 'status ' + state;
  $('statusText').textContent = text;
}

function teardown() {
  try { if (dc) dc.close(); } catch (e) { /* noop */ }
  try { if (pc) pc.close(); } catch (e) { /* noop */ }
  dc = null; pc = null; incoming = null;
  clearTimeout(connectTimer);
  sendQueue.length = 0; sending = false;
  receivedFiles.length = 0; updateFileActions();
  activeSend = null; peerV = 0;
  Object.keys(approvalWaiters).forEach((id) => { try { approvalWaiters[id]('gone'); } catch (e) {} delete approvalWaiters[id]; });
  Object.keys(pendingAsks).forEach((id) => delete pendingAsks[id]);
  // tap-to-connect call state (the matchmaker socket itself stays up)
  viaSignal = false; peerId = null; peerName = '';
  outgoingCall = null; incomingOffer = null;
  hideRinging();
  stopRing();
  $('callModal').classList.add('hidden');
  setStatus('idle', 'not connected');
  const pl = $('peerLabel');
  if (pl) pl.textContent = '…';
}

function newPC() {
  teardown();
  pc = new RTCPeerConnection(RTC_CFG);
  pc.onconnectionstatechange = () => {
    if (!pc) return;
    const s = pc.connectionState;
    if (s === 'connected') setStatus('ok', 'connected');
    else if (s === 'failed') setStatus('bad', viaSignal ? 'connection failed' : 'connection failed — try fresh codes');
    else if (s === 'disconnected' || s === 'closed') setStatus('bad', 'disconnected');
    else setStatus('idle', 'connecting…');
  };
  pc.ondatachannel = (e) => { dc = e.channel; wireDC(); };
}

function waitGathering() {
  return new Promise((res) => {
    if (!pc || pc.iceGatheringState === 'complete') return res();
    const to = setTimeout(res, 6000);
    const h = () => {
      if (pc && pc.iceGatheringState === 'complete') {
        clearTimeout(to);
        pc.removeEventListener('icegatheringstatechange', h);
        res();
      }
    };
    pc.addEventListener('icegatheringstatechange', h);
  });
}

/* ---------- host flow ---------- */
async function hostStart() {
  newPC();
  $('hostCode').value = '';
  $('hostAnswer').value = '';
  $('qr').innerHTML = '';
  dc = pc.createDataChannel('localdrop', { ordered: true });
  wireDC();
  setStatus('idle', 'generating code…');
  try {
    await pc.setLocalDescription(await pc.createOffer());
    await waitGathering();
    const code = encodeSDP(pc.localDescription);
    $('hostCode').value = code;
    renderQR(code);
    setStatus('idle', 'waiting for partner…');
  } catch (err) {
    setStatus('bad', 'failed to start');
  }
}

function renderQR(text) {
  const el = $('qr');
  el.innerHTML = '';
  try {
    new QRCode(el, { text, width: 200, height: 200, correctLevel: QRCode.CorrectLevel.M });
  } catch (e) {
    el.innerHTML = '<p class="hint">Code is too long for a QR — copy it instead.</p>';
  }
}

$('btnConnect').addEventListener('click', async () => {
  const code = $('hostAnswer').value.trim();
  if (!code || !pc) return;
  const parsed = parseCode(code, 'answer');
  if (parsed.error) {
    alert("That doesn't look like a reply code.\n\nCopy the FULL reply code from the other device's Receive screen (tap its Copy button) and paste it here.");
    return;
  }
  try {
    await pc.setRemoteDescription(parsed.desc);
    setStatus('idle', 'connecting…');
    watchConnect(20000);
  } catch (e) {
    alert("That reply code didn't work — double-check it and try again.");
  }
});

/* ---------- join flow ---------- */
$('btnMakeReply').addEventListener('click', async () => {
  const code = $('joinCode').value.trim();
  if (!code) {
    setStatus('bad', 'code needed');
    alert("Paste the sharer's code first.\n\n• Open LocalDrop on the other device and tap Share\n• Copy the FULL share code (tap its Copy button) or scan its QR\n• Paste it here, then tap “Create my reply code”");
    return;
  }
  const parsed = parseCode(code, 'offer');
  if (parsed.error) {
    setStatus('bad', 'bad code');
    alert("That doesn't look like a LocalDrop share code.\n\n• Open LocalDrop on the other device and tap Share\n• Copy the FULL share code (tap its Copy button) or scan its QR\n• Paste it here and try again");
    return;
  }
  newPC();
  setStatus('idle', 'generating reply…');
  try {
    await pc.setRemoteDescription(parsed.desc);
    await pc.setLocalDescription(await pc.createAnswer());
    await waitGathering();
    $('joinReply').value = encodeSDP(pc.localDescription);
    $('replyWrap').classList.remove('hidden');
    setStatus('idle', 'waiting for host…');
  } catch (e) {
    setStatus('bad', 'bad code');
    alert("That code didn't work — double-check it and try again.");
  }
});

/* ---------- data channel ---------- */
function wireDC() {
  dc.binaryType = 'arraybuffer';
  dc.onopen = () => {
    clearTimeout(connectTimer);
    $('messages').innerHTML = '';
    $('peerLabel').textContent = peerName ? '↔ ' + peerName : 'connected';
    // protocol handshake: announce our version so the peer knows which
    // frames we understand (file-ask approval, file-cancel, clip).
    peerV = 0;
    try { dc.send(JSON.stringify({ t: 'hello', v: APP_V })); } catch (e) { /* noop */ }
    // a signaled call just landed: clear call UI and remember the device
    hideRinging();
    stopRing();
    $('callModal').classList.add('hidden');
    outgoingCall = null; incomingOffer = null;
    if (peerId) rememberDevice(peerId, peerName);
    go('chat');
    setStatus('ok', 'connected');
    sysMsg('Connected 🔗 — send photos, files, folders, text, links, or your clipboard (📋).');
  };
  dc.onmessage = onData;
  dc.onclose = () => {
    setStatus('bad', 'disconnected');
    sysMsg('Partner disconnected.');
  };
}

function onData(e) {
  if (typeof e.data === 'string') {
    let m;
    try { m = JSON.parse(e.data); } catch (err) { return; }
    if (!m || typeof m.t !== 'string') return;
    if (m.t === 'msg') addText(false, m.text);
    else if (m.t === 'hello') { peerV = Math.max(0, m.v | 0); }
    else if (m.t === 'clip' && typeof m.text === 'string') {
      addText(false, m.text);
      sysMsg('📋 Clipboard received' + (peerName ? ' from ' + peerName : ''));
    }
    else if (m.t === 'file-meta') {
      // legacy path (peer < v9): files arrive unannounced, auto-accepted
      incoming = { meta: m, parts: [], got: 0, el: fileCard(false, dispName(m), m.size, null, m.mime) };
    } else if (m.t === 'file-ask') onFileAsk(m);
    else if ((m.t === 'file-ok' || m.t === 'file-no') && approvalWaiters[m.id]) {
      approvalWaiters[m.id](m.t === 'file-ok' ? 'ok' : 'no');
    }
    else if (m.t === 'file-cancel' && m.id) onFileCancel(m.id);
    else if (m.t === 'file-end' && incoming && incoming.meta.id === m.id) {
      finishFile();
    }
  } else if (incoming) {
    incoming.parts.push(e.data);
    incoming.got += e.data.byteLength;
    setProg(incoming.el, incoming.got / incoming.meta.size);
  }
}

/* ---------- chat UI ---------- */
function scrollDown() {
  const box = $('messages');
  box.scrollTop = box.scrollHeight;
}
function sysMsg(t) {
  const d = document.createElement('div');
  d.className = 'sys';
  d.textContent = t;
  $('messages').appendChild(d);
  scrollDown();
}
function timeNow() {
  return new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}
function addText(mine, text) {
  const d = document.createElement('div');
  d.className = 'bubble ' + (mine ? 'mine' : 'theirs');
  d.innerHTML = '<div class="text">' + linkify(text) + '</div><div class="meta">' + timeNow() + '</div>';
  $('messages').appendChild(d);
  scrollDown();
}
function basename(p) {
  return String(p || '').split('/').pop();
}
function dispName(m) {
  return (m && (m.path || m.name)) || 'file';
}
function iconFor(mime) {
  mime = String(mime || '');
  if (mime.indexOf('image/') === 0) return '🖼️';
  if (mime.indexOf('video/') === 0) return '🎬';
  if (mime.indexOf('audio/') === 0) return '🎵';
  if (mime === 'application/pdf') return '📕';
  if (/zip|rar|7z|tar|gzip/.test(mime)) return '🗜️';
  return '📄';
}
function fmtSpeed(bps) {
  if (bps < 1024) return Math.round(bps) + ' B/s';
  if (bps < 1048576) return (bps / 1024).toFixed(1) + ' KB/s';
  return (bps / 1048576).toFixed(1) + ' MB/s';
}
function fileCard(mine, name, size, id, mime) {
  const d = document.createElement('div');
  d.className = 'bubble ' + (mine ? 'mine' : 'theirs');
  d.innerHTML =
    '<div class="fbody"><div class="frow">' +
    '<span class="fic">' + iconFor(mime) + '</span>' +
    '<div class="finfo"><div class="fname">' + esc(name) + '</div>' +
    '<div class="fsize">' + fmtSize(size) + '</div></div>' +
    (id ? '<button class="xbtn" title="Cancel transfer">✕</button>' : '') +
    '</div>' +
    '<div class="fnote"></div>' +
    '<div class="track"><div class="bar busy"></div></div>' +
    '<div class="spd"></div></div>';
  $('messages').appendChild(d);
  scrollDown();
  return d;
}
function setProg(el, p) {
  const bar = el.querySelector('.bar');
  if (!bar) return;
  bar.style.width = Math.min(100, p * 100) + '%';
  bar.classList.toggle('busy', p < 1);
}
function setCardNote(el, t) {
  const n = el.querySelector('.fnote');
  if (!n) return;
  n.textContent = t || '';
  n.style.display = t ? 'block' : 'none';
}
function setSpeed(el, bps) {
  const s = el.querySelector('.spd');
  if (!s) return;
  s.textContent = bps > 0 ? fmtSpeed(bps) : '';
}
function markDone(el) {
  setProg(el, 1);
  setSpeed(el, 0);
  const x = el.querySelector('.xbtn');
  if (x) x.style.display = 'none';
}
function finishFile() {
  const cur = incoming;
  incoming = null;
  const blob = new Blob(cur.parts, { type: cur.meta.mime });
  const url = URL.createObjectURL(blob);
  setProg(cur.el, 1);
  const body = cur.el.querySelector('.fbody');
  if (cur.meta.mime.indexOf('image/') === 0) {
    const img = document.createElement('img');
    img.src = url;
    img.className = 'preview';
    img.loading = 'lazy';
    img.alt = cur.meta.name;
    body.appendChild(img);
  }
  const a = document.createElement('a');
  a.href = url;
  a.download = basename(cur.meta.name);
  a.className = 'dl';
  a.textContent = '⬇ Download';
  body.appendChild(a);
  scrollDown();
  markDone(cur.el);
  // track for Download-all / Share
  receivedFiles.push({ name: basename(cur.meta.name), path: dispName(cur.meta), blob });
  updateFileActions();
}

/* ---------- download-all (ZIP) + share ---------- */
const receivedFiles = []; // [{name, path, blob}] — completed incoming files this session

function updateFileActions() {
  const n = receivedFiles.length;
  const wrap = $('fileActions');
  const dl = $('btnDlAll');
  const sh = $('btnShareAll');
  if (!wrap || !dl || !sh) return;
  const canShare = !!(navigator.share);
  dl.classList.toggle('hidden', n < 2);
  sh.classList.toggle('hidden', !(canShare && n >= 1));
  wrap.classList.toggle('hidden', n < 2 && !(canShare && n >= 1));
  dl.textContent = '⬇ All (' + n + ')';
}

function uniqueZipPath(p, used) {
  if (!used.has(p)) { used.add(p); return p; }
  const i = p.lastIndexOf('.');
  const base = i > 0 ? p.slice(0, i) : p;
  const ext = i > 0 ? p.slice(i) : '';
  let k = 2, q;
  do { q = base + ' (' + (k++) + ')' + ext; } while (used.has(q));
  used.add(q);
  return q;
}

async function downloadAll() {
  const btn = $('btnDlAll');
  if (!receivedFiles.length || !btn || btn.disabled) return;
  btn.disabled = true;
  const orig = btn.textContent;
  btn.textContent = 'Zipping…';
  try {
    if (typeof ZipWriter === 'undefined') throw new Error('zip unavailable');
    const used = new Set();
    const files = [];
    for (const f of receivedFiles) {
      const buf = new Uint8Array(await f.blob.arrayBuffer());
      files.push({ path: uniqueZipPath(ZipWriter.sanitizePath(f.path || f.name), used), data: buf });
    }
    const bytes = ZipWriter.zipStore(files);
    const url = URL.createObjectURL(new Blob([bytes], { type: 'application/zip' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = 'localdrop-' + new Date().toISOString().slice(0, 19).replace(/[:T]/g, '-') + '.zip';
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 60000);
  } catch (e) {
    btn.textContent = 'Failed — try again';
    await new Promise((r) => setTimeout(r, 1500));
  }
  btn.disabled = false;
  btn.textContent = orig;
}

async function shareAll() {
  if (!receivedFiles.length || !navigator.share) return;
  try {
    const files = receivedFiles.map((f) =>
      new File([f.blob], f.name, { type: f.blob.type || 'application/octet-stream' }));
    const data = { files, title: 'LocalDrop files' };
    if (navigator.canShare && !navigator.canShare(data)) return;
    await navigator.share(data);
  } catch (e) { /* user cancelled or share failed — ignore */ }
}

/* ---------- sending ---------- */
function sendText() {
  const inp = $('textInput');
  const t = inp.value.trim();
  if (!t || !dc || dc.readyState !== 'open') return;
  dc.send(JSON.stringify({ t: 'msg', text: t, ts: Date.now() }));
  addText(true, t);
  inp.value = '';
}

function enqueueFile(file) {
  if (!file) return;
  if (!file._ldId) file._ldId = uid();
  sendQueue.push(file);
  pumpQueue();
}

/* Wait for the peer's verdict on a file-ask. Resolves 'ok', 'no', 'timeout'
   or 'gone' (connection dropped mid-wait). */
function waitForApproval(id, ms) {
  return new Promise((res) => {
    const to = setTimeout(() => { delete approvalWaiters[id]; res('timeout'); }, ms);
    approvalWaiters[id] = (r) => { clearTimeout(to); delete approvalWaiters[id]; res(r); };
  });
}

function sendFrame(o) {
  if (dc && dc.readyState === 'open') {
    try { dc.send(JSON.stringify(o)); } catch (e) { /* noop */ }
  }
}

/* Cancel an outgoing transfer — the one on the wire or one still queued. */
function cancelSend(id) {
  if (activeSend && activeSend.id === id) {
    activeSend.cancelled = true;
    const w = approvalWaiters[id];
    if (w) w('no'); // release a transfer stuck waiting for approval
  } else {
    const i = sendQueue.findIndex((f) => f._ldId === id);
    if (i >= 0) { sendQueue.splice(i, 1); sysMsg('Removed from the send queue.'); }
  }
  sendFrame({ t: 'file-cancel', id });
}

/* Cancel an incoming transfer (v9 ask-flow only — legacy peers can't abort). */
function cancelReceive(id) {
  if (incoming && incoming.meta.id === id) {
    const el = incoming.el;
    incoming = null;
    setCardNote(el, 'cancelled');
    markDone(el);
  }
  sendFrame({ t: 'file-cancel', id });
}

/* The peer aborted a transfer (or withdrew a pending ask). */
function onFileCancel(id) {
  if (activeSend && activeSend.id === id) {
    activeSend.cancelled = true; // the pump loop marks the card
    const w = approvalWaiters[id];
    if (w) w('no');
    return;
  }
  if (incoming && incoming.meta.id === id) {
    const el = incoming.el;
    incoming = null;
    setCardNote(el, 'cancelled by sender');
    markDone(el);
    return;
  }
  const p = pendingAsks[id];
  if (p) {
    delete pendingAsks[id];
    const row = p.el.querySelector('.askrow');
    if (row) row.remove();
    setCardNote(p.el, 'withdrawn by sender');
  }
}

async function pumpQueue() {
  if (sending || !sendQueue.length) return;
  if (!dc || dc.readyState !== 'open') return;
  sending = true;
  const file = sendQueue.shift();
  const id = file._ldId || uid();
  const meta = {
    t: 'file-meta', id,
    name: file.name, size: file.size,
    mime: file.type || 'application/octet-stream',
  };
  if (file._ldPath) meta.path = file._ldPath;
  const el = fileCard(true, dispName(meta), file.size, id, meta.mime);
  const xb = el.querySelector('.xbtn');
  if (xb) xb.addEventListener('click', () => cancelSend(id));
  const transfer = { id, cancelled: false };
  activeSend = transfer;
  try {
    if (peerV >= APP_V) {
      // v9 approval flow: ask first, stream only on Accept
      setCardNote(el, '⏳ waiting for approval…');
      sendFrame(Object.assign({}, meta, { t: 'file-ask' }));
      const verdict = await waitForApproval(id, 90000);
      if (transfer.cancelled) {
        setCardNote(el, 'cancelled');
      } else if (verdict === 'ok') {
        setCardNote(el, '');
      } else {
        setCardNote(el, verdict === 'no' ? 'declined' : 'no answer — not sent');
        markDone(el);
        activeSend = null; sending = false; pumpQueue();
        return;
      }
      if (transfer.cancelled) { markDone(el); activeSend = null; sending = false; pumpQueue(); return; }
    } else {
      sendFrame(meta); // legacy peer: classic unannounced send
    }
    const CHUNK = 16384;
    let off = 0, lastT = Date.now(), lastOff = 0;
    while (off < file.size) {
      if (transfer.cancelled) break;
      while (dc.bufferedAmount > 512 * 1024) {
        await sleep(60);
        if (transfer.cancelled) break;
      }
      if (transfer.cancelled) break;
      const buf = await file.slice(off, off + CHUNK).arrayBuffer();
      dc.send(buf);
      off += buf.byteLength;
      setProg(el, off / file.size);
      const now = Date.now();
      if (now - lastT >= 500) {
        setSpeed(el, (off - lastOff) / ((now - lastT) / 1000));
        lastT = now; lastOff = off;
      }
    }
    if (transfer.cancelled) {
      setCardNote(el, 'cancelled');
    } else {
      dc.send(JSON.stringify({ t: 'file-end', id }));
    }
    markDone(el);
  } catch (err) {
    el.querySelector('.fname').textContent += ' — failed';
    markDone(el);
  }
  activeSend = null;
  sending = false;
  pumpQueue();
}

/* ---------- incoming file approval (v9) ---------- */
function onFileAsk(m) {
  if (!m || !m.id || pendingAsks[m.id]) return;
  if (incoming) { sendFrame({ t: 'file-no', id: m.id }); return; } // busy: don't clobber the active transfer
  const d = document.createElement('div');
  d.className = 'bubble theirs ask';
  d.innerHTML =
    '<div class="fbody"><div class="frow">' +
    '<span class="fic">' + iconFor(m.mime) + '</span>' +
    '<div class="finfo"><div class="fname">' + esc(dispName(m)) + '</div>' +
    '<div class="fsize">' + fmtSize(m.size) + '</div></div></div>' +
    '<div class="fnote">wants to send you this file</div>' +
    '<div class="askrow"><button class="btn small okbtn">✓ Accept</button>' +
    '<button class="btn small nobtn">✕ Decline</button></div></div>';
  $('messages').appendChild(d);
  scrollDown();
  pendingAsks[m.id] = { meta: m, el: d };
  const ok = d.querySelector('.okbtn');
  const no = d.querySelector('.nobtn');
  if (ok) ok.addEventListener('click', () => acceptAsk(m.id));
  if (no) no.addEventListener('click', () => declineAsk(m.id));
}

function acceptAsk(id) {
  const p = pendingAsks[id];
  if (!p) return;
  delete pendingAsks[id];
  const m = p.meta;
  if (p.el && p.el.remove) p.el.remove();
  incoming = { meta: m, parts: [], got: 0, el: fileCard(false, dispName(m), m.size, m.id, m.mime) };
  const xb = incoming.el.querySelector('.xbtn');
  if (xb) xb.addEventListener('click', () => cancelReceive(m.id));
  sendFrame({ t: 'file-ok', id });
  scrollDown();
}

function declineAsk(id) {
  const p = pendingAsks[id];
  if (!p) return;
  delete pendingAsks[id];
  const row = p.el.querySelector('.askrow');
  if (row) row.remove();
  setCardNote(p.el, 'declined');
  sendFrame({ t: 'file-no', id });
}

/* ---------- clipboard sync ---------- */
async function sendClipboard() {
  if (!dc || dc.readyState !== 'open') {
    sysMsg('Connect first, then tap 📋 to share your clipboard.');
    return;
  }
  const clip = navigator.clipboard;
  if (!clip) {
    alert("Clipboard access isn't available here.\n\nIt needs a secure (https) page and a browser that allows clipboard access.");
    return;
  }
  try {
    let text = '';
    try { text = await clip.readText(); } catch (e) { /* denied or empty */ }
    if (text && text.trim()) {
      sendFrame({ t: 'clip', text, ts: Date.now() });
      addText(true, text);
      sysMsg('📋 Clipboard sent' + (peerV > 0 && peerV < APP_V ? ' — their app looks outdated, ask them to refresh the page if nothing arrives.' : ''));
      return;
    }
    // no text — look for an image (e.g. a screenshot)
    if (clip.read) {
      const items = await clip.read();
      for (const it of items) {
        const imgType = (it.types || []).find((t) => t.indexOf('image/') === 0);
        if (imgType) {
          const blob = await it.getType(imgType);
          const ext = (imgType.split('/')[1] || 'png').split('+')[0];
          const f = new File([blob], 'clipboard-' + Date.now() + '.' + ext, { type: imgType });
          enqueueFile(f);
          sysMsg('📋 Clipboard image queued');
          return;
        }
      }
    }
    sysMsg('Clipboard is empty — copy some text or an image first.');
  } catch (err) {
    alert("Couldn't read the clipboard.\n\nYour browser may need permission — allow clipboard access for this site and try again.");
  }
}

/* ---------- folder send: walk a dropped / picked directory ---------- */
async function filesFromEntry(entry, base) {
  const out = [];
  if (entry.isFile) {
    const file = await new Promise((res, rej) => entry.file(res, rej));
    out.push({ file, path: base ? base + '/' + file.name : file.name });
  } else if (entry.isDirectory) {
    const dirPath = base ? base + '/' + entry.name : entry.name;
    const reader = entry.createReader();
    for (;;) {
      const batch = await new Promise((res, rej) => reader.readEntries(res, rej));
      if (!batch.length) break;
      for (const child of batch) {
        const sub = await filesFromEntry(child, dirPath);
        out.push.apply(out, sub);
      }
    }
  }
  return out;
}

function entryOf(item) {
  if (!item) return null;
  if (item.webkitGetAsEntry) return item.webkitGetAsEntry();
  if (item.getAsEntry) return item.getAsEntry(); // newer Firefox
  return null;
}

async function handleFolderDrop(items) {
  const collected = [];
  for (const it of items) {
    let entry = null;
    try { entry = entryOf(it); } catch (e) { /* noop */ }
    if (!entry) continue;
    try {
      const found = await filesFromEntry(entry, '');
      collected.push.apply(collected, found);
    } catch (e) { /* skip unreadable entries */ }
  }
  if (!collected.length) {
    sysMsg('Nothing sendable in that drop.');
    return;
  }
  if (collected.length > 500) {
    sysMsg('Big drop — sending the first 500 files.');
    collected.length = 500;
  }
  collected.forEach(({ file, path }) => { file._ldPath = path; enqueueFile(file); });
  sysMsg('Sending ' + collected.length + (collected.length === 1 ? ' file' : ' files') + '…');
}

/* ---------- tap-to-connect: identity & storage ---------- */
const LS_ID = 'ld_id', LS_NAME = 'ld_name', LS_SERVER = 'ld_server', LS_KNOWN = 'ld_known', LS_PRIV = 'ld_private';
/* Built-in free matchmaker (Render). Used unless the user sets their own server
   in settings — or explicitly clears the field to disable tap-to-connect. */
const DEFAULT_SERVER = 'wss://localdrop-l8ly.onrender.com';

function storeGet(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
function storeSet(k, v) { try { localStorage.setItem(k, v); } catch (e) { /* noop */ } }

/* Stable per-device id, created once and kept in localStorage. */
function getDeviceId() {
  let id = storeGet(LS_ID);
  if (!id) {
    id = (window.crypto && crypto.randomUUID) ? crypto.randomUUID() : (uid() + uid());
    storeSet(LS_ID, id);
  }
  return id;
}

/* Friendly default name, e.g. "iPhone-A3F9". */
function suggestName(ua) {
  ua = String(ua || '').toLowerCase();
  const base = /iphone|ipad|ipod/.test(ua) ? 'iPhone'
    : /android/.test(ua) ? 'Android' : 'Computer';
  return base + '-' + Math.random().toString(36).slice(2, 6).toUpperCase();
}

/* Normalize what the user types into the server field:
   bare host -> wss://host, http(s):// -> ws(s)://. */
function normalizeServerUrl(u) {
  u = String(u || '').trim();
  if (!u) return '';
  if (/^wss?:\/\//i.test(u)) return u;
  if (/^https:\/\//i.test(u)) return 'wss://' + u.slice(8);
  if (/^http:\/\//i.test(u)) return 'ws://' + u.slice(7);
  return 'wss://' + u;
}

/* Nearby devices first, then alphabetical. */
function sortRoster(devices) {
  return (devices || []).slice().sort((a, b) => {
    if (!!a.nearby !== !!b.nearby) return a.nearby ? -1 : 1;
    return String(a.name || '').localeCompare(String(b.name || ''));
  });
}

function timeAgo(ts, now) {
  const s = Math.max(0, Math.floor(((now || Date.now()) - ts) / 1000));
  if (s < 60) return 'just now';
  const m = Math.floor(s / 60);
  if (m < 60) return m + ' min ago';
  const h = Math.floor(m / 60);
  if (h < 24) return h + (h === 1 ? ' hr ago' : ' hrs ago');
  const d = Math.floor(h / 24);
  return d + (d === 1 ? ' day ago' : ' days ago');
}

/* Add/update a remembered device; newest first, capped at 50. */
function upsertKnown(list, entry) {
  const rest = (list || []).filter((k) => k.id !== entry.id);
  rest.unshift({
    id: entry.id,
    name: String(entry.name || 'Unknown device').slice(0, 32),
    lastSeen: entry.lastSeen || Date.now(),
  });
  return rest.slice(0, 50);
}

function loadKnown() {
  try {
    const v = JSON.parse(storeGet(LS_KNOWN) || '[]');
    return Array.isArray(v) ? v : [];
  } catch (e) { return []; }
}

/* Client-side sanity check for an inbound signal payload. */
function validSignalPayload(p) {
  if (!p || typeof p !== 'object') return false;
  if (p.kind === 'declined') return true;
  if (p.kind === 'ice') return p.candidate === null || typeof p.candidate === 'object';
  if (p.kind === 'offer' || p.kind === 'answer') return sdpStr(p.sdp).slice(0, 3) === 'v=0';
  return false;
}

/* SDP may arrive as a bare string or as a serialized RTCSessionDescription
   ({type, sdp}) — normalize to the string form WebRTC APIs require. */
function sdpStr(v) {
  if (typeof v === 'string') return v;
  if (v && typeof v.sdp === 'string') return v.sdp;
  return '';
}

function avatarLetter(name) {
  const m = String(name || '').match(/[a-z0-9]/i);
  return m ? m[0].toUpperCase() : '?';
}

/* ---------- service worker update prompter ----------
   If a newer version shipped while this tab runs the old one, offer a
   one-tap refresh instead of silently staying stale. */
function watchForUpdates() {
  const sw = navigator.serviceWorker;
  if (!sw || !sw.getRegistration) return;
  navigator.serviceWorker.getRegistration().then((reg) => {
    if (!reg) return;
    const check = () => { try { const p = reg.update(); if (p && p.catch) p.catch(() => {}); } catch (e) { /* noop */ } };
    check(); // ask for a fresh sw.js now, not whenever the browser feels like it
    reg.addEventListener('updatefound', () => {
      const nw = reg.installing;
      if (!nw) return;
      nw.addEventListener('statechange', () => {
        if (nw.state === 'installed' && navigator.serviceWorker.controller) showUpdateToast();
      });
    });
    setInterval(check, 30 * 60 * 1000); // re-check every 30 min on long-lived tabs
    // also check whenever the tab becomes visible again (e.g. phone unlocked)
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) check();
    });
  }).catch(() => {});
}
function showUpdateToast() {
  let t = document.getElementById('updateToast');
  if (t) { t.classList.add('show'); return; }
  t = document.createElement('div');
  t.id = 'updateToast';
  const s = document.createElement('span');
  s.textContent = '✨ New version available';
  const b = document.createElement('button');
  b.textContent = 'Refresh';
  b.addEventListener('click', () => window.location.reload());
  t.appendChild(s);
  t.appendChild(b);
  document.body.appendChild(t);
  const show = () => t.classList.add('show');
  if (window.requestAnimationFrame) window.requestAnimationFrame(show); else show();
}

/* ---------- tap-to-connect: matchmaker socket ---------- */
let myId = null, myName = '';
let sig = null, sigGen = 0, sigTimer = null, sigWakeTimer = null, hbTimer = null;
let sigBackoff = 2000, sigWanted = false;
let roster = [];
let outgoingCall = null;   // {id, name} — we rang them
let incomingOffer = null;  // {from, fromName, sdp} — they rang us
let peerId = null, peerName = '';
let viaSignal = false;

function sigSend(o) {
  if (sig && sig.readyState === 1) {
    try { sig.send(JSON.stringify(o)); } catch (e) { /* noop */ }
  }
}

/* (Re)register with the matchmaker, including the private-mode flag. */
function sendRegister() {
  sigSend({ t: 'register', id: myId, name: myName, private: storeGet(LS_PRIV) === '1' });
}

function setSigDot(s) {
  $('sigDot').className = 'sigdot' + (s === 'on' ? ' on' : s === 'bad' ? ' bad' : '');
}
function showSigNotice(t) {
  const el = $('sigNotice');
  el.textContent = t;
  el.classList.toggle('hidden', !t);
}

/* Keep the visible server link in sync with the matchmaker in use.
   wss:// -> https:// so it opens in a browser tab. */
function renderServerLink(url) {
  const a = $('serverLink');
  if (!a) return;
  const line = a.closest ? a.closest('.serverline') : null;
  if (!url) {
    if (line) line.classList.add('hidden');
    return;
  }
  if (line) line.classList.remove('hidden');
  a.href = String(url).replace(/^wss:\/\//i, 'https://').replace(/^ws:\/\//i, 'http://');
  a.textContent = String(url).replace(/^wss?:\/\//i, '').replace(/\/$/, '');
}

function sigConnect() {
  // Unset -> built-in free server. Explicitly cleared ('') -> tap-to-connect off.
  const stored = storeGet(LS_SERVER);
  const url = normalizeServerUrl(stored === null ? DEFAULT_SERVER : stored);
  sigWanted = !!url;
  renderServerLink(url);
  sigGen += 1;
  const gen = sigGen;
  clearTimeout(sigTimer);
  clearTimeout(sigWakeTimer);
  clearInterval(hbTimer);
  try { if (sig) sig.close(); } catch (e) { /* noop */ }
  sig = null;
  roster = [];
  renderRoster();
  if (!sigWanted) {
    setSigDot('off');
    showSigNotice('');
    return;
  }
  setSigDot('off');
  showSigNotice('Connecting to matchmaker…');
  // Render's free tier sleeps when idle — first connect of the day can take
  // ~30-60s while it wakes. Say so instead of looking stuck.
  sigWakeTimer = setTimeout(() => {
    if (gen === sigGen && sig && sig.readyState === 0) {
      showSigNotice('Waking up the free server — it sleeps when idle. First connect can take ~30–60 seconds…');
    }
  }, 8000);
  let ws;
  try { ws = new WebSocket(url); } catch (e) { retrySig(gen); return; }
  sig = ws;
  ws.onopen = () => {
    if (gen !== sigGen) { try { ws.close(); } catch (e) { /* noop */ } return; }
    clearTimeout(sigWakeTimer);
    sigBackoff = 2000;
    setSigDot('on');
    showSigNotice('');
    sendRegister();
    clearInterval(hbTimer);
    hbTimer = setInterval(() => sigSend({ t: 'heartbeat' }), 25000);
  };
  ws.onmessage = (e) => { if (gen === sigGen) handleSigMsg(e.data); };
  ws.onerror = () => { /* onclose follows with the retry */ };
  ws.onclose = () => {
    if (gen !== sigGen) return;
    clearTimeout(sigWakeTimer);
    clearInterval(hbTimer);
    setSigDot('bad');
    showSigNotice("Couldn't reach the matchmaker — you can still pair with a code.");
    roster = [];
    renderRoster();
    renderKnown();
    retrySig(gen);
  };
}
function retrySig(gen) {
  clearTimeout(sigTimer);
  if (gen !== sigGen || !sigWanted) return;
  sigTimer = setTimeout(() => { if (gen === sigGen) sigConnect(); }, Math.min(sigBackoff, 30000));
  sigBackoff *= 2;
}

function handleSigMsg(raw) {
  let m;
  try { m = JSON.parse(raw); } catch (e) { return; }
  if (!m || typeof m !== 'object') return;
  if (m.t === 'roster' && Array.isArray(m.devices)) {
    roster = m.devices;
    renderRoster();
    renderKnown();
  } else if (m.t === 'signal' && m.payload) {
    onRemoteSignal(m.from, m.fromName || 'Unknown device', m.payload);
  } else if (m.t === 'error' && m.msg) {
    showSigNotice('Matchmaker: ' + m.msg);
  }
}

/* ---------- tap-to-connect: calling ---------- */
function renderRoster() {
  const list = $('rosterList');
  list.innerHTML = '';
  if (!sigWanted) {
    list.innerHTML = '<p class="empty">Tap-to-connect is off (server cleared in settings) — use Share / Receive codes below.</p>';
    return;
  }
  const devs = sortRoster(roster);
  if (!devs.length) {
    list.innerHTML = '<p class="empty">No other devices online right now. Keep this page open — they\'ll appear here.</p>';
    return;
  }
  devs.forEach((d) => {
    const b = document.createElement('button');
    b.className = 'dev';
    const sub = d.nearby ? 'on your network' : 'online';
    b.innerHTML =
      '<span class="avatar">' + esc(avatarLetter(d.name)) + '</span>' +
      '<span class="who"><span class="dname">' + esc(d.name) + '</span><br>' +
      '<span class="dsub">' + sub + '</span></span>' +
      (d.nearby ? '<span class="badge">nearby</span>' : '') +
      '<span class="go">›</span>';
    b.addEventListener('click', () => callDevice(d.id, d.name));
    list.appendChild(b);
  });
}

function renderKnown() {
  const list = $('knownList');
  list.innerHTML = '';
  const online = new Set(roster.map((d) => d.id));
  const offline = loadKnown().filter((k) => !online.has(k.id));
  if (!offline.length) {
    list.innerHTML = '<p class="empty">Devices you connect to will be remembered here.</p>';
    return;
  }
  offline.forEach((k) => {
    const d = document.createElement('div');
    d.className = 'dev off';
    d.innerHTML =
      '<span class="avatar">' + esc(avatarLetter(k.name)) + '</span>' +
      '<span class="who"><span class="dname">' + esc(k.name) + '</span><br>' +
      '<span class="dsub">last seen ' + esc(timeAgo(k.lastSeen, Date.now())) + '</span></span>';
    list.appendChild(d);
  });
}

function rememberDevice(id, name) {
  if (!id) return;
  storeSet(LS_KNOWN, JSON.stringify(upsertKnown(loadKnown(), {
    id, name: name || 'Unknown device', lastSeen: Date.now(),
  })));
  renderKnown();
}

function showRinging(name, onCancel) {
  const el = $('callState');
  el.classList.remove('hidden');
  el.innerHTML = '';
  const s = document.createElement('span');
  s.textContent = '📞 Ringing ' + name + '…';
  const b = document.createElement('button');
  b.className = 'btn small';
  b.textContent = 'Cancel';
  b.addEventListener('click', onCancel);
  el.appendChild(s);
  el.appendChild(b);
}
function hideRinging() {
  const el = $('callState');
  if (el) { el.classList.add('hidden'); el.innerHTML = ''; }
}

/* ---------- incoming-call ringtone + vibration ----------
   Pure Web Audio (no sound files): classic two-tone ring every 3s.
   AudioContext is unlocked on the first user gesture (autoplay policy);
   vibration covers Android (iOS Safari has no vibrate API). */
let ringTimer = null, ringCtx = null;
function ensureRingCtx() {
  try {
    if (!ringCtx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (AC) ringCtx = new AC();
    }
    if (ringCtx && ringCtx.state === 'suspended') {
      const p = ringCtx.resume();
      if (p && p.catch) p.catch(() => {});
    }
  } catch (e) { /* noop */ }
  return ringCtx;
}
if (typeof window !== 'undefined' && window.addEventListener) {
  const unlockAudio = () => ensureRingCtx();
  window.addEventListener('pointerdown', unlockAudio, { once: true });
  window.addEventListener('keydown', unlockAudio, { once: true });
}
function playRingOnce(ctx, when) {
  [440, 480].forEach((f) => {
    const o = ctx.createOscillator(), g = ctx.createGain();
    o.type = 'sine';
    o.frequency.value = f;
    g.gain.setValueAtTime(0.0001, when);
    g.gain.exponentialRampToValueAtTime(0.22, when + 0.05);
    g.gain.setValueAtTime(0.22, when + 0.9);
    g.gain.exponentialRampToValueAtTime(0.0001, when + 1.0);
    o.connect(g);
    g.connect(ctx.destination);
    o.start(when);
    o.stop(when + 1.05);
  });
}
function startRing() {
  stopRing();
  ensureRingCtx();
  const buzz = () => { try { if (navigator.vibrate) navigator.vibrate([600, 300, 600]); } catch (e) { /* noop */ } };
  const ring = () => {
    buzz();
    try {
      if (ringCtx) {
        if (ringCtx.state === 'suspended') ensureRingCtx();
        playRingOnce(ringCtx, ringCtx.currentTime + 0.05);
      }
    } catch (e) { /* noop */ }
  };
  ring();
  ringTimer = setInterval(ring, 3000);
}
function stopRing() {
  if (ringTimer) { clearInterval(ringTimer); ringTimer = null; }
  try { if (navigator.vibrate) navigator.vibrate(0); } catch (e) { /* noop */ }
}

/* Outgoing: tap a device -> send offer through the matchmaker (trickle ICE). */
function callDevice(id, name) {
  if (outgoingCall || incomingOffer) return;
  if (dc && dc.readyState === 'open') return;
  newPC();
  outgoingCall = { id, name };
  viaSignal = true; peerId = id; peerName = name;
  dc = pc.createDataChannel('localdrop', { ordered: true });
  wireDC();
  setStatus('idle', 'ringing ' + name + '…');
  showRinging(name, cancelCall);
  pc.onicecandidate = (e) => {
    if (e.candidate) sigSend({ t: 'signal', to: id, payload: { kind: 'ice', candidate: e.candidate } });
  };
  (async () => {
    try {
      await pc.setLocalDescription(await pc.createOffer());
      sigSend({ t: 'signal', to: id, payload: { kind: 'offer', sdp: pc.localDescription.sdp } });
      watchConnect(20000);
    } catch (e) { endCallAttempt("couldn't start the call"); }
  })();
}

/* Incoming signal from the matchmaker. */
function onRemoteSignal(from, fromName, p) {
  if (!validSignalPayload(p)) return;
  if (p.kind === 'offer') {
    if (outgoingCall || incomingOffer || (dc && dc.readyState === 'open')) {
      sigSend({ t: 'signal', to: from, payload: { kind: 'declined' } }); // busy
      return;
    }
    incomingOffer = { from, fromName, sdp: p.sdp };
    $('callTitle').textContent = fromName + ' wants to connect';
    $('callModal').classList.remove('hidden');
    startRing();
  } else if (p.kind === 'answer') {
    if (outgoingCall && outgoingCall.id === from && pc) {
      const sdp = sdpStr(p.sdp);
      if (!sdp) { endCallAttempt("couldn't connect"); return; }
      pc.setRemoteDescription({ type: 'answer', sdp })
        .catch(() => endCallAttempt("couldn't connect"));
    }
  } else if (p.kind === 'ice') {
    const mine = outgoingCall && outgoingCall.id === from;
    const theirs = incomingOffer && incomingOffer.from === from;
    if (pc && (mine || theirs) && p.candidate) {
      pc.addIceCandidate(p.candidate).catch(() => {});
    }
  } else if (p.kind === 'declined') {
    if (outgoingCall && outgoingCall.id === from) {
      endCallAttempt(fromName + ' declined');
    } else if (incomingOffer && incomingOffer.from === from) {
      incomingOffer = null; // caller hung up while ringing us
      $('callModal').classList.add('hidden');
      stopRing();
      setStatus('idle', 'not connected');
    }
  }
}

async function acceptCall() {
  const inv = incomingOffer;
  incomingOffer = null;
  stopRing();
  $('callModal').classList.add('hidden');
  if (!inv) return;
  newPC();
  viaSignal = true; peerId = inv.from; peerName = inv.fromName;
  setStatus('idle', 'connecting…');
  pc.onicecandidate = (e) => {
    if (e.candidate) sigSend({ t: 'signal', to: inv.from, payload: { kind: 'ice', candidate: e.candidate } });
  };
  try {
    const offerSdp = sdpStr(inv.sdp);
    if (!offerSdp) throw new Error('bad offer sdp');
    await pc.setRemoteDescription({ type: 'offer', sdp: offerSdp });
    await pc.setLocalDescription(await pc.createAnswer());
    sigSend({ t: 'signal', to: inv.from, payload: { kind: 'answer', sdp: pc.localDescription.sdp } });
    watchConnect(20000);
  } catch (e) { endCallAttempt("couldn't connect"); }
}

function declineCall() {
  const inv = incomingOffer;
  incomingOffer = null;
  stopRing();
  $('callModal').classList.add('hidden');
  if (inv) sigSend({ t: 'signal', to: inv.from, payload: { kind: 'declined' } });
  setStatus('idle', 'not connected');
}

function cancelCall() {
  const oc = outgoingCall;
  outgoingCall = null;
  if (oc) sigSend({ t: 'signal', to: oc.id, payload: { kind: 'declined' } });
  hideRinging();
  teardown();
}

function endCallAttempt(msg) {
  outgoingCall = null;
  hideRinging();
  setStatus('bad', msg);
  try { if (pc) pc.close(); } catch (e) { /* noop */ }
  pc = null; dc = null;
  viaSignal = false; peerId = null; peerName = '';
}

/* ---------- tap-to-connect: wiring ---------- */
function initTapToConnect() {
  myId = getDeviceId();
  myName = storeGet(LS_NAME) || '';
  const nameInp = $('nameInput');
  nameInp.value = myName || suggestName(navigator.userAgent || '');
  if (!myName) { myName = nameInp.value; storeSet(LS_NAME, myName); }
  nameInp.addEventListener('change', () => {
    myName = nameInp.value.trim().slice(0, 24) || suggestName('');
    nameInp.value = myName;
    storeSet(LS_NAME, myName);
    sendRegister();
  });
  const srvInp = $('serverInput');
  srvInp.value = storeGet(LS_SERVER) || '';
  srvInp.placeholder = DEFAULT_SERVER + ' (default)';
  srvInp.addEventListener('change', () => {
    const u = normalizeServerUrl(srvInp.value);
    srvInp.value = u;
    storeSet(LS_SERVER, u);
    sigConnect();
  });
  const privTgl = $('privToggle');
  privTgl.checked = storeGet(LS_PRIV) === '1';
  privTgl.addEventListener('change', () => {
    storeSet(LS_PRIV, privTgl.checked ? '1' : '0');
    sendRegister();
  });
  $('btnAccept').addEventListener('click', acceptCall);
  $('btnDecline').addEventListener('click', declineCall);
  renderKnown();
  sigConnect();
}

/* ---------- wiring ---------- */
initTapToConnect();
watchForUpdates();
$('btnHost').addEventListener('click', () => { go('host'); hostStart(); });
$('btnJoin').addEventListener('click', () => {
  $('replyWrap').classList.add('hidden');
  $('joinCode').value = '';
  $('joinReply').value = '';
  go('join');
});
document.querySelectorAll('[data-go]').forEach((b) =>
  b.addEventListener('click', () => { teardown(); go(b.getAttribute('data-go')); })
);
$('copyHost').addEventListener('click', () => copyText($('hostCode').value));
$('copyJoin').addEventListener('click', () => copyText($('joinReply').value));
$('hostCode').addEventListener('focus', function () { this.select(); });
$('joinReply').addEventListener('focus', function () { this.select(); });
$('btnSend').addEventListener('click', sendText);
$('textInput').addEventListener('keydown', (e) => { if (e.key === 'Enter') sendText(); });
$('fileInput').addEventListener('change', (e) => {
  Array.from(e.target.files).forEach(enqueueFile);
  e.target.value = '';
});
$('folderInput').addEventListener('change', (e) => {
  const files = Array.from(e.target.files || []);
  files.forEach((f) => { f._ldPath = f.webkitRelativePath || f.name; enqueueFile(f); });
  if (files.length) sysMsg('Sending folder (' + files.length + ' files)…');
  e.target.value = '';
});
$('btnClip').addEventListener('click', sendClipboard);
$('btnDlAll').addEventListener('click', downloadAll);
$('btnShareAll').addEventListener('click', shareAll);
document.addEventListener('paste', (e) => {
  if ($('view-chat').classList.contains('hidden')) return;
  const files = (e.clipboardData && e.clipboardData.files) || [];
  Array.from(files).forEach(enqueueFile);
});

/* ---------- drag & drop send (desktop: drag files onto the chat) ---------- */
(function initDropSend() {
  const chatView = $('view-chat');
  const overlay = $('dropOverlay');
  let dragDepth = 0;

  const chatVisible = () => !chatView.classList.contains('hidden');
  const live = () => dc && dc.readyState === 'open';
  const hasFiles = (e) =>
    !!(e.dataTransfer && e.dataTransfer.types &&
       Array.prototype.indexOf.call(e.dataTransfer.types, 'Files') !== -1);

  // Never let a dropped file navigate the browser away — even outside chat.
  window.addEventListener('dragover', (e) => e.preventDefault());
  window.addEventListener('drop', (e) => {
    if (!chatVisible()) e.preventDefault();
  });

  chatView.addEventListener('dragenter', (e) => {
    if (!chatVisible() || !hasFiles(e)) return;
    e.preventDefault();
    dragDepth++;
    if (live()) overlay.classList.remove('hidden');
  });
  chatView.addEventListener('dragover', (e) => {
    if (chatVisible()) e.preventDefault(); // must cancel to allow the drop
  });
  chatView.addEventListener('dragleave', (e) => {
    if (!chatVisible() || !hasFiles(e)) return;
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) overlay.classList.add('hidden');
  });
  chatView.addEventListener('drop', (e) => {
    e.preventDefault();
    dragDepth = 0;
    overlay.classList.add('hidden');
    if (!chatVisible()) return;
    if (!live()) {
      sysMsg('Not connected — connect first, then drop files here to send them.');
      return;
    }
    const dt = e.dataTransfer;
    const items = dt && dt.items ? Array.from(dt.items) : [];
    // Folders (and files) via the FileSystem API — preserves folder structure
    if (items.length && items.some((it) => entryOf(it))) {
      handleFolderDrop(items);
      return;
    }
    const files = Array.from((dt && dt.files) || []);
    if (!files.length) return;
    files.forEach(enqueueFile);
    sysMsg('Sending ' + files.length + (files.length === 1 ? ' file' : ' files') + '…');
  });
})();

/* ---------- PWA: service worker + install prompt ---------- */
if ('serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    navigator.serviceWorker.register('./sw.js').catch(() => {});
  });
}
let deferredPrompt = null;
window.addEventListener('beforeinstallprompt', (e) => {
  e.preventDefault();
  deferredPrompt = e;
  $('installBtn').classList.remove('hidden');
});
$('installBtn').addEventListener('click', async () => {
  if (!deferredPrompt) return;
  deferredPrompt.prompt();
  try { await deferredPrompt.userChoice; } catch (err) { /* noop */ }
  deferredPrompt = null;
  $('installBtn').classList.add('hidden');
});
(function showIOSHint() {
  const isIOS = /iphone|ipad|ipod/i.test(navigator.userAgent || '');
  if (isIOS && !window.navigator.standalone) {
    $('iosHint').classList.remove('hidden');
  }
})();

setStatus('idle', 'not connected');
