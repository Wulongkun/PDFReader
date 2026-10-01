//! 授权：短激活码（HMAC，服务器校验）+ Cloudflare Worker 在线激活 + 本地票据验签。
//!
//! 设计（闭源发行，代码里只放公钥，私钥绝不出现在本程序）：
//! - 激活码 = 25 位 base32（Office 风格 5×5），前 11 位是「发放日期 + 随机码」，
//!   后 14 位是 HMAC 校验值（70 bit）；**码本身不携带签名**，真伪由 Worker 用共享密钥
//!   重算 HMAC 判定，因此能短到 Office 那种长度；
//! - 激活时：算本机指纹（MachineGuid）→ POST 到 Worker → Worker 验 HMAC + 用**服务器时间**
//!   判「发放后 3 天内」窗口 → 用 Ed25519 私钥签发绑定机器的激活票据（receipt）；
//! - 之后离线只用本地验票据签名 + 核对机器指纹，不再联网。
//!
//! 私钥只存在于 keygen（离线）与 Worker（Cloudflare secret），程序里只有公钥，
//! 因此「伪造激活码」不可行（没有共享密钥算不出有效 HMAC）。

use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Worker 激活端点地址（部署后填成你的 `*.workers.dev` 域名）。
const ACTIVATION_URL: &str = "https://pdfreader-activate.zhihan.workers.dev";

/// 公钥（32 字节 hex），只用于验票据（receipt）签名。
const PUBLIC_KEY_HEX: &str = "5051895d69d78206bfb79b0bd92a7fe1ba815ef2eee57a9b9b39eaf541674751";

/// 激活码字母表（与 keygen / Worker 一致，不含 I/L/O/U）。
const CODE_ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 激活码长度（去横杠后）。
const CODE_LEN: usize = 25;

/// 激活票据载荷（Worker 签发）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptPayload {
    #[serde(rename = "type")]
    pub kind: String,
    pub code_hash: String,
    pub machine_id: String,
    pub activated_at: i64,
    #[serde(default)]
    pub edition: String,
}

fn verifying_key() -> VerifyingKey {
    let bytes = HEXLOWER
        .decode(PUBLIC_KEY_HEX.as_bytes())
        .expect("公钥 hex 非法");
    let arr: [u8; 32] = bytes.as_slice().try_into().expect("公钥必须 32 字节");
    VerifyingKey::from_bytes(&arr).expect("公钥无法解析")
}

/// 验证「base64url(payload ‖ 签名)」形式的签名字符串，返回 JSON 载荷。
/// 签名固定为末尾 64 字节（Ed25519 签名长度）。
fn verify_signed<T: for<'de> Deserialize<'de>>(signed: &str) -> Result<T, String> {
    let bytes = BASE64URL_NOPAD
        .decode(signed.as_bytes())
        .map_err(|e| format!("编码错误：{e}"))?;
    if bytes.len() < 64 {
        return Err("签名数据过短".to_string());
    }
    let (payload, sig) = bytes.split_at(bytes.len() - 64);
    let sig = Signature::from_slice(sig).map_err(|e| e.to_string())?;
    verifying_key()
        .verify(payload, &sig)
        .map_err(|_| "签名校验失败（可能被篡改或来自其他发行者）".to_string())?;
    serde_json::from_slice(payload).map_err(|e| format!("载荷解析失败：{e}"))
}

/// 校验激活码格式（本地预检，不验真伪）：去横杠、转大写、检查长度与字母表。
/// 真正的校验在 Worker（重算 HMAC + 判时间窗口）。返回归一化后的 25 位码。
pub fn validate_code(code: &str) -> Result<String, String> {
    let code = code.trim().to_ascii_uppercase().replace('-', "");
    if code.len() != CODE_LEN {
        return Err(format!(
            "激活码格式错误（应为 {CODE_LEN} 位，形如 XXXXX-XXXXX-XXXXX-XXXXX-XXXXX）"
        ));
    }
    if !code.chars().all(|c| CODE_ALPHABET.contains(c)) {
        return Err("激活码含非法字符".to_string());
    }
    Ok(code)
}

/// 校验激活票据：验签 + 类型 + 机器指纹匹配。全离线。
pub fn verify_receipt(receipt: &str, machine_id: &str) -> Result<ReceiptPayload, String> {
    let payload: ReceiptPayload = verify_signed(receipt.trim())?;
    if payload.kind != "receipt" {
        return Err("激活票据类型错误".to_string());
    }
    if payload.machine_id != machine_id {
        return Err("激活票据与本机不匹配".to_string());
    }
    Ok(payload)
}

/// 读取本机机器指纹（Windows MachineGuid，稳定、与硬件/系统绑定）。
pub fn machine_id() -> Result<String, String> {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        use winreg::RegKey;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key = hklm
            .open_subkey(r"SOFTWARE\Microsoft\Cryptography")
            .map_err(|e| format!("读取注册表失败：{e}"))?;
        let guid: String = key
            .get_value("MachineGuid")
            .map_err(|e| format!("读取 MachineGuid 失败：{e}"))?;
        Ok(guid)
    }
    #[cfg(not(windows))]
    {
        Ok("non-windows-device".to_string())
    }
}

/// 当前是否已激活 Pro（验票据签名 + 核对机器指纹，全离线）。
pub fn is_pro(config: &Config) -> bool {
    if config.license.receipt.trim().is_empty() {
        return false;
    }
    match machine_id() {
        Ok(mid) => verify_receipt(&config.license.receipt, &mid).is_ok(),
        Err(_) => false,
    }
}

#[derive(Serialize)]
struct ActivateRequest<'a> {
    code: &'a str,
    machine_id: &'a str,
}

#[derive(Deserialize)]
struct ActivateResponse {
    ok: bool,
    #[serde(default)]
    receipt: String,
    #[serde(default)]
    error: String,
}

/// 把激活码与机器指纹发给 Worker，返回 Worker 签发的激活票据（base64url）。
pub async fn activate_online(code: &str, machine_id: &str) -> Result<String, String> {
    let resp = reqwest::Client::new()
        .post(ACTIVATION_URL)
        .json(&ActivateRequest { code, machine_id })
        .send()
        .await
        .map_err(|e| format!("网络请求失败：{e}"))?;

    let status = resp.status();
    let body: ActivateResponse = resp
        .json()
        .await
        .map_err(|e| format!("激活响应解析失败（HTTP {status}）：{e}"))?;

    if !body.ok {
        return Err(if body.error.is_empty() {
            "激活失败".to_string()
        } else {
            body.error
        });
    }
    if body.receipt.is_empty() {
        return Err("激活成功但未返回票据".to_string());
    }
    Ok(body.receipt)
}
