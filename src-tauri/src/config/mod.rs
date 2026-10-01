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
    /// 竖排古籍模式：OCR 前把页面逆时针旋转 90°，让竖列变横排，修复竖排文字的乱序/换行。
    pub vertical: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerConfig {
    /// 竖排古籍阅读模式：页面水平连续排列、从右往左读（第 1 页在最右，页码越大越靠左）。
    pub rtl: bool,
    /// 竖排目录位置：`top`（顶部横条）或 `left`（左侧竖条）。目录为悬浮层，不影响画布。
    pub toc_position: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            translate: TranslateConfig::default(),
            ocr: OcrConfig::default(),
            viewer: ViewerConfig::default(),
        }
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            mode: "local".to_string(),
            model: String::new(),
            detail: "auto".to_string(),
            glm_api_key: String::new(),
            lang: "auto".to_string(),
            vertical: false,
        }
    }
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self { rtl: false, toc_position: "top".to_string() }
    }
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            provider: "openai-compatible".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: String::new(),
            model: "deepseek-chat".to_string(),
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
