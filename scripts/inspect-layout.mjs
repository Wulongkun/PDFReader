import { readFileSync } from 'node:fs';
import { getDocument, GlobalWorkerOptions } from '../src/vendor/pdf.min.mjs';
import { WorkerMessageHandler } from '../src/vendor/pdf.worker.min.mjs';

// 在 Node 里跑浏览器版 pdf.js：用一个回环 port 直连 WorkerMessageHandler。
class LoopbackPort {
  constructor() { this._on = null; this._pending = new Map(); this._id = 0; }
  on(ev, cb) { if (ev === 'message') this._on = cb; }
  postMessage(msg) {
    if (msg && msg.action) {
      const send = (m) => this._on && this._on({ data: m });
      // 简化：直接在主线程驱动 handler（文本提取不依赖真正的 worker 线程）。
      Promise.resolve().then(() => {
        this._handler ? this._handler(msg, send) : null;
      });
    }
  }
  start() { this._handler = WorkerMessageHandler; }
  terminate() {}
}

const port = new LoopbackPort();
port.start();
GlobalWorkerOptions.workerPort = port;

const pdfPath = process.argv[2];
const data = new Uint8Array(readFileSync(pdfPath));
const doc = await getDocument({ data }).promise;
const n = doc.numPages;
console.log('pages:', n);

for (let p = 1; p <= Math.min(3, n); p++) {
  const page = await doc.getPage(p);
  const tc = await page.getTextContent();
  const items = (tc.items || []).filter(i => 'str' in i && i.str.trim());
  const vp = page.getViewport({ scale: 1 });
  console.log(`\n=== page ${p} viewport ${vp.width.toFixed(0)}x${vp.height.toFixed(0)} items=${items.length} ===`);

  // x0 histogram (40 bins over item x0)
  const xs = items.map(i => i.transform[4]);
  const minX = Math.min(...xs), maxX = Math.max(...xs);
  const B = 40;
  const hist = new Array(B).fill(0);
  for (const i of items) {
    const k = Math.max(0, Math.min(B-1, Math.floor((i.transform[4]-minX)/(maxX-minX)*B)));
    hist[k]++;
  }
  const maxH = Math.max(...hist);
  let bar = '';
  for (let k = 0; k < B; k++) {
    const v = hist[k];
    bar += (v === 0 ? '.' : (v === maxH ? '#' : 'o')) + '';
  }
  console.log('x0 range', minX.toFixed(0), '..', maxX.toFixed(0));
  console.log('hist:', bar, ' (max=' + maxH + ')');
  // print first 6 items
  for (const i of items.slice(0, 6)) {
    console.log(`  y=${i.transform[5].toFixed(1)} x0=${i.transform[4].toFixed(1)} w=${(i.width||0).toFixed(1)} h=${(i.height||0).toFixed(1)} "${i.str.slice(0,30)}"`);
  }
}
