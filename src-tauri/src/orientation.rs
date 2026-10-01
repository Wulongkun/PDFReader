//! 排版方向（横排/竖排）检测结果缓存：读写 `%APPDATA%/PDFReader/orientation.json`。
//!
//! 检测本身在前端用 PDF.js 完成（较慢：需采样渲染多页做投影判定），结果缓存到本地，
//! 下次打开同一本书直接命中，避免每次打开都重复检测。

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 排版方向编码：1 = 竖排，0 = 横排，-1 = 无法判定（也缓存，避免反复检测同一本无法判定的书）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrientationCache {
    pub entries: HashMap<String, i8>,
}

impl OrientationCache {
    /// 缓存文件路径：`%APPDATA%/PDFReader/orientation.json`。
    pub fn path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("PDFReader").join("orientation.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取排版缓存失败：{}", path.display()))?;
        let cache = serde_json::from_str(&text)
            .with_context(|| format!("解析排版缓存失败：{}", path.display()))?;
        Ok(cache)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建排版缓存目录失败：{}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("写入排版缓存失败：{}", path.display()))?;
        Ok(())
    }

    /// 查询缓存：`None` = 未缓存；`Some(code)` = 已缓存（1/0/-1）。
    pub fn get(&self, path: &str) -> Option<i8> {
        self.entries.get(path).copied()
    }

    pub fn set(&mut self, path: String, code: i8) {
        self.entries.insert(path, code);
    }
}
