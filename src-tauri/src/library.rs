//! 书架与阅读记录：读写 `%APPDATA%/PDFReader/library.json`。
//!
//! 与 [`crate::config::Config`] 一样，只把用户的书架/最近阅读/收藏持久化在本地，
//! 不涉及任何敏感信息。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 书架顶层条目：可以是单个 PDF 文件，也可以是文件夹（点进去浏览其中子项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookEntry {
    pub path: String,
    pub name: String,
    pub is_folder: bool,
    pub added_at: u64,
}

/// 最近阅读条目（仅 PDF，按打开时间倒序，最多 30 条）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: String,
    pub name: String,
    pub last_opened_at: u64,
}

/// `list_folder` 的即时结果：某目录的直接子项（不递归）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderContents {
    pub entries: Vec<BookEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub books: Vec<BookEntry>,
    pub recent: Vec<RecentEntry>,
    /// 收藏的 PDF 绝对路径（顺序即收藏先后）。
    pub favorites: Vec<String>,
    /// 每本书上次阅读页：PDF 绝对路径 → 页码（1-based），下次打开直接跳回。
    pub page_positions: HashMap<String, u32>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            books: Vec::new(),
            recent: Vec::new(),
            favorites: Vec::new(),
            page_positions: HashMap::new(),
        }
    }
}

/// 当前 Unix 时间戳（毫秒）。
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 从 PDF 文件路径取展示名：文件 stem（去掉 `.pdf`），取不到时回退到完整路径。
pub fn file_stem_name(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// 从目录路径取展示名：目录名，取不到时回退到完整路径。
pub fn dir_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// 列出目录的直接子项：子目录在前、PDF 文件随后，各自按名排序（忽略大小写）。
/// 只用于「点击文件夹进入浏览」，不递归、不持久化。
pub fn scan_dir(dir: &str) -> Vec<BookEntry> {
    let mut folders: Vec<BookEntry> = Vec::new();
    let mut files: Vec<BookEntry> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(BookEntry {
                    name: path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: path.to_string_lossy().into_owned(),
                    is_folder: true,
                    added_at: 0,
                });
            } else if path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false)
            {
                files.push(BookEntry {
                    name: path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: path.to_string_lossy().into_owned(),
                    is_folder: false,
                    added_at: 0,
                });
            }
        }
    }
    folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    folders.extend(files);
    folders
}

/// 递归列出目录下所有 PDF 文件的绝对路径（迭代 DFS，避免深层目录栈溢出）。
/// 用于「导入文件夹」时后台预识别其中所有书籍的排版方向。
pub fn scan_pdfs_recursive(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_string()];
    while let Some(cur) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&cur) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.to_string_lossy().into_owned());
                } else if path
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("pdf"))
                    .unwrap_or(false)
                {
                    out.push(path.to_string_lossy().into_owned());
                }
            }
        }
    }
    out
}

impl Library {
    /// 书架文件路径：`%APPDATA%/PDFReader/library.json`。
    pub fn path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("PDFReader").join("library.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取书架文件失败：{}", path.display()))?;
        let lib = serde_json::from_str(&text)
            .with_context(|| format!("解析书架文件失败：{}", path.display()))?;
        Ok(lib)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建书架目录失败：{}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("写入书架文件失败：{}", path.display()))?;
        Ok(())
    }
}
