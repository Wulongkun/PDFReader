// PDF.js 已本地化到 src/vendor/，打包后的应用无需联网即可渲染 PDF。
// 升级方式：见 README「升级 PDF.js」。
import * as pdfjsLib from './vendor/pdf.min.mjs';

pdfjsLib.GlobalWorkerOptions.workerSrc = './vendor/pdf.worker.min.mjs';

const invoke = window.__TAURI__?.core?.invoke;
const $ = (id) => document.getElementById(id);

const state = {
  pdfDoc: null,
  name: '',
  path: '',
  pageNum: 1,
  scale: 1.2,
  fitScale: null,
  extractedText: '',
  pageEls: [],
  tocItems: null,
  tocPage: 0,             // 双栏目录当前页（0-based）
  tocPages: 1,            // 双栏目录总页数
  vp1Cache: [],           // 各页 scale=1 视口缓存（页面尺寸恒定，避免重建时异步取页导致闪白）
  zooming: false,         // 缩放手势进行中（期间跳过懒渲染、不清理页面，避免闪白）
  zoomAnchor: null,       // 缩放锚点：鼠标下的页 + 页内分数(0..1)，缩放时保持该 PDF 点不动
  zoomAnchorClient: null, // 锚点在视口中的位置（跟随鼠标缩放）
  rtl: false,             // 竖排古籍阅读模式：页面水平从右往左连续排列（第 1 页在最右）
  pro: false,             // 是否已激活 Pro（解锁导出三件套）
  sidebarPinned: false,   // 悬浮翻译栏是否固定（固定后点击 PDF 画布不收起）
};

function setStatus(msg) { $('status').textContent = msg || ''; }
function setEnabled(ids, on) { ids.forEach((id) => { $(id).disabled = !on; }); }

// 追踪布局/缩放问题：把关键状态写入 diag.log（带时间戳，便于看先后顺序）。
function logDiag(msg) {
  if (invoke) { try { invoke('log_diag', { msg: `[${Date.now() % 100000}] ${msg}` }); } catch { /* 忽略 */ } }
}

// ===== 授权（Pro 门控） =====

// 把授权状态渲染到设置里的状态行（无状态参数时重新拉取）。
function renderLicenseUi(s) {
  const el = $('license-status');
  if (el) {
    if (s && s.pro) {
      el.textContent = 'Pro 已激活' + (s.activated_at ? `（${new Date(s.activated_at * 1000).toLocaleDateString()}）` : '');
    } else {
      el.textContent = (s && s.message) ? s.message : '免费版';
    }
  }
}

// 从后端拉取授权状态，同步到 state.pro 与工具栏角标。
async function refreshLicenseStatus() {
  if (!invoke) return;
  try {
    const s = await invoke('get_license_status');
    state.pro = !!(s && s.pro);
    updateProBadges();
    renderLicenseUi(s);
    return s;
  } catch { /* 拉取失败按免费处理 */ state.pro = false; updateProBadges(); return null; }
}

// 用激活码在线激活；成功返回 true，失败弹提示并返回 false。
async function activateLicense(code) {
  if (!invoke) return false;
  try {
    const s = await invoke('activate_license', { code });
    if (s && s.pro) {
      state.pro = true;
      updateProBadges();
      renderLicenseUi(s);
      setStatus('激活成功，Pro 已解锁');
      return true;
    }
    setStatus('激活失败：' + ((s && s.message) || '未知错误'));
    return false;
  } catch (err) {
    setStatus('激活失败：' + err);
    return false;
  }
}

// 打开激活对话框（先填充已填过的激活码，便于用户续填）。
function openActivate() {
  const code = $('license-code') && $('license-code').value.trim();
  if (code) $('activate-code').value = code;
  $('activate-msg').textContent = '';
  $('activate').showModal();
}

// 提交激活：读码 → 调后端 → 成功后关闭对话框。
async function submitActivate() {
  const code = $('activate-code').value.trim();
  if (!code) { $('activate-msg').textContent = '请输入激活码'; return; }
  $('activate-msg').textContent = '正在激活…';
  const ok = await activateLicense(code);
  if (ok) {
    $('activate-msg').textContent = '';
    if ($('license-code')) $('license-code').value = code;
    $('activate').close();
  } else {
    $('activate-msg').textContent = $('status').textContent || '激活失败';
  }
}

// Pro 门控：未激活时弹出激活框并返回 false；已激活直接返回 true。
function requirePro() {
  if (state.pro) return true;
  setStatus('导出 Word / 文本 / 译文为 Pro 版专属，请先激活');
  openActivate();
  return false;
}

// 同步导出按钮的锁图标：未激活显示锁（提示需激活），激活后完全无标识。
function updateProBadges() {
  ['btn-export', 'btn-export-word', 'btn-export-word-tr'].forEach((id) => {
    const el = $(id);
    if (!el) return;
    el.classList.toggle('locked', !state.pro);
  });
}

// 设置分页切换：高亮对应分类，显示对应面板。
function switchSettingsTab(name) {
  document.querySelectorAll('.settings-tab').forEach((b) => {
    b.classList.toggle('active', b.dataset.tab === name);
  });
  document.querySelectorAll('.settings-panel').forEach((p) => {
    p.classList.toggle('active', p.dataset.panel === name);
  });
}


