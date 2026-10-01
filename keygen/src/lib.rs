//! PDFReader 激活码签发核心逻辑（GUI 调用）。
//!
//! 激活码：25 位 base32（Office 风格 5×5，字母表不含 I/L/O/U），
//! `NNNNNNNNNNN`（11 位随机码）+ `MMMMMMMMMMMMMM`（14 位 HMAC 校验值，70 bit）。
//! 无时间限制（已按机器绑定防滥用），真伪由 Worker 用共享密钥重算 HMAC 判定。
//! 机器绑定票据（receipt）仍用 Ed25519 签名，由 Worker 签发。

use data_encoding::HEXLOWER;
use ed25519_dalek::SigningKey;
use getrandom::getrandom;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// base32 字母表（Crockford，去掉易混淆的 I / L / O / U）。
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// 随机码位数（55 bit）。
pub const NONCE_CHARS: usize = 11;
/// HMAC 校验值位数（70 bit）。
pub const MAC_CHARS: usize = 14;
/// 激活码总长度（去横杠后）。
pub const CODE_LEN: usize = NONCE_CHARS + MAC_CHARS;

const PKCS8_PREFIX: &str = "302e020100300506032b657004220420";

type HmacSha256 = Hmac<Sha256>;

/// 一套密钥的全部信息。
#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub seed_hex: String,
    pub public_hex: String,
    pub pkcs8_hex: String,
}

/// 私钥文件路径：与可执行文件同目录（双击 exe 也能找到）。
pub fn private_key_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("private.key")
}

/// 把一个 u128 编码成固定位数的 base32（高位在前）。
pub fn encode_base32(mut value: u128, digits: usize) -> String {
    let mut chars = vec!['0'; digits];
    for i in (0..digits).rev() {
        chars[i] = ALPHABET[(value & 0x1F) as usize] as char;
        value >>= 5;
    }
    chars.into_iter().collect()
}

/// 由 seed 派生 HMAC 密钥（域分离，避免与 Ed25519 签名密钥混用）。
fn derive_hmac_key(seed: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"pdfreader-code");
    h.update(seed);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

/// 生成一套 Ed25519 密钥对（不落盘，由调用方决定是否保存）。
pub fn generate_keypair() -> Result<KeyInfo, String> {
    let mut seed = [0u8; 32];
    getrandom(&mut seed).map_err(|e| format!("生成随机数失败：{e}"))?;
    Ok(keyinfo_from_seed(&seed))
}

/// 由 seed 反推公钥 / PKCS#8（用于显示，也供 generate_keypair 复用）。
pub fn keyinfo_from_seed(seed: &[u8; 32]) -> KeyInfo {
    let signing = SigningKey::from_bytes(seed);
    let verifying = signing.verifying_key();
    KeyInfo {
        seed_hex: HEXLOWER.encode(seed),
        public_hex: HEXLOWER.encode(verifying.as_bytes()),
        pkcs8_hex: format!("{PKCS8_PREFIX}{}", HEXLOWER.encode(seed)),
    }
}

/// 把 64 位 hex 解析成 seed（供「导入已有密钥」使用）。
pub fn hex_to_seed(hex: &str) -> Option<[u8; 32]> {
    let bytes = HEXLOWER.decode(hex.trim().as_bytes()).ok()?;
    bytes.try_into().ok()
}

/// 从私钥文件读取 seed。
pub fn load_seed() -> Result<[u8; 32], String> {
    let path = private_key_path();
    let sk_hex = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取私钥失败（{}）：{e}", path.display()))?
        .trim()
        .to_string();
    let bytes = HEXLOWER
        .decode(sk_hex.as_bytes())
        .map_err(|e| format!("私钥 hex 非法：{e}"))?;
    bytes.try_into().map_err(|_| "私钥必须 32 字节".to_string())
}

/// 把 seed 写入私钥文件。
pub fn save_seed(seed: &[u8; 32]) -> Result<(), String> {
    let path = private_key_path();
    std::fs::write(&path, format!("{}\n", HEXLOWER.encode(seed)))
        .map_err(|e| format!("写入私钥失败（{}）：{e}", path.display()))
}

/// 用 seed 签发一张 25 位激活码（无时间限制）。
pub fn issue_code(seed: &[u8; 32]) -> Result<String, String> {
    let hmac_key = derive_hmac_key(seed);

    // 55 bit 随机码，让每张码都唯一。
    let mut nonce_bytes = [0u8; 7];
    getrandom(&mut nonce_bytes).map_err(|e| format!("生成随机数失败：{e}"))?;
    nonce_bytes[0] &= 0x7F; // 56 bit -> 55 bit
    let mut nonce: u128 = 0;
    for b in nonce_bytes {
        nonce = (nonce << 8) | b as u128;
    }
    let nonce_str = encode_base32(nonce, NONCE_CHARS);

    // HMAC 覆盖随机码，取前 70 bit 作校验值。
    let mut mac = HmacSha256::new_from_slice(&hmac_key).map_err(|e| format!("HMAC 初始化失败：{e}"))?;
    mac.update(nonce_str.as_bytes());
    let mac_bytes = mac.finalize().into_bytes();
    let mut mac70: u128 = 0;
    for i in 0..9 {
        mac70 = (mac70 << 8) | mac_bytes[i] as u128;
    }
    mac70 >>= 2; // 72 bit -> 70 bit
    let mac_str = encode_base32(mac70, MAC_CHARS);

    let raw = format!("{nonce_str}{mac_str}");
    Ok(group_code(&raw))
}

/// 每 5 位一组，用 `-` 连接（Office 风格）。
pub fn group_code(raw: &str) -> String {
    raw.chars()
        .collect::<Vec<_>>()
        .chunks(5)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

// 编译期保证总长度确实是 25 位。
const _: () = assert!(CODE_LEN == 25);

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> [u8; 32] {
        hex_to_seed(s).expect("seed hex")
    }

    /// 用固定测试 seed 签一张码，按 Worker 同款算法重算 HMAC 校验值，确认真实有效。
    #[test]
    fn issued_code_is_valid() {
        // 仅用于测试的假 seed，与真实私钥无关。
        let seed = hex("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let code = issue_code(&seed).expect("issue");

        assert_eq!(code.len(), 29, "应含 4 个横杠：{code}");
        assert_eq!(code.chars().filter(|c| *c == '-').count(), 4);

        let raw: String = code.chars().filter(|c| *c != '-').collect();
        assert_eq!(raw.len(), CODE_LEN);
        assert!(raw.bytes().all(|b| ALPHABET.contains(&b)));

        // 重算后 14 位 MAC，应与码一致。
        let nonce_str = &raw[..NONCE_CHARS];
        let hmac_key = derive_hmac_key(&seed);
        let mut mac = HmacSha256::new_from_slice(&hmac_key).unwrap();
        mac.update(nonce_str.as_bytes());
        let mac_bytes = mac.finalize().into_bytes();
        let mut mac70: u128 = 0;
        for i in 0..9 {
            mac70 = (mac70 << 8) | mac_bytes[i] as u128;
        }
        mac70 >>= 2;
        assert_eq!(encode_base32(mac70, MAC_CHARS), &raw[NONCE_CHARS..]);
    }
}
