// Builds a multi-size .ico from per-size PNGs in src-tauri/icons/source (PNG-compressed ICO entries).
// Small sizes come from the simplified/hand-tuned art so they stay crisp in the taskbar.
//   node scripts/make-ico.mjs                          → icon.ico    from ico-<size>.png
//   node scripts/make-ico.mjs indexer- indexer.ico     → indexer.ico from indexer-<size>.png
import { readFileSync, writeFileSync } from "node:fs";

const [prefix = "ico-", outName = "icon.ico"] = process.argv.slice(2);
const dir = new URL("../src-tauri/icons/source/", import.meta.url);
const sizes = [16, 24, 32, 48, 64, 128, 256];
const images = sizes.map((s) => ({ size: s, data: readFileSync(new URL(`${prefix}${s}.png`, dir)) }));

const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0); // reserved
header.writeUInt16LE(1, 2); // type: icon
header.writeUInt16LE(images.length, 4);

const entries = Buffer.alloc(16 * images.length);
let offset = 6 + entries.length;
images.forEach((img, i) => {
  const e = i * 16;
  entries.writeUInt8(img.size >= 256 ? 0 : img.size, e); // width (0 = 256)
  entries.writeUInt8(img.size >= 256 ? 0 : img.size, e + 1); // height
  entries.writeUInt8(0, e + 2); // palette colors
  entries.writeUInt8(0, e + 3); // reserved
  entries.writeUInt16LE(1, e + 4); // color planes
  entries.writeUInt16LE(32, e + 6); // bits per pixel
  entries.writeUInt32LE(img.data.length, e + 8);
  entries.writeUInt32LE(offset, e + 12);
  offset += img.data.length;
});

const out = new URL(`../src-tauri/icons/${outName}`, import.meta.url);
writeFileSync(out, Buffer.concat([header, entries, ...images.map((i) => i.data)]));
console.log(`${outName}: ${images.length} sizes (${sizes.join(", ")}), ${offset} bytes`);
