// 从源 logo 生成 Tauri 所需图标（PNG + 多尺寸 ICO）。运行：node scripts/gen-icons.mjs [source.png]
// 默认源：scripts/logo.png。纯 JS 实现 PNG 解码/缩放/编码，无需第三方依赖。
import { inflateSync, deflateSync } from 'node:zlib';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const outDir = join(root, 'src-tauri', 'icons');
const srcPath = process.argv[2] || join(root, 'scripts', 'logo.png');
mkdirSync(outDir, { recursive: true });

// CRC32（PNG 块校验）
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, 'ascii');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

// ---------- PNG 解码（8 位 RGB/RGBA，非隔行） ----------
function decodePng(buf) {
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error('不是 PNG 文件');
  let pos = 8;
  let width = 0, height = 0, bitDepth = 0, colorType = 0;
  const idat = [];
  while (pos + 8 <= buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString('ascii', pos + 4, pos + 8);
    const data = buf.subarray(pos + 8, pos + 8 + len);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      if (data[12] !== 0) throw new Error('不支持隔行扫描的 PNG');
    } else if (type === 'IDAT') {
      idat.push(Buffer.from(data));
    } else if (type === 'IEND') {
      break;
    }
    pos += 12 + len;
  }
  if (bitDepth !== 8 || (colorType !== 2 && colorType !== 6)) {
    throw new Error(`不支持的 PNG（位深=${bitDepth}，颜色类型=${colorType}）`);
  }
  const bpp = colorType === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * bpp;
  const recon = new Uint8Array(width * height * bpp);
  const paeth = (a, b, c) => {
    const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
    return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
  };
  for (let y = 0; y < height; y++) {
    const row = y * (stride + 1);
    const filter = raw[row];
    const prev = (y - 1) * stride;
    for (let i = 0; i < stride; i++) {
      const v = raw[row + 1 + i];
      const left = i >= bpp ? recon[y * stride + i - bpp] : 0;
      const up = y > 0 ? recon[prev + i] : 0;
      const upLeft = y > 0 && i >= bpp ? recon[prev + i - bpp] : 0;
      let r;
      switch (filter) {
        case 0: r = v; break;
        case 1: r = v + left; break;
        case 2: r = v + up; break;
        case 3: r = v + ((left + up) >> 1); break;
        case 4: r = v + paeth(left, up, upLeft); break;
        default: throw new Error('未知滤波类型 ' + filter);
      }
      recon[y * stride + i] = r & 0xff;
    }
  }
  // 转 RGBA
  const rgba = new Uint8Array(width * height * 4);
  for (let p = 0; p < width * height; p++) {
    const s = p * bpp, d = p * 4;
    rgba[d] = recon[s];
    rgba[d + 1] = recon[s + 1];
    rgba[d + 2] = recon[s + 2];
    rgba[d + 3] = bpp === 4 ? recon[s + 3] : 255;
  }
  return { width, height, rgba };
}

// ---------- 面积平均缩放下采样（抗锯齿） ----------
function resize(rgba, w, h, W, H) {
  const out = new Uint8Array(W * H * 4);
  for (let ty = 0; ty < H; ty++) {
    const y0 = Math.floor(ty * h / H);
    const y1 = Math.max(y0 + 1, Math.floor((ty + 1) * h / H));
    for (let tx = 0; tx < W; tx++) {
      const x0 = Math.floor(tx * w / W);
      const x1 = Math.max(x0 + 1, Math.floor((tx + 1) * w / W));
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = y0; sy < y1; sy++) {
        for (let sx = x0; sx < x1; sx++) {
          const idx = (sy * w + sx) * 4;
          r += rgba[idx]; g += rgba[idx + 1]; b += rgba[idx + 2]; a += rgba[idx + 3];
        }
      }
      const n = (y1 - y0) * (x1 - x0);
      const o = (ty * W + tx) * 4;
      out[o] = Math.round(r / n);
      out[o + 1] = Math.round(g / n);
      out[o + 2] = Math.round(b / n);
      out[o + 3] = Math.round(a / n);
    }
  }
  return out;
}

// ---------- PNG 编码（8 位 RGBA） ----------
function encodePng(w, h, rgba) {
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;  // bit depth
  ihdr[9] = 6;  // color type: RGBA
  const stride = w * 4;
  const raw = Buffer.alloc((stride + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    raw.set(rgba.subarray(y * stride, (y + 1) * stride), y * (stride + 1) + 1);
  }
  const idat = deflateSync(raw, { level: 9 });
  return Buffer.concat([sig, chunk('IHDR', ihdr), chunk('IDAT', idat), chunk('IEND', Buffer.alloc(0))]);
}

// ---------- ICO 编码（多尺寸，PNG 压缩） ----------
function encodeIco(pngs) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type: icon
  header.writeUInt16LE(pngs.length, 4);
  const entries = [];
  const datas = [];
  let offset = 6 + pngs.length * 16;
  for (const { size, buf } of pngs) {
    const e = Buffer.alloc(16);
    e[0] = size >= 256 ? 0 : size;
    e[1] = size >= 256 ? 0 : size;
    e[2] = 0; e[3] = 0;
    e.writeUInt16LE(1, 4);   // planes
    e.writeUInt16LE(32, 6);  // bpp
    e.writeUInt32LE(buf.length, 8);
    e.writeUInt32LE(offset, 12);
    entries.push(e);
    datas.push(buf);
    offset += buf.length;
  }
  return Buffer.concat([header, ...entries, ...datas]);
}

// ---------- 生成 ----------
const src = decodePng(readFileSync(srcPath));
const sizes = { '32x32.png': 32, '128x128.png': 128, '128x128@2x.png': 256, 'icon.png': 512 };
for (const [name, size] of Object.entries(sizes)) {
  writeFileSync(join(outDir, name), encodePng(size, size, resize(src.rgba, src.width, src.height, size, size)));
}
const icoSizes = [16, 24, 32, 48, 64, 128, 256];
const icoPngs = icoSizes.map((size) => ({
  size,
  buf: encodePng(size, size, resize(src.rgba, src.width, src.height, size, size)),
}));
writeFileSync(join(outDir, 'icon.ico'), encodeIco(icoPngs));
console.log('icons generated from', srcPath, '->', outDir);
