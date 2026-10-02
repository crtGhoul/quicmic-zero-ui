/* Minimal ZIP writer — "stored" (no compression), no dependencies.
 * zipStore(files) takes [{path, data: Uint8Array}] and returns a Uint8Array
 * of a valid .zip file. Verified against Info-ZIP's `unzip`.
 * Implicit directory entries (paths like "docs/a.txt") are fine —
 * unzip/Windows/macOS all handle them without explicit dir records. */
'use strict';

const ZipWriter = (() => {
  // CRC-32 (ISO 3309), table-driven.
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = (c & 1) ? (0xEDB88320 ^ (c >>> 1)) : (c >>> 1);
    table[n] = c >>> 0;
  }
  function crc32(u8) {
    let c = 0xFFFFFFFF;
    for (let i = 0; i < u8.length; i++) c = table[(c ^ u8[i]) & 0xFF] ^ (c >>> 8);
    return (c ^ 0xFFFFFFFF) >>> 0;
  }

  const hasTE = typeof TextEncoder !== 'undefined';
  const te = hasTE ? new TextEncoder() : null;
  function utf8(s) {
    if (te) return te.encode(s);
    const out = []; // tiny fallback for ancient browsers
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c < 128) out.push(c);
      else if (c < 2048) out.push(192 | (c >> 6), 128 | (c & 63));
      else out.push(224 | (c >> 12), 128 | ((c >> 6) & 63), 128 | (c & 63));
    }
    return new Uint8Array(out);
  }

  function dosDateTime(d) {
    return {
      time: ((d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1)) & 0xFFFF,
      date: (((d.getFullYear() - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate()) & 0xFFFF,
    };
  }

  /* Clean a filename for use inside the zip: no drive letters, no "..",
     forward slashes only, never empty. */
  function sanitizePath(p) {
    let s = String(p || 'file').replace(/\\/g, '/');
    s = s.split('/').filter((seg) => seg && seg !== '.' && seg !== '..').join('/');
    return s || 'file';
  }

  function zipStore(files) {
    const body = [];    // local file headers + file data
    const central = []; // central directory records
    let offset = 0;
    let centralSize = 0;
    const now = dosDateTime(new Date());

    for (const f of files) {
      const nameU8 = utf8(sanitizePath(f.path));
      const data = f.data; // Uint8Array
      const crc = crc32(data);

      const lh = new DataView(new ArrayBuffer(30));
      lh.setUint32(0, 0x04034b50, true); // local file header signature
      lh.setUint16(4, 20, true);         // version needed to extract
      lh.setUint16(6, 0x0800, true);     // flags: UTF-8 filenames
      lh.setUint16(8, 0, true);          // method: stored
      lh.setUint16(10, now.time, true);
      lh.setUint16(12, now.date, true);
      lh.setUint32(14, crc, true);
      lh.setUint32(18, data.length, true);
      lh.setUint32(22, data.length, true);
      lh.setUint16(26, nameU8.length, true);
      lh.setUint16(28, 0, true);         // extra field length
      body.push(new Uint8Array(lh.buffer), nameU8, data);

      const ch = new DataView(new ArrayBuffer(46));
      ch.setUint32(0, 0x02014b50, true); // central directory signature
      ch.setUint16(4, 20, true);         // version made by
      ch.setUint16(6, 20, true);         // version needed
      ch.setUint16(8, 0x0800, true);
      ch.setUint16(10, 0, true);
      ch.setUint16(12, now.time, true);
      ch.setUint16(14, now.date, true);
      ch.setUint32(16, crc, true);
      ch.setUint32(20, data.length, true);
      ch.setUint32(24, data.length, true);
      ch.setUint16(28, nameU8.length, true);
      ch.setUint16(30, 0, true);
      ch.setUint16(32, 0, true);
      ch.setUint16(34, 0, true);
      ch.setUint16(36, 0, true);
      ch.setUint32(38, 0, true);
      ch.setUint32(42, offset, true);    // offset of local header
      central.push(new Uint8Array(ch.buffer), nameU8);

      centralSize += 46 + nameU8.length;
      offset += 30 + nameU8.length + data.length;
    }

    const end = new DataView(new ArrayBuffer(22));
    end.setUint32(0, 0x06054b50, true); // end of central directory
    end.setUint16(8, files.length, true);
    end.setUint16(10, files.length, true);
    end.setUint32(12, centralSize, true);
    end.setUint32(16, offset, true);
    end.setUint16(20, 0, true);         // comment length

    const out = new Uint8Array(offset + centralSize + 22);
    let p = 0;
    const push = (u8) => { out.set(u8, p); p += u8.length; };
    body.forEach(push);
    central.forEach(push);
    push(new Uint8Array(end.buffer));
    return out;
  }

  return { zipStore, crc32, sanitizePath };
})();

if (typeof module !== 'undefined' && module.exports) {
  module.exports = ZipWriter;
}
