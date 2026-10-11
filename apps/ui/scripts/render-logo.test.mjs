import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { inflateSync } from 'node:zlib';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const ICONS = path.join(ROOT, 'assets', 'icons');
const PNG_DIR = path.join(ICONS, 'png');
const TAURI_DIR = path.join(ROOT, 'apps', 'ui', 'src-tauri');
const SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
const PNG_SIG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function readIhdr(buf) {
  assert.ok(buf.subarray(0, 8).equals(PNG_SIG), 'PNG signature');
  assert.equal(buf.toString('ascii', 12, 16), 'IHDR', 'first chunk is IHDR');
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20), bitDepth: buf[24], colorType: buf[25] };
}

function parsePng(buf) {
  assert.ok(buf.subarray(0, 8).equals(PNG_SIG), 'PNG signature');
  let off = 8;
  let ihdr = null;
  const idat = [];
  while (off + 12 <= buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString('ascii', off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === 'IHDR') ihdr = { width: data.readUInt32BE(0), height: data.readUInt32BE(4), bitDepth: data[8], colorType: data[9] };
    else if (type === 'IDAT') idat.push(data);
    else if (type === 'IEND') break;
    off += 12 + len;
  }
  return { ihdr, raw: inflateSync(Buffer.concat(idat)) };
}

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  if (pb <= pc) return b;
  return c;
}

function unfilter(raw, width, height, bpp) {
  const stride = width * bpp;
  const out = Buffer.alloc(stride * height);
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < height; y++) {
    const ft = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const cur = out.subarray(y * stride, (y + 1) * stride);
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? cur[x - bpp] : 0;
      const b = prev[x];
      const c = x >= bpp ? prev[x - bpp] : 0;
      let pred;
      switch (ft) {
        case 0: pred = 0; break;
        case 1: pred = a; break;
        case 2: pred = b; break;
        case 3: pred = (a + b) >> 1; break;
        case 4: pred = paeth(a, b, c); break;
        default: throw new Error(`bad filter type ${ft} on row ${y}`);
      }
      cur[x] = (line[x] + pred) & 0xff;
    }
    prev = cur;
  }
  return out;
}

const near = (c, ref, tol) => c.every((v, i) => Math.abs(v - ref[i]) <= tol);

test('icon PNGs exist with correct IHDR width and height', () => {
  for (const size of SIZES) {
    const file = path.join(PNG_DIR, `icon-${size}.png`);
    assert.ok(existsSync(file), `missing ${file}`);
    const h = readIhdr(readFileSync(file));
    assert.equal(h.width, size, `icon-${size}.png width`);
    assert.equal(h.height, size, `icon-${size}.png height`);
  }
});

test('icon-1024.png is 8-bit RGBA (colour type 6)', () => {
  const h = readIhdr(readFileSync(path.join(PNG_DIR, 'icon-1024.png')));
  assert.equal(h.bitDepth, 8);
  assert.equal(h.colorType, 6);
});

test('icon-1024.png corner pixels are transparent and centre is opaque logo colour', () => {
  const { ihdr, raw } = parsePng(readFileSync(path.join(PNG_DIR, 'icon-1024.png')));
  const { width, height } = ihdr;
  const px = unfilter(raw, width, height, 4);
  const at = (x, y) => { const i = (y * width + x) * 4; return [px[i], px[i + 1], px[i + 2], px[i + 3]]; };
  assert.equal(at(0, 0)[3], 0, 'pixel (0,0) alpha');
  assert.equal(at(3, 3)[3], 0, 'pixel (3,3) alpha');
  const c = at(512, 512);
  assert.equal(c[3], 255, 'centre (512,512) alpha');
  const rgb = c.slice(0, 3);
  assert.ok(near(rgb, [237, 28, 36], 24) || near(rgb, [255, 255, 255], 24),
    `centre RGB ${rgb} is neither logo red (237,28,36) nor white within tolerance 24`);
});

test('clipcheck.ico header is valid ICO with 7 entries', () => {
  const ico = readFileSync(path.join(ICONS, 'clipcheck.ico'));
  assert.equal(ico.readUInt16LE(0), 0, 'reserved');
  assert.equal(ico.readUInt16LE(2), 1, 'type');
  assert.equal(ico.readUInt16LE(4), 7, 'count');
});

test('clipcheck.ico entries have expected sizes, valid PNG payloads, and no overlap', () => {
  const ico = readFileSync(path.join(ICONS, 'clipcheck.ico'));
  const expected = [16, 24, 32, 48, 64, 128, 256];
  const dirEnd = 6 + 16 * expected.length;
  const ranges = [];
  expected.forEach((size, i) => {
    const e = 6 + 16 * i;
    const w = ico[e] === 0 ? 256 : ico[e];
    const hgt = ico[e + 1] === 0 ? 256 : ico[e + 1];
    assert.equal(w, size, `entry ${i} width`);
    assert.equal(hgt, size, `entry ${i} height`);
    const bytes = ico.readUInt32LE(e + 8);
    const off = ico.readUInt32LE(e + 12);
    assert.ok(off >= dirEnd, `entry ${i} payload overlaps directory`);
    assert.ok(off + bytes <= ico.length, `entry ${i} payload exceeds file`);
    const payload = ico.subarray(off, off + bytes);
    const h = readIhdr(payload);
    assert.equal(h.width, size, `entry ${i} PNG width`);
    assert.equal(h.height, size, `entry ${i} PNG height`);
    ranges.push([off, off + bytes]);
  });
  ranges.sort((a, b) => a[0] - b[0]);
  for (let i = 1; i < ranges.length; i++) {
    assert.ok(ranges[i][0] >= ranges[i - 1][1], 'payload ranges overlap');
  }
});

test('assets/icons/clipcheck.svg equals logo/logo.svg byte-for-byte', () => {
  const a = readFileSync(path.join(ICONS, 'clipcheck.svg'));
  const b = readFileSync(path.join(ROOT, 'logo', 'logo.svg'));
  assert.equal(a.equals(b), true);
});

test('apps/ui/public/logo.svg equals logo/logo.svg byte-for-byte', () => {
  const a = readFileSync(path.join(ROOT, 'apps', 'ui', 'public', 'logo.svg'));
  const b = readFileSync(path.join(ROOT, 'logo', 'logo.svg'));
  assert.equal(a.equals(b), true);
});

test('tauri.conf.json bundle.icon is non-empty and every entry exists', () => {
  const conf = JSON.parse(readFileSync(path.join(TAURI_DIR, 'tauri.conf.json'), 'utf8'));
  const icons = conf.bundle?.icon;
  assert.ok(Array.isArray(icons) && icons.length > 0, 'bundle.icon must be a non-empty array');
  for (const entry of icons) {
    const abs = path.resolve(TAURI_DIR, entry);
    assert.ok(existsSync(abs), `bundle.icon entry "${entry}" does not exist at ${abs}`);
  }
});

test('tauri.conf.json bundle.icon includes icons/icon.ico and icons/32x32.png', () => {
  const conf = JSON.parse(readFileSync(path.join(TAURI_DIR, 'tauri.conf.json'), 'utf8'));
  assert.ok(conf.bundle.icon.includes('icons/icon.ico'), 'missing icons/icon.ico');
  assert.ok(conf.bundle.icon.includes('icons/32x32.png'), 'missing icons/32x32.png');
});

test('apps/ui/scripts/make-icons.mjs does not exist', () => {
  assert.equal(existsSync(path.join(ROOT, 'apps', 'ui', 'scripts', 'make-icons.mjs')), false);
});
