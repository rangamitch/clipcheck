// Renders the ClipCheck logo into the shared icon assets.
//
// Input:  logo/logo.svg (master art, edge-to-edge, transparent corners)
// Output (repo root, assets/icons/):
//   png/icon-<size>.png  for size in PNG_SIZES
//   clipcheck.ico        multi-size ICO with embedded PNG entries (ICO_SIZES)
//   clipcheck.svg        byte-for-byte copy of logo/logo.svg
//
// Usage (from anywhere): node apps/ui/scripts/render-logo.mjs

import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';

export const PNG_SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
export const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];

const here = path.dirname(fileURLToPath(import.meta.url));
// apps/ui/scripts -> repo root
const repoRoot = path.resolve(here, '..', '..', '..');
const svgSource = path.join(repoRoot, 'logo', 'logo.svg');
const outDir = path.join(repoRoot, 'assets', 'icons');
const pngDir = path.join(outDir, 'png');

/**
 * Renders the SVG to a square PNG of the given size. Transparent background is kept
 * (no background option), so the rounded corners stay alpha 0.
 * @param {Buffer} svg
 * @param {number} size
 * @returns {Buffer}
 */
export function renderPng(svg, size) {
  const resvg = new Resvg(svg, { fitTo: { mode: 'width', value: size } });
  const rendered = resvg.render();
  const png = rendered.asPng();
  if (rendered.width !== size || rendered.height !== size) {
    throw new Error(`expected ${size}x${size}, got ${rendered.width}x${rendered.height}`);
  }
  return png;
}

/**
 * Builds an ICO file from PNG payloads. Each entry is stored as an embedded PNG
 * (Windows Vista+ format). Sizes of 256 are encoded as 0 in the width/height bytes.
 * @param {{ size: number, png: Buffer }[]} entries
 * @returns {Buffer}
 */
export function buildIco(entries) {
  const count = entries.length;
  const headerLen = 6;
  const dirLen = 16 * count;
  const header = Buffer.alloc(headerLen);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type: 1 = icon
  header.writeUInt16LE(count, 4);

  const dir = Buffer.alloc(dirLen);
  let offset = headerLen + dirLen;
  entries.forEach(({ size, png }, i) => {
    const base = i * 16;
    const dim = size >= 256 ? 0 : size;
    dir.writeUInt8(dim, base); // width
    dir.writeUInt8(dim, base + 1); // height
    dir.writeUInt8(0, base + 2); // colour count (0 = none)
    dir.writeUInt8(0, base + 3); // reserved
    dir.writeUInt16LE(1, base + 4); // colour planes
    dir.writeUInt16LE(32, base + 6); // bits per pixel
    dir.writeUInt32LE(png.length, base + 8); // payload size
    dir.writeUInt32LE(offset, base + 12); // payload offset
    offset += png.length;
  });

  return Buffer.concat([header, dir, ...entries.map((e) => e.png)]);
}

async function main() {
  const svg = await readFile(svgSource);
  await mkdir(pngDir, { recursive: true });

  const pngs = new Map();
  for (const size of PNG_SIZES) {
    const png = renderPng(svg, size);
    pngs.set(size, png);
    await writeFile(path.join(pngDir, `icon-${size}.png`), png);
  }

  const ico = buildIco(ICO_SIZES.map((size) => ({ size, png: pngs.get(size) })));
  await writeFile(path.join(outDir, 'clipcheck.ico'), ico);
  await copyFile(svgSource, path.join(outDir, 'clipcheck.svg'));

  console.log(
    `rendered ${PNG_SIZES.length} PNGs, ICO with ${ICO_SIZES.length} entries, and SVG into ${path.relative(repoRoot, outDir)}`,
  );
}

// Run only when executed directly (not when imported by tests).
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error(err);
    process.exitCode = 1;
  });
}
