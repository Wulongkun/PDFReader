#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""PDFReader 激活码在线签发后端（纯标准库，密钥仅在服务器端，绝不进浏览器）。

- 签发算法与 keygen/src/lib.rs 逐位一致（Crockford base32 + HMAC-SHA256）
- 会话登录：HMAC 签名 Cookie；登录页为自定义美化页面
- 备注（时间/自定义）与激活码一并写入历史记录（history.jsonl）
- 历史记录独立页面 /history，支持搜索 + 分页 + 导出 CSV
"""
import base64
import hashlib
import hmac
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
NONCE_CHARS = 11
MAC_CHARS = 14
CODE_LEN = NONCE_CHARS + MAC_CHARS
SEED_PATH = "/opt/keygen/private.key"
PASSWORD_PATH = "/opt/keygen/admin.password"
HISTORY_PATH = "/opt/keygen/history.jsonl"
LISTEN_HOST = "127.0.0.1"
LISTEN_PORT = 8901
MAX_COUNT = 200
MAX_NOTE = 200
MAX_PAGE_SIZE = 500
SESSION_TTL = 24 * 3600
COOKIE_NAME = "keygen_session"

# ---------------------------------------------------------------- 签发算法

def encode_base32(value: int, digits: int) -> str:
    chars = ["0"] * digits
    for i in range(digits - 1, -1, -1):
        chars[i] = ALPHABET[value & 0x1F]
        value >>= 5
    return "".join(chars)


def derive_hmac_key(seed: bytes) -> bytes:
    return hashlib.sha256(b"pdfreader-code" + seed).digest()


def issue_code(seed: bytes) -> str:
    hmac_key = derive_hmac_key(seed)
    nonce_bytes = bytearray(os.urandom(7))
    nonce_bytes[0] &= 0x7F
    nonce = 0
    for b in nonce_bytes:
        nonce = (nonce << 8) | b
    nonce_str = encode_base32(nonce, NONCE_CHARS)
    mac = hmac.new(hmac_key, nonce_str.encode("ascii"), hashlib.sha256).digest()
    mac70 = 0
    for i in range(9):
        mac70 = (mac70 << 8) | mac[i]
    mac70 >>= 2
    mac_str = encode_base32(mac70, MAC_CHARS)
    raw = nonce_str + mac_str
    return "-".join(raw[i:i + 5] for i in range(0, CODE_LEN, 5))


# ---------------------------------------------------------------- 密钥 / 密码

_seed_cache = None
_password_cache = None


def get_seed() -> bytes:
    global _seed_cache
    if _seed_cache is None:
        with open(SEED_PATH, "rb") as f:
            hexstr = f.read().decode("ascii").strip()
        _seed_cache = bytes.fromhex(hexstr)
        if len(_seed_cache) != 32:
            raise RuntimeError("私钥必须 32 字节")
    return _seed_cache


def get_password() -> str:
    global _password_cache
    if _password_cache is None:
        with open(PASSWORD_PATH, "rb") as f:
            _password_cache = f.read().decode("utf-8").strip()
    return _password_cache


# ---------------------------------------------------------------- 历史记录

_history_lock = threading.Lock()


def append_history(note: str, codes: list) -> None:
    now = time.time()
    rec = {
        "ts": now,
        "time": time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(now)),
        "note": note,
        "count": len(codes),
        "codes": codes,
    }
    line = json.dumps(rec, ensure_ascii=False) + "\n"
    with _history_lock:
        is_new = not os.path.exists(HISTORY_PATH)
        with open(HISTORY_PATH, "a", encoding="utf-8") as f:
            f.write(line)
        if is_new:
            try:
                os.chmod(HISTORY_PATH, 0o600)
            except OSError:
                pass


def read_history(q: str = "", page: int = 1, page_size: int = 20):
    """返回 (records, total, total_pages)，按时间倒序，支持分页。"""
    needle = (q or "").lower()
    all_records = []
    try:
        with open(HISTORY_PATH, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                # 兼容旧字段名 param（曾叫「参数」），统一为 note。
                note = rec.get("note") or rec.get("param") or ""
                if needle and needle not in note.lower():
                    continue
                rec["note"] = note
                all_records.append(rec)
    except FileNotFoundError:
        pass

    all_records.sort(key=lambda r: r.get("ts", 0), reverse=True)
    total = len(all_records)
    total_pages = (total + page_size - 1) // page_size if total else 0
    page = max(1, page)
    if total_pages and page > total_pages:
        page = total_pages
    start = (page - 1) * page_size
    records = all_records[start:start + page_size]
    return records, total, total_pages


# ---------------------------------------------------------------- 会话

def _session_key() -> bytes:
    return hashlib.sha256(b"pdfreader-session" + get_seed()).digest()


def _b64(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def make_token() -> str:
    payload = str(int(time.time()) + SESSION_TTL).encode()
    sig = hmac.new(_session_key(), payload, hashlib.sha256).digest()
    return _b64(payload) + "." + _b64(sig)


def verify_token(token: str) -> bool:
    try:
        p, s = token.split(".", 1)
        payload = base64.urlsafe_b64decode(p + "=" * (-len(p) % 4))
        sig = base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))
        exp = int(payload)
    except (ValueError, IndexError):
        return False
    if exp < int(time.time()):
        return False
    expected = hmac.new(_session_key(), payload, hashlib.sha256).digest()
    return hmac.compare_digest(sig, expected)


# ---------------------------------------------------------------- 通用 CSS

_CSS = r"""
  :root {
    --bg: #f4f6fb; --card: #fff; --ink: #1c2433; --muted: #67718a;
    --accent: #3b5bfd; --accent-ink: #fff; --line: #e6e9f2; --ok: #0e8a52;
    --mono: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--ink);
    font: 15px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
      "Microsoft YaHei", "Noto Sans CJK SC", sans-serif; -webkit-font-smoothing: antialiased;
  }
  .wrap { max-width: 720px; margin: 0 auto; padding: 24px 16px calc(32px + env(safe-area-inset-bottom)); }
  header { margin-bottom: 18px; display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  header .t h1 { margin: 0 0 4px; font-size: 22px; letter-spacing: .5px; }
  header .t p { margin: 0; color: var(--muted); font-size: 14px; }
  .hbtns { display: flex; gap: 8px; flex-shrink: 0; }
  .mini { border: 1px solid var(--line); background: #fff; color: var(--ink); border-radius: 8px;
    padding: 7px 12px; font-size: 13px; cursor: pointer; white-space: nowrap; text-decoration: none; }
  .mini:hover { border-color: var(--accent); color: var(--accent); }
  .mini.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); }
  .mini.primary:hover { filter: brightness(1.06); color: var(--accent-ink); }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 14px; padding: 18px;
    margin-bottom: 16px; box-shadow: 0 1px 2px rgba(28,36,51,.04); }
  .card h2 { margin: 0 0 4px; font-size: 15px; }
  .hint { color: var(--muted); font-size: 13px; margin: 0 0 14px; }
  .status { color: var(--muted); font-size: 13px; min-height: 20px; }
  .status.ok { color: var(--ok); }
  .empty { color: var(--muted); text-align: center; padding: 20px 0; font-size: 14px; }
  footer { text-align: center; color: var(--muted); font-size: 12px; margin-top: 8px; }
  @media (max-width: 420px) {
    header h1 { font-size: 20px; }
  }