function dataUrlToBytes(dataUrl) {
  const b64 = dataUrl.split(',')[1];
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

// 后台扫描件规范化完成后，用规范化后的字节重新加载当前文档（大文件黑块修复）。
// 用 path 比对，避免切换文档后旧文档的后台结果覆盖新文档。
{
  const listen = window.__TAURI__?.event?.listen;
  if (listen) {
    listen('pdf-normalized', async (e) => {
      const payload = e.payload;
      const dataUrl = payload && payload.data_url;
      if (!state.pdfDoc || !dataUrl) return;
      if (payload.path && payload.path !== state.path) return;
      const page = state.pageNum;
      try {
        setStatus('正在应用扫描件优化…');
        const bytes = dataUrlToBytes(dataUrl);
        state.pdfDoc = await pdfjsLib.getDocument({ data: bytes }).promise;
        state.vp1Cache = [];
        await rebuildPages();
        if (page > 1) goTo(page); else updatePageIndicator(1);
        setStatus('');
      } catch (err) {
        setStatus('扫描件优化失败：' + err);
      }
    });
  }
}

// 译文导出进度：后端逐段并发翻译时上报 { done, total }，状态栏显示实时进度。
{
  const listen = window.__TAURI__?.event?.listen;
  if (listen) {
    listen('translate-progress', (e) => {
      const p = e.payload;
      if (p && typeof p.done === 'number' && typeof p.total === 'number') {
        setStatus(`正在翻译全文… ${p.done}/${p.total}`);
      }
    });
  }
}

// 读取配置里的竖排古籍（RTL）阅读模式开关。
async function loadRtlSetting() {
  try {
    const cfg = await invoke('get_config');
    return !!(cfg && cfg.viewer && cfg.viewer.rtl);
  } catch { return false; }
}

// 读取配置里的竖排目录位置（top / left / double），并应用到 body class。返回位置字符串。
async function applyTocPosition() {
  let pos = 'top';
  try {
    const cfg = await invoke('get_config');
    pos = (cfg && cfg.viewer && cfg.viewer.toc_position) || 'top';
  } catch { /* 保持默认 top */ }
  if (pos !== 'left' && pos !== 'double') pos = 'top';
  document.body.classList.toggle('toc-top', pos === 'top');
  document.body.classList.toggle('toc-left', pos === 'left');
  document.body.classList.toggle('toc-double', pos === 'double');
  applyTocHeight();
  return pos;
}

// 目录高度（顶部位置）本地保存，拖拽调节后记忆。
const TOC_HEIGHT_KEY = 'toc-height';
function applyTocHeight() {
  let h = 0;
  try { h = parseFloat(localStorage.getItem(TOC_HEIGHT_KEY)) || 0; } catch { h = 0; }
  const panel = $('toc-panel');
  if (panel && h > 0) panel.style.setProperty('--toc-height', h + 'px');
}
function saveTocHeight(h) {
  try { localStorage.setItem(TOC_HEIGHT_KEY, String(h)); } catch { /* 忽略 */ }
}

// 自动检测书籍排版方向（横排/竖排）：采样前几页文本层，综合两种信号投票。
// (1) 90° 旋转文字项：竖排 PDF 的文字项常带 90° 旋转（|b| > |a|）。
// (2) 未旋转但竖排（字符正立、按列堆叠）的项：比较相邻文字项的坐标——
//     同列内 Δy 显著而 Δx≈0 → 竖排；同行内 Δx 显著而 Δy≈0 → 横排；跨行/跨列的大跳自然偏向文档真实方向。
// 返回 true=竖排 / false=横排 / null=无法判定（扫描件或文本太少）。
async function detectVertical(doc) {
  if (!doc) return null;
  // 跳过封面/扉页/序/目录等前置页（常横排或稀疏，会干扰正文方向），从第 6 页开始采样；
  // 短文档（<6 页）无前置页，仍从头采样。
  const start = doc.numPages >= 6 ? 6 : 1;
  const end = Math.min(start + 4, doc.numPages);   // 最多 5 页
  let vert = 0, horiz = 0;
  for (let p = start; p <= end; p++) {
    try {
      const page = await doc.getPage(p);
      const tc = await page.getTextContent();
      let prev = null;
      for (const it of (tc && tc.items) || []) {
        if (!('str' in it) || !it.str.trim()) continue;
        const t = it.transform || [];
        const a = t[0] || 0, b = t[1] || 0;
        if (!a && !b) continue;
        if (Math.abs(b) > Math.abs(a)) { vert++; prev = null; continue; }   // 90° 旋转 → 竖排
        // 未旋转项：按相邻项坐标判断行进方向
        const x = t[4] || 0, y = t[5] || 0;
        const scale = Math.max(it.width || 0, it.height || 0, 1);   // 字号刻度，容错子像素抖动
        if (prev) {
          const dx = Math.abs(x - prev.x), dy = Math.abs(y - prev.y);
          if (dy > dx * 2 && dy > scale * 0.3) vert++;       // 向下堆叠 → 竖排
          else if (dx > dy * 2 && dx > scale * 0.3) horiz++; // 向右行进 → 横排
        }
        prev = { x, y };
      }
    } catch { /* 采样页失败则跳过 */ }
  }
  const total = vert + horiz;
  if (total < 30) return null;   // 文本太少（多为扫描件），无法可靠判定
  const ratio = vert / total;
  if (ratio >= 0.6) return true;
  if (ratio <= 0.4) return false;
  return null;                   // 混合排版 / 不确定，交回手动设置
}

// ===== 扫描版（无文本层）排版方向检测：图像投影法 =====
// 不依赖 OCR，纯几何判断：把页面二值化成「墨迹/空白」，分别沿行、列统计墨迹投影，
// 数每条轴上的「墨迹连段数」。横排书：行投影被行间距切成多段（一行一段），列投影几乎连续；
// 竖排书：列投影被列间距切成多段（一列一段），行投影几乎连续。
// 泛黄纸底用 Otsu 自适应阈值，四周空白边距按内容范围裁剪，抗干扰强、且与字号/字体无关。
// 返回 true=竖排 / false=横排 / null=页面空白或图文混合、无法判定。

// Otsu 全局二值化阈值：兼容白底与泛黄纸底。
function otsuThreshold(gray) {
  const hist = new Uint32Array(256);
  for (let i = 0; i < gray.length; i++) hist[gray[i]]++;
  const total = gray.length;
  let sum = 0;
  for (let i = 0; i < 256; i++) sum += i * hist[i];
  let sumB = 0, wB = 0, best = -1, bestVar = 0;
  for (let t = 0; t < 256; t++) {
    wB += hist[t];
    if (!wB) continue;
    const wF = total - wB;
    if (!wF) break;
    sumB += t * hist[t];
    const mB = sumB / wB, mF = (sum - sumB) / wF;
    const v = wB * wF * (mB - mF) * (mB - mF);
    if (v > bestVar) { bestVar = v; best = t; }
  }
  return best < 0 ? 128 : best;
}

// 数 profile[start..end] 里「墨迹连段」的数量；profile[i] >= minInk 视为有墨迹。
function countRuns(profile, start, end, minInk) {
  let runs = 0, inRun = false;
  for (let i = start; i <= end; i++) {
    const ink = profile[i] >= minInk;
    if (ink && !inRun) { runs++; inRun = true; }
    else if (!ink) inRun = false;
  }
  return runs;
}

// 单页投影检测：渲染 → 二值化 → 行/列墨迹投影 → 比较两轴连段数。
async function detectVerticalByProjection(page) {
  // 投影法低分辨率即可（最长边 ~1400px），比整页高质量 OCR 快得多。
  const vp1 = page.getViewport({ scale: 1 });
  const maxEdge = Math.max(vp1.width, vp1.height);
  const scale = Math.max(1, 1400 / maxEdge);
  const canvas = await pageToCanvas(page, scale);
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const { data } = img;
  const W = canvas.width, H = canvas.height, N = W * H;
  const gray = new Uint8Array(N);
  for (let i = 0, p = 0; i < N; i++, p += 4) {
    gray[i] = (data[p] * 299 + data[p + 1] * 587 + data[p + 2] * 114) / 1000 | 0;
  }
  const th = otsuThreshold(gray);
  const row = new Uint32Array(H), col = new Uint32Array(W);
  for (let y = 0; y < H; y++) {
    const base = y * W;
    for (let x = 0; x < W; x++) {
      if (gray[base + x] < th) { row[y]++; col[x]++; }
    }
  }
  // 内容范围：去掉四周空白边距（避免页边距被误判成「连段」）。
  const rowTh = Math.max(3, H * 0.01 | 0), colTh = Math.max(3, W * 0.01 | 0);
  let top = 0, bottom = H - 1, left = 0, right = W - 1;
  while (top < H && row[top] < rowTh) top++;
  while (bottom > top && row[bottom] < rowTh) bottom--;
  while (left < W && col[left] < colTh) left++;
  while (right > left && col[right] < colTh) right--;
  const rowRuns = countRuns(row, top, bottom, rowTh);
  const colRuns = countRuns(col, left, right, colTh);
  // 内容量校验：正文页应占页面相当比例。空白页/窄条注文/单栏目录等稀疏页的连段数不可靠，
  // 判为 null（不作数），由上层继续往后找正文页。
  const contentW = right - left + 1, contentH = bottom - top + 1;
  const contentArea = contentW * contentH;
  const pageArea = W * H;
  let totalInk = 0;
  for (let y = 0; y < H; y++) totalInk += row[y];
  logDiag(`[proj] rowRuns=${rowRuns} colRuns=${colRuns} box=${left},${top}-${right},${bottom} area=${(contentArea / pageArea * 100).toFixed(1)}% ink=${(totalInk / pageArea * 100).toFixed(2)}% scale=${scale.toFixed(2)}`);
  if (contentArea < pageArea * 0.25) return null;   // 内容区不足页面 1/4（空白/窄条）→ 不作数
  if (totalInk < pageArea * 0.005) return null;     // 墨迹过少 → 同样不作数
  // 方向判定。夹批/眉批（评本）会在行方向额外制造大量墨迹段，使 rowRuns 虚高，
  // 因此「列投影分节数」本身是更可靠的竖排信号：横排书几乎不会出现 8 列以上的分节，
  // 而古籍半叶正文通常有 14~20 列（参见红楼梦三家评本 colRuns=18、rowRuns 却达 9~20）。
  if (rowRuns >= 3 && rowRuns > colRuns * 2) return false;   // 行投影显著更多 → 横排
  if (colRuns >= 8 && colRuns >= rowRuns) return true;       // 8 列以上且列数≥行段数 → 竖排
  return null;                                               // 图文混合/不确定
}

// 扫描版兜底检测入口：跳过前置页，从第 6 页起跳页采样到 ~80 页，对有效结果**多数投票**。
// 前置页（封面/目录/凡例/图咏/出版说明）常为横排或图文混排，会把方向带偏——若只取第一个有效页，
// 一旦第 6 页是横排前置页就会误判（如红楼梦三家评本）。多采几页投票后，正文竖排自然胜出。
// 稀疏/空白页会被 detectVerticalByProjection 判为 null 并跳过，不计票。
async function detectVerticalScanned(doc) {
  if (!doc) return null;
  const n = doc.numPages;
  // 短文档（<6 页）无前置页，逐页采样；长文档从第 6 页起每 4 页采一页，最多到第 80 页。
  const pages = [];
  if (n < 6) {
    for (let p = 1; p <= n; p++) pages.push(p);
  } else {
    for (let p = 6; p <= Math.min(n, 80); p += 4) pages.push(p);
  }

  let vert = 0, horiz = 0;
  for (let i = 0; i < pages.length; i++) {
    let verdict = null;
    try {
      const page = await doc.getPage(pages[i]);
      verdict = await detectVerticalByProjection(page);
    } catch { /* 采样页失败则跳过 */ }
    if (verdict === true) vert++;
    else if (verdict === false) horiz++;
    else continue; // 空白/稀疏/图文混合页不作数（不计票）

    // 领先方即便剩余页全部投给对方也无法被反超时，提前终止以省去无谓的整页渲染。
    const remaining = pages.length - i - 1;
    if (vert - horiz > remaining) return true;
    if (horiz - vert > remaining) return false;
  }
  if (vert > horiz) return true;
  if (horiz > vert) return false;
  return null;
}

// 排版方向缓存编码（与 Rust 侧 orientation.rs 约定一致）。
const ORIENT_VERTICAL = 1, ORIENT_HORIZONTAL = 0, ORIENT_UNKNOWN = -1;

// 解析一本书的排版方向：优先读本地缓存（导入/上次打开时已识别），未命中才现场检测并写回。
// 返回 true=竖排 / false=横排 / null=无法判定。
async function resolveOrientation(path, doc) {
  const cached = await invoke('get_orientation', { path }).catch(() => null);
  if (cached === ORIENT_VERTICAL) return true;
  if (cached === ORIENT_HORIZONTAL) return false;
  if (cached === ORIENT_UNKNOWN) return null;

  let detected = await detectVertical(doc);
  if (detected === null) detected = await detectVerticalScanned(doc);
  const code = detected === true ? ORIENT_VERTICAL : detected === false ? ORIENT_HORIZONTAL : ORIENT_UNKNOWN;
  await invoke('set_orientation', { path, code }).catch(() => {});
  return detected;
}

// 打开 PDF 对话框：选择文件后走公共加载流程（与书架点击书籍共用）。
async function openPdf() {
  try {
    setStatus('正在打开文件…');
    const result = await invoke('open_pdf');
    if (!result) { setStatus(''); return; } // 用户取消选择

    await loadPdfDocument(result.path, result.name);
    invoke('record_recent', { path: result.path, name: nameFromPath(result.path) }).catch(() => {});
  } catch (err) {
    reportOpenError('打开失败：' + err);
  }
}

// 打开失败提示：书架模式下状态栏被隐藏，须回到书架顶部的提示行；否则用阅读器状态栏。
function reportOpenError(msg) {
  if (document.body.classList.contains('shelf-mode')) shelfStatus(msg);
  else setStatus(msg);
}

// 打开内置用户手册：从随应用打包的资源直接 fetch，无需依赖磁盘上的文件。
// 手册作为内置 PDF 直接走公共加载流程（与普通文档一致），用固定伪路径做排版缓存键。
const MANUAL_PATH = '__内置使用手册__';
async function openManual() {
  try {
    $('settings').close();
    setStatus('正在打开用户手册…');
    const resp = await fetch('manual.pdf');
    if (!resp.ok) throw new Error('手册资源缺失（HTTP ' + resp.status + '）');
    const buf = await resp.arrayBuffer();
    await loadPdfData(new Uint8Array(buf), MANUAL_PATH, '用户手册');
  } catch (err) {
    reportOpenError('打开用户手册失败：' + err);
  }
}

// 加载并渲染一份 PDF：二进制读取 → PDF.js 解析 → 方向检测 → 重建页面。
// 由 openPdf（对话框）与 openBook（书架点击）共用。
async function loadPdfDocument(path, name) {
  // 二进制读取（ArrayBuffer），避免 base64 往返——大扫描件打开更快。
  const bytes = await invoke('read_pdf', { path });
  await loadPdfData(new Uint8Array(bytes), path, name);
}

// 用已就绪的字节加载并渲染 PDF（openPdf / 书架 / 内置手册共用）。
async function loadPdfData(data, path, name) {
  state.pdfDoc = await pdfjsLib.getDocument({ data }).promise;
  state.name = name;
  state.path = path;
  state.pageNum = 1;
  state.fitScale = null;
  state.scale = 1.2;
  state.extractedText = '';
  state.tocItems = null;
  state.vp1Cache = [];
  $('toc-list').innerHTML = '';
  $('toc-generate').style.display = 'none';
  $('toc-hint').style.display = 'none';
  $('text-content').textContent = '尚未提取。';
  $('text-content').classList.remove('markdown');
  $('translation').textContent = '';
  $('translation').classList.remove('markdown');
  $('btn-copy').disabled = true;

  setEnabled([
    'btn-prev', 'btn-next', 'page-input',
    'btn-zoom-out', 'btn-zoom-in', 'btn-fit',
    'btn-extract', 'btn-extract-pages', 'btn-export', 'btn-export-word', 'btn-export-word-tr', 'btn-translate', 'btn-toc',
  ], true);

  // 清掉上一文档可能残留的缩放状态。
  resetZoomState();

  // 判断横排/竖排：优先用本地缓存，未命中再现场检测（文本层旋转 + 扫描件投影），并写回缓存。
  const detected = await resolveOrientation(path, state.pdfDoc);
  state.rtl = detected === true;
  $('pdf-pages').classList.toggle('rtl', state.rtl);
  document.body.classList.toggle('rtl', state.rtl); // 竖排时目录/翻译栏左右互换
  await applyTocPosition(); // 应用竖排目录位置（top/left）
  resetSidebarLayout(); // 新文档：清理悬浮翻译栏的固定/展开状态，回到干净初始态
  // 竖排按高度适应、横排按宽度适应，按钮提示随排版方向切换。
  $('btn-fit').title = state.rtl ? '适应高度' : '适应宽度';

  // 切到阅读器视图后再取尺寸：从书架打开时 reader 仍隐藏，此时 clientWidth/Height 为 0，
  // 会得到 null 的 fitScale，且竖排模式下 scrollWidth=0 导致初始滚动停在最左端（=最后一页）。
  showViewer();

  // 默认适应视口：常规竖排按宽度适应、古籍（RTL）按高度适应，横向连续阅读且无纵向滚动条。
  const p1 = await state.pdfDoc.getPage(1);
  const vp1 = p1.getViewport({ scale: 1 });
  state.fitScale = state.rtl
    ? fitScaleForHeight($('viewer').clientHeight, vp1.height)
    : fitScaleFor($('viewer').clientWidth, vp1.width);
  logDiag(`[open] clientWidth=${$('viewer').clientWidth} clientHeight=${$('viewer').clientHeight} vp1=${vp1.width.toFixed(1)}x${vp1.height.toFixed(1)} rtl=${state.rtl} detect=${detected} fitScale=${state.fitScale}`);

  await rebuildPages();
  const viewer = $('viewer');
  if (state.rtl) {
    // 第 1 页在最右，初始滚动到最右端。
    viewer.scrollLeft = viewer.scrollWidth;
    viewer.scrollTop = 0;
  } else {
    viewer.scrollTop = 0;
  }
  updatePageIndicator();
  // 若目录侧边栏处于展开状态，载入新文档的目录。
  if (!$('toc-panel').classList.contains('collapsed')) openToc();
  setStatus(detected === true ? '检测到竖排排版，已切换竖排阅读模式'
    : detected === null ? '未能识别排版方向，默认按横排显示；竖排古籍请在设置开启'
    : '');
}

const devicePixel = () => window.devicePixelRatio || 1;

// 渲染倍率上限：大页面（扫描书 / 长页）在 HiDPI 下按 devicePixelRatio 渲染会得到超大画布，
// 部分 WebView2 / Chromium 会因此首帧合成成黑块（GPU 纹理上限）。限制画布长边不超过
// MAX_RENDER_EDGE 设备像素，超出则按比例降低渲染倍率；小画布不受影响。
const MAX_RENDER_EDGE = 4096;
function renderScale(cssW, cssH) {
  const d = devicePixel();
  const edge = Math.max(cssW, cssH) * d;
  if (edge <= MAX_RENDER_EDGE) return d;
  return Math.max(1, MAX_RENDER_EDGE / Math.max(cssW, cssH));
}

// 滚动中降清渲染：滚动时用低 DPR 快速出图（扫描页降分辨率几乎无感），
// 停下后再把可见页高清重绘，避免渲染块把滚动动画卡掉帧、出现空白页追赶。
let scrollActive = false;
let scrollSettleTimer = null;

function markScrollActivity() {
  if (!scrollActive) scrollActive = true;
  clearTimeout(scrollSettleTimer);
  scrollSettleTimer = setTimeout(() => {
    scrollActive = false;
    refreshVisiblePages();
  }, 140);
}

// 滚动停止后：把当前可见页中「低清渲染」的标记为需高清重绘
// （文本层按 CSS 倍率定位、不受 DPR 影响，无需重做）。
function refreshVisiblePages() {
  if (!state.pdfDoc || state.zooming) return;
  const scale = currentScale();
  for (const i of visiblePageIndices()) {
    const wrap = state.pageEls && state.pageEls[i];
    if (!wrap || wrap.dataset.rendered !== '1' || wrap.dataset.rendering === '1') continue;
    const renderedDpr = parseFloat(wrap.dataset.dpr || '0');
    // 高清基准必须与 renderPageCanvas 里的 renderScale 一致：大页面受 MAX_RENDER_EDGE 限制，
    // 实际高清 DPR 会低于 devicePixelRatio。若仍拿 devicePixel 当基准，会把「已渲染到上限」的页
    // 误判成未高清而反复重绘；且重绘时画布尺寸不变 → 无 prev 过渡 → 白底一闪，滚动停止后
    // 每次 settle 都闪一次（「闪好几下」）。这里用页面真实尺寸算 renderScale 作为基准。
    const vp1 = state.vp1Cache && state.vp1Cache[i];
    const fullD = vp1 ? renderScale(vp1.width * scale, vp1.height * scale) : devicePixel();
    if (renderedDpr >= fullD - 0.05) continue; // 已是高清，无需重绘
    delete wrap.dataset.rendered;
    enqueuePageRender(i);
  }
}

function currentScale() {
  return state.fitScale || state.scale;
}

// 计算「适应宽度」的缩放倍率。viewer 尚未布局完成（clientWidth≈0）或页面宽异常时返回 null，
// 避免用负数/近零倍率把页面缩成一条细线。
function fitScaleFor(clientWidth, vpWidth) {
  if (!(clientWidth > 48) || !(vpWidth > 0)) return null;
  const s = (clientWidth - 48) / vpWidth;
  return s > 0 ? s : null;
}

// 计算「适应高度」的缩放倍率（竖排古籍 RTL 模式用）：页面横向连续排列，纵向不再滚动。
// -50 = 上下内边距 40 + 底部横向滚动条 10；页面在容器内垂直居中（CSS margin:auto 0），
// 上下留白一致且收窄，不再下宽上窄。
function fitScaleForHeight(clientHeight, vpHeight) {
  if (!(clientHeight > 50) || !(vpHeight > 0)) return null;
  const s = (clientHeight - 50) / vpHeight;
  return s > 0 ? s : null;
}

// 为每一页创建占位容器（含正确尺寸的空 canvas），保证滚动条高度正确；
// 实际像素渲染由 IntersectionObserver 在页进入视野时懒加载。
async function rebuildPages(preRendered) {
  if (!state.pdfDoc) return;
  const container = $('pdf-pages');
  pageObserver.disconnect();

  const n = state.pdfDoc.numPages;
  const scale = currentScale();
  logDiag(`[rebuild] scale=${scale} fitScale=${state.fitScale} state.scale=${state.scale} n=${n}`);
  // 页面尺寸（scale=1 视口）恒定，缓存后重建可同步完成；
  // 先取齐视口再清空容器，避免 innerHTML 清空后在异步取页期间画出空白（缩放提交时闪白）。
  if (!state.vp1Cache || state.vp1Cache.length !== n) {
    state.vp1Cache = await Promise.all(
      Array.from({ length: n }, (_, i) =>
        state.pdfDoc.getPage(i + 1).then((p) => p.getViewport({ scale: 1 })),
      ),
    );
  }
  const vps = state.vp1Cache;
  const pre = preRendered || new Map();

  // 文本层按 --scale-factor 定位，须与 canvas 的渲染倍率保持一致。
  container.style.setProperty('--scale-factor', String(scale));
  // 页间距随缩放等比放大：让 transform 预览（整块等比缩放）与重绘布局完全一致，
  // 锚点换算 anchor.x/cur*target 才能精确成立，缩放时鼠标指向的内容不漂移。
  container.style.setProperty('--page-gap', (18 * scale) + 'px');

  container.innerHTML = '';
  renderPending.clear(); // 页面已重建，清掉旧页面遗留的待渲染索引
  const frag = document.createDocumentFragment();
  state.pageEls = new Array(n);
  // 竖排古籍（RTL）：DOM 顺序反转——第 n 页在最左、第 1 页在最右；state.pageEls 仍按页码索引。
  const indices = Array.from({ length: n }, (_, i) => i);
  if (state.rtl) indices.reverse();
  indices.forEach((i) => {
    const vp1 = vps[i];
    const w = vp1.width * scale;
    const h = vp1.height * scale;
    const wrap = document.createElement('div');
    wrap.className = 'pdf-page';
    wrap.dataset.index = String(i);

    const canvas = document.createElement('canvas');
    const d = renderScale(w, h);
    canvas.width = Math.floor(w * d);
    canvas.height = Math.floor(h * d);
    canvas.style.width = w + 'px';
    canvas.style.height = h + 'px';

    // 文本层覆盖在 canvas 上（透明文字，仅用于选择/复制）。
    const textLayer = document.createElement('div');
    textLayer.className = 'text-layer';

    // 已预渲染的页面直接填入清晰位图（提交缩放时无缝切换，避免闪白）。
    const bitmap = pre.get(i);
    if (bitmap) {
      canvas.getContext('2d').drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      wrap.dataset.rendered = '1';
      wrap.dataset.dpr = String(d); // 预渲染用的是完整 DPR，标记为高清
    }

    wrap.appendChild(canvas);
    wrap.appendChild(textLayer);
    frag.appendChild(wrap);
    state.pageEls[i] = wrap;
    pageObserver.observe(wrap);
  });
  container.appendChild(frag);
  $('zoom-info').textContent = Math.round(scale * 100) + '%';
}

// 渲染第 i 页（0-based）的 canvas 位图。
async function renderPageCanvas(i) {
  const wrap = state.pageEls && state.pageEls[i];
  if (!wrap || wrap.dataset.rendered === '1' || wrap.dataset.rendering === '1') return;
  wrap.dataset.rendering = '1';
  let renderErr = null;
  try {
    const canvas = wrap.querySelector('canvas');
    const page = await state.pdfDoc.getPage(i + 1);
    const scale = currentScale();
    const viewport = page.getViewport({ scale });
    // 滚动中用低 DPR 渲染（更快，扫描页几乎无感），停下后 refreshVisiblePages 会高清重绘。
    const d = renderScale(viewport.width, viewport.height);
    const effD = (scrollActive && !state.zooming) ? Math.min(d, 1) : d;
    const W = Math.floor(viewport.width * effD);
    const H = Math.floor(viewport.height * effD);

    // 升级重绘（可见画布上已有旧位图，典型如滚动中的低清图 → 停止后的高清图）：
    // 先渲染到离屏画布，完成后再一次性贴回可见画布。若直接画在可见画布上，PDF.js 分块渲染
    // 会逐块刷新，低清→高清过渡时出现「扫描线式」闪屏；离屏 + 末尾原子 blit 只切一帧，
    // 期间可见画布一直保留旧位图，不闪白。首帧（无旧位图）仍直接画在可见画布上，让内容尽快出现。
    const upgrade = canvas.width > 1 && canvas.height > 1 && !!wrap.dataset.dpr;
    const target = upgrade ? document.createElement('canvas') : canvas;
    target.width = W;
    target.height = H;
    if (!upgrade) {
      canvas.style.width = Math.floor(viewport.width) + 'px';
      canvas.style.height = Math.floor(viewport.height) + 'px';
    }
    const ctx = target.getContext('2d');
    // 先铺白底：部分 WebView2 会把「透明画布」合成成黑色块（类似 0×0 画布黑块问题），
    // 铺一层白底可避免大画布首帧出现黑屏。
    ctx.fillStyle = '#fff';
    ctx.fillRect(0, 0, W, H);
    ctx.setTransform(effD, 0, 0, effD, 0, 0);
    await page.render({ canvasContext: ctx, viewport }).promise;

    // 升级重绘：把离屏成品一次性贴回可见画布（同步、原子，无中间态）。
    if (upgrade) {
      canvas.width = W;
      canvas.height = H;
      canvas.style.width = Math.floor(viewport.width) + 'px';
      canvas.style.height = Math.floor(viewport.height) + 'px';
      canvas.getContext('2d').drawImage(target, 0, 0);
    }
    wrap.dataset.rendered = '1';
    wrap.dataset.dpr = String(effD); // 记录本页当前渲染用的 DPR，供 refresh 判断是否需高清重绘
  } catch (err) {
    renderErr = String((err && err.message) || err);
  } finally {
    delete wrap.dataset.rendering;
  }
  // 诊断（仅第一页）：统计暗像素占比，判断内容是否真的被画出来
  // （≈0=只有白底没画出内容；≈5%=正常；≈95%=整页反相成黑图），
  // 连同画布尺寸 / DPR / WebGL / 渲染错误写入本地日志，用于定位「大页面打开黑屏」。
  if (i === 0) logFirstPageDiag(wrap, renderErr);
}

// 第一页渲染诊断：把关键信息写入 `%APPDATA%/PDFReader/diag.log`（不显示在界面上）。
async function logFirstPageDiag(wrap, renderErr) {
  try {
    const canvas = wrap.querySelector('canvas');
    const w = canvas.width, h = canvas.height;
    let dark = 0, samples = 0, edge = '?', center = '?';
    try {
      // 缩小到 64×64 再采样：整屏 getImageData 会强制 GPU→CPU 读回大画布（最高 4096×4096，
      // 约 67MB），页面每次重渲染都跑一次会导致明显卡顿；缩小后既保留「是否黑屏/空白」信号，
      // 又几乎零开销，不影响缩放流畅度。
      const S = 64;
      const small = document.createElement('canvas');
      small.width = S; small.height = S;
      const sctx = small.getContext('2d');
      sctx.drawImage(canvas, 0, 0, w, h, 0, 0, S, S);
      const img = sctx.getImageData(0, 0, S, S).data;
      for (let p = 0; p < img.length; p += 4) {
        if (img[p] < 100 && img[p + 1] < 100 && img[p + 2] < 100) dark++;
        samples++;
      }
      const px = (x, y) => { const k = (y * S + x) * 4; return `${img[k]},${img[k + 1]},${img[k + 2]}`; };
      edge = px(Math.min(4, S - 1), Math.min(4, S - 1));
      center = px(S >> 1, S >> 1);
    } catch { /* getImageData 可能失败，忽略 */ }
    const webgl = !!document.createElement('canvas').getContext('webgl');
    const pct = samples ? (100 * dark / samples).toFixed(1) : '0';
    const msg = `[诊断] name=${state.name} size=${w}x${h} dpr=${devicePixel().toFixed(2)} webgl=${webgl} dark=${dark}/${samples}(${pct}%) edge=(${edge}) center=(${center}) renderErr=${renderErr || 'none'}`;
    if (invoke) { try { await invoke('log_diag', { msg }); } catch { /* 忽略日志写入失败 */ } }
  } catch { /* 诊断本身出错时静默忽略 */ }
}

// 为第 i 页构建可选中的文本层（透明文字覆盖在 canvas 上，仅用于选择/复制）。
// 扫描页 getTextContent 为空，得到空层，自然不可选择——这正是「先支持非扫描 PDF」。
async function renderTextLayer(i) {
  const wrap = state.pageEls && state.pageEls[i];
  if (!wrap || wrap.dataset.textLayer === '1' || wrap.dataset.textBuilding === '1') return;
  wrap.dataset.textBuilding = '1';
  const token = (wrap._tlToken || 0) + 1;
  wrap._tlToken = token;
  const layer = wrap.querySelector('.text-layer');
  try {
    const page = await state.pdfDoc.getPage(i + 1);
    const scale = currentScale();
    const viewport = page.getViewport({ scale });
    const textContent = await page.getTextContent();
    if (wrap._tlToken !== token) return; // 期间已被清理，放弃
    const tl = new pdfjsLib.TextLayer({
      textContentSource: textContent,
      container: layer,
      viewport,
    });
    wrap._textLayer = tl;
    await tl.render();
    if (wrap._tlToken !== token) { try { tl.cancel(); } catch { /* ignore */ } return; }
    wrap.dataset.textLayer = '1';
  } catch (err) {
    // 空层（扫描页 / 失败 / 已取消）均按完成处理，避免反复重试。
    if (wrap._tlToken === token) wrap.dataset.textLayer = '1';
  } finally {
    if (wrap._tlToken === token) delete wrap.dataset.textBuilding;
  }
}

// 页面进入视野时渲染 canvas 与文本层。
async function renderPageAt(i) {
  if (!state.pageEls || !state.pageEls[i]) return;
  // 缩放手势期间整页跳过渲染：缩小会一次性露出多页，逐页画 canvas（主线程重活）会让
  // rAF 动画掉帧、高刷屏尤为明显（一卡一卡）。新露出的页面先显示占位白块，提交缩放时
  // 由 observer 重新触发（state.zooming 已复位）统一重渲染，手感优先。
  if (state.zooming) return;
  await renderPageCanvas(i);
  await renderTextLayer(i);
}

// 页面远离视野时清空其位图与文本层以释放内存（占位尺寸保留）。
function clearPage(i) {
  renderPending.delete(i);
  const wrap = state.pageEls && state.pageEls[i];
  if (!wrap) return;

  // 位图
  if (wrap.dataset.rendered === '1') {
    const canvas = wrap.querySelector('canvas');
    // 归零到 1×1 而非 0×0：部分 WebView2 会把 0×0 画布合成成黑色块，
    // 导致滚动时页面下方出现黑块；1×1 透明位图缩放后仍透出白色背景。
    canvas.width = 1;
    canvas.height = 1;
    delete wrap.dataset.rendered;
  }

  // 文本层
  wrap._tlToken = (wrap._tlToken || 0) + 1; // 使进行中的构建失效
  const tl = wrap._textLayer;
  if (tl) { try { tl.cancel(); } catch { /* ignore */ } wrap._textLayer = null; }
  const layer = wrap.querySelector('.text-layer');
  if (layer) layer.textContent = '';
  delete wrap.dataset.textLayer;
  delete wrap.dataset.textBuilding;
}

// 渲染队列：滚动/缩放时页面进入视野，不立即并发渲染（多页同时画 canvas 会让滚动掉帧），
// 而是入队、每帧只渲染最靠近视口中心的一页，串行推进、逐帧让位。
const renderPending = new Set();
let renderRunning = false;

function enqueuePageRender(i) {
  if (!state.pageEls || !state.pageEls[i]) return;
  renderPending.add(i);
  pumpPageRenders();
}

function pumpPageRenders() {
  if (renderRunning) return;
  renderRunning = true;
  (async () => {
    try {
      while (renderPending.size && !state.zooming) {
        const i = pickRenderTarget();
        if (i == null) { renderPending.clear(); break; }
        renderPending.delete(i);
        // renderPageAt 内部（PDF.js 分块渲染）已逐帧让出主线程，这里不再额外让帧，
        // 提高串行吞吐，避免滚动快时空白页追赶。
        await renderPageAt(i);
      }
      if (state.zooming) renderPending.clear(); // 缩放期间跳过，提交后 observer 会重新入队
    } finally {
      renderRunning = false;
    }
  })();
}

// 从待渲染集合里选最该先画的一页：已与视口相交的优先，其次离视口中心最近。
function pickRenderTarget() {
  const vrect = $('viewer').getBoundingClientRect();
  const cx = vrect.left + vrect.width / 2;
  const cy = vrect.top + vrect.height / 2;
  let best = null, bestScore = Infinity;
  for (const i of renderPending) {
    const el = state.pageEls && state.pageEls[i];
    if (!el || el.dataset.rendered === '1' || el.dataset.rendering === '1') continue;
    const r = el.getBoundingClientRect();
    const visible = state.rtl
      ? (r.right >= vrect.left && r.left <= vrect.right)
      : (r.bottom >= vrect.top && r.top <= vrect.bottom);
    const dx = r.left + r.width / 2 - cx;
    const dy = r.top + r.height / 2 - cy;
    const score = (visible ? 0 : 1e6) + dx * dx + dy * dy;
    if (score < bestScore) { bestScore = score; best = i; }
  }
  return best;
}

// 页面进入「视野 ± 一个半视口」时入队渲染，离开时释放。
const pageObserver = new IntersectionObserver(
  (entries) => {
    // 缩放手势期间完全跳过懒渲染/清理：transform 预览每帧都在改变页面的视觉位置，观察器会
    // 随之每帧触发、无谓地入队又清空（拖慢预览动画）；提交后由 commitZoom 统一重建可见页，
    // 观察器届时（state.zooming 已复位）再恢复工作。
    if (state.zooming) return;
    for (const e of entries) {
      const i = Number(e.target.dataset.index);
      if (e.isIntersecting) enqueuePageRender(i);
      else clearPage(i);
    }
  },
  { root: $('viewer'), rootMargin: '150% 150%' },
);

// 竖排古籍（RTL）模式的当前页：页面从右往左排列，读数位置取视口右缘。
function rtlCurrentPage() {
  const viewer = $('viewer');
  const n = state.pdfDoc.numPages;
  // 已滚到最左端（书末）：当前页为最后一页。
  if (viewer.scrollLeft <= 1) return n;
  const rightEdge = viewer.scrollLeft + viewer.clientWidth - 16;
  const pages = state.pageEls;
  // offsetLeft 随页码递增而递减（第 1 页最右、第 n 页最左），
  // 二分找「左缘 <= 右缘」的最小页码，避免每帧 O(n) 扫描大书导致滚动卡顿。
  let lo = 0, hi = n;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (pages[mid].offsetLeft <= rightEdge) hi = mid;
    else lo = mid + 1;
  }
  return Math.min(lo + 1, n);
}

// 视口中间的页（提取文字用）：竖排取水平中间、横排取垂直中间，而非阅读起始位置。
function viewportCenterPage() {
  if (!state.pdfDoc) return 1;
  const viewer = $('viewer');
  const n = state.pdfDoc.numPages;
  const pages = state.pageEls;
  if (!pages || !pages.length) return state.pageNum || 1;

  if (state.rtl) {
    const centerX = viewer.scrollLeft + viewer.clientWidth / 2;
    let lo = 0, hi = n;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (pages[mid].offsetLeft <= centerX) hi = mid;
      else lo = mid + 1;
    }
    return Math.min(lo + 1, n);
  }
  const centerY = viewer.scrollTop + viewer.clientHeight / 2;
  let lo = 0, hi = n;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (pages[mid].offsetTop + pages[mid].offsetHeight >= centerY) hi = mid;
    else lo = mid + 1;
  }
  return Math.min(lo + 1, n);
}

