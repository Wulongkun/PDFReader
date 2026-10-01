// 重排 .ico 目录条目，让最大尺寸（256×256）排到最前。
// 原因：Tauri 运行时窗口图标（任务栏图标）只解码 icon_dir.entries()[0]，
// 若第一个条目是 16×16，任务栏图标会被放大成低清。exe 资源不受影响（Windows 自选尺寸）。
import { readFileSync, writeFileSync } from 'node:fs';

const p = process.argv[2];
if (!p) {
  console.error('用法: node reorder-ico.mjs <path.ico>');
  process.exit(1);
}
const buf = readFileSync(p);
if (buf.length < 6 || buf.readUInt16LE(2) !== 1) {
  console.error('不是合法的 .ico 文件');
  process.exit(1);
}
const count = buf.readUInt16LE(4);
const entries = [];
for (let i = 0; i < count; i++) {
  const off = 6 + i * 16;
  const w = buf[off] === 0 ? 256 : buf[off];
  const h = buf[off + 1] === 0 ? 256 : buf[off + 1];
  entries.push({ raw: buf.subarray(off, off + 16), area: w * h, w, h });
}
const sorted = [...entries].sort((a, b) => b.area - a.area);
const header = buf.subarray(0, 6);
const data = buf.subarray(6 + count * 16); // 各帧位图数据（偏移是绝对地址，保持不变）
const out = Buffer.concat([header, ...sorted.map((e) => e.raw), data]);
writeFileSync(p, out);
console.log('重排前:', entries.map((e) => `${e.w}x${e.h}`).join(', '));
console.log('重排后:', sorted.map((e) => `${e.w}x${e.h}`).join(', '));
