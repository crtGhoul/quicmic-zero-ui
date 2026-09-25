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
let incoming = null;          // file currently being received
const sendQueue = [];
let sending = false;

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
  // tap-to-connect call state (the matchmaker socket itself stays up)
  viaSignal = false; peerId = null; peerName = '';
  outgoingCall = null; incomingOffer = null;
  hideRinging();
  $('callModal').classList.add('hidden');
  setStatus('idle', 'not connected');
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
    // a signaled call just landed: clear call UI and remember the device
    hideRinging();
    $('callModal').classList.add('hidden');
    outgoingCall = null; incomingOffer = null;
    if (peerId) rememberDevice(peerId, peerName);
    go('chat');
    setStatus('ok', 'connected');
    sysMsg('Connected 🔗 — send photos, files, text or links.');
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
    if (m.t === 'msg') addText(false, m.text);
    else if (m.t === 'file-meta') {
      incoming = { meta: m, parts: [], got: 0, el: fileCard(false, m.name, m.size) };
    } else if (m.t === 'file-end' && incoming && incoming.meta.id === m.id) {
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
function fileCard(mine, name, size) {
  const d = document.createElement('div');
  d.className = 'bubble ' + (mine ? 'mine' : 'theirs');
  d.innerHTML =
    '<div class="fbody"><div class="fname">' + esc(name) + '</div>' +
    '<div class="fsize">' + fmtSize(size) + '</div>' +
    '<div class="track"><div class="bar"></div></div></div>';
  $('messages').appendChild(d);
  scrollDown();
  return d;
}
function setProg(el, p) {
  el.querySelector('.bar').style.width = Math.min(100, p * 100) + '%';
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
  a.download = cur.meta.name;
  a.className = 'dl';
  a.textContent = '⬇ Download';
  body.appendChild(a);
  scrollDown();
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
  sendQueue.push(file);
  pumpQueue();
}

async function pumpQueue() {
  if (sending || !sendQueue.length) return;
  if (!dc || dc.readyState !== 'open') return;
  sending = true;
  const file = sendQueue.shift();
  const id = uid();
  const el = fileCard(true, file.name, file.size);
  try {
    dc.send(JSON.stringify({
      t: 'file-meta', id,
      name: file.name, size: file.size,
      mime: file.type || 'application/octet-stream',
    }));
    const CHUNK = 16384;
    let off = 0;
    while (off < file.size) {
      while (dc.bufferedAmount > 512 * 1024) await sleep(60);
      const buf = await file.slice(off, off + CHUNK).arrayBuffer();
      dc.send(buf);
      off += buf.byteLength;
      setProg(el, off / file.size);
    }
    dc.send(JSON.stringify({ t: 'file-end', id }));
    setProg(el, 1);
  } catch (err) {
    el.querySelector('.fname').textContent += ' — failed';
  }
  sending = false;
  pumpQueue();
}

/* ---------- tap-to-connect: identity & storage ---------- */
const LS_ID = 'ld_id', LS_NAME = 'ld_name', LS_SERVER = 'ld_server', LS_KNOWN = 'ld_known';
const LS_AUTOCONN = 'ld_autoconn', LS_VIBRATE = 'ld_vibrate';

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

/* ---------- tap-to-connect: matchmaker socket ---------- */
let myId = null, myName = '';
let autoConn = true, vibrateOn = true; // Drop settings (persisted)
let sig = null, sigGen = 0, sigTimer = null, hbTimer = null;
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

function setSigDot(s) {
  $('sigDot').className = 'sigdot' + (s === 'on' ? ' on' : s === 'bad' ? ' bad' : '');
}

/* Auto-connect toggle: on -> (re)join the matchmaker now; off -> tear the
   socket down. Manual Share/Receive code pairing always keeps working. */
function setAutoConn(v) {
  autoConn = !!v;
  storeSet(LS_AUTOCONN, autoConn ? '1' : '0');
  if (autoConn) {
    sigConnect();
    return;
  }
  sigWanted = false;
  sigGen += 1;
  clearTimeout(sigTimer);
  clearInterval(hbTimer);
  try { if (sig) sig.close(); } catch (e) { /* noop */ }
  sig = null;
  roster = [];
  renderRoster();
  renderKnown();
  setSigDot('off');
  showSigNotice('');
}
function showSigNotice(t) {
  const el = $('sigNotice');
  el.textContent = t;
  el.classList.toggle('hidden', !t);
}

function sigConnect() {
  const url = normalizeServerUrl(storeGet(LS_SERVER) || '');
  sigWanted = !!url;
  sigGen += 1;
  const gen = sigGen;
  clearTimeout(sigTimer);
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
  let ws;
  try { ws = new WebSocket(url); } catch (e) { retrySig(gen); return; }
  sig = ws;
  ws.onopen = () => {
    if (gen !== sigGen) { try { ws.close(); } catch (e) { /* noop */ } return; }
    sigBackoff = 2000;
    setSigDot('on');
    showSigNotice('');
    sigSend({ t: 'register', id: myId, name: myName });
    clearInterval(hbTimer);
    hbTimer = setInterval(() => sigSend({ t: 'heartbeat' }), 25000);
  };
  ws.onmessage = (e) => { if (gen === sigGen) handleSigMsg(e.data); };
  ws.onerror = () => { /* onclose follows with the retry */ };
  ws.onclose = () => {
    if (gen !== sigGen) return;
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
    list.innerHTML = '<p class="empty">Add a matchmaker server in settings to see nearby devices — or use Share / Receive below.</p>';
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
    if (vibrateOn && navigator.vibrate) {
      try { navigator.vibrate([120, 60, 120]); } catch (e) { /* noop */ }
    }
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
      setStatus('idle', 'not connected');
    }
  }
}

async function acceptCall() {
  const inv = incomingOffer;
  incomingOffer = null;
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
    sigSend({ t: 'register', id: myId, name: myName });
  });
  const srvInp = $('serverInput');
  srvInp.value = storeGet(LS_SERVER) || '';
  srvInp.addEventListener('change', () => {
    const u = normalizeServerUrl(srvInp.value);
    srvInp.value = u;
    storeSet(LS_SERVER, u);
    sigConnect();
  });
  $('btnAccept').addEventListener('click', acceptCall);
  $('btnDecline').addEventListener('click', declineCall);
  renderKnown();
  // Drop settings: restore persisted toggles, then join only if wanted.
  autoConn = storeGet(LS_AUTOCONN) !== '0';
  vibrateOn = storeGet(LS_VIBRATE) !== '0';
  $('autoConnChk').checked = autoConn;
  $('vibrateChk').checked = vibrateOn;
  $('autoConnChk').addEventListener('change', (e) => setAutoConn(e.target.checked));
  $('vibrateChk').addEventListener('change', (e) => {
    vibrateOn = e.target.checked;
    storeSet(LS_VIBRATE, vibrateOn ? '1' : '0');
  });
  if (autoConn) sigConnect(); else setSigDot('off');
}

/* ---------- wiring ---------- */
initTapToConnect();
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
document.addEventListener('paste', (e) => {
  if ($('view-chat').classList.contains('hidden')) return;
  const files = (e.clipboardData && e.clipboardData.files) || [];
  Array.from(files).forEach(enqueueFile);
});


setStatus('idle', 'not connected');
