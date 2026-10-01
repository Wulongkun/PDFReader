// PDFReader 激活服务（Cloudflare Worker，境外部署，免 ICP 备案）。
//
// 流程：接收 {code, machine_id} → 校验 25 位激活码的 HMAC（防伪造）→ 用 Ed25519 私钥
// 签发绑定机器指纹的激活票据（receipt）→ 返回给客户端。
// 激活码无时间限制（机器绑定已防滥用），真伪由 HMAC 共享密钥判定。
//
// 私钥绝不硬编码在仓库里：用 `wrangler secret put ACTIVATION_PRIVATE_KEY` 注入
// （本地调试写进 .dev.vars）。HMAC 密钥与 Ed25519 私钥都从同一个 seed 派生。
//
// 部署：
//   cd worker
//   wrangler secret put ACTIVATION_PRIVATE_KEY   # 粘贴 keygen 输出的 PKCS#8 hex
//   wrangler deploy
// 然后把返回的 `https://pdfreader-activate.<你的子域>.workers.dev` 填进
// src-tauri/src/license.rs 的 ACTIVATION_URL。

const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN = 25;
const NONCE_CHARS = 11; // 随机码
const MAC_CHARS = 14;   // HMAC 校验值（70 bit）
const PKCS8_PREFIX_BYTES = 16; // Ed25519 PKCS#8 前缀长度（去掉后是 32 字节 seed）

export default {
  async fetch(request, env) {
    const privateKeyHex = env.ACTIVATION_PRIVATE_KEY || "";

    if (request.method !== "POST") {
      return json({ ok: false, error: "只接受 POST 请求" });
    }
    if (!privateKeyHex) {
      return json({ ok: false, error: "服务端未配置私钥" });
    }

    let body;
    try {
      body = await request.json();
    } catch {
      return json({ ok: false, error: "请求体不是合法 JSON" });
    }

    const code = String(body.code || "").trim();
    const machineId = String(body.machine_id || "").trim();
    if (!machineId) {
      return json({ ok: false, error: "缺少机器指纹" });
    }

    // 归一化：去横杠、转大写。
    const raw = code.replace(/-/g, "").toUpperCase();
    if (raw.length !== CODE_LEN) {
      return json({ ok: false, error: "激活码格式错误（应为 25 位，形如 XXXXX-XXXXX-XXXXX-XXXXX-XXXXX）" });
    }
    if (![...raw].every((c) => ALPHABET.includes(c))) {
      return json({ ok: false, error: "激活码含非法字符" });
    }

    try {
      // 校验 HMAC（防伪造）：覆盖前 11 位（随机码），比对后 14 位。
      const prefix = raw.slice(0, NONCE_CHARS);
      const macStr = raw.slice(NONCE_CHARS);

      const seed = hexToBytes(privateKeyHex).slice(PKCS8_PREFIX_BYTES);
      const hmacKeyBytes = await sha256Bytes(
        concatBytes(new TextEncoder().encode("pdfreader-code"), seed)
      );
      const hmacKey = await crypto.subtle.importKey(
        "raw", hmacKeyBytes, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]
      );
      const macSig = new Uint8Array(
        await crypto.subtle.sign("HMAC", hmacKey, new TextEncoder().encode(prefix))
      );
      let mac70 = 0n;
      for (let i = 0; i < 9; i++) mac70 = (mac70 << 8n) | BigInt(macSig[i]);
      mac70 >>= 2n;
      let expect = "";
      let v = mac70;
      for (let i = 0; i < MAC_CHARS; i++) {
        expect = ALPHABET[Number(v & 31n)] + expect;
        v >>= 5n;
      }
      if (expect !== macStr) {
        return json({ ok: false, error: "激活码无效" });
      }

      // 票据 payload 与客户端 license.rs 的 ReceiptPayload 字段一致。
      const now = Math.floor(Date.now() / 1000);
      const receiptPayload = {
        type: "receipt",
        code_hash: await sha256Hex(new TextEncoder().encode(raw)),
        machine_id: machineId,
        activated_at: now,
        edition: "pro",
      };
      const receiptBytes = new TextEncoder().encode(JSON.stringify(receiptPayload));

      const privKey = await crypto.subtle.importKey(
        "pkcs8",
        hexToBytes(privateKeyHex),
        { name: "Ed25519" },
        false,
        ["sign"]
      );
      const receiptSig = new Uint8Array(await crypto.subtle.sign("Ed25519", privKey, receiptBytes));
      const receipt = b64url(receiptBytes, receiptSig);

      return json({ ok: true, receipt });
    } catch (err) {
      return json({ ok: false, error: `激活失败：${err && err.message ? err.message : err}` });
    }
  },
};

// —— 工具函数 ——

function json(obj) {
  return new Response(JSON.stringify(obj), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

// hex → Uint8Array
function hexToBytes(hex) {
  const s = hex.replace(/\s+/g, "");
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function concatBytes(a, b) {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

// payload ‖ 签名 → base64url（票据用）
function b64url(payload, sig) {
  const bytes = new Uint8Array(payload.length + sig.length);
  bytes.set(payload, 0);
  bytes.set(sig, payload.length);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function sha256Bytes(bytes) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
}

async function sha256Hex(bytes) {
  const digest = await sha256Bytes(bytes);
  return Array.from(digest)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