// 根据滚动位置更新「当前页」指示。
function updatePageIndicator(forcePage) {
  if (!state.pdfDoc) return;
  let current;
  if (forcePage) {
    current = forcePage;
  } else if (state.rtl) {
    current = rtlCurrentPage();
  } else {
    const viewer = $('viewer');
    const top = viewer.scrollTop + 16; // 视口顶部对应内容位置（含 padding）
    const pages = state.pageEls;
    const n = pages.length;
    // offsetTop 随页码递增，二分找「底部 >= 视口顶」的最小页码，避免 O(n) 扫描。
    let lo = 0, hi = n;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (pages[mid].offsetTop + pages[mid].offsetHeight >= top) hi = mid;
      else lo = mid + 1;
    }
    current = Math.min(lo + 1, n);
  }
  state.pageNum = current;
  $('page-input').value = current;
  $('page-total').textContent = '/ ' + state.pdfDoc.numPages;
  highlightToc(current);
}

// 高亮目录中当前页对应的条目（最后一个页码 <= 当前页的项）。
function highlightToc(current) {
  const items = state.tocItems;
  if (!items || !items.length) return;
  let active = -1;
  for (let i = 0; i < items.length; i++) {
    const p = items[i].pageIdx;
    if (p == null) continue;
    if (p + 1 <= current) active = i;
    else break;
  }
  const rows = $('toc-list').querySelectorAll('.toc-item');
  rows.forEach((r, i) => r.classList.toggle('active', i === active));
}

// 页面跳转滑动：固定时长（约 260ms）缓动到目标位置——无论跨多少页都只滑一小段，
// 既有滑动的手感，又不会像原生 smooth 滚动那样按距离耗时、逐页长滑。连点目录时取消上一段。
let pageJumpAnim = null;
function cancelPageJump() {
  if (pageJumpAnim) { cancelAnimationFrame(pageJumpAnim); pageJumpAnim = null; }
}

// 先瞬时 scrollIntoView 读出精确目标位置（含 scroll-padding），再立刻还原并缓动过去。
// 这样既保留 scrollIntoView 的对齐语义，又把「跳变」变成一小段滑动。
function slideToEl(el, opts) {
  cancelPageJump();
  cancelRtlScroll();
  const viewer = $('viewer');
  const sl0 = viewer.scrollLeft, st0 = viewer.scrollTop;
  const prevBehavior = viewer.style.scrollBehavior;
  viewer.style.scrollBehavior = 'auto';
  el.scrollIntoView(opts); // 瞬时定位，读出精确目标
  const sl1 = viewer.scrollLeft, st1 = viewer.scrollTop;
  viewer.scrollLeft = sl0;
  viewer.scrollTop = st0;
  viewer.style.scrollBehavior = prevBehavior;
  const dl = sl1 - sl0, dt = st1 - st0;
  if (!dl && !dt) return; // 已在目标位置
  const dur = 260, t0 = performance.now();
  const step = (now) => {
    const t = Math.min(1, (now - t0) / dur);
    const e = t < 0.5 ? 2 * t * t : -1 + (4 - 2 * t) * t; // easeInOutQuad
    viewer.scrollLeft = sl0 + dl * e;
    viewer.scrollTop = st0 + dt * e;
    if (t < 1) pageJumpAnim = requestAnimationFrame(step);
    else pageJumpAnim = null;
  };
  pageJumpAnim = requestAnimationFrame(step);
}

async function goTo(n) {
  if (!state.pdfDoc) return;
  const clamped = Math.min(Math.max(1, n), state.pdfDoc.numPages);
  const wrap = state.pageEls[clamped - 1];
  if (!wrap) return;
  cancelRtlScroll(); // 停止滚轮缓动，交给跳转接管
  // 先渲染目标页再跳转，避免滑过去时仍是空白。
  await renderPageAt(clamped - 1);
  if (state.rtl) {
    // 竖排古籍：目标页对齐到视口右侧（从右往左读）。
    slideToEl(wrap, { inline: 'end', block: 'nearest', behavior: 'auto' });
  } else {
    slideToEl(wrap, { block: 'start', behavior: 'auto' });
  }
  updatePageIndicator(clamped); // 直接反映目标页，避免读取尚未移动的 scrollTop
}

// 缩放（跟随鼠标、内容固定）：手势期间用 transform 实时预览（GPU 合成、高刷屏丝滑、锚点内容
// 不动），短 debounce 后一次性「重排 + 原子贴回高清位图 + 还原滚动」，把鼠标下的内容点还原到
// 鼠标处。提交时先离屏预渲染可见页，再在同一帧内同步清掉 transform、推进布局、贴回位图、算准
// 滚动——预览与最终态同一套坐标，中间无交换跳变，也不闪白。
let zoomEpoch = 0;          // 缩放手势代次：丢弃过期的异步渲染，避免旧倍率位图覆盖新布局
let zoomBase = 1;           // 手势开始时的布局倍率（无页面命中时按容器坐标换算锚点）
let zoomTarget = 1;         // 目标倍率（相对已提交 scale 的乘数）
let zoomDisplay = 1;        // 当前预览显示倍率（向 zoomTarget 平滑逼近）
let zoomAnimId = null;      // 预览动画的 rAF 句柄
let zoomPrevT = 0;          // 上一动画帧时间戳（帧率无关指数平滑）
let zoomCommitTimer = null; // 手势结束后的提交 debounce（收敛触发为主，此为兜底）
let zoomCommitting = false;   // commitZoom 是否在进行中（离屏预渲染期间）
let zoomCommitPending = false; // 提交进行中又收到新提交请求，结束后补跑一次

// 把离屏位图按倍率贴回第 i 页画布（同步、原子，无中间空白帧）。
function blitPageBitmap(i, scale, bmp) {
  const wrap = state.pageEls[i];
  const canvas = wrap.querySelector('canvas');
  const vp1 = state.vp1Cache[i];
  const d = renderScale(vp1.width * scale, vp1.height * scale);
  canvas.width = Math.floor(vp1.width * scale * d);
  canvas.height = Math.floor(vp1.height * scale * d);
  // CSS 尺寸取整，与 renderPageCanvas 里 Math.floor(viewport.width) 保持一致：
  // 否则提交后懒渲染会再把宽度抹平成整数，整列页面往下移个零点几像素（终点轻微跳一下）。
  canvas.style.width = Math.floor(vp1.width * scale) + 'px';
  canvas.style.height = Math.floor(vp1.height * scale) + 'px';
  canvas.getContext('2d').drawImage(bmp, 0, 0, canvas.width, canvas.height);
  wrap.dataset.rendered = '1';
  wrap.dataset.dpr = String(d);
}

// 把所有页的布局尺寸推进到倍率 scale（不重建 DOM，只改画布 CSS 尺寸 + 清文本层）。
// 旧位图先保留作占位（CSS 拉伸，不闪白），标记待重渲染，稍后由 commitZoom 贴回预渲染
// 位图或收尾的 enqueuePageRender（结束后）换成新倍率高清位图；无位图的页保持 1×1 空白。
function rescalePages(scale) {
  const container = $('pdf-pages');
  container.style.setProperty('--scale-factor', String(scale));
  container.style.setProperty('--page-gap', (18 * scale) + 'px');
  renderPending.clear();
  const vps = state.vp1Cache;
  for (let i = 0; i < state.pageEls.length; i++) {
    const wrap = state.pageEls[i];
    const w = vps[i].width * scale;
    const h = vps[i].height * scale;
    const canvas = wrap.querySelector('canvas');
    const hadBitmap = wrap.dataset.rendered === '1';
    if (wrap.dataset.textLayer === '1' || wrap.dataset.textBuilding === '1') clearTextLayerOnly(i);
    if (!hadBitmap) {
      canvas.width = 1;
      canvas.height = 1;
    }
    delete wrap.dataset.rendered;
    canvas.style.width = Math.floor(w) + 'px';
    canvas.style.height = Math.floor(h) + 'px';
  }
}

// 仅清空第 i 页的文本层（缩放期间保留画布位图用于拉伸占位）。
function clearTextLayerOnly(i) {
  const wrap = state.pageEls && state.pageEls[i];
  if (!wrap) return;
  wrap._tlToken = (wrap._tlToken || 0) + 1;
  const tl = wrap._textLayer;
  if (tl) { try { tl.cancel(); } catch { /* ignore */ } wrap._textLayer = null; }
  const layer = wrap.querySelector('.text-layer');
  if (layer) layer.textContent = '';
  delete wrap.dataset.textLayer;
  delete wrap.dataset.textBuilding;
}

// 复位缩放相关状态（打开新文档 / 适应宽度 / 手势收尾时调用）。
function resetZoomState() {
  clearTimeout(zoomCommitTimer);
  zoomCommitTimer = null;
  zoomEpoch++;
  zoomBase = 1;
  zoomTarget = 1;
  zoomDisplay = 1;
  if (zoomAnimId) { cancelAnimationFrame(zoomAnimId); zoomAnimId = null; }
  zoomPrevT = 0;
  cancelRtlScroll();
  cancelPageJump(); // 缩放/适应/开新书时接管滚动，停掉目录跳转滑动
  state.zooming = false;
  state.zoomAnchor = null;
  state.zoomAnchorClient = null;
  const container = $('pdf-pages');
  if (container) { container.style.transform = ''; container.style.transformOrigin = ''; container.classList.remove('zooming'); }
}

// 临时关闭 smooth 滚动，精确设置滚动位置（提交缩放时用，避免平滑滚动干扰定位）。
function setScrollInstant(el, left, top) {
  const prev = el.style.scrollBehavior;
  el.style.scrollBehavior = 'auto';
  el.scrollLeft = left;
  el.scrollTop = top;
  el.style.scrollBehavior = prev;
}

// 缩放后把鼠标下的 PDF 点放回鼠标位置。用「页 + 页内分数」精确定位：页面居中布局下，
// 容器本地坐标混入了不随缩放线性变化的居中边距，页内分数随页面等比缩放，定位才精确。
// 以「画布铺满窗口」为分界、按轴各自处理：比窗口大的轴正常锚定（到边缘自动夹紧），
// 即「严格保持鼠标指向的 PDF 点不动」；比窗口小的轴浏览器会把 scroll 夹到 0、页面由
// CSS 居中，即「自适应居中」。分界正是「内容铺满窗口」，无需额外判断。
function repositionAfterScale() {
  const viewer = $('viewer');
  const a = state.zoomAnchor;
  const m = state.zoomAnchorClient;
  if (!a || !m) return;
  void viewer.offsetHeight; // 强制布局 flush，确保 resize 后的尺寸/滚动范围已结算
  const vrect = viewer.getBoundingClientRect();
  const maxL = Math.max(0, viewer.scrollWidth - viewer.clientWidth);
  const maxT = Math.max(0, viewer.scrollHeight - viewer.clientHeight);
  let sl, st;
  const pageEl = a.page != null && state.pageEls ? state.pageEls[a.page] : null;
  if (pageEl) {
    const prect = pageEl.getBoundingClientRect();
    sl = viewer.scrollLeft + ((prect.left - vrect.left) + a.fx * prect.width) - m.x;
    st = viewer.scrollTop + ((prect.top - vrect.top) + a.fy * prect.height) - m.y;
  } else {
    // 未命中页面（点在页间空白）时的退化：容器坐标 ×(目标倍率 / 手势起始倍率)。
    const crect = $('pdf-pages').getBoundingClientRect();
    sl = viewer.scrollLeft + (crect.left - vrect.left + (a.x / zoomBase) * currentScale()) - m.x;
    st = viewer.scrollTop + (crect.top - vrect.top + (a.y / zoomBase) * currentScale()) - m.y;
  }
  // 数值防御：异常 rect/除零产生 NaN/Infinity，或越界时夹紧，避免把滚动打到文档尽头。
  if (!Number.isFinite(sl)) sl = viewer.scrollLeft;
  if (!Number.isFinite(st)) st = viewer.scrollTop;
  sl = Math.max(0, Math.min(maxL, sl));
  st = Math.max(0, Math.min(maxT, st));
  logDiag(`[zoom-pos] page=${a.page != null ? a.page : '-'} fx=${a.fx == null ? '-' : a.fx.toFixed(3)} fy=${a.fy == null ? '-' : a.fy.toFixed(3)} scale=${currentScale().toFixed(3)} sl=${sl.toFixed(0)} st=${st.toFixed(0)} dL=${(sl - viewer.scrollLeft).toFixed(0)} dT=${(st - viewer.scrollTop).toFixed(0)} maxL=${maxL} maxT=${maxT}`);
  setScrollInstant(viewer, sl, st);
}

function zoom(factor, clientX, clientY) {
  if (!state.pdfDoc) return;
  const viewer = $('viewer');
  const cur = currentScale();
  const target = Math.min(5, Math.max(0.3, cur * zoomTarget * factor));
  const newTarget = target / cur;
  if (Math.abs(newTarget - zoomTarget) < 0.001) return;

  zoomEpoch++;
  // 停止进行中的滚轮/目录跳转滑动，避免其继续改写 scroll 与缩放锚定打架、页面乱滑。
  cancelRtlScroll();
  cancelPageJump();

  // 手势第一步：记录鼠标下的 PDF 锚点（所在页 + 页内分数 0..1）与鼠标视口位置，
  // 后续每步都按该点还原。页面居中布局下容器本地坐标混入了不随缩放线性变化的居中边距，
  // 页内分数随页面等比缩放，定位精确；点在页间空白时退化为容器坐标法。
  if (!state.zoomAnchor) {
    state.zooming = true;
    zoomBase = cur;
    $('pdf-pages').classList.add('zooming'); // 提升为独立合成层，让 transform 预览走 GPU
    const vrect = viewer.getBoundingClientRect();
    const mx = clientX != null ? clientX - vrect.left : viewer.clientWidth / 2;
    const my = clientY != null ? clientY - vrect.top : viewer.clientHeight / 2;
    const crect = $('pdf-pages').getBoundingClientRect();
    const anchor = {
      x: mx - (crect.left - vrect.left), // 容器本地坐标（无页面命中时的退化锚点）
      y: my - (crect.top - vrect.top),
    };
    // 记录视口/容器/滚动/鼠标，供预览期间解析地算出「提交后锚点会落在哪」（含滚动夹紧），
    // 从而反推 transform-origin，使预览终点与提交布局严格一致（缩小露边/居中时不再跳一下）。
    anchor.mx = mx;
    anchor.my = my;
    anchor.vleft = vrect.left;
    anchor.vtop = vrect.top;
    anchor.sl = viewer.scrollLeft;
    anchor.st = viewer.scrollTop;
    anchor.cw = crect.width;
    anchor.ch = crect.height;
    anchor.contentW = viewer.clientWidth - 40;  // 视口内容区宽（去左右 padding）
    anchor.contentH = viewer.clientHeight - 40; // 视口内容区高（去上下 padding）
    const el = document.elementFromPoint(vrect.left + mx, vrect.top + my);
    const pageEl = el && el.closest ? el.closest('.pdf-page') : null;
    if (pageEl) {
      const prect = pageEl.getBoundingClientRect();
      if (prect.width > 0 && prect.height > 0) {
        anchor.page = Number(pageEl.dataset.index);
        anchor.fx = (mx - (prect.left - vrect.left)) / prect.width;
        anchor.fy = (my - (prect.top - vrect.top)) / prect.height;
        anchor.pageLeft = prect.left - crect.left; // 页在容器中的本地位置（cur 倍率）
        anchor.pageTop = prect.top - crect.top;
      }
    }
    state.zoomAnchor = anchor;
    state.zoomAnchorClient = { x: mx, y: my };
  }

  zoomTarget = newTarget;
  $('zoom-info').textContent = Math.round(target * 100) + '%';

  // 手势期间 transform 实时预览：按轴选择 origin（锚定鼠标 or 贴边/居中），
  // 使预览终点与提交后的布局严格一致，缩小到「画布露边/居中」时不再跳一下。
  setZoomPreviewOrigin();
  startZoomPreview();

  // 收尾提交由预览动画收敛时触发（见 stepZoomPreview）；此处仅留一个长兜底计时器，
  // 防止极端情况下收敛未触发导致永不提交。1500ms 远大于最坏收敛时长（约 0.6s），正常不会走到。
  clearTimeout(zoomCommitTimer);
  zoomCommitTimer = setTimeout(() => { commitZoom(); }, 1500);
}

// ===== 竖排（RTL）滚轮平滑滚动 =====
// 纵向滚轮映射成横向 scrollLeft：用 rAF 缓动平滑逼近目标位置，代替瞬时跳变，
// 鼠标滚轮（离散格）滚动时更有惯性、更丝滑；触控板小增量下几乎无感。
let rtlScrollTarget = null;  // null = 未在动画中
let rtlScrollAnim = null;
let rtlScrollPrevT = 0;

function nudgeRtlScroll(delta) {
  cancelPageJump(); // 滚轮手动滚动接管，停掉目录跳转滑动
  const viewer = $('viewer');
  const max = Math.max(0, viewer.scrollWidth - viewer.clientWidth);
  if (rtlScrollTarget === null) rtlScrollTarget = viewer.scrollLeft;
  rtlScrollTarget = Math.max(0, Math.min(max, rtlScrollTarget - delta));
  if (rtlScrollAnim === null) {
    rtlScrollPrevT = 0;
    rtlScrollAnim = requestAnimationFrame(stepRtlScroll);
  }
}

function stepRtlScroll(now) {
  rtlScrollAnim = null;
  const viewer = $('viewer');
  const dt = rtlScrollPrevT ? Math.min(now - rtlScrollPrevT, 64) : 16;
  rtlScrollPrevT = now;
  const k = 1 - Math.exp(-dt / 100); // 时间常数 100ms，帧率无关
  const next = viewer.scrollLeft + (rtlScrollTarget - viewer.scrollLeft) * k;
  if (Math.abs(rtlScrollTarget - next) < 0.5) {
    viewer.scrollLeft = rtlScrollTarget;
    rtlScrollTarget = null;
    return;
  }
  viewer.scrollLeft = next;
  rtlScrollAnim = requestAnimationFrame(stepRtlScroll);
}

// 取消平滑滚动（翻页 / 适应 / 缩放提交 / 打开新书时避免动画继续覆盖滚动位置）。
function cancelRtlScroll() {
  if (rtlScrollAnim) { cancelAnimationFrame(rtlScrollAnim); rtlScrollAnim = null; }
  rtlScrollTarget = null;
  rtlScrollPrevT = 0;
}

// 把第 pageNo（1-based）页以 scale 渲染到离屏 canvas，返回该 canvas（供提交缩放前预渲染）。
async function renderToOffscreen(pageNo, scale) {
  const page = await state.pdfDoc.getPage(pageNo);
  const viewport = page.getViewport({ scale });
  const d = renderScale(viewport.width, viewport.height);
  const off = document.createElement('canvas');
  off.width = Math.floor(viewport.width * d);
  off.height = Math.floor(viewport.height * d);
  const ctx = off.getContext('2d');
  ctx.setTransform(d, 0, 0, d, 0, 0);
  await page.render({ canvasContext: ctx, viewport }).promise;
  return off;
}

// 当前在视野内的页面索引（0-based）。
function visiblePageIndices() {
  const vrect = $('viewer').getBoundingClientRect();
  const out = [];
  for (let i = 0; i < state.pageEls.length; i++) {
    const r = state.pageEls[i].getBoundingClientRect();
    const visible = state.rtl
      ? (r.right >= vrect.left && r.left <= vrect.right)
      : (r.bottom >= vrect.top && r.top <= vrect.bottom);
    if (visible) out.push(i);
  }
  return out;
}

// 预计算 scale=1 下全部页面的内容度量（最大页宽/高、整列总高、整行总宽），供缩放预览期间
// 解析地推算滚动夹紧边界；缓存键为 vp1Cache 引用，文档重建后自动失效。
function contentMetricsAtScale1() {
  if (state._metricsFor !== state.vp1Cache) {
    const vps = state.vp1Cache || [];
    let maxW = 0, maxH = 0, totalH = 0, rowW = 0;
    for (let i = 0; i < vps.length; i++) {
      const v = vps[i];
      maxW = Math.max(maxW, v.width);
      maxH = Math.max(maxH, v.height);
      totalH += v.height;
      rowW += v.width;
    }
    // 横排（LTR）每页 margin-bottom 一个 gap（含最后一页），容器另加固定 padding-bottom 16px；
    // 竖排（RTL）用 flex gap，只在页与页之间（N-1 个）、无 padding-bottom。16px 固定值在调用处另加。
    const n = Math.max(0, vps.length);
    state._metrics = {
      maxW, maxH,
      totalH: totalH + n * 18,                  // LTR 整列总高（含所有 margin-bottom）
      rowW: rowW + Math.max(0, n - 1) * 18,     // RTL 整行总宽（flex gap）
    };
    state._metricsFor = state.vp1Cache;
  }
  return state._metrics || { maxW: 0, maxH: 0, totalH: 0, rowW: 0 };
}

