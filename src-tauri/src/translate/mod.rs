//! 翻译相关。

pub mod client;
pub mod latex;
pub mod latex_ast;
pub mod mtef;
pub mod ole;
pub mod omml;

use serde::{Deserialize, Serialize};

/// 一次翻译请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateRequest {
    pub text: String,
    /// 源语言，`auto` 表示自动检测。
    pub source_lang: String,
    pub target_lang: String,
}

/// 翻译结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateResult {
    pub translated_text: String,
    /// 检测出的源语言（当前实现暂不返回）。
    pub detected_lang: Option<String>,
}

/// 云端版面解析出的单页结构化内容。
///
/// 由视觉大模型把一页图片解析成：正文（公式/页眉/页脚/页码已跳过）、表格（按行列）、
/// 图片（带归一化位置框与图题）。同时用于命令返回值（`Serialize`）与大模型 JSON
/// 解析（`Deserialize`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractedPage {
    /// 正文段落，按阅读顺序。
    pub paragraphs: Vec<String>,
    /// 表格列表。
    pub tables: Vec<TableRows>,
    /// 图片列表。
    pub figures: Vec<FigureBox>,
}

impl Default for ExtractedPage {
    fn default() -> Self {
        Self { paragraphs: Vec::new(), tables: Vec::new(), figures: Vec::new() }
    }
}

/// 一张表格：`rows[i][j]` 是第 i 行第 j 列的文字。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TableRows {
    pub rows: Vec<Vec<String>>,
}

impl Default for TableRows {
    fn default() -> Self {
        Self { rows: Vec::new() }
    }
}

/// 一处图片：`bbox` 为 0~1000 归一化坐标 `[x0, y0, x1, y1]`，`caption` 为图题。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FigureBox {
    /// 归一化包围盒（左上 x、左上 y、右下 x、右下 y）。
    pub bbox: Vec<f32>,
    /// 图题（如 "Fig. 2. ..."）。
    pub caption: String,
}

impl Default for FigureBox {
    fn default() -> Self {
        Self { bbox: Vec::new(), caption: String::new() }
    }
}
