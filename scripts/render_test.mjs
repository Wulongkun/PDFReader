import * as pdfjsLib from '../src/vendor/pdf.min.mjs';
import { readFileSync } from 'fs';

const data = new Uint8Array(readFileSync('D:/副业/PE_eng.pdf'));
const doc = await pdfjsLib.getDocument({ data }).promise;
const page = await doc.getPage(1);
const viewport = page.getViewport({ scale: 0.1 });
const W = Math.floor(viewport.width), H = Math.floor(viewport.height);

let captured = null;
const ctx = {
  save(){}, restore(){}, transform(){}, setTransform(){}, scale(){}, translate(){},
  beginPath(){}, moveTo(){}, lineTo(){}, closePath(){}, clip(){}, stroke(){}, fill(){},
  fillRect(){}, rect(){}, drawImage(){},
  createImageData(w,h){ return { width:w, height:h, data:new Uint8ClampedArray(w*h*4) }; },
  getImageData(){ return null; },
  putImageData(imgData){ captured = imgData; },
  resetTransform(){}, setLineDash(){},
  getTransform(){ return {a:1,b:0,c:0,d:1,e:0,f:0}; },
};
try {
  await page.render({ canvasContext: ctx, viewport, canvas: null }).promise;
} catch (e) { console.log('render err', e.message); }
if (captured) {
  const d = captured.data;
  let mn=255, mx=0, sum=0, n=d.length/4;
  for (let i=0;i<d.length;i+=4){ const g=d[i]; if(g<mn)mn=g; if(g>mx)mx=g; sum+=g; }
  console.log('captured', captured.width, 'x', captured.height, 'gray min', mn, 'max', mx, 'mean', Math.round(sum/n));
} else {
  console.log('no putImageData captured');
}