// 计算并设置 transform-origin。解析地算出「提交后锚点会落在哪」：按轴对滚动做 [0,max] 夹紧
// （内容小于视口时自动贴边/居中），再反推 origin，使 transform 预览每一帧都与最终布局一致。
// 这样缩小到「画布露边/居中」时，滚动夹紧在预览中就已平滑发生，提交瞬间不再跳一下。
function setZoomPreviewOrigin() {
  const a = state.zoomAnchor;
  if (!a) return;
  const k = zoomDisplay;
  const denom = 1 - k;
  if (denom < 1e-4) {
    // k≈1（尚未缩放）：transform 近乎恒等，origin 取值无所谓，直接锚定鼠标。
    $('pdf-pages').style.transformOrigin = `${a.x}px ${a.y}px`;
    return;
  }
  const s = zoomBase * k;          // 当前视觉倍率
  const m = contentMetricsAtScale1();
  const pad = 20;                  // 视口四周 padding
  const clampScroll = (v, hi) => (v < 0 ? 0 : v > hi ? hi : v);

  const vp = a.page != null ? state.vp1Cache[a.page] : null;
  if (!vp) {
    // 无页面命中（点在页间空白/边距）：退化为容器坐标按倍率线性缩放，同样解析地复现
    // 提交后的滚动夹紧，缩小露边/居中时同样不跳。此时锚点没有「页内分数」，只按 a.x/a.y 线性缩。
    let maxL, maxT;
    if (state.rtl) {
      maxL = Math.max(0, m.rowW * s - a.contentW);
      maxT = Math.max(0, m.maxH * s - a.contentH);
    } else {
      maxL = Math.max(0, m.maxW * s - a.contentW);
      maxT = Math.max(0, m.totalH * s + 16 - a.contentH); // +16 = 容器固定 padding-bottom
    }
    const usl = pad + a.x * k - a.mx;
    const ust = pad + a.y * k - a.my;
    const csl = clampScroll(usl, maxL);
    const cst = clampScroll(ust, maxT);
    const ox = (a.sl - csl) / denom;
    const oy = (a.st - cst) / denom;
    $('pdf-pages').style.transformOrigin = `${ox}px ${oy}px`;
    return;
  }

  const pw = vp.width * s;
  const ph = vp.height * s;
  let pageLeft_s, pageTop_s, usl, maxL, ust, maxT;
  if (state.rtl) {
    // 竖排：横向整行滚动，纵向页面 margin:auto 0 居中。
    pageLeft_s = a.pageLeft * k;
    usl = pad + pageLeft_s + a.fx * pw - a.mx;
    maxL = Math.max(0, m.rowW * s - a.contentW);
    // 竖排页面垂直居中相对的是「视口内容高度」而非手势起始时的容器高度（缩到铺满时容器会变矮），
    // 用 a.contentH 才能让预览的居中位置与提交后的 margin:auto 0 居中一致，避免上下跳一下。
    pageTop_s = Math.max(0, (a.contentH - ph) / 2);
    ust = pad + pageTop_s + a.fy * ph - a.my;
    maxT = Math.max(0, m.maxH * s - a.contentH);
  } else {
    // 横排：页面水平 margin:0 auto 居中，纵向整列滚动。
    pageLeft_s = Math.max(0, (a.cw - pw) / 2);
    usl = pad + pageLeft_s + a.fx * pw - a.mx;
    maxL = Math.max(0, m.maxW * s - a.cw);
    pageTop_s = a.pageTop * k;
    ust = pad + pageTop_s + a.fy * ph - a.my;
    maxT = Math.max(0, m.totalH * s + 16 - a.contentH); // +16 = 容器固定 padding-bottom
  }

  const csl = clampScroll(usl, maxL);
  const cst = clampScroll(ust, maxT);
  // 反推 origin：使 transform 把锚点（容器本地 a.x/a.y）恰好映射到「夹紧后」的最终视口位置。
  const ox = (a.sl - csl + pageLeft_s + a.fx * pw - k * a.x) / denom;
  const oy = (a.st - cst + pageTop_s + a.fy * ph - k * a.y) / denom;
  $('pdf-pages').style.transformOrigin = `${ox}px ${oy}px`;
}

// 启动/继续缩放预览动画：每帧把 zoomDisplay 向 zoomTarget 平滑逼近，直到收敛。
// 帧率无关的指数平滑（时间常数 80ms），60Hz 与 144Hz 手感一致。
function startZoomPreview() {
  if (zoomAnimId) return; // 动画已在跑，会继续逼近新目标
  zoomPrevT = 0;
  zoomAnimId = requestAnimationFrame(stepZoomPreview);
}

function stepZoomPreview(now) {
  zoomAnimId = null;
  const dt = zoomPrevT ? Math.min(now - zoomPrevT, 64) : 16; // 限制 dt，后台切回不跳变
  zoomPrevT = now;
  const k = 1 - Math.exp(-dt / 80);
  zoomDisplay += (zoomTarget - zoomDisplay) * k;
  // origin 随当前预览倍率按轴切换：内容一旦缩到「小于视口」就贴边/居中，
  // 切换点正好在「铺满视口」处（唯一布局位置），因此切换无跳变。
  setZoomPreviewOrigin();
  if (Math.abs(zoomTarget - zoomDisplay) < 0.0015) {
    // 预览已精确收敛到目标倍率：定格到精确值并立即收尾提交。这样预览终点与提交布局严格一致，
    // 无「终点差一点」的跳变；且提交的离屏预渲染发生在动画停止之后，不再与 rAF 抢主线程掉帧。
    zoomDisplay = zoomTarget;
    $('pdf-pages').style.transform = `scale(${zoomDisplay})`;
    clearTimeout(zoomCommitTimer);
    zoomCommitTimer = null;
    commitZoom();
    return;
  }
  $('pdf-pages').style.transform = `scale(${zoomDisplay})`;
  zoomAnimId = requestAnimationFrame(stepZoomPreview);
}

// 缩放手势结束：按目标倍率重排 + 原子贴回高清位图 + 还原锚点，恢复清晰度与正确滚动范围。
// 提交前先把可见页离屏预渲染；预渲染完成后，在同一同步块内清掉 transform 预览、推进布局、
// 贴回位图、算准滚动——中间不跨帧，浏览器只画一次，预览到最终态无交换跳变、不闪白。
async function commitZoom() {
  if (!state.pdfDoc || !state.zooming) return; // 无进行中的手势（如兜底计时器迟到）则忽略
  // 上一次提交（离屏预渲染）尚未结束：标记待补跑，结束后由 finally 接管，避免并发预渲染争抢主线程。
  if (zoomCommitting) { zoomCommitPending = true; return; }
  zoomCommitting = true;
  try {
    const epoch = zoomEpoch;
    const container = $('pdf-pages');
    const target = Math.min(5, Math.max(0.3, currentScale() * zoomTarget));

    // 预渲染可见页（含上下相邻页，防边界漏白）到离屏位图。
    const indices = visiblePageIndices();
    const set = new Set(indices);
    for (const i of indices) { set.add(i - 1); set.add(i + 1); }
    const pre = new Map();
    await Promise.all(
      [...set].filter((i) => i >= 0 && i < state.pdfDoc.numPages).map(async (i) => {
        try { pre.set(i, await renderToOffscreen(i + 1, target)); }
        catch { /* 预渲染失败则退化为懒加载 */ }
      }),
    );

    // 期间若又有新的缩放手势 / 复位，放弃本次过时的提交，由新提交接管。
    if (epoch !== zoomEpoch) return;

    // 以下为同一同步块：清预览 transform → 推进布局 → 贴回高清 → 还原滚动，原子完成。
    state.fitScale = null;
    state.scale = target;
    container.style.transform = '';
    container.style.transformOrigin = '';
    container.classList.remove('zooming');
    rescalePages(target);
    for (const [i, bmp] of pre) {
      if (state.pageEls[i]) blitPageBitmap(i, target, bmp);
    }
    repositionAfterScale();

    // 收尾：复位手势态，重建可见页文本层，补齐未预渲染页的 canvas。
    state.zooming = false;
    state.zoomAnchor = null;
    state.zoomAnchorClient = null;
    if (zoomAnimId) { cancelAnimationFrame(zoomAnimId); zoomAnimId = null; }
    zoomTarget = 1;
    zoomDisplay = 1;
    zoomPrevT = 0;
    for (const i of visiblePageIndices()) {
      if (state.pageEls[i]) enqueuePageRender(i);
    }
    updatePageIndicator();
  } finally {
    zoomCommitting = false;
    if (zoomCommitPending) {
      zoomCommitPending = false;
      commitZoom();
    }
  }
}

async function fitWidth() {
  if (!state.pdfDoc) return;
  resetZoomState();
  const viewer = $('viewer');
  const page = await state.pdfDoc.getPage(1);
  const vp1 = page.getViewport({ scale: 1 });
  // 古籍（RTL）按高度适应，其余按宽度适应。
  const fs = state.rtl
    ? fitScaleForHeight(viewer.clientHeight, vp1.height)
    : fitScaleFor(viewer.clientWidth, vp1.width);
  logDiag(`[fitWidth] clientWidth=${viewer.clientWidth} clientHeight=${viewer.clientHeight} vp1=${vp1.width.toFixed(1)} rtl=${state.rtl} fitScale=${fs} scale=${state.scale}`);
  if (fs == null) return; // viewer 尚未布局完成，跳过本次重排，避免页面缩成一条
  state.fitScale = fs;
  await rebuildPages();
  const wrap = state.pageEls[state.pageNum - 1];
  if (wrap) {
    if (state.rtl) wrap.scrollIntoView({ inline: 'end', block: 'nearest', behavior: 'auto' });
    else wrap.scrollIntoView({ block: 'start', behavior: 'auto' });
  }
}

// ---------- 结构化提取（布局分析 + 双栏阅读顺序 + 标题层级） ----------

function median(arr) {
  if (!arr.length) return 0;
  const s = arr.slice().sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

// 数学字体：LaTeX 数学字体（Computer Modern Math 系列、AMS、MathType 等）。
// 用于识别公式区域（正文一般用普通字体，公式用数学字体）。
function isMathFont(name) {
  const n = (name || '').toLowerCase();
  return /(cmmi|cmsy|cmex|cmr|msam|msbm|math|symbol|stix|mtmi|mtsy|mtex)/.test(n);
}

// 从 PDF 文本层提取原始文本项（未聚行），字段 { str, x0, x1, y, size, bold, italic, math }。
function extractTextItems(items) {
  const list = [];
  for (const it of items) {
    if (!('str' in it) || !it.str.trim()) continue;
    const t = it.transform;
    const x = t[4];
    list.push({
      str: it.str,
      x0: x,
      x1: x + (it.width || 0),
      y: t[5],
      size: it.height || Math.max(Math.abs(t[0]), Math.abs(t[3])),
      bold: /bold/i.test(it.fontName || ''),
      italic: /italic|oblique/i.test(it.fontName || ''),
      math: isMathFont(it.fontName),
    });
  }
  return list;
}

// 检测双栏的垂直栏缝：把文本项「左缘 + 右缘」投影到 x 轴，在页面中央区域找一段
// 既无词开始、也无词结束的低值「谷」。通栏标题/摘要的单词横跨栏缝（左缘在左、右缘在右），
// 不会在栏缝处留下边缘，因此该方法天然不受通栏内容干扰。
function detectColumnSplit(items) {
  const n = items.length;
  if (n < 12) return null;
  const minX = Math.min(...items.map((i) => i.x0));
  const maxX = Math.max(...items.map((i) => i.x1));
  const width = maxX - minX;
  if (width <= 0) return null;

  const B = 120;
  const edge = new Array(B).fill(0); // 每个 bin：以该位置为左缘或右缘的文本项数
  for (const it of items) {
    const a = Math.max(0, Math.min(B - 1, Math.floor(((it.x0 - minX) / width) * (B - 1))));
    const b = Math.max(0, Math.min(B - 1, Math.floor(((it.x1 - minX) / width) * (B - 1))));
    edge[a]++;
    edge[b]++;
  }

  const lo = Math.floor(B * 0.15);
  const hi = Math.floor(B * 0.85);
  const thresh = Math.max(2, n * 0.01);
  // 收集中央区域的低值连续段作为候选栏缝。
  const gaps = [];
  let runStart = -1;
  for (let k = lo; k <= hi; k++) {
    if (edge[k] <= thresh) {
      if (runStart < 0) runStart = k;
    } else if (runStart >= 0) {
      if (k - runStart >= 2) gaps.push([runStart, k - runStart]);
      runStart = -1;
    }
  }
  if (runStart >= 0 && hi + 1 - runStart >= 2) gaps.push([runStart, hi + 1 - runStart]);
  if (!gaps.length) return null;

  // 双栏等宽，栏缝应在页面中央附近：取离中央最近、且两侧都有足够内容的缝。
  const centerPx = minX + width / 2;
  let best = null;
  for (const [gs, len] of gaps) {
    const c = minX + ((gs + len / 2) / B) * width;
    let left = 0;
    let right = 0;
    for (let k = 0; k < gs; k++) left += edge[k];
    for (let k = gs + len; k < B; k++) right += edge[k];
    if (left < n * 0.3 || right < n * 0.3) continue; // 两侧都要有内容，避免单栏/半边页误判
    const d = Math.abs(c - centerPx);
    if (!best || d < best.d) best = { center: c, d };
  }
  return best ? { center: best.center } : null;
}

// 把一栏内（或整页）的文本项按 y 聚成行，返回 { y, text, size, bold, italic, x0, x1 }。
// 传入 center 时会在栏缝处把「左右两栏同 y」的行拆成两行。
function buildLines(items, center) {
  const list = items.slice();
  if (!list.length) return [];
  // PDF 坐标系 y 向上，页面顶部 y 更大 → 降序即自上而下；同 y 内按 x 升序。
  list.sort((a, b) => b.y - a.y || a.x0 - b.x0);
  const yTol = median(list.map((i) => i.size)) * 0.5;
  const rows = [];
  for (const it of list) {
    const last = rows[rows.length - 1];
    if (last && Math.abs(last.y - it.y) <= yTol) {
      last.items.push(it);
      last.y = (last.y * (last.items.length - 1) + it.y) / last.items.length;
    } else {
      rows.push({ y: it.y, items: [it] });
    }
  }
  // 栏缝判定阈值：约 1.2 倍基准字号（词间空隙远小于此，栏缝远大于此）。
  const gapTol = center != null ? median(list.map((i) => i.size)) * 1.2 : Infinity;
  const lines = [];
  for (const row of rows) {
    row.items.sort((a, b) => a.x0 - b.x0);
    const parts = splitRow(row.items, center, gapTol);
    for (const part of parts) {
      // 公式（数学字体项）不进入正文文本：无论显示公式还是行内上下标/符号，
      // 一律剔除，避免公式乱码或 LaTeX 碎片混进提取与翻译结果。
      const text = part.filter((i) => !i.math).map((i) => i.str).join(' ').trim();
      lines.push({
        y: part[0].y,
        text,
        size: Math.max(...part.map((i) => i.size)),
        bold: part.some((i) => i.bold),
        italic: part.some((i) => i.italic),
        x0: Math.min(...part.map((i) => i.x0)),
        x1: Math.max(...part.map((i) => i.x1)),
      });
    }
  }
  return lines.filter((l) => l.text);
}

// 把一行的文本项在栏缝处拆开：若某空隙横跨栏缝且足够宽，则视为左右两栏，拆成两段；
// 否则保持整行（通栏标题/摘要等）。
function splitRow(items, center, gapTol) {
  if (center == null) return [items];
  for (let k = 0; k < items.length - 1; k++) {
    if (items[k].x1 <= center && center <= items[k + 1].x0) {
      const gap = items[k + 1].x0 - items[k].x1;
      return gap > gapTol ? [items.slice(0, k + 1), items.slice(k + 1)] : [items];
    }
  }
  return [items];
}

// 正文基准字号：按文字量加权的字号中位数。
function bodyFontSize(lines) {
  const arr = lines.map((l) => ({ size: l.size, w: l.text.length }))
    .sort((a, b) => a.size - b.size);
  const total = arr.reduce((s, a) => s + a.w, 0);
  let acc = 0;
  for (const a of arr) { acc += a.w; if (acc >= total / 2) return a.size; }
  return arr.length ? arr[Math.floor(arr.length / 2)].size : 10;
}

// 由字号（+ 加粗 / 篇幅）推断标题层级：0=正文，1/2/3=一级/二级/三级标题。
function headingLevel(size, bold, textLen, body) {
  if (!(body > 0)) return 0;
  const r = size / body;
  if (r >= 1.4) return 1;
  if (r >= 1.2) return 2;
  if (r >= 1.1) return 3;
  if (bold && r >= 1.0 && textLen <= 80) return 3;
  return 0;
}

// 把 PDF 字号（近似 pt）归一化到 0.5pt 精度，并限制在合理区间。
function roundSize(s) {
  if (!(s > 0)) return 0;
  const pt = Math.round(s * 2) / 2;
  return Math.min(72, Math.max(4, pt));
}

// 判断一行是否居中（左右留白接近且都较大）。
function isCentered(line, minX, maxX) {
  const width = maxX - minX;
  if (width <= 0) return false;
  const lineW = line.x1 - line.x0;
  const left = line.x0 - minX;
  const right = maxX - line.x1;
  return lineW < width * 0.7 && left > width * 0.12 && Math.abs(left - right) < width * 0.08;
}

// 正文块的左缘：取各行 x0 的中位数（对悬挂缩进 / 缩进更稳健）。
function blockLeftEdge(lines) {
  const xs = lines.map((l) => l.x0).sort((a, b) => a - b);
  return xs.length ? xs[Math.floor(xs.length / 2)] : 0;
}

// 对一页的文本层做布局分析，返回结构化块
// [{ level, text, size, bold, italic, center, indent }]。
function layoutAnalysis(tc) {
  const items = extractTextItems(tc.items || []);
  if (!items.length) return [];
  const split = detectColumnSplit(items);
  const lines = buildLines(items, split ? split.center : null);
  return blocksFromLines(lines, split);
}

// 段内行拼接：普通行以单个空格连接；行尾连字符（英文单词跨行）去掉后直连，
// 避免出现多余换行，使段落与原文一致。
function joinPara(parts) {
  let out = parts[0] || '';
  for (let i = 1; i < parts.length; i++) {
    const prev = parts[i - 1];
    const cur = parts[i];
    if (/[A-Za-z0-9]-$/.test(prev) && /^[a-z]/.test(cur)) {
      out = out.slice(0, -1) + cur; // 跨行连字符：去掉后直接连接
    } else {
      out += ' ' + cur;
    }
  }
  return out;
}

// 对「已按自上而下排序、且左右栏已拆开」的行列表做布局分析，返回结构化块
// [{ level, text, size, bold, italic, center, indent }]。
// 行对象形如 { y, text, size, bold, italic, x0, x1 }。
// split 为栏缝中心（单栏为 null）；顺序为自上而下（单栏）或「通栏 → 左栏 → 右栏」（双栏）。
// 双栏论文按列优先顺序重排为单栏；正文行合并成段落（段内不换行），
// 段落边界由「标题 / 居中行 / 空行 / 首行缩进」判定；标题层级（大纲）保持不变。
function blocksFromLines(lines, split) {
  if (!lines.length) return [];

  let groups;
  if (split) {
    const full = [], left = [], right = [];
    const fullIdx = [];
    let firstCol = lines.length;
    let lastCol = -1;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      const spans = line.x0 < split.center && line.x1 > split.center;
      if (spans) {
        full.push(line);
        fullIdx.push(i);
      } else if ((line.x0 + line.x1) / 2 < split.center) {
        left.push(line);
        if (i < firstCol) firstCol = i;
        if (i > lastCol) lastCol = i;
      } else {
        right.push(line);
        if (i < firstCol) firstCol = i;
        if (i > lastCol) lastCol = i;
      }
    }
    // 通栏行按垂直位置放回两栏前后，避免「页尾的通栏段落」被提前到两栏之前：
    // 栏区之前 → 左栏 → 右栏 → 栏区之内 → 栏区之后。用原序列下标而非 y 值
    // 判断前后，兼容 PDF（y 向上）与 OCR（y 向下）两种坐标系。
    const above = full.filter((_, k) => fullIdx[k] < firstCol);
    const below = full.filter((_, k) => fullIdx[k] > lastCol);
    const within = full.filter((_, k) => fullIdx[k] >= firstCol && fullIdx[k] <= lastCol);
    groups = [above, left, right, within, below];
  } else {
    groups = [lines];
  }

  const body = bodyFontSize(lines);
  const blocks = [];

  for (const group of groups) {
    if (!group.length) continue;
    const gMinX = Math.min(...group.map((l) => l.x0));
    const gMaxX = Math.max(...group.map((l) => l.x1));
    const leftEdge = blockLeftEdge(group);
    // 正常行距：组内相邻行的 y 差中位数（y 可为向上或向下坐标，取绝对值）。
    const pitches = [];
    for (let k = 1; k < group.length; k++) pitches.push(Math.abs(group[k - 1].y - group[k].y));
    const pitch = median(pitches) || body * 1.2;
    // 首行缩进阈值：约半个基准字号（首行缩进通常 ≥ 0.5em，对齐抖动 < 0.3em）。
    const indentTol = Math.max(3, body * 0.5);

    let para = null;
    let prevIndent = 0;
    const flush = () => {
      if (!para) return;
      blocks.push({
        level: para.level,
        text: joinPara(para.parts),
        size: roundSize(para.size),
        bold: para.bold,
        italic: para.italic,
        center: para.center,
        indent: para.center ? 0 : para.indent > 1 ? para.indent : 0,
      });
      para = null;
    };

    for (let k = 0; k < group.length; k++) {
      const line = group[k];
      const lvl = headingLevel(line.size, line.bold, line.text.length, body);
      const center = isCentered(line, gMinX, gMaxX);
      // 左缩进：相对正文块左缘的偏移（仅当明显偏移且非居中时保留）。
      const indent = center ? 0 : Math.max(0, Math.round((line.x0 - leftEdge) * 2) / 2);
      const gap = k > 0 ? Math.abs(group[k - 1].y - line.y) : 0;

      // 段落边界：标题 / 居中行 / 空行（行距明显变大）/ 首行缩进（左缘明显右移）。
      const isBreak =
        lvl > 0 ||
        center ||
        (k > 0 && gap > pitch * 1.8) ||
        (k > 0 && indent > indentTol && prevIndent <= indentTol);

      if (isBreak) {
        flush();
        if (lvl > 0 || center) {
          // 标题 / 居中行单独成块，不并入段落，保证大纲级别不变。
          blocks.push({
            level: lvl,
            text: line.text,
            size: roundSize(line.size),
            bold: line.bold,
            italic: line.italic,
            center,
            indent: 0,
          });
          prevIndent = indent;
          continue;
        }
      }

      if (!para) {
        para = {
          level: lvl, center, indent,
          size: line.size, bold: line.bold, italic: line.italic,
          parts: [line.text],
        };
      } else {
        para.parts.push(line.text);
        if (line.size > para.size) para.size = line.size;
        para.bold = para.bold || line.bold;
        para.italic = para.italic || line.italic;
      }
      prevIndent = indent;
    }
    flush();
  }
  return blocks;
}