"""

_LOGIN_HTML = r"""<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>登录 · PDFReader 激活码生成器</title>
<style>
  :root {
    --ink: #1c2433; --muted: #6b7590; --accent: #3b5bfd; --accent-ink: #fff;
    --line: #e6e9f2; --err: #d64545;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
    padding: 20px 16px calc(20px + env(safe-area-inset-bottom));
    background: linear-gradient(160deg, #eef1ff 0%, #e8f0fb 45%, #f2f5fb 100%);
    font: 15px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
      "Microsoft YaHei", "Noto Sans CJK SC", sans-serif;
    color: var(--ink); -webkit-font-smoothing: antialiased;
  }
  .card {
    width: 100%; max-width: 400px; background: #fff; border: 1px solid var(--line);
    border-radius: 18px; padding: 34px 28px 28px;
    box-shadow: 0 12px 40px rgba(28,36,51,.10);
  }
  .brand { display: flex; flex-direction: column; align-items: center; margin-bottom: 22px; }
  .mark {
    width: 56px; height: 56px; border-radius: 15px; margin-bottom: 14px;
    background: linear-gradient(135deg, #3b5bfd, #6f8bff);
    display: flex; align-items: center; justify-content: center;
    box-shadow: 0 6px 18px rgba(59,91,253,.35);
  }
  .mark svg { width: 30px; height: 30px; }
  .brand h1 { margin: 0 0 4px; font-size: 19px; letter-spacing: .3px; }
  .brand p { margin: 0; color: var(--muted); font-size: 13px; }
  label { display: block; font-size: 13px; color: var(--muted); margin: 0 0 8px; }
  .field { position: relative; }
  input[type=password] {
    width: 100%; border: 1px solid var(--line); border-radius: 12px; background: #fafbfe;
    padding: 13px 46px 13px 14px; font-size: 16px; color: var(--ink);
    transition: border .15s, box-shadow .15s;
  }
  input[type=password]:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px rgba(59,91,253,.12); }
  .toggle {
    position: absolute; right: 6px; top: 50%; transform: translateY(-50%);
    border: 0; background: transparent; color: #9aa3b8; cursor: pointer; padding: 8px; font-size: 13px;
  }
  .btn {
    width: 100%; border: 0; border-radius: 12px; background: var(--accent); color: var(--accent-ink);
    font-size: 16px; font-weight: 600; cursor: pointer; padding: 13px 18px; margin-top: 16px;
    transition: filter .15s, transform .05s;
  }
  .btn:hover { filter: brightness(1.06); }
  .btn:active { transform: translateY(1px); }
  .btn:disabled { opacity: .6; cursor: not-allowed; }
  .msg { min-height: 22px; margin-top: 12px; font-size: 13px; color: var(--err); text-align: center; }
  footer { margin-top: 22px; text-align: center; color: #a0a8bd; font-size: 12px; }
  .shake { animation: shake .35s; }
  @keyframes shake {
    0%,100% { transform: translateX(0); }
    25% { transform: translateX(-6px); }
    50% { transform: translateX(6px); }
    75% { transform: translateX(-4px); }
  }
</style>
</head>
<body>
  <form class="card" id="login">
    <div class="brand">
      <div class="mark">
        <svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <rect x="3" y="11" width="18" height="11" rx="2"/>
          <path d="M7 11V7a5 5 0 0 1 10 0v4"/>
        </svg>
      </div>
      <h1>PDFReader 激活码生成器</h1>
      <p>请登录后签发激活码</p>
    </div>
    <label for="pw">登录密码</label>
    <div class="field">
      <input type="password" id="pw" autocomplete="current-password" placeholder="请输入密码" autofocus>
      <button type="button" class="toggle" id="toggle">显示</button>
    </div>
    <button type="submit" class="btn" id="btn">登 录</button>
    <div class="msg" id="msg"></div>
    <footer>仅供授权卖家使用</footer>
  </form>
<script>
(function () {
  var form = document.getElementById('login');
  var pw = document.getElementById('pw');
  var msg = document.getElementById('msg');
  var btn = document.getElementById('btn');
  var toggle = document.getElementById('toggle');

  toggle.addEventListener('click', function () {
    var show = pw.type === 'password';
    pw.type = show ? 'text' : 'password';
    toggle.textContent = show ? '隐藏' : '显示';
  });

  form.addEventListener('submit', function (e) {
    e.preventDefault();
    btn.disabled = true;
    msg.textContent = '';
    fetch('login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: pw.value })
    }).then(function (r) {
      return r.json().then(function (j) { return { ok: r.ok, j: j }; });
    }).then(function (res) {
      if (res.ok) { location.reload(); return; }
      msg.textContent = res.j.error || '登录失败';
      form.classList.remove('shake'); void form.offsetWidth; form.classList.add('shake');
      pw.select();
    }).catch(function () {
      msg.textContent = '网络错误，请重试';
    }).finally(function () { btn.disabled = false; });
  });
})();
</script>
</body>
</html>
"""

_APP_HTML = r"""<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>PDFReader 激活码生成器</title>
<style>
  :root {
    --bg: #f4f6fb; --card: #fff; --ink: #1c2433; --muted: #67718a;
    --accent: #3b5bfd; --accent-ink: #fff; --line: #e6e9f2; --ok: #0e8a52;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--ink);
    font: 15px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
      "Microsoft YaHei", "Noto Sans CJK SC", sans-serif; -webkit-font-smoothing: antialiased;
  }
  .wrap { max-width: 560px; margin: 0 auto; padding: 24px 16px calc(32px + env(safe-area-inset-bottom)); }
  header { margin-bottom: 18px; display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  header .t h1 { margin: 0 0 4px; font-size: 22px; letter-spacing: .5px; }
  header .t p { margin: 0; color: var(--muted); font-size: 14px; }
  .hbtns { display: flex; gap: 8px; flex-shrink: 0; }
  .mini { border: 1px solid var(--line); background: #fff; color: var(--ink); border-radius: 8px;
    padding: 7px 12px; font-size: 13px; cursor: pointer; white-space: nowrap; text-decoration: none; }
  .mini:hover { border-color: var(--accent); color: var(--accent); }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 14px; padding: 18px;
    margin-bottom: 16px; box-shadow: 0 1px 2px rgba(28,36,51,.04); }
  .card h2 { margin: 0 0 4px; font-size: 15px; }
  .hint { color: var(--muted); font-size: 13px; margin: 0 0 14px; }
  .counts { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 14px; }
  .pill { border: 1px solid var(--line); background: #fff; color: var(--ink); border-radius: 999px;
    padding: 7px 16px; font-size: 14px; cursor: pointer; transition: all .15s; }
  .pill.active { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); font-weight: 600; }
  .note-input { width: 100%; border: 1px solid var(--line); border-radius: 10px;
    padding: 10px 12px; font-size: 15px; background: #fff; margin-bottom: 14px; }
  .note-input:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px rgba(59,91,253,.10); }
  .row { display: flex; gap: 10px; align-items: stretch; }
  .num { width: 86px; border: 1px solid var(--line); border-radius: 10px; text-align: center;
    font-size: 15px; padding: 10px 8px; background: #fff; }
  .btn { flex: 1; border: 0; border-radius: 10px; background: var(--accent); color: var(--accent-ink);
    font-size: 16px; font-weight: 600; cursor: pointer; padding: 12px 18px; min-height: 46px; transition: filter .15s; }
  .btn:hover { filter: brightness(1.06); }
  .btn:active { transform: translateY(1px); }
  .btn:disabled { opacity: .55; cursor: not-allowed; }
  .status { color: var(--muted); font-size: 13px; min-height: 20px; }
  .status.ok { color: var(--ok); }
  .result { margin-top: 14px; }
  .code-row { display: flex; align-items: center; gap: 10px; padding: 8px 0; border-bottom: 1px solid var(--line); }
  .code-row:last-child { border-bottom: 0; }
  .code-row .code { flex: 1; min-width: 0; font-family: Consolas, "Liberation Mono", Menlo, monospace;
    font-size: 15px; font-weight: 600; letter-spacing: .3px; word-break: break-all; }
  .copy-mini { border: 1px solid var(--line); background: #fff; color: var(--ink); border-radius: 7px;
    padding: 4px 10px; font-size: 12px; cursor: pointer; white-space: nowrap; }
  .copy-mini:hover { border-color: var(--accent); color: var(--accent); }
  footer { text-align: center; color: var(--muted); font-size: 12px; margin-top: 8px; }
  @media (max-width: 420px) {
    .code-row .code { font-size: 13px; }
    header h1 { font-size: 20px; }
  }
</style>
</head>
<body>
  <div class="wrap">
    <header>
      <div class="t">
        <h1>PDFReader 激活码生成器</h1>
        <p>生成无时间限制激活码，绑定首次激活的机器。</p>
      </div>
      <div class="hbtns">
        <a class="mini" href="history">历史记录</a>
        <button class="mini" id="logout">退出</button>
      </div>
    </header>

    <section class="card">
      <h2>签发激活码</h2>
      <p class="hint">选择数量；备注可选填时间或自定义值，随码存入历史记录。</p>
      <div class="counts" id="counts">
        <button class="pill active" data-n="1">1 个</button>
        <button class="pill" data-n="5">5 个</button>
        <button class="pill" data-n="10">10 个</button>
        <button class="pill" data-n="20">20 个</button>
        <button class="pill" data-n="50">50 个</button>
      </div>
      <input class="note-input" id="note" maxlength="200" placeholder="备注（可选）：客户 / 订单号">
      <div class="row">
        <input class="num" id="num" type="number" min="1" max="200" value="1" inputmode="numeric">
        <button class="btn" id="gen">生成激活码</button>
      </div>
      <div class="status" id="status"></div>
      <div class="result" id="result" hidden>
        <div style="display:flex; justify-content:flex-end; gap:8px; margin-bottom:4px;">
          <button class="copy-mini" id="copyAll">复制全部</button>
        </div>
        <div id="codes"></div>
      </div>
    </section>

    <footer>密钥仅保存在服务器端，浏览器无法获取。</footer>
  </div>

<script>
(function () {
  var MAX = 200;
  var count = 1;
  var lastCodes = [];

  var countsEl = document.getElementById('counts');
  var numEl = document.getElementById('num');
  var noteEl = document.getElementById('note');
  var genBtn = document.getElementById('gen');
  var statusEl = document.getElementById('status');
  var resultEl = document.getElementById('result');
  var codesEl = document.getElementById('codes');

  function setCount(n) {
    count = n; numEl.value = n;
    countsEl.querySelectorAll('.pill').forEach(function (p) {
      p.classList.toggle('active', parseInt(p.getAttribute('data-n'), 10) === n);
    });
  }
  countsEl.addEventListener('click', function (e) {
    var p = e.target.closest('.pill');
    if (p) setCount(parseInt(p.getAttribute('data-n'), 10));
  });
  numEl.addEventListener('change', function () {
    var n = parseInt(numEl.value, 10);
    if (!n || n < 1) n = 1;
    if (n > MAX) n = MAX;
    setCount(n);
  });

  document.getElementById('logout').addEventListener('click', function () {
    fetch('logout', { method: 'POST' }).finally(function () { location.reload(); });
  });

  function setStatus(msg, ok) {
    statusEl.textContent = msg || '';
    statusEl.className = 'status' + (ok ? ' ok' : '');
  }

  function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
      return navigator.clipboard.writeText(text).catch(function () { return fallbackCopy(text); });
    }
    return fallbackCopy(text);
  }
  function fallbackCopy(text) {
    return new Promise(function (resolve, reject) {
      var ta = document.createElement('textarea');
      ta.value = text; ta.setAttribute('readonly', '');
      ta.style.position = 'fixed'; ta.style.opacity = '0';
      document.body.appendChild(ta); ta.focus(); ta.select();
      try {
        var ok = document.execCommand('copy');
        document.body.removeChild(ta);
        ok ? resolve() : reject(new Error('copy failed'));
      } catch (e) { document.body.removeChild(ta); reject(e); }
    });
  }
  function flash(btn) {
    var old = btn.textContent;
    btn.textContent = '已复制';
    setTimeout(function () { btn.textContent = old; }, 1000);
  }

  function renderCodes(codes) {
    codesEl.innerHTML = '';
    codes.forEach(function (code) {
      var row = document.createElement('div'); row.className = 'code-row';
      var cd = document.createElement('span'); cd.className = 'code'; cd.textContent = code;
      var b = document.createElement('button'); b.className = 'copy-mini'; b.textContent = '复制';
      b.addEventListener('click', function () {
        copyText(code).then(function () { flash(b); }, function () { setStatus('复制失败，请手动复制'); });
      });
      row.appendChild(cd); row.appendChild(b);
      codesEl.appendChild(row);
    });
  }

  genBtn.addEventListener('click', function () {
    genBtn.disabled = true;
    setStatus('正在生成…');
    fetch('api/issue', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ count: count, note: noteEl.value.trim() })
    }).then(function (r) {
      if (r.status === 401) { location.reload(); return null; }
      return r.json().then(function (j) { return { ok: r.ok, j: j }; });
    }).then(function (res) {
      if (!res) return;
      if (!res.ok) throw new Error(res.j.error || '请求失败');
      lastCodes = res.j.codes || [];
      renderCodes(lastCodes);
      resultEl.hidden = false;
      setStatus('已生成 ' + lastCodes.length + ' 个激活码，已存入历史记录', true);
    }).catch(function (e) { setStatus('生成失败：' + e.message); })
      .finally(function () { genBtn.disabled = false; });
  });

  document.getElementById('copyAll').addEventListener('click', function () {
    copyText(lastCodes.join('\n')).then(function () { setStatus('已复制全部码', true); },
      function () { setStatus('复制失败，请手动复制'); });
  });
})();
</script>
</body>
</html>
"""

_HISTORY_HTML = r"""<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>历史记录 · PDFReader 激活码生成器</title>
<style>
  :root {
    --bg: #f4f6fb; --card: #fff; --ink: #1c2433; --muted: #67718a;
    --accent: #3b5bfd; --accent-ink: #fff; --line: #e6e9f2; --ok: #0e8a52;
    --mono: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--ink);
    font: 15px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC",
      "Microsoft YaHei", "Noto Sans CJK SC", sans-serif; -webkit-font-smoothing: antialiased;
  }
  .wrap { max-width: 720px; margin: 0 auto; padding: 24px 16px calc(32px + env(safe-area-inset-bottom)); }
  header { margin-bottom: 18px; display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  header .t h1 { margin: 0 0 4px; font-size: 22px; letter-spacing: .5px; }
  header .t p { margin: 0; color: var(--muted); font-size: 14px; }
  .hbtns { display: flex; gap: 8px; flex-shrink: 0; }
  .mini { border: 1px solid var(--line); background: #fff; color: var(--ink); border-radius: 8px;
    padding: 7px 12px; font-size: 13px; cursor: pointer; white-space: nowrap; text-decoration: none; }
  .mini:hover { border-color: var(--accent); color: var(--accent); }
  .mini.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); }
  .mini.primary:hover { filter: brightness(1.06); color: var(--accent-ink); }
  .mini:disabled { opacity: .45; cursor: not-allowed; }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 14px; padding: 18px;
    margin-bottom: 16px; box-shadow: 0 1px 2px rgba(28,36,51,.04); }
  .card h2 { margin: 0 0 4px; font-size: 15px; }
  .toolbar { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 12px; align-items: center; }
  .search { flex: 1 1 160px; min-width: 0; border: 1px solid var(--line); border-radius: 10px;
    padding: 8px 12px; font-size: 14px; background: #fff; }
  .search:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 2px rgba(59,91,253,.10); }
  .meta { color: var(--muted); font-size: 13px; margin-bottom: 10px; }
  .rec { border: 1px solid var(--line); border-radius: 10px; margin-bottom: 10px; overflow: hidden; background: #fff; }
  .rec-top { display: flex; align-items: center; gap: 10px; padding: 10px 12px; background: #fafbfe; border-bottom: 1px solid var(--line); }
  .rec-time { font-family: var(--mono); font-size: 13px; color: var(--muted); white-space: nowrap; }
  .rec-count { font-size: 12px; color: var(--muted); white-space: nowrap; }
  .rec-top .spacer { flex: 1; }
  .rec-note { padding: 9px 12px 0; font-size: 13px; color: var(--ink); font-weight: 600; word-break: break-all; }
  .rec-note.empty { color: #b4bccd; font-weight: 400; }
  .copy-mini { border: 1px solid var(--line); background: #fff; color: var(--ink); border-radius: 7px;
    padding: 4px 10px; font-size: 12px; cursor: pointer; white-space: nowrap; }
  .copy-mini:hover { border-color: var(--accent); color: var(--accent); }
  .rec-codes { padding: 4px 12px 8px; }
  .code-row { display: flex; align-items: center; gap: 10px; padding: 6px 0; }
  .code-row .code { flex: 1; min-width: 0; font-family: var(--mono); font-size: 14px; font-weight: 600;
    letter-spacing: .3px; word-break: break-all; }
  .empty { color: var(--muted); text-align: center; padding: 20px 0; font-size: 14px; }
  .pager { display: flex; align-items: center; justify-content: center; gap: 12px; margin-top: 14px; }
  .pager .page-info { font-size: 13px; color: var(--muted); }
  footer { text-align: center; color: var(--muted); font-size: 12px; margin-top: 8px; }
  @media (max-width: 420px) {
    .code-row .code { font-size: 12px; }
    header h1 { font-size: 20px; }
  }
</style>
</head>
<body>
  <div class="wrap">
    <header>
      <div class="t">
        <h1>历史记录</h1>
        <p>查看已签发的激活码与备注。</p>
      </div>
      <div class="hbtns">
        <a class="mini" href="./">返回生成页</a>
        <button class="mini" id="logout">退出</button>
      </div>
    </header>

    <section class="card">
      <div class="toolbar">
        <input class="search" id="search" placeholder="按备注搜索…">
        <button class="mini" id="refresh" type="button">刷新</button>
        <button class="mini primary" id="export" type="button">导出 CSV</button>
      </div>
      <div class="meta" id="meta"></div>
      <div id="list"></div>
      <div id="empty" class="empty">暂无记录</div>
      <div class="pager" id="pager" hidden>
        <button class="mini" id="prev">上一页</button>
        <span class="page-info" id="pageInfo"></span>
        <button class="mini" id="next">下一页</button>
      </div>
    </section>

    <footer>密钥仅保存在服务器端，浏览器无法获取。</footer>
  </div>

<script>
(function () {
  var PAGE_SIZE = 20;
  var filter = '';
  var page = 1;
  var totalPages = 0;
  var total = 0;

  var searchEl = document.getElementById('search');
  var listEl = document.getElementById('list');
  var emptyEl = document.getElementById('empty');
  var metaEl = document.getElementById('meta');
  var pagerEl = document.getElementById('pager');
  var pageInfoEl = document.getElementById('pageInfo');
  var prevBtn = document.getElementById('prev');
  var nextBtn = document.getElementById('next');

  function pad(x) { return (x < 10 ? '0' : '') + x; }

  document.getElementById('logout').addEventListener('click', function () {
    fetch('logout', { method: 'POST' }).finally(function () { location.reload(); });
  });

  function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
      return navigator.clipboard.writeText(text).catch(function () { return fallbackCopy(text); });
    }
    return fallbackCopy(text);
  }
  function fallbackCopy(text) {
    return new Promise(function (resolve, reject) {
      var ta = document.createElement('textarea');
      ta.value = text; ta.setAttribute('readonly', '');
      ta.style.position = 'fixed'; ta.style.opacity = '0';
      document.body.appendChild(ta); ta.focus(); ta.select();
      try {
        var ok = document.execCommand('copy');
        document.body.removeChild(ta);
        ok ? resolve() : reject(new Error('copy failed'));
      } catch (e) { document.body.removeChild(ta); reject(e); }
    });
  }
  function flash(btn) {
    var old = btn.textContent;
    btn.textContent = '已复制';
    setTimeout(function () { btn.textContent = old; }, 1000);
  }

  function render(records) {
    listEl.innerHTML = '';
    emptyEl.style.display = records.length ? 'none' : 'block';
    records.forEach(function (rec) {
      var box = document.createElement('div'); box.className = 'rec';

      var top = document.createElement('div'); top.className = 'rec-top';
      var t = document.createElement('span'); t.className = 'rec-time'; t.textContent = rec.time || '';
      var c = document.createElement('span'); c.className = 'rec-count'; c.textContent = (rec.count || rec.codes.length) + ' 个';
      var spacer = document.createElement('span'); spacer.className = 'spacer';
      var cp = document.createElement('button'); cp.className = 'copy-mini'; cp.textContent = '复制本组';
      cp.addEventListener('click', function () {
        copyText((rec.codes || []).join('\n')).then(function () { flash(cp); });
      });
      top.appendChild(t); top.appendChild(c); top.appendChild(spacer); top.appendChild(cp);
      box.appendChild(top);

      var n = document.createElement('div'); n.className = 'rec-note' + (rec.note ? '' : ' empty');
      n.textContent = rec.note ? ('备注：' + rec.note) : '无备注';
      box.appendChild(n);

      var codes = document.createElement('div'); codes.className = 'rec-codes';
      (rec.codes || []).forEach(function (code) {
        var row = document.createElement('div'); row.className = 'code-row';
        var cd = document.createElement('span'); cd.className = 'code'; cd.textContent = code;
        var b = document.createElement('button'); b.className = 'copy-mini'; b.textContent = '复制';
        b.addEventListener('click', function () {
          copyText(code).then(function () { flash(b); });
        });
        row.appendChild(cd); row.appendChild(b);
        codes.appendChild(row);
      });
      box.appendChild(codes);
      listEl.appendChild(box);
    });
  }

  function updatePager() {
    pagerEl.hidden = total === 0;
    if (total === 0) { metaEl.textContent = ''; return; }
    metaEl.textContent = '共 ' + total + ' 条记录';
    pageInfoEl.textContent = '第 ' + page + ' / ' + totalPages + ' 页';
    prevBtn.disabled = page <= 1;
    nextBtn.disabled = page >= totalPages;
  }

  function load() {
    fetch('api/history?q=' + encodeURIComponent(filter) + '&page=' + page + '&page_size=' + PAGE_SIZE)
      .then(function (r) {
        if (r.status === 401) { location.reload(); return null; }
        return r.json();
      })
      .then(function (j) {
        if (!j) return;
        total = j.total || 0;
        totalPages = j.total_pages || 0;
        if (totalPages && page > totalPages) page = totalPages;
        render(j.records || []);
        updatePager();
      })
      .catch(function () {
        emptyEl.style.display = 'block';
        emptyEl.textContent = '加载失败，请刷新重试';
      });
  }

  var searchTimer = null;
  searchEl.addEventListener('input', function () {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(function () {
      filter = searchEl.value.trim();
      page = 1;
      load();
    }, 300);
  });

  document.getElementById('refresh').addEventListener('click', function () { load(); });
  prevBtn.addEventListener('click', function () { if (page > 1) { page--; load(); } });
  nextBtn.addEventListener('click', function () { if (page < totalPages) { page++; load(); } });

  function csvCell(v) {
    v = (v == null ? '' : String(v)).replace(/\r?\n/g, ' ');
    return '"' + v.replace(/"/g, '""') + '"';
  }
  function ts() {
    var d = new Date();
    return '' + d.getFullYear() + pad(d.getMonth() + 1) + pad(d.getDate()) + '_' +
      pad(d.getHours()) + pad(d.getMinutes()) + pad(d.getSeconds());
  }

  document.getElementById('export').addEventListener('click', function () {
    var all = [];
    var p = 1, per = 500;
    function fetchPage() {
      return fetch('api/history?q=' + encodeURIComponent(filter) + '&page=' + p + '&page_size=' + per)
        .then(function (r) { return r.json(); })
        .then(function (j) {
          all = all.concat(j.records || []);
          if (j.page < j.total_pages) { p++; return fetchPage(); }
        });
    }
    fetchPage().then(function () {
      var lines = ['时间,备注,激活码'];
      all.forEach(function (rec) {
        (rec.codes || []).forEach(function (c) {
          lines.push(csvCell(rec.time) + ',' + csvCell(rec.note) + ',' + csvCell(c));
        });
      });
      var blob = new Blob(['﻿' + lines.join('\r\n')], { type: 'text/csv;charset=utf-8;' });
      var a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = '激活码记录_' + ts() + '.csv';
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      setTimeout(function () { URL.revokeObjectURL(a.href); }, 1000);
    }).catch(function () { alert('导出失败'); });
  });

  load();
})();
</script>
</body>
</html>
"""


# ---------------------------------------------------------------- HTTP

class Handler(BaseHTTPRequestHandler):
    server_version = "pdfreader-keygen/1.3"

    def _send(self, code: int, body: bytes, ctype: str, headers=None) -> None:
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _json(self, code: int, obj: dict, headers=None) -> None:
        self._send(code, json.dumps(obj, ensure_ascii=False).encode("utf-8"),
                   "application/json; charset=utf-8", headers)

    def _cookie(self, name: str):
        for part in (self.headers.get("Cookie") or "").split(";"):
            part = part.strip()
            if part.startswith(name + "="):
                return part[len(name) + 1:]
        return None

    def _authed(self) -> bool:
        tok = self._cookie(COOKIE_NAME)
        return bool(tok) and verify_token(tok)

    def do_GET(self):  # noqa: N802
        if self.path in ("/", "/index.html"):
            if self._authed():
                self._send(200, _APP_HTML.encode("utf-8"), "text/html; charset=utf-8")
            else:
                self._send(200, _LOGIN_HTML.encode("utf-8"), "text/html; charset=utf-8")
        elif self.path == "/history":
            if self._authed():
                self._send(200, _HISTORY_HTML.encode("utf-8"), "text/html; charset=utf-8")
            else:
                self._send(200, _LOGIN_HTML.encode("utf-8"), "text/html; charset=utf-8")
        elif self.path == "/api/status":
            self._json(200, {"ok": True, "authed": self._authed()})
        elif self.path.startswith("/api/history"):
            self._handle_history()
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):  # noqa: N802
        if self.path == "/login":
            self._handle_login()
        elif self.path == "/logout":
            self._json(200, {"ok": True}, {"Set-Cookie": _logout_cookie()})
        elif self.path == "/api/issue":
            self._handle_issue()
        else:
            self._json(404, {"error": "not found"})

    def _read_json(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        return json.loads(raw.decode("utf-8") or "{}")

    def _handle_login(self):
        try:
            data = self._read_json()
            pw = data.get("password", "")
        except (ValueError, TypeError, json.JSONDecodeError):
            self._json(401, {"ok": False, "error": "请求格式错误"})
            return
        if isinstance(pw, str) and hmac.compare_digest(pw.encode(), get_password().encode()):
            self._json(200, {"ok": True}, {"Set-Cookie": _set_cookie()})
        else:
            self._json(401, {"ok": False, "error": "密码错误"})

    def _handle_issue(self):
        if not self._authed():
            self._json(401, {"error": "未登录"})
            return
        try:
            data = self._read_json()
            count = int(data.get("count", 1))
            note = str(data.get("note", "") or "").strip()[:MAX_NOTE]
        except (ValueError, TypeError, json.JSONDecodeError):
            self._json(400, {"error": "请求格式错误"})
            return
        if count < 1 or count > MAX_COUNT:
            self._json(400, {"error": f"数量需在 1~{MAX_COUNT} 之间"})
            return
        try:
            seed = get_seed()
            codes = [issue_code(seed) for _ in range(count)]
            append_history(note, codes)
        except Exception as e:  # noqa: BLE001
            self._json(500, {"error": f"签发失败：{e}"})
            return
        self._json(200, {"count": count, "note": note, "codes": codes})

    def _handle_history(self):
        if not self._authed():
            self._json(401, {"error": "未登录"})
            return
        qs = parse_qs(urlparse(self.path).query)
        q = (qs.get("q", [""])[0] or "").strip()
        try:
            page = int(qs.get("page", ["1"])[0])
        except (ValueError, TypeError):
            page = 1
        try:
            page_size = int(qs.get("page_size", ["20"])[0])
        except (ValueError, TypeError):
            page_size = 20
        page_size = max(1, min(page_size, MAX_PAGE_SIZE))
        records, total, total_pages = read_history(q, page, page_size)
        self._json(200, {
            "records": records, "total": total,
            "page": page, "page_size": page_size, "total_pages": total_pages,
        })

    def log_message(self, fmt, *args):  # noqa: A003
        sys.stderr.write("[keygen] %s - %s\n" % (self.address_string(), fmt % args))


def _set_cookie() -> str:
    return (f"{COOKIE_NAME}={make_token()}; HttpOnly; Path=/; "
            f"Max-Age={SESSION_TTL}; SameSite=Lax")


def _logout_cookie() -> str:
    return f"{COOKIE_NAME}=; HttpOnly; Path=/; Max-Age=0; SameSite=Lax"


def main() -> None:
    server = ThreadingHTTPServer((LISTEN_HOST, LISTEN_PORT), Handler)
    sys.stderr.write(f"[keygen] listening on {LISTEN_HOST}:{LISTEN_PORT}\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
