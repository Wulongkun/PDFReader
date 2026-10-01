//! PDF 文字提取。
//!
//! 当前架构下，文本层提取由前端 PDF.js（`getTextContent`）完成，
//! 这里保留 Rust 侧的提取入口，用于后续实现「整篇文档导出」或「后端批量处理」。
//!
//! 计划接入的库（二选一）：
//! - [`pdfium-render`]（成熟、文字提取质量最好，依赖 PDFium）
//! - [`pdf_oxide`]（纯 Rust、速度快，API 较年轻）
//! - [`lopdf`] 层级过低，只适合解析结构，不适合直接做文本提取。

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// 某一页提取出的文字。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageText {
    pub page: u32,
    pub text: String,
}

/// 提取整篇文档的文本层（占位实现，待接入 Rust 侧 PDF 库）。
pub fn extract_text(_bytes: &[u8]) -> Result<Vec<PageText>> {
    Err(anyhow::anyhow!(
        "Rust 侧文字提取尚未实现：当前由前端 PDF.js 负责文本层提取"
    ))
}