// 把 OCR 返回的逐行包围盒（像素坐标）换算成 pt 行对象并做布局分析，
// 让扫描页也能还原字号 / 行距 / 居中 / 双栏 / 标题层级。
// OCR 无法提供加粗 / 斜体信息，这两项始终为 false。
function ocrLinesToBlocks(lines, ocrScale) {
  const s = ocrScale || 1;
  const rows = [];
  for (const l of lines || []) {
    const text = (l.text || '').trim();
    const h = (l.h || 0) / s;
    if (!text || !(h > 0)) continue;
    rows.push({
      y: (l.y || 0) / s,
      text,
      size: h,
      bold: false,
      italic: false,
      x0: (l.x || 0) / s,
      x1: ((l.x || 0) + (l.w || 0)) / s,
    });
  }
  // OCR 行 y 向下、升序即自上而下，可直接交给布局分析。
  rows.sort((a, b) => a.y - b.y);
  const split = detectColumnSplit(rows);
  return blocksFromLines(rows, split);
}

// 计算 OCR 用渲染倍率：目标约 180 DPI，最长边不超过 3000px。
function ocrScale(page) {
  let scale = 2.5;
  const vp = page.getViewport({ scale });
  const maxEdge = Math.max(vp.width, vp.height);
  if (maxEdge > 3000) scale *= 3000 / maxEdge;
  return scale;
}

// 把页面按指定倍率渲染到离屏 canvas，返回 canvas 本身（供 OCR 或裁剪插图复用）。
// 命名用 pageToCanvas 以区别于上面的 renderPageCanvas(i)（分页懒加载渲染）。
async function pageToCanvas(page, scale) {
  const viewport = page.getViewport({ scale });
  const canvas = document.createElement('canvas');
  canvas.width = Math.ceil(viewport.width);
  canvas.height = Math.ceil(viewport.height);
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  // 铺白底：透明画布在部分 WebView2 里会合成成黑块，且导出 JPEG 时透明区会变黑。
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  await page.render({ canvasContext: ctx, viewport }).promise;
  return canvas;
}

// 把离屏 canvas 逆时针旋转 90°（用于竖排古籍：让竖列变横排，修复 OCR 乱序/换行）。
function rotate90CCW(canvas) {
  const out = document.createElement('canvas');
  out.width = canvas.height;
  out.height = canvas.width;
  const ctx = out.getContext('2d', { willReadFrequently: true });
  ctx.translate(0, canvas.width);
  ctx.rotate(-Math.PI / 2);
  ctx.drawImage(canvas, 0, 0);
  return out;
}

// 渲染页面用于 OCR / 云端解析，返回 { canvas, dataUrl }。
// cloud=true：最长边压到 2000px 并以 JPEG(q0.85) 发送——超大扫描件的 PNG 可达数 MB，
// 云端（GLM / 大模型）会因图片过大而拒绝；压缩后体积小一个数量级，且对 OCR 精度影响很小。
// cloud=false（本地 OCR）：保持 PNG + 180 DPI 目标不变。
// vertical=true：竖排古籍模式，先逆时针旋转 90° 再输出（仅云端 OCR 文本路径使用）。
async function pageToOcrImage(page, cloud, vertical) {
  let scale = ocrScale(page);
  if (cloud) {
    const vp = page.getViewport({ scale });
    const maxEdge = Math.max(vp.width, vp.height);
    if (maxEdge > 2000) scale *= 2000 / maxEdge;
  }
  let canvas = await pageToCanvas(page, scale);
  if (vertical) canvas = rotate90CCW(canvas);
  const dataUrl = cloud ? canvas.toDataURL('image/jpeg', 0.85) : canvas.toDataURL('image/png');
  return { canvas, dataUrl };
}

// 按 0~1000 归一化 bbox 从整页 canvas 裁剪出插图，返回 PNG data URL。
// 外扩 2% 余量以容错视觉模型定位偏差，越界时裁剪到页内。
function cropFigure(canvas, bbox) {
  const [x0, y0, x1, y1] = bbox;
  const W = canvas.width;
  const H = canvas.height;
  const pad = 0.02;
  let cx0 = (x0 / 1000) * W;
  let cy0 = (y0 / 1000) * H;
  let cx1 = (x1 / 1000) * W;
  let cy1 = (y1 / 1000) * H;
  const dx = (cx1 - cx0) * pad;
  const dy = (cy1 - cy0) * pad;
  cx0 = Math.max(0, cx0 - dx);
  cy0 = Math.max(0, cy0 - dy);
  cx1 = Math.min(W, cx1 + dx);
  cy1 = Math.min(H, cy1 + dy);
  const w = Math.max(1, Math.round(cx1 - cx0));
  const h = Math.max(1, Math.round(cy1 - cy0));
  const out = document.createElement('canvas');
  out.width = w;
  out.height = h;
  out.getContext('2d').drawImage(canvas, cx0, cy0, w, h, 0, 0, w, h);
  return out.toDataURL('image/png');
}

// 有界并发的 map：最多同时跑 limit 个异步任务，保持结果顺序。
async function mapWithConcurrency(items, limit, fn) {
  const results = new Array(items.length);
  let i = 0;
  const workers = Array.from(
    { length: Math.min(limit, items.length) },
    async () => {
      while (i < items.length) {
        const idx = i++;
        results[idx] = await fn(items[idx], idx);
      }
    },
  );
  await Promise.all(workers);
  return results;
}

// 判断一行是否为 Markdown 标题（# …，允许 # 后无空格），返回 { level, text } 或 null。
function parseMdHeading(line) {
  let s = String(line || '').trim();
  // 大模型可能用 HTML 标题标签 <h1>…</h1> 标记小节。
  const htag = /^<h([1-6])[^>]*>([\s\S]*?)<\/h\1>$/i.exec(s);
  if (htag) {
    return { level: Math.min(Number(htag[1]), 3), text: htag[2].trim() };
  }
  // 剥掉可能包在 # 外的其它 HTML 标签（<b>## 6</b> / <strong>## 6</strong> 等），
  // 否则 # 无法匹配到行首，会原样漏进正文。
  s = s.replace(/<\/?[a-zA-Z][^>]*>/g, '');
  // 若整行被 Markdown 强调包裹（`**## 5 ...**` / `__## 5 ...__` / `*## 5 ...*`），
  // 先剥掉这层让 # 能匹配到行首，否则 # 会原样漏进正文；标题内部的强调
  // （`## **5 ...**`）交给 stripInlineMd 处理。
  const wrapped = /^([*_]{1,2})([\s\S]+?)\1$/.exec(s);
  if (wrapped) s = wrapped[2].trim();
  const m = /^(#{1,6})\s*(.+)$/.exec(s);
  if (!m) return null;
  return { level: Math.min(m[1].length, 3), text: m[2].trim() };
}

// 提取整篇文档为「每页的结构化块数组」（pagesBlocks[i] = blocks）。
// withOcr=false：仅文本层（快速，侧栏「提取文字」用）；
// withOcr=true：文本层 + 扫描页 OCR（导出 / 目录用，OCR 页有界并发）。
// 把 GLM-OCR 返回的 Markdown 解析成导出块列表。
// GLM 的 md_results 里：`#` 开头是标题、`|…|` 是表格、`$$…$$`（可能跨行）是块级公式、
// `$…$` 是行内公式（原样保留，交给 Rust 转 OMML/Unicode）、偶有 `<sub>`/`<sup>` 等 HTML。
// 这里把它们还原成结构化 {level, text, table}，避免 #、HTML 标签、竖线等原样落进文档。
function parseMarkdownBlocks(md) {
  const lines = String(md || '').split('\n');
  const blocks = [];
  const base = () => ({ level: 0, text: '', size: 0, bold: false, italic: false, center: false, indent: 0 });

  // 追加文本块；center / indent 来自当前上下文（<div align="center"> 居中、列表缩进）。
  const pushText = (text, level, center, indent) => {
    const t = stripInlineMd(text);
    if (!t) return;
    const b = { ...base(), text: t, level: level || 0 };
    if (center) b.center = true;
    if (indent) b.indent = indent;
    blocks.push(b);
  };

  let i = 0;
  let centering = false; // 是否处于 <div align="center"> 区块内
  while (i < lines.length) {
    const line = lines[i];
    const trimmed = line.trim();

    // 居中区块起止：<div align="center"> / <center> 开启，</div> / </center> 关闭。
    if (/^<div\s+[^>]*align\s*=\s*["']?center["']?[^>]*>\s*$/i.test(trimmed) || /^<center>\s*$/i.test(trimmed)) {
      centering = true;
      i++;
      continue;
    }
    if (/^<\/div>\s*$/i.test(trimmed) || /^<\/center>\s*$/i.test(trimmed)) {
      centering = false;
      i++;
      continue;
    }

    // 块级公式：$$…$$（允许跨行，GLM 常把公式独占数行）。
    if (/^\s*\$\$/.test(line)) {
      const seg = collectDisplayMath(lines, i);
      pushText(seg.text, 0, centering);
      i = seg.next;
      continue;
    }

    // 标题：# / ## / ### …（允许 # 后无空格）。
    const h = parseMdHeading(line);
    if (h) {
      pushText(h.text, h.level, centering);
      i++;
      continue;
    }

    // 标题标记独占一行（GLM 偶发把 `##` 与标题文字拆成两行）：把下一行非空文本合并成标题。
    const markerOnly = /^(#{1,6})\s*$/.exec(trimmed);
    if (markerOnly) {
      const level = Math.min(markerOnly[1].length, 3);
      let j = i + 1;
      while (j < lines.length && !lines[j].trim()) j++;
      // 下一行本身是标题/表格/块级公式则不再合并，直接丢弃本行标记。
      if (j < lines.length && !/^(#{1,6})\s|\$\$|\|/.test(lines[j].trim())) {
        pushText(lines[j], level, centering);
        i = j + 1;
      } else {
        i++;
      }
      continue;
    }

    // 诊断：以 # 开头却未被识别成标题的行，用于定位 ## 泄漏的确切格式。
    if (/^\s*#/.test(line)) {
      logDiag('[glm-hash-leak] ' + JSON.stringify(line));
    }

    // Markdown 表格：连续以 | 开头的行。
    if (/^\s*\|/.test(line)) {
      const rows = [];
      let j = i;
      while (j < lines.length && /^\s*\|/.test(lines[j])) {
        const cells = parseMdTableRow(lines[j]);
        if (cells) rows.push(cells);
        j++;
      }
      // 去掉分隔行（|---|、|:---:|）。
      const data = rows.filter((r) => !r.every((c) => /^:?-{2,}:?$/.test(c.trim())));
      if (data.length) blocks.push({ ...base(), table: data });
      i = j;
      continue;
    }

    // 有序列表：`1. ` / `2. ` … 保留序号文字，左缩进一级（18pt）。
    if (/^\d{1,3}[.)]\s+\S/.test(trimmed)) {
      pushText(trimmed, 0, centering, 18);
      i++;
      continue;
    }

    // 无序列表：`- ` / `* ` / `+ ` … 保留项目符号，左缩进一级。
    if (/^[-*+]\s+\S/.test(trimmed)) {
      pushText(trimmed, 0, centering, 18);
      i++;
      continue;
    }

    // 普通文本行。
    pushText(line, 0, centering);
    i++;
  }
  return blocks;
}

// 收集从 lines[start]（以 $$ 开头）起的块级公式，跨行直到闭合的 $$。
// GLM 的块级公式有两种写法：① `$$…$$` 同一行；② `$$` 独占一行（开、闭各占一行，
// 中间是多行 LaTeX）。旧逻辑只认第一种，遇到第二种会一路吞到页尾，把后面的 `## 标题`
// 全部并进公式里——这正是 `##` 泄漏的根因。这里按起始行形态区分两种模式。
function collectDisplayMath(lines, start) {
  let buf = '';
  let j = start;
  const blockMode = /^\s*\$\$\s*$/.test(lines[start]);
  for (; j < lines.length; j++) {
    const line = lines[j];
    buf += (buf ? '\n' : '') + line;
    if (blockMode) {
      // 独占一行：遇到下一行「仅 $$」即闭合。
      if (j > start && /^\s*\$\$\s*$/.test(line)) break;
    } else {
      // 同一行：遇到第二个 $$ 即闭合。
      const first = line.indexOf('$$');
      if (line.indexOf('$$', first + 2) !== -1) break;
    }
  }
  return { text: buf, next: j + 1 };
}

// 把一行 | a | b | 拆成单元格数组，去首尾因 | 边界产生的空单元。
function parseMdTableRow(line) {
  const cells = line.trim().split('|');
  if (cells.length && cells[0].trim() === '') cells.shift();
  if (cells.length && cells[cells.length - 1].trim() === '') cells.pop();
  return cells.map((c) => stripInlineMd(c.trim()));
}

// 去掉行内 Markdown/HTML：**加粗**、*斜体*、`code`、<sub>/<sup>（转 Unicode 上下标）、
// 其它 HTML 标签直接丢弃；`$…$` 公式原样保留。
function stripInlineMd(s) {
  let t = String(s || '');
  t = t.replace(/<sub>(.*?)<\/sub>/gi, (_, c) => toUnicodeScript(c, false));
  t = t.replace(/<sup>(.*?)<\/sup>/gi, (_, c) => toUnicodeScript(c, true));
  t = t.replace(/<[^>]+>/g, '');                          // 其余 HTML 标签丢弃
  t = t.replace(/\*\*(.+?)\*\*/g, '$1');                  // **加粗**
  t = t.replace(/(^|[^*])\*([^*\s][^*]*?)\*(?!\*)/g, '$1$2'); // *斜体*
  t = t.replace(/`([^`]+)`/g, '$1');                      // 行内代码
  t = t.replace(/^#{1,6}\s*/, '');                        // 兜底：剥掉漏网的标题 # 标记
  t = t.replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"');
  return t.trim();
}

// 把字符串逐字符转成 Unicode 上下标（仅常见字母/数字/符号），任一字符无对应时整体退回原串。
function toUnicodeScript(s, isSup) {
  const sup = {
    '0': '⁰', '1': '¹', '2': '²', '3': '³', '4': '⁴', '5': '⁵', '6': '⁶', '7': '⁷', '8': '⁸', '9': '⁹',
    '+': '⁺', '-': '⁻', '=': '⁼', '(': '⁽', ')': '⁾', 'n': 'ⁿ', 'i': 'ⁱ',
    'a': 'ᵃ', 'b': 'ᵇ', 'c': 'ᶜ', 'd': 'ᵈ', 'e': 'ᵉ', 'f': 'ᶠ', 'g': 'ᵍ', 'h': 'ʰ',
    'j': 'ʲ', 'k': 'ᵏ', 'l': 'ˡ', 'm': 'ᵐ', 'o': 'ᵒ', 'p': 'ᵖ', 'r': 'ʳ', 's': 'ˢ',
    't': 'ᵗ', 'u': 'ᵘ', 'v': 'ᵛ', 'w': 'ʷ', 'x': 'ˣ', 'y': 'ʸ', 'z': 'ᶻ',
  };
  const sub = {
    '0': '₀', '1': '₁', '2': '₂', '3': '₃', '4': '₄', '5': '₅', '6': '₆', '7': '₇', '8': '₈', '9': '₉',
    '+': '₊', '-': '₋', '=': '₌', '(': '₍', ')': '₎',
    'a': 'ₐ', 'e': 'ₑ', 'h': 'ₕ', 'i': 'ᵢ', 'j': 'ⱼ', 'k': 'ₖ', 'l': 'ₗ', 'm': 'ₘ',
    'n': 'ₙ', 'o': 'ₒ', 'p': 'ₚ', 'r': 'ᵣ', 's': 'ₛ', 't': 'ₜ', 'u': 'ᵤ', 'v': 'ᵥ', 'x': 'ₓ',
  };
  const map = isSup ? sup : sub;
  let out = '';
  for (const ch of s) {
    const v = map[ch];
    if (v === undefined) return s;
    out += v;
  }
  return out;
}

async function extractPages(withOcr) {
  const total = state.pdfDoc.numPages;
  const pagesBlocks = new Array(total);
  const ocrPages = [];
  // 云端 OCR（GLM / 大模型）对超大图片敏感，需压缩；本地 OCR 保持原样。
  const cfg = withOcr ? await invoke('get_config') : null;
  const cloud = !!(cfg && (cfg.ocr.mode === 'glm' || cfg.ocr.mode === 'llm'));
  const vertical = !!(cfg && cfg.ocr.vertical && cloud);

  for (let i = 0; i < total; i++) {
    const page = await state.pdfDoc.getPage(i + 1);
    const tc = await page.getTextContent();
    const hasText = (tc.items || []).some((it) => 'str' in it && it.str.trim());
    pagesBlocks[i] = hasText ? layoutAnalysis(tc) : [];
    if (!hasText && withOcr) ocrPages.push(i);
  }

  if (ocrPages.length) {
    let done = 0;
    setStatus(`正在识别扫描页… 0 / ${ocrPages.length}`);
    const results = await mapWithConcurrency(ocrPages, 4, async (pageIdx) => {
      const page = await state.pdfDoc.getPage(pageIdx + 1);
      const scale = ocrScale(page); // 本地 OCR 包围盒换算用（cloud 时无包围盒，不依赖它）
      const { dataUrl: png } = await pageToOcrImage(page, cloud, vertical);
      const result = await invoke('ocr_image', { request: { png_data_url: png } });
      done++;
      setStatus(`正在识别扫描页… ${done} / ${ocrPages.length}`);
      // 原始输出留存（GLM 为 Markdown；本地 OCR 为逐行文本），供排查 ## 泄漏。
      if (result && result.text && result.text.trim()) {
        ocrRawDump.push(`===== 第 ${pageIdx + 1} 页（OCR 原始输出）=====\n` + result.text);
      }
      // 本地 OCR 返回逐行包围盒 → 还原排版；大模型 OCR 仅有文本 → 按行拆分回退。
      if (result && result.lines && result.lines.length) {
        return ocrLinesToBlocks(result.lines, scale);
      }
      // 大模型 / GLM OCR 返回 Markdown（含 # 标题、| 表格、$$…$$ 公式、行内 $…$）：
      // 解析成结构化块，避免 #、HTML 标签、竖线等原样落进文档。
      return result && result.text && result.text.trim()
        ? parseMarkdownBlocks(result.text)
        : [];
    });
    ocrPages.forEach((pageIdx, k) => { pagesBlocks[pageIdx] = results[k]; });
  }
  return pagesBlocks;
}

// 提取整篇文档为扁平的块列表（顺序即页序）。
async function extractDocument(withOcr) {
  const pagesBlocks = await extractPages(withOcr);
  const blocks = [];
  for (let i = 0; i < pagesBlocks.length; i++) blocks.push(...(pagesBlocks[i] || []));
  return blocks;
}

// 云端版面解析：逐页把渲染图发给视觉大模型，返回扁平的导出项列表。
// 每项是正文 {level,text,...}、表格 {table} 或图片 {image} 之一（顺序即页序；
// 单页内按「正文 → 表格 → 图片(带图题)」排列）。
async function extractDocumentCloud() {
  const total = state.pdfDoc.numPages;
  let done = 0;
  setStatus(`云端解析页面… 0 / ${total}`);

  const results = await mapWithConcurrency(
    Array.from({ length: total }, (_, i) => i),
    2,
    async (pageIdx) => {
      const page = await state.pdfDoc.getPage(pageIdx + 1);
      // 同一 canvas 既用于发送（压缩后），也用于按归一化 bbox 裁剪插图，保证坐标一致。
      const { canvas, dataUrl: png } = await pageToOcrImage(page, true);
      const extracted = await invoke('extract_page', { request: { png_data_url: png } });
      done++;
      setStatus(`云端解析页面… ${done} / ${total}`);

      // 原始输出留存（未做标题清洗），供排查 ## 泄漏。
      if (extracted && Array.isArray(extracted.paragraphs)) {
        ocrRawDump.push(`===== 第 ${pageIdx + 1} 页（云端 extract_page 原始段落）=====\n` + extracted.paragraphs.join('\n'));
      }

      const pageItems = [];
      let pendingLevel = 0;
      for (const t of extracted.paragraphs || []) {
        const text = (t || '').trim();
        if (!text) continue;
        // 大模型偶发用 Markdown 标题（## …）标记小节，剥掉 # 并转成大纲级别；
        // 顺带清理加粗 / 斜体 / 上下标 HTML，行内 $…$ 公式原样保留。
        // 标题标记独占一项（`##` 与标题拆成两项）时，把级别记下套到下一项。
        const markerOnly = /^(#{1,6})\s*$/.exec(text);
        if (markerOnly) {
          pendingLevel = Math.min(markerOnly[1].length, 3);
          continue;
        }
        const h = parseMdHeading(text);
        if (!h && /#/.test(text)) {
          // 诊断：记录未被识别成标题、但仍含 # 的原始段落，便于定位 ## 泄漏的确切格式。
          logDiag('[cloud-hash-leak] ' + JSON.stringify(text));
        }
        const level = h ? h.level : pendingLevel;
        pendingLevel = 0;
        const cleaned = stripInlineMd(h ? h.text : text);
        if (cleaned) {
          pageItems.push({ level, text: cleaned, size: 0, bold: false, italic: false, center: false, indent: 0 });
        }
      }
      for (const tbl of extracted.tables || []) {
        const rows = (tbl.rows || []).map((r) => (r || []).map((c) => c || ''));
        if (rows.length) {
          pageItems.push({ level: 0, text: '', size: 0, bold: false, italic: false, center: false, indent: 0, table: rows });
        }
      }
      for (const fig of extracted.figures || []) {
        if (Array.isArray(fig.bbox) && fig.bbox.length >= 4) {
          const figPng = cropFigure(canvas, fig.bbox);
          pageItems.push({ level: 0, text: '', size: 0, bold: false, italic: false, center: true, indent: 0, image: { png_data_url: figPng } });
        }
        const caption = (fig.caption || '').trim();
        if (caption) {
          pageItems.push({ level: 0, text: caption, size: 0, bold: false, italic: false, center: true, indent: 0 });
        }
      }
      return pageItems;
    },
  );

  const items = [];
  for (const pageItems of results) items.push(...(pageItems || []));
  return items;
}

