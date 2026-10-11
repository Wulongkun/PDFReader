//! 额度：机器绑定 key + 服务端按页计费（试用与 Pro 统一）。
//!
//! 分层（每个 AI 操作判定一次）：
//! - **Pro**（有效激活票据）→ 固定赠送页数（服务端 PRO_GRANT，激活时授予），用完转充值/BYOK；
//! - **BYOK**（用户把默认 key 换成自己的 key）→ 不限，跳过计费（花的是用户自己的钱）；
//! - **试用**（默认）→ 服务端按页扣减（50 页），走作者中转。
//!
//! key 是额度服务按机器指纹（MachineGuid）签发的随机串，存本地 config（不展示给用户）。
//! 额度以 machine_id 为主键存在服务端 —— 删配置/重装后重新 register 命中同一 machine_id，
//! 返回同一 key 与已扣减额度，无法通过删文件/改机器号重置。

use serde::Deserialize;

use crate::config::{Config, AUTHOR_GLM_KEY, AUTHOR_RELAY_TOKEN};

/// 额度服务端点（作者服务器，经 nginx 反代到 /quota/）。
const QUOTA_URL: &str = "http://yuanjingzh.cn/quota";

/// `POST /register` 的响应。
#[derive(Deserialize)]
pub struct RegisterResponse {
    pub trial_key: String,
    #[allow(dead_code)]
    pub granted: u32,
    #[allow(dead_code)]
    pub used: u32,
    #[allow(dead_code)]
    pub remaining: u32,
}

/// `POST /consume` 的响应。
#[derive(Deserialize)]
struct ConsumeResponse {
    ok: bool,
    #[serde(default)]
    remaining: u32,
    #[serde(default)]
    error: String,
}

/// 一次额度查询的结果。
#[derive(Debug, Clone, Deserialize)]
pub struct QuotaInfo {
    pub granted: u32,
    pub used: u32,
    pub remaining: u32,
    /// 6 位识别号（充值/联系作者时用，服务端绑定 machine_id、唯一）。
    #[serde(default)]
    pub account_no: Option<u32>,
}

/// 翻译 / 大模型 OCR / 版面解析这类「走 translate 配置」的调用是否计费：
/// 正在用作者的中转 token（等于内嵌默认 key）时才计费（Pro 与试用统一）。
pub fn should_charge_translate(config: &Config) -> bool {
    !AUTHOR_RELAY_TOKEN.is_empty()
        && config.translate.api_key.trim() == AUTHOR_RELAY_TOKEN
}

/// GLM-OCR（直连智谱）是否计费：正在用作者的 GLM key 时才计费（Pro 与试用统一）。
/// 作者 GLM key 未填（空）时 GLM-OCR 本来就用不了，故不计费。
pub fn should_charge_glm(config: &Config) -> bool {
    !AUTHOR_GLM_KEY.is_empty()
        && config.ocr.glm_api_key.trim() == AUTHOR_GLM_KEY
}

/// 原生 Word 导出（GLM-OCR 公式识别引擎）是否走作者 key 计费。
/// 导出引擎用 `word.glm_api_key`（与 `ocr.glm_api_key` 共用同一把），故单独判定。
pub fn should_charge_word_glm(config: &Config) -> bool {
    !AUTHOR_GLM_KEY.is_empty()
        && config.word.glm_api_key.trim() == AUTHOR_GLM_KEY
}

/// 是否处于「按页计费」模式（用于前端显示额度）。任一作者 key 在用即计费（试用与 Pro 统一）。
pub fn is_metered(config: &Config) -> bool {
    should_charge_translate(config) || should_charge_glm(config)
}

/// 首次注册：用机器指纹换一个绑定机器的试用 key（服务端幂等：同 machine_id 返回同 key）。
pub async fn register(machine_id: &str) -> Result<RegisterResponse, String> {
    let resp = reqwest::Client::new()
        .post(format!("{QUOTA_URL}/register"))
        .json(&serde_json::json!({ "machine_id": machine_id }))
        .send()
        .await
        .map_err(|e| format!("额度服务连接失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("注册试用额度失败（HTTP {status}）：{body}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("额度服务响应解析失败：{e}"))
}

/// 原子扣减 N 页；额度不足或 key 不匹配返回错误。成功返回扣减后剩余页数。
pub async fn consume(machine_id: &str, trial_key: &str, pages: u32) -> Result<u32, String> {
    let resp = reqwest::Client::new()
        .post(format!("{QUOTA_URL}/consume"))
        .json(&serde_json::json!({
            "machine_id": machine_id,
            "trial_key": trial_key,
            "pages": pages,
        }))
        .send()
        .await
        .map_err(|e| format!("额度服务连接失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("扣减额度失败（HTTP {status}）：{body}"));
    }
    let r: ConsumeResponse =
        serde_json::from_str(&body).map_err(|e| format!("额度服务响应解析失败：{e}"))?;
    if r.ok {
        Ok(r.remaining)
    } else if r.error.is_empty() {
        Err("额度已用完".to_string())
    } else {
        Err(r.error)
    }
}

/// 查询剩余额度。
pub async fn status(machine_id: &str, trial_key: &str) -> Result<QuotaInfo, String> {
    let resp = reqwest::Client::new()
        .get(format!("{QUOTA_URL}/status"))
        .query(&[("machine_id", machine_id), ("trial_key", trial_key)])
        .send()
        .await
        .map_err(|e| format!("额度服务连接失败：{e}"))?;
    let code = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !code.is_success() {
        return Err(format!("查询额度失败（HTTP {code}）：{body}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("额度服务响应解析失败：{e}"))
}

/// Pro 激活成功后调用：把激活票据交给额度服务验签，授予固定赠送页数（幂等）。
pub async fn activate_pro(machine_id: &str, trial_key: &str, receipt: &str) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .post(format!("{QUOTA_URL}/activate-pro"))
        .json(&serde_json::json!({
            "machine_id": machine_id,
            "trial_key": trial_key,
            "receipt": receipt,
        }))
        .send()
        .await
        .map_err(|e| format!("额度服务连接失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Pro 额度授予失败（HTTP {status}）：{body}"));
    }
    Ok(())
}
