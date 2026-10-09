//! 应用配置：读写 `%APPDATA%/PDFReader/config.json`。
//!
//! API Key 等敏感信息只保存在本地配置文件，绝不硬编码在源码里。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub translate: TranslateConfig,
    pub ocr: OcrConfig,
    pub viewer: ViewerConfig,
    pub word: NativeWordConfig,
    pub license: LicenseConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslateConfig {
    /// 服务提供方标识，预留扩展（如 `deepseek` / `qwen` / `deepl` / `baidu`）。
    pub provider: String,
    /// OpenAI 兼容接口的 Base URL，例如 `https://api.deepseek.com/v1`。
    pub base_url: String,
    /// API Key，仅保存在本地配置文件。
    pub api_key: String,
    /// 模型名，例如 `deepseek-chat`。
    pub model: String,
    /// 源语言，`auto` 表示自动检测。
    pub source_lang: String,
    /// 目标语言，默认中文。
    pub target_lang: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrConfig {
    /// OCR 方式：`local`（Windows 自带 OCR）、`llm`（大模型视觉识别）或 `glm`（智谱 GLM-OCR 版面解析）。
    pub mode: String,
    /// 大模型 OCR 用的视觉模型名；留空则回退到 `translate.model`。
    pub model: String,
    /// 大模型 OCR 的图片清晰度：`auto`（默认，不传该字段）/ `high`（原图）/ `low`（降采样省 token）。
    pub detail: String,
    /// GLM-OCR（智谱 BigModel）的 API Key，独立于翻译用的 Key，仅保存在本地配置文件。
    pub glm_api_key: String,
    /// 本地 OCR 语言（BCP-47，如 `zh-Hans` / `en-US`）；`auto`（默认）跟随系统语言包。
    pub lang: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerConfig {
    /// 排版方向：`auto`（逐本自动检测）/ `horizontal`（强制横排）/ `vertical`（强制竖排）。
    pub orientation: String,
    /// 竖排目录位置：`top`（顶部横条）或 `left`（左侧竖条）。目录为悬浮层，不影响画布。
    pub toc_position: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeWordConfig {
    /// `vlm` 引擎的接口地址（OpenAI 兼容 Base URL）。
    pub api_base: String,
    /// `vlm` 引擎的 API Key。
    pub api_key: String,
    /// `vlm` 引擎的模型名。
    pub model: String,
    /// `glm-ocr` 引擎的智谱 API Key（形如 `<id>.<secret>`）。
    pub glm_api_key: String,
    /// 公式格式：`omml`（Word 原生公式，默认）/ `mathtype`（MathType OLE 对象）。
    pub formula_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LicenseConfig {
    /// 用户输入的激活码（仅回填展示用，真正的授权凭据是 `receipt`）。
    pub code: String,
    /// Worker 签发的激活票据（base64url 编码的 `payload ‖ 签名`）。
    pub receipt: String,
    /// 激活时绑定的机器指纹（Windows MachineGuid）。
    pub machine_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            translate: TranslateConfig::default(),
            ocr: OcrConfig::default(),
            viewer: ViewerConfig::default(),
            word: NativeWordConfig::default(),
            license: LicenseConfig::default(),
        }
    }
}

impl Config {
    /// 统一识别引擎：文字提取 OCR（`ocr.mode`，取值 `local` / `llm` / `glm`）与公式识别共用同一选择。
    /// 此处把文字引擎映射为原生 Word 导出需要的标识：`llm` → `vlm`、`glm` → `glm-ocr`、其余 → `null`。
    pub fn word_engine(&self) -> &str {
        match self.ocr.mode.as_str() {
            "llm" => "vlm",
            "glm" => "glm-ocr",
            _ => "null",
        }
    }
}

impl Default for LicenseConfig {
    fn default() -> Self {
        Self { code: String::new(), receipt: String::new(), machine_id: String::new() }
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            mode: "glm".to_string(),
            model: String::new(),
            detail: "auto".to_string(),
            glm_api_key: String::new(),
            lang: "auto".to_string(),
        }
    }
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self { orientation: "auto".to_string(), toc_position: "top".to_string() }
    }
}

impl Default for NativeWordConfig {
    fn default() -> Self {
        Self {
            api_base: String::new(),
            api_key: String::new(),
            model: String::new(),
            glm_api_key: String::new(),
            formula_format: "omml".to_string(),
        }
    }
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            provider: "openai-compatible".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: String::new(),
            model: "deepseek-v4-flash".to_string(),
            source_lang: "auto".to_string(),
            target_lang: "中文".to_string(),
        }
    }
}

impl Config {
    /// 配置文件路径：`%APPDATA%/PDFReader/config.json`。
    pub fn config_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("PDFReader").join("config.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取配置文件失败：{}", path.display()))?;
        let cfg = serde_json::from_str(&text)
            .with_context(|| format!("解析配置文件失败：{}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建配置目录失败：{}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("写入配置文件失败：{}", path.display()))?;
        Ok(())
    }
}