// 把云端导出项拍平成纯文本（表格按行用制表符连接、图片用占位符），供 .txt 导出。
function cloudItemsToText(items) {
  const lines = [];
  for (const it of items) {
    if (it.table) {
      for (const row of it.table) lines.push(row.join('\t'));
      lines.push('');
    } else if (it.image) {
      lines.push('[图片]');
    } else if (it.text && it.text.trim()) {
      lines.push(it.text.trim());
    }
  }
  return lines.join('\n').trim();
}

// 提取「当前页」文字；若该页无文本层（扫描页）且 withOcr 为真，则仅对这一页做 OCR。
async function extractCurrentPage(withOcr, pageNo) {
  const page = await state.pdfDoc.getPage(pageNo || state.pageNum);
  const tc = await page.getTextContent();
  const hasText = (tc.items || []).some((it) => 'str' in it && it.str.trim());
  if (hasText) return layoutAnalysis(tc);
  if (!withOcr) return [];

  const cfg = await invoke('get_config');
  const cloud = cfg.ocr.mode === 'glm' || cfg.ocr.mode === 'llm';
  const vertical = !!(cfg.ocr.vertical && cloud);
  const scale = ocrScale(page);
  const { dataUrl: png } = await pageToOcrImage(page, cloud, vertical);
  const result = await invoke('ocr_image', { request: { png_data_url: png } });
  // 本地 OCR 返回逐行包围盒 → 还原排版；大模型 / GLM OCR 返回 Markdown 文本。
  if (result && result.lines && result.lines.length) {
    return ocrLinesToBlocks(result.lines, scale);
  }
  return result && result.text && result.text.trim()
    ? parseMarkdownBlocks(result.text)
    : [];
}

async function extractText() {
  if (!state.pdfDoc) return;
  showSidebar();
  const pageNo = viewportCenterPage();
  setStatus(`正在提取第 ${pageNo} 页文字…`);
  try {
    const blocks = await extractCurrentPage(true, pageNo);
    state.extractedText = blocks.map((b) => b.text).join('\n').trim();
    $('text-content').innerHTML =
      state.extractedText ? renderMarkdown(state.extractedText) : '（当前页未识别到文字）';
    $('text-content').classList.toggle('markdown', !!state.extractedText);
    $('text-content').classList.remove('muted');
    setStatus(state.extractedText ? `已提取第 ${pageNo} 页文字` : '当前页未识别到文字');
  } catch (err) {
    setStatus('提取失败：' + err);
  }
}

// ---------- 页面提取（抽取指定页为独立 PDF） ----------

// 解析页码范围输入："1-3,5,7-9" → 去重、排序后的 1-based 页码数组；非法输入返回 null。
function parsePageRange(input, total) {
  const parts = String(input || '').split(/[,，;；\s]+/).filter(Boolean);
  if (!parts.length) return null;
  const set = new Set();
  for (const part of parts) {
    const m = /^(\d+)\s*[-–—~]\s*(\d+)$/.exec(part);
    if (m) {
      let a = Number(m[1]);
      let b = Number(m[2]);
      if (a > b) [a, b] = [b, a];
      for (let p = a; p <= b; p++) set.add(p);
    } else if (/^\d+$/.test(part)) {
      set.add(Number(part));
    } else {
      return null;
    }
  }
  const pages = [...set].filter((p) => p >= 1 && p <= total).sort((a, b) => a - b);
  return pages.length ? pages : null;
}

function openExtractPages() {
  if (!state.pdfDoc) return;
  // 默认填当前页，方便「只抽当前这一页」。
  $('extract-pages-range').value = String(state.pageNum);
  $('extract-pages').showModal();
}

function closeExtractPages() {
  $('extract-pages').close();
}

async function exportPagesToPdf() {
  const total = state.pdfDoc.numPages;
  const raw = $('extract-pages-range').value.trim();
  // 留空按当前页处理。
  const pages = raw ? parsePageRange(raw, total) : [state.pageNum];
  if (!pages) {
    setStatus('页码范围格式无效，例如 1-3,5,7-9');
    return;
  }
  closeExtractPages();
  const label = pages.length === 1
    ? `第${pages[0]}页`
    : `第${pages[0]}-${pages[pages.length - 1]}页`;
  const name = (state.name || '文档').replace(/\.pdf$/i, '') + `（${label}）.pdf`;
  setStatus('正在提取页面…');
  try {
    const saved = await invoke('extract_pages', {
      path: state.path,
      pages,
      suggestedName: name,
    });
    setStatus(`已导出：${saved}`);
  } catch (err) {
    setStatus('提取失败：' + err);
  }
}

// 混合提取（文本层 + 扫描页 OCR）整篇文档并导出为 .txt。
// 当 OCR 方式为「大模型 / GLM」时走云端版面解析（去公式、去页眉页脚、表格还原）。
async function exportText() {
  if (!state.pdfDoc) return;
  if (!requirePro()) return;
  const btn = $('btn-export');
  btn.disabled = true;
  try {
    const cfg = await invoke('get_config');
    let full;
    if (cfg.ocr.mode === 'llm') {
      try {
        const items = await extractDocumentCloud();
        full = cloudItemsToText(items);
      } catch (err) {
        setStatus(`云端解析失败（${err}），已回退本地提取`);
        const blocks = await extractDocument(true);
        full = blocks.map((b) => b.text).join('\n').trim();
      }
    } else {
      const blocks = await extractDocument(true);
      full = blocks.map((b) => b.text).join('\n').trim();
    }
    if (!full) { setStatus('未提取到任何文字'); return; }

    $('text-content').innerHTML = renderMarkdown(full);
    $('text-content').classList.add('markdown');
    $('text-content').classList.remove('muted');
    const name = (state.name || '文档').replace(/\.pdf$/i, '') + '.txt';
    const saved = await invoke('export_text', { request: { text: full, suggested_name: name } });
    setStatus(`已导出：${saved}`);
  } catch (err) {
    setStatus('导出失败：' + err);
  } finally {
    btn.disabled = false;
  }
}

// ---------- 公式预览（MathJax → SVG → PNG） ----------

// 惰性加载 MathJax（首次需要时才加载本地 vendor 脚本，避免拖慢启动）。
let mathJaxLoad = null;

// 导出 Word 期间累积的「OCR 原始输出」（未做标题清洗），导出结束后写入 ocr_raw.txt，
// 便于定位 ## 标题泄漏的确切格式。
let ocrRawDump = [];
function loadMathJax() {
  if (mathJaxLoad) return mathJaxLoad;
  if (window.MathJax && window.MathJax.startup && window.MathJax.startup.promise) {
    mathJaxLoad = window.MathJax.startup.promise;
    return mathJaxLoad;
  }
  mathJaxLoad = new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = './vendor/mathjax/tex-svg.js';
    s.onload = () => {
      const mj = window.MathJax;
      if (mj && mj.startup && mj.startup.promise) resolve(mj.startup.promise);
      else reject(new Error('MathJax 未正确初始化'));
    };
    s.onerror = () => reject(new Error('MathJax 脚本加载失败'));
    document.head.appendChild(s);
  });
  return mathJaxLoad;
}

// 把一段 LaTeX 渲染成 PNG 预览（返回 {data_url, width, height}，CSS 像素）；
// 渲染失败返回 null，后端会回退为纯文本公式。
async function renderFormulaPng(latex) {
  try {
    await loadMathJax();
    const container = document.createElement('div');
    container.style.position = 'absolute';
    container.style.visibility = 'hidden';
    container.style.left = '-10000px';
    container.textContent = '\\(' + latex + '\\)';
    document.body.appendChild(container);
    await window.MathJax.typesetPromise([container]);
    const svg = container.querySelector('svg');
    if (!svg) { container.remove(); return null; }
    // 用浏览器实际布局尺寸（px）；取不到再按 ex=8px 换算。
    let w = svg.getBoundingClientRect().width;
    let h = svg.getBoundingClientRect().height;
    if (!w || !h) {
      const ex = (s) => { const m = /^([0-9.]+)ex$/.exec(String(s || '').trim()); return m ? parseFloat(m[1]) * 8 : 0; };
      w = ex(svg.getAttribute('width'));
      h = ex(svg.getAttribute('height'));
    }
    if (!w || !h) { container.remove(); return null; }
    w = Math.round(w * 100) / 100;
    h = Math.round(h * 100) / 100;
    svg.setAttribute('width', w + 'px');
    svg.setAttribute('height', h + 'px');
    const xml = new XMLSerializer().serializeToString(svg);
    container.remove();
    // SVG → Image → canvas（4× 高清）→ PNG data URL。
    const url = URL.createObjectURL(new Blob([xml], { type: 'image/svg+xml' }));
    const img = new Image();
    await new Promise((res, rej) => {
      img.onload = res;
      img.onerror = () => rej(new Error('SVG 图片解码失败'));
      img.src = url;
    });
    const scale = 4;
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(w * scale));
    canvas.height = Math.max(1, Math.round(h * scale));
    const ctx = canvas.getContext('2d');
    ctx.scale(scale, scale);
    ctx.drawImage(img, 0, 0, w, h);
    URL.revokeObjectURL(url);
    return { data_url: canvas.toDataURL('image/png'), width: w, height: h };
  } catch (err) {
    logDiag('公式预览渲染失败：' + latex + ' → ' + err);
    return null;
  }
}

// 导出为带大纲级别的 .docx（标题层级由前端字号启发式判定，与原文一致）。
// 当 OCR 方式为「大模型 / GLM」时走云端版面解析：公式/页眉页脚去除、表格还原、
// 插图以图片插入 Word。
async function exportWord() {
  if (!state.pdfDoc) return;
  if (!requirePro()) return;
  const btn = $('btn-export-word');
  btn.disabled = true;
  ocrRawDump = [];
  try {
    const paragraphs = await collectExportParagraphs();
    if (!paragraphs.length) { setStatus('未提取到任何文字'); return; }
    const name = (state.name || '文档').replace(/\.pdf$/i, '') + '.docx';
    setStatus('正在渲染公式预览…');
    const previews = await renderFormulaPreviews(paragraphs);
    setStatus('正在生成 Word 文档…');
    const saved = await invoke('export_docx', {
      request: { paragraphs, suggested_name: name, previews },
    });
    setStatus(`已导出：${saved}`);
    // 把 OCR 原始输出（未清洗）整体写入 ocr_raw.txt，便于排查 ## 标题泄漏。
    if (ocrRawDump.length) {
      try {
        const rawPath = await invoke('save_ocr_raw', { text: ocrRawDump.join('\n\n') });
        logDiag('OCR 原始输出已存：' + rawPath);
        setStatus(`已导出：${saved}；OCR 原文：${rawPath}`);
      } catch { /* 原始输出留存失败不阻塞导出 */ }
    }
  } catch (err) {
    setStatus('导出失败：' + err);
  } finally {
    btn.disabled = false;
  }
}

// 导出一份「完全翻译」的 Word：先把全文段落翻译成目标语言（公式用占位符保护、不翻译，
// 标题层级 / 表格 / 图片 / 字号等结构原样保留），再走与 exportWord 完全相同的 OLE 注入
// 流程，因此格式与导出的原文一致。
async function exportWordTranslated() {
  if (!state.pdfDoc) return;
  if (!requirePro()) return;
  const btn = $('btn-export-word-tr');
  btn.disabled = true;
  try {
    const cfg = await invoke('get_config');
    if (!cfg.translate.api_key) {
      setStatus('请先在「设置」里填写翻译 API Key，再导出译文');
      return;
    }
    const paragraphs = await collectExportParagraphs();
    if (!paragraphs.length) { setStatus('未提取到任何文字'); return; }
    const name = (state.name || '文档').replace(/\.pdf$/i, '') + '（译文）.docx';
    setStatus('正在翻译全文（公式保持原样，多段并发）…');
    const translated = await invoke('translate_paragraphs', { paragraphs });
    if (!translated || !translated.length) { setStatus('翻译结果为空'); return; }
    setStatus('正在渲染公式预览…');
    const previews = await renderFormulaPreviews(translated);
    setStatus('正在生成 Word 文档…');
    const saved = await invoke('export_docx', {
      request: { paragraphs: translated, suggested_name: name, previews },
    });
    setStatus(`已导出：${saved}`);
  } catch (err) {
    setStatus('导出失败：' + err);
  } finally {
    btn.disabled = false;
  }
}

// 提取导出用的结构化段落（云端解析，失败回退本地提取），与 exportWord / exportWordTranslated 共用。
async function collectExportParagraphs() {
  const cfg = await invoke('get_config');
  const useCloud = cfg.ocr.mode === 'llm';
  if (useCloud) {
    try {
      return await extractDocumentCloud();
    } catch (err) {
      setStatus(`云端解析失败（${err}），已回退本地提取`);
      return await extractDocument(true);
    }
  }
  return await extractDocument(true);
}

// 汇总去重公式 → MathJax 批量渲染 PNG 预览（两步导出，与 exportWord / exportWordTranslated 共用）。
async function renderFormulaPreviews(paragraphs) {
  let previews = [];
  try {
    const formulas = await invoke('collect_math', { paragraphs });
    if (formulas && formulas.length) {
      const rendered = await mapWithConcurrency(formulas, 4, async (latex) => {
        const png = await renderFormulaPng(latex);
        return png ? { latex, png_data_url: png.data_url, width: png.width, height: png.height } : null;
      });
      previews = rendered.filter(Boolean);
    }
  } catch (err) {
    // 渲染失败不阻塞导出：后端会把无预览的公式回退为纯文本。
    logDiag('公式预览渲染失败：' + err);
    previews = [];
  }
  return previews;
}

function getSelectedText() {
  const sel = window.getSelection();
  return sel ? sel.toString().trim() : '';
}

// ---------- 目录（书签 / 标题导航） ----------

// 把 PDF 书签 dest 解析成 0-based 页码。
async function resolveDestPage(dest) {
  if (!dest) return null;
  try {
    let d = dest;
    if (typeof d === 'string') d = await state.pdfDoc.getDestination(d);
    if (Array.isArray(d) && d[0]) return await state.pdfDoc.getPageIndex(d[0]);
  } catch { /* 解析失败按无页码处理 */ }
  return null;
}

// 读取 PDF 内置书签目录（无书签时返回 null）。
async function getOutline() {
  try {
    const outline = await state.pdfDoc.getOutline();
    if (!outline || !outline.length) return null;
    const items = [];
    async function walk(nodes, depth) {
      for (const n of nodes) {
        items.push({
          title: (n.title || '').trim(),
          pageIdx: await resolveDestPage(n.dest),
          level: Math.min(depth, 3),
        });
        if (n.items && n.items.length) await walk(n.items, depth + 1);
      }
    }
    await walk(outline, 1);
    return items.length ? items : null;
  } catch {
    return null;
  }
}

// 无书签时：从字号启发式识别的标题生成目录（扫描页走 OCR，较慢）。
async function generateToc() {
  const btn = $('toc-generate');
  btn.disabled = true;
  setStatus('正在分析标题生成目录…');
  try {
    const pagesBlocks = await extractPages(true);
    const items = [];
    for (let i = 0; i < pagesBlocks.length; i++) {
      for (const b of pagesBlocks[i] || []) {
        if (b.level > 0 && b.text.trim()) {
          items.push({ title: b.text.trim(), pageIdx: i, level: b.level });
        }
      }
    }
    state.tocItems = items;
    renderToc(items);
    $('toc-generate').style.display = 'none';
    $('toc-hint').style.display = 'none';
    setStatus(items.length ? '目录已生成' : '未识别到标题');
  } catch (err) {
    setStatus('目录生成失败：' + err);
  } finally {
    btn.disabled = false;
  }
}

// 阿拉伯数字逐位转汉字：0→〇、1-9→一…九（不加十百等位数，如 567→五六七）。
const CN_DIGITS = ['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
function toChineseDigits(str) {
  return String(str).replace(/\d/g, (d) => CN_DIGITS[Number(d)]);
}

// ===== 精简 Markdown 渲染 =====
// 支持：标题（章节）、粗体/斜体（标记）、行内代码/代码块、行内公式 $...$、块级公式 $$...$$/\[...\]、
// 无序/有序列表、引用、分隔线、链接。公式用 KaTeX 渲染。
function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// 用 KaTeX 渲染一段公式；KaTeX 未加载或渲染失败时回退为转义文本（斜体样式）。
function renderMath(tex, display) {
  if (window.katex) {
    try {
      return window.katex.renderToString(tex, { displayMode: display, throwOnError: false });
    } catch { /* 渲染失败回退 */ }
  }
  return (display ? '<div class="math-block">' : '<span class="math">') + escapeHtml(tex) + (display ? '</div>' : '</span>');
}

// 判断 LaTeX 是否只是「文献引用上标」：^1、^{1}、^[1]、^{[1,2]}、^[1-3] 等。
// OCR 常把正文里的引用上标（如 ¹ / [1]）误识别成公式。是则返回上标内容，否则 null。
function citationSuperscript(latex) {
  const t = latex.trim();
  const m = t.match(/^\^\s*\{?(\[?[0-9][0-9,\-–—\s]*\]?)\}?$/);
  if (!m) return null;
  return m[1].trim();
}

// 行内元素：先提取公式占位，转义其余文本，再按「代码 → 粗体 → 斜体 → 链接」替换，最后还原公式。
function renderInline(s) {
  const math = [];
  let t = String(s).replace(/\$([^$\n]+)\$/g, (m, tex) => {
    // 引用上标（^1 / ^{[1]} 等）恢复成 HTML 上标，而不是渲染成公式。
    const sup = citationSuperscript(tex);
    math.push(sup !== null ? `<sup>${escapeHtml(sup)}</sup>` : renderMath(tex, false));
    return '\u0000' + (math.length - 1) + '\u0000';
  });
  t = escapeHtml(t);
  t = t.replace(/`([^`]+)`/g, '<code>$1</code>');
  t = t.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  t = t.replace(/(^|[^*])\*([^*\s][^*]*)\*/g, '$1<em>$2</em>');
  t = t.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
  t = t.replace(/\u0000(\d+)\u0000/g, (m, i) => math[Number(i)]);
  return t;
}

function renderMarkdown(text) {
  if (!text) return '';
  const lines = String(text).replace(/\r\n?/g, '\n').split('\n');
  let out = '';
  let para = [];
  let inCode = false;
  let codeBuf = [];
  let listType = null;
  let inBlockMath = false;
  let mathBuf = [];

  const flushPara = () => {
    if (para.length) { out += '<p>' + para.map(renderInline).join('<br>') + '</p>'; para = []; }
  };
  const flushList = () => { if (listType) { out += '</' + listType + '>'; listType = null; } };

  for (const raw of lines) {
    const line = raw.replace(/\s+$/, '');

    if (/^\s*```/.test(line)) {
      flushPara(); flushList();
      if (inCode) {
        out += '<pre><code>' + escapeHtml(codeBuf.join('\n')) + '</code></pre>';
        codeBuf = []; inCode = false;
      } else {
        inCode = true;
      }
      continue;
    }
    if (inCode) { codeBuf.push(raw); continue; }

    // 块级公式：$$ 单独一行 → 进入/退出多行公式模式
    if (/^\s*\$\$\s*$/.test(line)) {
      flushPara(); flushList();
      if (inBlockMath) {
        out += renderMath(mathBuf.join('\n'), true);
        mathBuf = []; inBlockMath = false;
      } else {
        inBlockMath = true;
        mathBuf = [];
      }
      continue;
    }

    // 正在多行公式中：累积内容
    if (inBlockMath) { mathBuf.push(line); continue; }

    if (!line.trim()) { flushPara(); flushList(); continue; }

    // 块级公式：单行 $$...$$ 或 \[...\]
    const bm = line.match(/^\s*\$\$(.+)\$\$\s*$/) || line.match(/^\s*\\\[(.+)\\\]\s*$/);
    if (bm) { flushPara(); flushList(); out += renderMath(bm[1], true); continue; }

    // 块级公式开始：以 $$ 开头、同一行未闭合（$$ 后跟内容的多行开始）
    if (/^\s*\$\$/.test(line)) {
      flushPara(); flushList();
      inBlockMath = true;
      const rest = line.replace(/^\s*\$\$/, '').trim();
      mathBuf = rest ? [rest] : [];
      continue;
    }

    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) { flushPara(); flushList(); out += `<h${h[1].length}>${renderInline(h[2])}</h${h[1].length}>`; continue; }

    if (/^\s*([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) { flushPara(); flushList(); out += '<hr>'; continue; }

    const ul = line.match(/^\s*[-*+]\s+(.*)$/);
    const ol = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (ul || ol) {
      flushPara();
      const type = ol ? 'ol' : 'ul';
      if (listType !== type) { flushList(); out += `<${type}>`; listType = type; }
      out += `<li>${renderInline((ul || ol)[1])}</li>`;
      continue;
    }

    const q = line.match(/^\s*>\s?(.*)$/);
    if (q) { flushPara(); flushList(); out += `<blockquote>${renderInline(q[1])}</blockquote>`; continue; }

    flushList();
    para.push(line);
  }

  // 若块级公式未闭合，渲染已累积内容。
  if (inBlockMath) {
    out += renderMath(mathBuf.join('\n'), true);
  }

  if (inCode) out += '<pre><code>' + escapeHtml(codeBuf.join('\n')) + '</code></pre>';
  flushList();
  flushPara();
  return out;
}


// 创建单个目录条目按钮。
function createTocItem(it) {
  const row = document.createElement('button');
  row.type = 'button';
  row.className = 'toc-item';
  const verticalToc = state.rtl && (document.body.classList.contains('toc-top') || document.body.classList.contains('toc-double'));
  if (verticalToc) row.style.paddingTop = (12 + (it.level - 1) * 16) + 'px';
  else row.style.paddingLeft = (12 + (it.level - 1) * 16) + 'px';
  const label = document.createElement('span');
  label.className = 'toc-title';
  label.textContent = verticalToc ? toChineseDigits(it.title) : it.title;
  const pg = document.createElement('span');
  pg.className = 'toc-page';
  pg.textContent = it.pageIdx == null
    ? ''
    : (verticalToc ? toChineseDigits(it.pageIdx + 1) : String(it.pageIdx + 1));
  row.appendChild(label);
  row.appendChild(pg);
  row.addEventListener('click', () => {
    if (it.pageIdx != null) goTo(it.pageIdx + 1);
    // 竖排浮动目录：选中条目后自动收起目录。
    if (state.rtl && (document.body.classList.contains('toc-top') || document.body.classList.contains('toc-double'))) {
      const panel = $('toc-panel');
      if (!panel.classList.contains('collapsed')) panel.classList.add('collapsed');
    }
  });
  return row;
}

function renderToc(items) {
  const list = $('toc-list');
  list.innerHTML = '';
  if (!items.length) {
    const d = document.createElement('div');
    d.className = 'toc-hint';
    d.textContent = '未找到目录项。';
    list.appendChild(d);
    return;
  }
  const doubleMode = state.rtl && document.body.classList.contains('toc-double');
  if (doubleMode) {
    renderTocDouble(items);
    return;
  }
  const frag = document.createDocumentFragment();
  for (const it of items) frag.appendChild(createTocItem(it));
  list.appendChild(frag);
}

// 双栏模式：先渲染到测量页测出每页容量（flex-wrap 自动换行），再分成多页渲染。
function renderTocDouble(items) {
  const list = $('toc-list');
  list.innerHTML = '';
  const measure = document.createElement('div');
  measure.className = 'toc-sheet';
  for (const it of items) measure.appendChild(createTocItem(it));
  list.appendChild(measure);

  requestAnimationFrame(() => {
    // 按 offsetTop 分行，2 行一页。
    const rows = [];
    let cur = [];
    let prevTop = null;
    for (const el of measure.querySelectorAll('.toc-item')) {
      const top = el.offsetTop;
      if (prevTop === null || Math.abs(top - prevTop) < 8) cur.push(el);
      else { rows.push(cur); cur = [el]; }
      prevTop = top;
    }
    if (cur.length) rows.push(cur);
    const pages = [];
    for (let i = 0; i < rows.length; i += 2) pages.push(rows.slice(i, i + 2).flat());

    // 重新渲染成页。
    list.innerHTML = '';
    for (const pageEntries of pages) {
      const sheet = document.createElement('div');
      sheet.className = 'toc-sheet';
      for (const el of pageEntries) sheet.appendChild(el);
      list.appendChild(sheet);
    }
    state.tocPages = pages.length;
    state.tocPage = 0;
    updateTocPager();
    list.scrollLeft = 0;
  });
}

// 双栏翻页：index 0-based。
function goTocPage(index) {
  const n = state.tocPages || 1;
  const clamped = Math.min(Math.max(0, index), n - 1);
  state.tocPage = clamped;
  const list = $('toc-list');
  // vertical-rl 横向滚动方向与常规相反：scrollLeft=0 在最右，向左滚动为负值。
  list.scrollLeft = -clamped * list.clientWidth;
  updateTocPager();
}

function updateTocPager() {
  const n = state.tocPages || 1;
  $('toc-page-info').textContent = (state.tocPage + 1) + ' / ' + n;
  $('toc-page-prev').disabled = state.tocPage <= 0;
  $('toc-page-next').disabled = state.tocPage >= n - 1;
}

// 侧边栏展开/收起后 viewer 宽度变化，若处于「适应宽度」则重排页面。
function refitAfterResize() {
  setTimeout(() => {
    if (state.pdfDoc && state.fitScale) fitWidth();
  }, 220);
}

async function openToc() {
  if (!state.pdfDoc) return;
  const panel = $('toc-panel');
  panel.classList.remove('collapsed');
  refitAfterResize(); // 展开后 viewer 变窄，重排（独立于目录加载）
  if (!state.tocItems) {
    const list = $('toc-list');
    list.innerHTML = '<div class="toc-hint">正在读取目录…</div>';
    const outline = await getOutline();
    if (outline) {
      state.tocItems = outline;
      $('toc-generate').style.display = 'none';
      $('toc-hint').style.display = 'none';
      renderToc(outline);
    } else {
      $('toc-generate').style.display = '';
      $('toc-hint').style.display = '';
      list.innerHTML = '<div class="toc-hint">本文档没有书签目录。</div>';
    }
  }
}

function closeToc() {
  $('toc-panel').classList.add('collapsed');
  refitAfterResize();
}

function toggleToc() {
  if (!state.pdfDoc) return;
  if ($('toc-panel').classList.contains('collapsed')) openToc();
  else closeToc();
}

async function translate() {
  const text = getSelectedText() || state.extractedText;
  if (!text) { setStatus('请先打开 PDF、提取文字，或选中一段文字'); return; }
  showSidebar();
  setStatus('模型思考中…');
  $('translation').classList.add('markdown');
  $('translation').classList.remove('muted');
  $('btn-copy').disabled = true;

  // 流式接收增量译文，实时刷新显示（边生成边出字，观感更快）。
  let acc = '';
  let firstChunk = false;
  const listen = window.__TAURI__?.event?.listen;
  const unlisten = listen
    ? await listen('translate-chunk', (e) => {
        if (!firstChunk) {
          firstChunk = true;
          setStatus('正在生成译文…');
        }
        acc += e.payload || '';
        $('translation').innerHTML = renderMarkdown(acc);
        $('translation').scrollTop = $('translation').scrollHeight;
      })
    : null;

  try {
    const cfg = await invoke('get_config');
    const result = await invoke('translate_stream', {
      request: {
        text,
        source_lang: 'auto',
        target_lang: cfg.translate.target_lang || '中文',
      },
    });
    const finalText = result.translated_text || acc;
    $('translation').innerHTML = renderMarkdown(finalText);
    $('translation').classList.remove('muted');
    $('btn-copy').disabled = false;
    setStatus('翻译完成');
  } catch (err) {
    setStatus('翻译失败：' + err);
  } finally {
    if (unlisten) unlisten();
  }
}

async function copyTranslation() {
  const text = $('translation').textContent;
  try {
    await navigator.clipboard.writeText(text);
    setStatus('已复制到剪贴板');
  } catch {
    setStatus('复制失败，请手动选择：' + text);
  }
}

// 对话框可拖动：按住标题栏拖动整个 <dialog>。原生 modal 靠 inset+margin:auto 居中，
// 首次拖动时切换为显式 left/top 定位；拖动中限制在视口内，避免拖出屏幕无法找回。
// 关闭时复位定位，下次打开重新居中。
function makeDialogDraggable(dialog, handle) {
  let drag = null;
  handle.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    const rect = dialog.getBoundingClientRect();
    dialog.style.margin = '0';
    dialog.style.right = 'auto';
    dialog.style.bottom = 'auto';
    dialog.style.left = rect.left + 'px';
    dialog.style.top = rect.top + 'px';
    drag = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
    try { handle.setPointerCapture(e.pointerId); } catch { /* 捕获失败不阻塞拖动 */ }
  });
  handle.addEventListener('pointermove', (e) => {
    if (!drag) return;
    e.preventDefault();
    const rect = dialog.getBoundingClientRect();
    const maxX = Math.max(0, window.innerWidth - rect.width);
    const maxY = Math.max(0, window.innerHeight - rect.height);
    dialog.style.left = Math.max(0, Math.min(e.clientX - drag.dx, maxX)) + 'px';
    dialog.style.top = Math.max(0, Math.min(e.clientY - drag.dy, maxY)) + 'px';
  });
  const end = () => { drag = null; };
  handle.addEventListener('pointerup', end);
  handle.addEventListener('pointercancel', end);
  // 关闭后清掉显式定位，下次 showModal 重新居中。
  dialog.addEventListener('close', () => {
    dialog.style.left = dialog.style.top = dialog.style.right = dialog.style.bottom = '';
    dialog.style.margin = '';
  });
}

async function openSettings() {
  const cfg = await invoke('get_config');
  $('cfg-base-url').value = cfg.translate.base_url;
  $('cfg-api-key').value = cfg.translate.api_key;
  $('cfg-model').value = cfg.translate.model;
  $('cfg-target-lang').value = cfg.translate.target_lang;
  $('cfg-ocr-mode').value = cfg.ocr.mode || 'local';
  $('cfg-ocr-model').value = cfg.ocr.model || '';
  $('cfg-ocr-detail').value = cfg.ocr.detail || 'auto';
  $('cfg-ocr-glm-key').value = cfg.ocr.glm_api_key || '';
  $('cfg-ocr-lang').value = cfg.ocr.lang || 'auto';
  $('cfg-ocr-vertical').checked = !!cfg.ocr.vertical;
  $('cfg-viewer-rtl').checked = !!(cfg.viewer && cfg.viewer.rtl);
  $('cfg-toc-position').value = (cfg.viewer && cfg.viewer.toc_position) || 'top';
  await refreshLicenseStatus(); // 刷新 Pro 状态显示
  switchSettingsTab('model'); // 每次打开回到「模型配置」
  $('settings').showModal();
}

async function saveConfig() {
  try {
    await invoke('set_config', {
      config: {
        translate: {
          provider: 'openai-compatible',
          base_url: $('cfg-base-url').value.trim(),
          api_key: $('cfg-api-key').value.trim(),
          model: $('cfg-model').value.trim(),
          source_lang: 'auto',
          target_lang: $('cfg-target-lang').value.trim() || '中文',
        },
        ocr: {
          mode: $('cfg-ocr-mode').value || 'local',
          model: $('cfg-ocr-model').value.trim(),
          detail: $('cfg-ocr-detail').value || 'auto',
          glm_api_key: $('cfg-ocr-glm-key').value.trim(),
          lang: $('cfg-ocr-lang').value || 'auto',
          vertical: $('cfg-ocr-vertical').checked,
        },
        viewer: {
          rtl: $('cfg-viewer-rtl').checked,
          toc_position: $('cfg-toc-position').value || 'top',
        },
      },
    });
    $('settings').close();
    await applyTocPosition(); // 目录位置可能变化，更新 body class
    // 若已打开文档，按新的竖排古籍设置即时重排并回到当前页。
    if (state.pdfDoc) {
      const page = state.pageNum;
      state.rtl = await loadRtlSetting();
      $('pdf-pages').classList.toggle('rtl', state.rtl);
      document.body.classList.toggle('rtl', state.rtl);
      $('btn-fit').title = state.rtl ? '适应高度' : '适应宽度';
      // 目录里数字是否转汉字随排版方向变化，已加载目录则重渲染。
      if (state.tocItems) renderToc(state.tocItems);
      await rebuildPages();
      const wrap = state.pageEls[page - 1];
      if (wrap) {
        if (state.rtl) wrap.scrollIntoView({ inline: 'end', block: 'nearest', behavior: 'auto' });
        else wrap.scrollIntoView({ block: 'start', behavior: 'auto' });
      }
      updatePageIndicator(page);
    }
    setStatus('设置已保存');
  } catch (err) {
    setStatus('保存失败：' + err);
  }
}

// 悬浮翻译栏是否生效：竖排 + 顶部/双栏目录（目录与翻译栏均为浮层）；左侧目录与横排保持原布局。
function isFloatingSidebar() {
  return state.rtl && !document.body.classList.contains('toc-left');
}

// 固定翻译栏：固定后点击 PDF 画布不再收起。
function setSidebarPinned(v) {
  state.sidebarPinned = v;
  $('sidebar').classList.toggle('pinned', v);
  $('btn-sidebar-pin').classList.toggle('active', v);
}
function toggleSidebarPin() {
  setSidebarPinned(!state.sidebarPinned);
  if (state.sidebarPinned) {
    $('sidebar').classList.remove('collapsed');
    document.body.classList.add('sidebar-open');
  }
}
function collapseSidebar() {
  setSidebarPinned(false);
  $('sidebar').classList.add('collapsed');
  document.body.classList.remove('sidebar-open');
}
// 打开新文档时清理悬浮翻译栏的临时状态（固定/展开），回到干净的初始态。
function resetSidebarLayout() {
  setSidebarPinned(false);
  document.body.classList.remove('sidebar-open');
  if (isFloatingSidebar()) $('sidebar').classList.add('collapsed');
}

// 阅读模式：隐藏翻译面板，画布占满剩余宽度（重新适应宽度会保留当前页）。
// 悬浮翻译栏不参与布局，阅读模式仅收起浮层、退出时重新展开，无需重排画布。
function toggleReadingMode() {
  const on = document.body.classList.toggle('reading');
  $('btn-reading').classList.toggle('active', on);
  if (isFloatingSidebar()) {
    if (on) collapseSidebar();
    else showSidebar(); // 退出阅读模式：重新展开翻译栏
  } else if (state.pdfDoc) {
    if (on) {
      fitWidth();
    } else if ($('sidebar').classList.contains('collapsed')) {
      // 翻译栏原本是收起的：展开它会有宽度过渡，等过渡结束再重排画布。
      showSidebar();
      refitAfterResize();
    } else {
      // 翻译栏本就展开：display:none 撤销后立即可见，直接重排。
      fitWidth();
    }
  }
}

// 展开翻译栏（点击「提取文字」/「翻译」时自动弹出），并退出阅读模式。
// 悬浮态浮在 PDF 上展开、不改画布布局；横排/左侧目录则取消 reading 的隐藏。
function showSidebar() {
  $('sidebar').classList.remove('collapsed');
  document.body.classList.remove('reading');
  $('btn-reading').classList.remove('active');
  if (isFloatingSidebar()) {
    document.body.classList.add('sidebar-open');
  }
}

// ===== 书架首页（书架 / 最近阅读 / 收藏） =====

let currentFolder = null;   // null = 书架根；否则为当前浏览的文件夹绝对路径
let favSet = new Set();     // 当前收藏路径集合（渲染时同步，供星标高亮）

// FNV-1a 32 位哈希，作为封面缩略图的磁盘缓存键（十六进制）。
function fnv1a(str) {
  let h = 0x811c9dc5;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h.toString(16);
}

// 从路径取展示名（文件去 .pdf、目录取目录名）。
function nameFromPath(path) {
  const base = String(path).split(/[\\/]/).pop() || path;
  return base.replace(/\.pdf$/i, '');
}

function showShelf() {
  document.body.classList.add('shelf-mode');
  const b = $('bookshelf');
  if (b) { b.classList.remove('view-in'); void b.offsetWidth; b.classList.add('view-in'); }
  renderShelf();
}
function showViewer() {
  document.body.classList.remove('shelf-mode');
  const m = document.querySelector('main');
  if (m) { m.classList.remove('view-in'); void m.offsetWidth; m.classList.add('view-in'); }
  thumbQueue.length = 0; // 丢弃待生成封面（卡片即将重建），避免与正文加载抢资源
}

// 书架顶部的临时提示（工具栏状态栏在书架模式下隐藏，错误须在此显示）。
function shelfStatus(msg) {
  const el = $('shelf-status');
  if (!el) return;
  el.textContent = msg || '';
  if (msg) {
    clearTimeout(shelfStatus._t);
    shelfStatus._t = setTimeout(() => { el.textContent = ''; }, 4000);
  }
}

// 渲染整个书架：书架区（根或文件夹内）+ 最近阅读 + 收藏。
async function renderShelf() {
  try {
    const lib = await invoke('get_library');
    favSet = new Set(lib.favorites || []);

    const inFolder = !!currentFolder;
    let shelfEntries = lib.books || [];
    if (inFolder) {
      const fc = await invoke('list_folder', { path: currentFolder });
      shelfEntries = (fc && fc.entries) || [];
    }
    renderGrid('shelf-grid', shelfEntries, { showStar: true, showRemove: !inFolder });
    $('shelf-empty').textContent = inFolder
      ? '此文件夹内没有 PDF 或子文件夹。'
      : '书架还是空的，点击「添加书籍」或「添加文件夹」开始整理。';
    $('shelf-empty').classList.toggle('hidden', shelfEntries.length > 0);
    renderBreadcrumb();

    const recent = lib.recent || [];
    renderGrid('recent-grid', recent.map((r) => ({ path: r.path, name: r.name, is_folder: false })), { showStar: true });
    $('recent-section').classList.toggle('hidden', recent.length === 0);

    const favs = (lib.favorites || []).map((path) => ({ path, name: nameFromPath(path), is_folder: false }));
    renderGrid('fav-grid', favs, { showStar: true });
    $('fav-section').classList.toggle('hidden', favs.length === 0);
  } catch (err) {
    $('shelf-empty').classList.remove('hidden');
    $('shelf-empty').textContent = '读取书架失败：' + err;
  }
}

function renderGrid(id, entries, opts) {
  const grid = $(id);
  grid.innerHTML = '';
  const frag = document.createDocumentFragment();
  for (const e of entries || []) frag.appendChild(renderBookCard(e, opts));
  grid.appendChild(frag);
}

function renderBookCard(entry, opts) {
  opts = opts || {};
  const card = document.createElement('div');
  card.className = 'shelf-card';
  card.dataset.path = entry.path;

  const cover = document.createElement('div');
  cover.className = 'cover';
  const nameEl = document.createElement('div');
  nameEl.className = 'name';
  nameEl.textContent = entry.name || '';
  nameEl.title = entry.name || '';
  card.appendChild(cover);
  card.appendChild(nameEl);

  if (entry.is_folder) {
    cover.classList.add('cover-folder');
    cover.innerHTML = '<svg class="icon folder-icon"><use href="#icon-folder"/></svg>';
    card.addEventListener('click', () => enterFolder(entry.path));
  } else {
    cover.classList.add('cover-book');
    cover.dataset.path = entry.path;
    thumbObserver.observe(cover);
    card.addEventListener('click', () => openBook(entry.path, entry.name));
  }

  if (opts.showStar && !entry.is_folder) {
    const star = document.createElement('button');
    star.className = 'fav-star';
    star.title = '收藏';
    star.innerHTML = '<svg class="icon"><use href="#icon-star"/></svg>';
    star.classList.toggle('on', favSet.has(entry.path));
    star.addEventListener('click', (e) => { e.stopPropagation(); toggleFavorite(entry.path); });
    card.appendChild(star);
  }

  if (opts.showRemove) {
    const rm = document.createElement('button');
    rm.className = 'shelf-remove';
    rm.title = '从书架移除';
    rm.innerHTML = '<svg class="icon"><use href="#icon-close"/></svg>';
    rm.addEventListener('click', (e) => { e.stopPropagation(); removeBook(entry.path); });
    card.appendChild(rm);
  }

  return card;
}

function renderBreadcrumb() {
  const bc = $('shelf-breadcrumb');
  bc.innerHTML = '';
  const home = document.createElement('button');
  home.className = 'crumb';
  home.textContent = '书架';
  home.addEventListener('click', () => { currentFolder = null; renderShelf(); });
  bc.appendChild(home);
  if (currentFolder) {
    const sep = document.createElement('span');
    sep.className = 'crumb-sep';
    sep.textContent = '/';
    bc.appendChild(sep);
    const cur = document.createElement('span');
    cur.className = 'crumb-cur';
    cur.textContent = nameFromPath(currentFolder);
    cur.title = currentFolder;
    bc.appendChild(cur);
  }
}

function enterFolder(path) {
  currentFolder = path;
  renderShelf();
}

async function addBooks() {
  try {
    const lib = await invoke('add_books_dialog');
    renderShelf();
    // 后台预识别新加入书籍的排版方向（队列会跳过已缓存的）。
    enqueueOrientation((lib.books || []).filter((b) => !b.is_folder).map((b) => b.path));
  } catch (err) { shelfStatus('添加失败：' + err); }
}
async function addFolder() {
  try {
    const before = await invoke('get_library');
    const beforeFolders = new Set((before.books || []).filter((b) => b.is_folder).map((b) => b.path));
    const lib = await invoke('add_folder_dialog');
    renderShelf();
    // 只扫描本次新加入的文件夹：递归列出其下所有 PDF，后台预识别排版方向并缓存。
    for (const b of (lib.books || [])) {
      if (!b.is_folder || beforeFolders.has(b.path)) continue;
      const pdfs = await invoke('list_pdfs_recursive', { dir: b.path }).catch(() => []);
      enqueueOrientation(pdfs);
    }
  } catch (err) { shelfStatus('添加失败：' + err); }
}
async function removeBook(path) {
  try { await invoke('remove_book', { path }); renderShelf(); }
  catch (err) { shelfStatus('移除失败：' + err); }
}
async function toggleFavorite(path) {
  try { await invoke('toggle_favorite', { path }); renderShelf(); }
  catch (err) { shelfStatus('收藏失败：' + err); }
}

// ===== 排版方向后台预识别 =====
// 导入书籍/文件夹后，在后台串行识别其中未缓存的 PDF 排版方向并写回缓存，
// 这样之后打开这些书就无需再现场检测（检测需读整本 + 采样渲染多页，较慢）。
const orientQueue = [];
let orientBusy = false;

function enqueueOrientation(paths) {
  for (const p of paths || []) orientQueue.push(p);
  pumpOrientationQueue();
}

async function pumpOrientationQueue() {
  if (orientBusy) return;
  orientBusy = true;
  try {
    while (orientQueue.length) {
      const path = orientQueue.shift();
      try {
        await detectAndCacheOrientation(path);
      } catch { /* 单本失败不阻塞队列 */ }
      // 让出主线程，避免连续读大文件 + 渲染把 UI 卡死。
      await new Promise((r) => setTimeout(r, 40));
    }
  } finally {
    orientBusy = false;
  }
}

async function detectAndCacheOrientation(path) {
  const cached = await invoke('get_orientation', { path }).catch(() => null);
  if (cached != null) return; // 已缓存（含 0/-1），跳过
  const bytes = await invoke('read_pdf', { path });
  const doc = await pdfjsLib.getDocument({ data: new Uint8Array(bytes) }).promise;
  try {
    let d = await detectVertical(doc);
    if (d === null) d = await detectVerticalScanned(doc);
    const code = d === true ? ORIENT_VERTICAL : d === false ? ORIENT_HORIZONTAL : ORIENT_UNKNOWN;
    await invoke('set_orientation', { path, code }).catch(() => {});
  } finally {
    try { await doc.destroy(); } catch { /* ignore */ }
  }
}

// 从书架点开一本书：校验 + 触发扫描件归一化 → 加载 → 进入阅读器并记录最近阅读。
async function openBook(path, name) {
  thumbQueue.length = 0; // 先停掉封面生成，避免与即将加载的大文件抢磁盘/内存
  try {
    await invoke('open_pdf_path', { path });
    await loadPdfDocument(path, name);
    invoke('record_recent', { path, name }).catch(() => {});
  } catch (err) {
    reportOpenError('打开失败：' + err);
  }
}

// 封面缩略图：进入视口时先查磁盘缓存，未命中则渲染第一页并写回缓存。
// 大书读整本 + 解析很重：多本大书同时进视口若并发生成，会把磁盘/内存/线程池打满导致卡死，
// 因此统一入队串行生成（一次只处理一本）。
const thumbObserver = new IntersectionObserver(
  (entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      thumbObserver.unobserve(e.target);
      enqueueThumb(e.target);
    }
  },
  { rootMargin: '200px' },
);

const thumbQueue = [];
let thumbBusy = false;

function enqueueThumb(cover) {
  thumbQueue.push(cover);
  pumpThumbQueue();
}

async function pumpThumbQueue() {
  if (thumbBusy) return;
  thumbBusy = true;
  try {
    while (thumbQueue.length) {
      await renderThumb(thumbQueue.shift());
    }
  } finally {
    thumbBusy = false;
  }
}

async function renderThumb(cover) {
  const path = cover.dataset.path;
  const key = fnv1a(path);
  try {
    let dataUrl = await invoke('load_thumb', { key });
    if (!dataUrl) {
      dataUrl = await buildThumb(path);
      if (dataUrl) {
        try { await invoke('save_thumb', { key, pngDataUrl: dataUrl }); } catch { /* 缓存失败不阻塞显示 */ }
      }
    }
    if (dataUrl) {
      const img = document.createElement('img');
      img.className = 'cover-img';
      img.alt = '';
      img.src = dataUrl;
      cover.appendChild(img);
    } else {
      cover.classList.add('cover-error');
    }
  } catch {
    cover.classList.add('cover-error');
  }
}

// PDF.js 按需拉字节的传输层：只为封面拉取第 1 页所需的一小段字节，
// 避免像整本 `read_pdf` 那样把 90MB+ 的大文件读进内存、传过 IPC、再整体解析。
class TauriRangeTransport extends pdfjsLib.PDFDataRangeTransport {
  constructor(path, length) {
    super(length, null);
    this.path = path;
  }
  requestDataRange(begin, end) {
    invoke('read_pdf_range', { path: this.path, begin, end })
      .then((buf) => this.onDataRange(begin, new Uint8Array(buf)))
      .catch((err) => console.error('封面字节区间读取失败：', err));
  }
}

// 渲染 PDF 第一页为封面缩略图（PNG data URL），失败返回 null。
async function buildThumb(path) {
  const size = await invoke('pdf_file_size', { path });
  const transport = new TauriRangeTransport(path, size);
  const task = pdfjsLib.getDocument({ range: transport, disableAutoFetch: true, disableStream: true });
  let doc = null;
  let timer = null;
  try {
    // 加超时兜底：万一区间读取挂起（如文件被删），不能让串行队列整体卡住。
    doc = await Promise.race([
      task.promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => {
          task.destroy().catch(() => {});
          reject(new Error('封面生成超时'));
        }, 30000);
      }),
    ]);
    const page = await doc.getPage(1);
    const vp1 = page.getViewport({ scale: 1 });
    const scale = Math.min(220 / vp1.width, 293 / vp1.height);
    const vp = page.getViewport({ scale });
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.ceil(vp.width));
    canvas.height = Math.max(1, Math.ceil(vp.height));
    const ctx = canvas.getContext('2d');
    ctx.fillStyle = '#fff';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    await page.render({ canvasContext: ctx, viewport: vp }).promise;
    return canvas.toDataURL('image/png');
  } catch {
    return null;
  } finally {
    if (timer) clearTimeout(timer);
    if (doc) { try { await doc.destroy(); } catch { /* ignore */ } }
  }
}

function bindEvents() {
  $('btn-shelf').addEventListener('click', showShelf);
  $('btn-add-books').addEventListener('click', addBooks);
  $('btn-add-folder').addEventListener('click', addFolder);
  $('btn-open-pdf').addEventListener('click', openPdf);
  $('btn-open').addEventListener('click', openPdf);
  $('btn-prev').addEventListener('click', () => goTo(state.pageNum - 1));
  $('btn-next').addEventListener('click', () => goTo(state.pageNum + 1));
  $('page-input').addEventListener('change', (e) => goTo(Number(e.target.value)));

  // 记住鼠标在阅读区内最后停留的位置，让「放大/缩小」按钮也以该点为中心缩放（与 Ctrl+滚轮一致），
  // 而不是总以视口中心缩放——鼠标指向的内容尽量保持不变。
  let viewPointer = { x: null, y: null };
  $('viewer').addEventListener('pointermove', (e) => {
    viewPointer.x = e.clientX;
    viewPointer.y = e.clientY;
  });

  $('btn-zoom-out').addEventListener('click', () => zoom(1 / 1.25, viewPointer.x, viewPointer.y));
  $('btn-zoom-in').addEventListener('click', () => zoom(1.25, viewPointer.x, viewPointer.y));
  $('btn-fit').addEventListener('click', fitWidth);
  $('btn-extract').addEventListener('click', extractText);
  $('btn-extract-pages').addEventListener('click', openExtractPages);
  $('btn-export').addEventListener('click', exportText);
  $('btn-export-word').addEventListener('click', exportWord);
  $('btn-export-word-tr').addEventListener('click', exportWordTranslated);
  $('btn-translate').addEventListener('click', translate);
  $('btn-copy').addEventListener('click', copyTranslation);
  $('btn-settings').addEventListener('click', openSettings);
  $('btn-shelf-settings').addEventListener('click', openSettings);
  $('btn-reading').addEventListener('click', toggleReadingMode);
  $('btn-toc').addEventListener('click', toggleToc);
  $('toc-generate').addEventListener('click', generateToc);
  $('btn-toc-collapse').addEventListener('click', closeToc);
  $('btn-sidebar-pin').addEventListener('click', toggleSidebarPin);
  $('btn-sidebar-close').addEventListener('click', collapseSidebar);
  $('settings-form').addEventListener('submit', (e) => { e.preventDefault(); saveConfig(); });
  $('btn-cancel-cfg').addEventListener('click', () => $('settings').close());
  makeDialogDraggable($('settings'), $('settings').querySelector('h2.drag-handle'));
  document.querySelectorAll('.settings-tab').forEach((btn) => {
    btn.addEventListener('click', () => switchSettingsTab(btn.dataset.tab));
  });
  $('extract-pages-form').addEventListener('submit', (e) => { e.preventDefault(); exportPagesToPdf(); });
  $('btn-cancel-extract').addEventListener('click', closeExtractPages);
  $('activate-form').addEventListener('submit', (e) => { e.preventDefault(); submitActivate(); });
  $('btn-cancel-activate').addEventListener('click', () => $('activate').close());
  $('btn-activate').addEventListener('click', async () => {
    const code = $('license-code').value.trim();
    if (!code) { setStatus('请输入激活码'); return; }
    await activateLicense(code);
  });
  $('btn-manual').addEventListener('click', openManual);

  // Ctrl + 滚轮缩放（passive: false 才能 preventDefault 阻止页面滚动）。
  // 用 rAF 把快速连续滚动合并成一次缩放，并记录鼠标位置以便缩放时跟随鼠标。
  let zoomScheduled = false;
  let zoomAccum = 1;
  let zoomCursorX = 0;
  let zoomCursorY = 0;
  $('viewer').addEventListener('wheel', (e) => {
    if (!state.pdfDoc || !e.ctrlKey) return;
    e.preventDefault();
    zoomAccum *= (e.deltaY < 0 ? 1.1 : 1 / 1.1);
    zoomCursorX = e.clientX;
    zoomCursorY = e.clientY;
    if (zoomScheduled) return;
    zoomScheduled = true;
    requestAnimationFrame(() => {
      zoomScheduled = false;
      const f = zoomAccum;
      const x = zoomCursorX;
      const y = zoomCursorY;
      zoomAccum = 1;
      zoom(f, x, y);
    });
  }, { passive: false });

  // 竖排古籍（RTL）模式：纵向滚轮 → 页面左右移动（横向连续阅读）。
  // 触控板横向手势仍交给原生横向滚动；滚轮向下 = 向前读（往左移）。
  // 按住 Shift 时滚轮改为上下移动页面（放大后页面纵向溢出时用）。
  $('viewer').addEventListener('wheel', (e) => {
    if (!state.rtl || e.ctrlKey || e.metaKey) return;
    if (e.shiftKey) {
      e.preventDefault();
      const dy = Math.abs(e.deltaY) > Math.abs(e.deltaX) ? e.deltaY : e.deltaX;
      $('viewer').scrollTop -= dy;
      return;
    }
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) return; // 原生横向滚动处理
    e.preventDefault();
    nudgeRtlScroll(e.deltaY);
  }, { passive: false });

  // 目录悬浮层滚轮：顶部竖排时横向移动条目；双栏时切换页面；左侧普通列表走原生纵向滚动。
  let tocWheelAccum = 0;
  let tocWheelTimer = null;
  $('toc-panel').addEventListener('wheel', (e) => {
    if (!state.rtl || e.ctrlKey || e.metaKey) return;
    if (document.body.classList.contains('toc-double')) {
      e.preventDefault();
      tocWheelAccum += e.deltaY;
      if (Math.abs(tocWheelAccum) > 80) {
        const dir = tocWheelAccum > 0 ? 1 : -1;
        tocWheelAccum = 0;
        goTocPage(state.tocPage + dir);
      }
      clearTimeout(tocWheelTimer);
      tocWheelTimer = setTimeout(() => { tocWheelAccum = 0; }, 200);
    } else if (document.body.classList.contains('toc-top')) {
      e.preventDefault();
      $('toc-list').scrollLeft -= e.deltaY;
    }
  }, { passive: false });

  // 双栏翻页按钮。
  $('toc-page-prev').addEventListener('click', () => goTocPage(state.tocPage - 1));
  $('toc-page-next').addEventListener('click', () => goTocPage(state.tocPage + 1));

  let resizeTimer = null;
  window.addEventListener('resize', () => {
    if (!state.pdfDoc || !state.fitScale) return;
    clearTimeout(resizeTimer);
    // 合并连续 resize 事件，等布局稳定后再重排，避免瞬时 clientWidth≈0 导致页面缩成一条。
    resizeTimer = setTimeout(() => {
      fitWidth();
      // 双栏目录：窗口尺寸变化后重新分页（回到第一页）。
      if (state.rtl && document.body.classList.contains('toc-double') && state.tocItems) {
        renderToc(state.tocItems);
      }
    }, 160);
  });

  // 滚动时更新「当前页」指示（rAF 节流），并标记滚动中（低清渲染）。
  let scrollScheduled = false;
  $('viewer').addEventListener('scroll', () => {
    if (!state.pdfDoc) return;
    markScrollActivity();
    if (scrollScheduled) return;
    scrollScheduled = true;
    requestAnimationFrame(() => {
      scrollScheduled = false;
      updatePageIndicator();
    });
  });

  // 目录高度拖拽调节（顶部位置）：底部把手拖动改高度，并本地记忆。
  const tocResize = document.createElement('div');
  tocResize.className = 'toc-resize';
  $('toc-panel').appendChild(tocResize);
  let resizingToc = false;
  tocResize.addEventListener('pointerdown', (e) => {
    if (!document.body.classList.contains('toc-top')) return;
    resizingToc = true;
    try { tocResize.setPointerCapture(e.pointerId); } catch { /* ignore */ }
    e.preventDefault();
  });
  tocResize.addEventListener('pointermove', (e) => {
    if (!resizingToc) return;
    const panel = $('toc-panel');
    const top = panel.getBoundingClientRect().top;
    const h = Math.min(Math.max(e.clientY - top, 120), 640);
    panel.style.setProperty('--toc-height', h + 'px');
  });
  function endTocResize() {
    if (!resizingToc) return;
    resizingToc = false;
    const panel = $('toc-panel');
    const v = panel.style.getPropertyValue('--toc-height');
    const h = parseFloat(v) || 0;
    if (h > 0) saveTocHeight(h);
  }
  tocResize.addEventListener('pointerup', endTocResize);
  tocResize.addEventListener('pointercancel', endTocResize);

  // 点击 PDF 页面收起目录（悬浮层）；刚拖拽平移过则不触发，避免平移结束误收起。
  let suppressTocClick = false;
  $('viewer').addEventListener('click', () => {
    if (suppressTocClick) { suppressTocClick = false; return; }
    if (!state.rtl) return;
    const panel = $('toc-panel');
    if (!panel.classList.contains('collapsed')) panel.classList.add('collapsed');
    // 悬浮翻译栏：点击 PDF 画布收起（固定时除外）。
    if (isFloatingSidebar() && !state.sidebarPinned) collapseSidebar();
  });
  // 点击目录面板空白处（含右侧「目录」栏空白，但非条目/标签/收起按钮/翻页器/拖拽把手）收起目录。
  $('toc-panel').addEventListener('click', (e) => {
    if (!state.rtl) return;
    if (e.target.closest('.toc-item') || e.target.closest('.h3-label')
      || e.target.closest('#btn-toc-collapse') || e.target.closest('.toc-pager')
      || e.target.closest('.toc-resize')) return;
    const panel = $('toc-panel');
    if (!panel.classList.contains('collapsed')) panel.classList.add('collapsed');
  });

  // 拖拽平移页面：按下后一旦拖动立即平移，无需长按等待。
  // 单击 / 双击不进入平移，保留原生文本选择（仍可复制正文）；只有真正拖动时才切换为平移，
  // 此时清除选区并捕获指针，避免原生选择 / 边缘自动滚动与平移打架。
  let pan = null; // { id, sx, sy, moved }
  const viewerEl = $('viewer');
  viewerEl.addEventListener('pointerdown', (e) => {
    if (!state.pdfDoc || e.button !== 0) return;
    if (!(e.target && e.target.closest && e.target.closest('.pdf-page'))) return;
    cancelRtlScroll(); // 拖拽平移接管滚动，停掉滚轮缓动
    cancelPageJump();
    pan = { id: e.pointerId, sx: e.clientX, sy: e.clientY, moved: false };
  });
  viewerEl.addEventListener('pointermove', (e) => {
    if (!pan || e.pointerId !== pan.id) return;
    if (!pan.moved) {
      // 第一次拖动即进入平移：取消可能已开始的选区并捕获指针。
      pan.moved = true;
      suppressTocClick = true; // 拖拽平移后不收起目录
      document.body.classList.add('is-panning');
      const sel = window.getSelection();
      if (sel && sel.removeAllRanges) sel.removeAllRanges();
      try { viewerEl.setPointerCapture(pan.id); } catch { /* 捕获失败不阻塞平移 */ }
    }
    e.preventDefault(); // 阻止原生选择/自动滚动，平移由本处理器接管
    const dx = e.clientX - pan.sx;
    const dy = e.clientY - pan.sy;
    viewerEl.scrollLeft -= dx;
    viewerEl.scrollTop -= dy;
    pan.sx = e.clientX;
    pan.sy = e.clientY;
  });
  function endPan(e) {
    if (!pan) return;
    if (e && e.pointerId != null && e.pointerId !== pan.id) return;
    document.body.classList.remove('is-panning');
    if (pan.moved) { try { viewerEl.releasePointerCapture(pan.id); } catch { /* ignore */ } }
    pan = null;
  }
  viewerEl.addEventListener('pointerup', endPan);
  viewerEl.addEventListener('pointercancel', endPan);
  window.addEventListener('blur', () => endPan(null));
}

// Material 风格：按钮点击水波纹（点击点向外扩散的圆形涟漪）。
// 用事件委托统一处理，动态生成的按钮（如目录项）也能获得涟漪。
function initMaterialRipple() {
  document.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    const btn = e.target && e.target.closest ? e.target.closest('button') : null;
    if (!btn || btn.disabled) return;
    const rect = btn.getBoundingClientRect();
    const size = Math.max(rect.width, rect.height);
    const span = document.createElement('span');
    span.className = 'ripple';
    span.style.width = span.style.height = size + 'px';
    span.style.left = (e.clientX - rect.left - size / 2) + 'px';
    span.style.top = (e.clientY - rect.top - size / 2) + 'px';
    btn.appendChild(span);
    span.addEventListener('animationend', () => span.remove());
    setTimeout(() => span.remove(), 600); // 兜底：动画被中断时清理残留
  });
}

// ===== 工具栏溢出收纳：窄屏时把不常用按钮依次收进「更多」下拉，避免菜单栏换行 =====
// 目录（btn-toc）固定显示、永不收纳。收纳分三阶段：
//   1) 空间不足时，导出 Word / 导出译文 先收起文字、只留图标；
//   2) 仍放不下时，逐个收进提取/翻译/缩放等图标按钮；
//   3) 仍放不下时，两个导出按钮成对收进「更多」下拉。
function initToolbarOverflow() {
  const toolbar = document.querySelector('.toolbar');
  const menu = $('overflow-menu');
  const moreWrap = $('more-wrap');
  const btnMore = $('btn-more');
  const collapseOrder = [
    'btn-extract', 'btn-extract-pages', 'btn-translate', 'btn-export',
    'btn-zoom-out', 'btn-zoom-in', 'btn-fit',
  ];
  const exportIds = ['btn-export-word', 'btn-export-word-tr'];
  const items = [...collapseOrder, ...exportIds].map((id) => $(id)).filter(Boolean);
  const collapseBtns = collapseOrder.map((id) => $(id)).filter(Boolean);
  const exportBtns = exportIds.map((id) => $(id)).filter(Boolean);
  const home = new Map(); // 按钮 → 原 .tool-group

  // 每个候选按钮补一个文字标签：工具栏里隐藏（仍只显示图标），收进下拉时才显示。
  for (const btn of items) {
    home.set(btn, btn.parentElement);
    const label = document.createElement('span');
    label.className = 'overflow-label';
    label.textContent = btn.getAttribute('title') || btn.textContent.trim();
    btn.appendChild(label);
  }

  // 工具栏内容是否超出：取最右子元素右缘，与内容区右界比较。
  function overflowing() {
    const rects = [...toolbar.children].map((c) => c.getBoundingClientRect());
    if (!rects.length) return false;
    const right = Math.max(...rects.map((r) => r.right));
    const tb = toolbar.getBoundingClientRect();
    const padR = parseFloat(getComputedStyle(toolbar).paddingRight) || 0;
    return right > tb.right - padR + 1;
  }

  function relayout() {
    // 先清掉上一轮的隐藏标记，保证测量时所有按钮都在布局里。
    toolbar.querySelectorAll('.tool-group, .divider').forEach((el) => el.classList.remove('hidden'));
    moreWrap.classList.remove('hidden');
    toolbar.classList.remove('compact');
    // 还原所有已收纳按钮（逆序 prepend 保持原顺序）。
    const moved = [...menu.querySelectorAll('button')];
    for (let i = moved.length - 1; i >= 0; i--) {
      const btn = moved[i];
      const parent = home.get(btn);
      if (parent) parent.prepend(btn);
    }
    // 阶段 1：空间不足，导出按钮先收起文字、只留图标。
    if (overflowing()) toolbar.classList.add('compact');
    // 阶段 2：仍放不下，逐个收纳非导出按钮。
    for (let guard = 0; guard < collapseBtns.length && overflowing(); guard++) {
      const next = collapseBtns.find((b) => b.parentElement !== menu);
      if (!next) break;
      menu.appendChild(next);
    }
    // 阶段 3：仍放不下，两个导出按钮成对收进下拉。
    if (overflowing()) {
      for (const b of exportBtns) {
        if (b.parentElement !== menu) menu.appendChild(b);
      }
    }
    // 无收纳项时隐藏「更多」；空分组及其相邻分隔线隐藏。
    moreWrap.classList.toggle('hidden', menu.children.length === 0);
    toolbar.querySelectorAll('.tool-group').forEach((g) => {
      // 组内没有按钮时隐藏（含缩放组的「百分比」标签被遗留的情况）。
      g.classList.toggle('hidden', !g.querySelector('button'));
    });
    toolbar.querySelectorAll('.divider').forEach((d) => {
      const n = d.nextElementSibling;
      const p = d.previousElementSibling;
      d.classList.toggle('hidden',
        (n && n.classList.contains('hidden')) || (p && p.classList.contains('hidden')));
    });
  }

  // 下拉开关：点击按钮切换，点击外部或选中某项后关闭。
  btnMore.addEventListener('click', (e) => {
    e.stopPropagation();
    const open = menu.classList.toggle('open');
    btnMore.classList.toggle('active', open);
  });
  menu.addEventListener('click', () => {
    menu.classList.remove('open');
    btnMore.classList.remove('active');
  });
  document.addEventListener('click', (e) => {
    if (!moreWrap.contains(e.target)) {
      menu.classList.remove('open');
      btnMore.classList.remove('active');
    }
  });

  new ResizeObserver(() => relayout()).observe(toolbar);
  window.addEventListener('resize', relayout);
  relayout();
}

bindEvents();
initMaterialRipple();
initToolbarOverflow();
refreshLicenseStatus(); // 启动时拉取授权状态，填充 Pro 角标/锁
showShelf();
