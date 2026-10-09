//! Tauri 命令：前端 `invoke(...)` 调用的 Rust 侧入口。
//!
//! 约定：所有命令的错误都统一映射成 `String` 返回给前端，
//! 便于直接展示为可读的提示信息。

use tauri::Manager;
use tauri::State;
use tauri::Emitter;

use pdfomml::{Backend, Converter, Engine};

use crate::{
    app::AppState,
    config::Config,
    library::{self, Library},
    license,
    pdf::{extract, ocr, reader},
    translate::{ExtractedPage, TranslateRequest, TranslateResult},
};

/// `open_pdf` 的返回值：文件路径 + 名称。字节由前端随后调用 [`read_pdf`] 按需读取。
#[derive(serde::Serialize)]
pub struct OpenedPdf {
    pub path: String,
    pub name: String,
}

/// 后台扫描件规范化完成后的通知载荷：规范化后的 PDF 路径 + data URL。
#[derive(serde::Serialize, Clone)]
struct NormalizedPdf {
    path: String,
    data_url: String,
}

/// 弹出原生文件选择框，返回选中文件的路径与名称（不读字节，大文件秒开）。
///
/// 使用异步对话框：它在专用线程上以正确的 COM 公寓模式（STA）初始化，
/// 避免同步对话框在工作线程上偶发卡死、无法选择文件的问题。
#[tauri::command]
pub async fn open_pdf(app: tauri::AppHandle) -> Result<OpenedPdf, String> {
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("PDF 文件", &["pdf"])
        .pick_file()
        .await
        .ok_or_else(|| "未选择文件".to_string())?;

    let path = handle.path().to_string_lossy().into_owned();
    let name = handle.file_name();
    Ok(open_pdf_impl(&app, &path, &name))
}

/// 给定路径打开 PDF（书架点击书籍用）：校验文件存在与 PDF 文件头，触发与 [`open_pdf`]
/// 相同的扫描件后台归一化，返回 `{ path, name }`。
#[tauri::command]
pub fn open_pdf_path(app: tauri::AppHandle, path: String) -> Result<OpenedPdf, String> {
    let meta = std::fs::metadata(&path).map_err(|e| format!("文件不存在：{e}"))?;
    if !meta.is_file() {
        return Err("不是有效的文件".to_string());
    }
    // 只读文件头校验 PDF 魔数，避免为验证把整份大文件读进内存。
    use std::io::Read;
    let mut head = [0u8; 5];
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut head))
        .map_err(|e| format!("读取文件失败：{e}"))?;
    reader::validate_pdf(&head).map_err(|e| e.to_string())?;

    let name = library::file_stem_name(&path);
    Ok(open_pdf_impl(&app, &path, &name))
}

/// 打开一个 PDF 的公共逻辑：记录日志 + 后台触发扫描件规范化，返回 `OpenedPdf`。
fn open_pdf_impl(app: &tauri::AppHandle, path: &str, name: &str) -> OpenedPdf {
    // 记录打开日志：文件名 / 路径 / 大小，便于回溯扫描件体积、OCR 行为等问题。
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    append_log(&format!("打开 PDF：{name}（{path}，{size} 字节）"));

    // 修复扫描版 PDF 的 1-bit /Decode [1 0] 图像被渲染成黑色的问题，放到后台执行：
    // 解压/展开/重压缩整页位图属于 CPU 密集操作，不阻塞打开；规范化后字节若有变化，
    // 通过事件通知前端用规范化后的字节重新加载（无此类图像的 PDF 不会发事件）。
    let app2 = app.clone();
    let emit_path = path.to_string();
    let emit_path_notify = emit_path.clone();
    tauri::async_runtime::spawn(async move {
        let maybe_changed = tauri::async_runtime::spawn_blocking(move || {
            let bytes = std::fs::read(&emit_path).ok()?;
            let normalized = reader::normalize_scanned_pdf(&bytes).ok()?;
            (normalized != bytes).then_some(normalized)
        })
        .await;
        if let Ok(Some(normalized)) = maybe_changed {
            let _ = app2.emit(
                "pdf-normalized",
                NormalizedPdf {
                    path: emit_path_notify,
                    data_url: reader::to_data_url(&normalized),
                },
            );
        }
    });

    OpenedPdf {
        path: path.to_string(),
        name: name.to_string(),
    }
}

/// 读取 PDF 文件的原始字节，以二进制返回（`ArrayBuffer`）。
///
/// 相比把整份文件 base64 成 data URL 再在 JS 里 `atob` 回字节，这条路径避免了
/// base64 编解码与 JSON 字符串往返，打开 100MB+ 的扫描版 PDF 更快、内存更省。
///
/// 注意必须是 `async`：同步命令默认跑在 Tauri 主线程上，大文件 `std::fs::read`
/// 会把主线程卡住（所有 IPC/事件全部阻塞），表现为界面卡死。`async` 后落到
/// tokio 工作线程执行，主线程保持响应。
#[tauri::command]
pub async fn read_pdf(path: String) -> Result<tauri::ipc::Response, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败：{e}"))?;
    reader::validate_pdf(&bytes).map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// 读取 PDF 文件的字节区间 `[begin, end)`，以二进制返回。
///
/// 供前端 PDF.js 的 `PDFDataRangeTransport` 按需拉取封面所需的一小段字节，
/// 避免为一张缩略图把整份大文件读进内存再传过 IPC。
#[tauri::command]
pub async fn read_pdf_range(path: String, begin: u64, end: u64) -> Result<tauri::ipc::Response, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(&path).map_err(|e| format!("打开文件失败：{e}"))?;
    f.seek(SeekFrom::Start(begin)).map_err(|e| e.to_string())?;
    let len = end.saturating_sub(begin) as usize;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).map_err(|e| format!("读取字节区间失败：{e}"))?;
    Ok(tauri::ipc::Response::new(buf))
}

/// 返回 PDF 文件字节长度，供前端构造 `PDFDataRangeTransport`。
#[tauri::command]
pub async fn pdf_file_size(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|m| m.len())
        .map_err(|e| format!("获取文件信息失败：{e}"))
}

// ===== 书架 / 最近阅读 / 收藏 =====

/// 返回当前书架数据（书架顶层条目 + 最近阅读 + 收藏路径）。
#[tauri::command]
pub fn get_library(state: State<'_, AppState>) -> Result<Library, String> {
    Ok(state.library.lock().map_err(|e| e.to_string())?.clone())
}

/// 弹出多选文件框，把选中的 PDF 逐个加入书架（按路径去重），保存后返回整个书架。
/// 取消选择时返回当前书架（不视为错误）。
#[tauri::command]
pub async fn add_books_dialog(state: State<'_, AppState>) -> Result<Library, String> {
    let handles = rfd::AsyncFileDialog::new()
        .add_filter("PDF 文件", &["pdf"])
        .pick_files()
        .await
        .unwrap_or_default();

    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    if handles.is_empty() {
        return Ok(lib.clone());
    }
    let now = library::now_millis();
    for h in handles {
        let path = h.path().to_string_lossy().into_owned();
        if lib.books.iter().any(|b| !b.is_folder && b.path == path) {
            continue;
        }
        let name = library::file_stem_name(&path);
        lib.books.push(library::BookEntry {
            path,
            name,
            is_folder: false,
            added_at: now,
        });
    }
    lib.save().map_err(|e| e.to_string())?;
    Ok(lib.clone())
}

/// 弹出文件夹选择框，把选中的文件夹加入书架（按路径去重），保存后返回整个书架。
/// 取消选择时返回当前书架（不视为错误）。
#[tauri::command]
pub async fn add_folder_dialog(state: State<'_, AppState>) -> Result<Library, String> {
    let handle = match rfd::AsyncFileDialog::new().pick_folder().await {
        Some(h) => h,
        None => return Ok(state.library.lock().map_err(|e| e.to_string())?.clone()),
    };
    let path = handle.path().to_string_lossy().into_owned();

    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    if lib.books.iter().any(|b| b.is_folder && b.path == path) {
        return Ok(lib.clone());
    }
    let name = library::dir_name(&path);
    lib.books.push(library::BookEntry {
        path,
        name,
        is_folder: true,
        added_at: library::now_millis(),
    });
    lib.save().map_err(|e| e.to_string())?;
    Ok(lib.clone())
}

/// 从书架移除一个顶层条目（文件或文件夹）。
#[tauri::command]
pub fn remove_book(state: State<'_, AppState>, path: String) -> Result<Library, String> {
    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    lib.books.retain(|b| b.path != path);
    lib.save().map_err(|e| e.to_string())?;
    Ok(lib.clone())
}

/// 从「最近阅读」移除一条记录（只清历史，不删除书架条目或磁盘文件）。
#[tauri::command]
pub fn remove_recent(state: State<'_, AppState>, path: String) -> Result<Library, String> {
    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    lib.recent.retain(|r| r.path != path);
    lib.save().map_err(|e| e.to_string())?;
    Ok(lib.clone())
}

/// 列出某目录的直接子项（子目录在前、PDF 随后），用于「点击文件夹进入浏览」。
#[tauri::command]
pub fn list_folder(path: String) -> Result<library::FolderContents, String> {
    Ok(library::FolderContents {
        entries: library::scan_dir(&path),
    })
}

// ===== 排版方向缓存 =====

/// 查询某本书的排版方向缓存：`None` = 未缓存；`Some(1)` = 竖排；`Some(0)` = 横排；`Some(-1)` = 无法判定。
#[tauri::command]
pub fn get_orientation(state: State<'_, AppState>, path: String) -> Result<Option<i8>, String> {
    let cache = state.orientation.lock().map_err(|e| e.to_string())?;
    Ok(cache.get(&path))
}

/// 写入某本书的排版方向缓存（1=竖排 / 0=横排 / -1=无法判定），并持久化到本地。
#[tauri::command]
pub fn set_orientation(state: State<'_, AppState>, path: String, code: i8) -> Result<(), String> {
    let mut cache = state.orientation.lock().map_err(|e| e.to_string())?;
    cache.set(path, code);
    cache.save().map_err(|e| e.to_string())
}

/// 递归列出目录下所有 PDF 的绝对路径，用于「导入文件夹」后后台预识别排版方向。
/// 递归磁盘扫描可能较慢，故用 async 放到 worker 线程，避免阻塞主线程 IPC。
#[tauri::command]
pub async fn list_pdfs_recursive(dir: String) -> Result<Vec<String>, String> {
    Ok(library::scan_pdfs_recursive(&dir))
}

/// 收藏/取消收藏一个 PDF 路径，保存后返回整个书架。
#[tauri::command]
pub fn toggle_favorite(state: State<'_, AppState>, path: String) -> Result<Library, String> {
    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    if let Some(pos) = lib.favorites.iter().position(|p| *p == path) {
        lib.favorites.remove(pos);
    } else {
        lib.favorites.push(path);
    }
    lib.save().map_err(|e| e.to_string())?;
    Ok(lib.clone())
}

/// 记录一次打开（upsert 最近阅读），按打开时间倒序、截断到最新 30 条。
#[tauri::command]
pub fn record_recent(
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> Result<Library, String> {
    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    lib.recent.retain(|r| r.path != path);
    lib.recent.push(library::RecentEntry {
        path,
        name,
        last_opened_at: library::now_millis(),
    });
    lib.recent
        .sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at));
    lib.recent.truncate(30);
    lib.save().map_err(|e| e.to_string())?;
    Ok(lib.clone())
}

/// 读取某本书上次阅读页；未记录返回 `None`。
#[tauri::command]
pub fn get_last_page(state: State<'_, AppState>, path: String) -> Result<Option<u32>, String> {
    let lib = state.library.lock().map_err(|e| e.to_string())?;
    Ok(lib.page_positions.get(&path).copied())
}

/// 记录某本书的阅读页（按 PDF 路径键控），并持久化。
#[tauri::command]
pub fn set_last_page(state: State<'_, AppState>, path: String, page: u32) -> Result<(), String> {
    let mut lib = state.library.lock().map_err(|e| e.to_string())?;
    lib.page_positions.insert(path, page.max(1));
    lib.save().map_err(|e| e.to_string())
}

/// 保存封面缩略图 PNG：`key` 为前端 fnv1a(path) 的十六进制键，写入 `thumbs/{key}.png`。
#[tauri::command]
pub fn save_thumb(key: String, png_data_url: String) -> Result<(), String> {
    validate_thumb_key(&key)?;
    let bytes = reader::from_data_url(&png_data_url).map_err(|e| e.to_string())?;
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let dir = base.join("PDFReader").join("thumbs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{key}.png"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(())
}

/// 读取封面缩略图 PNG，返回 `data:image/png;base64,...`；不存在返回 `None`。
#[tauri::command]
pub fn load_thumb(key: String) -> Result<Option<String>, String> {
    validate_thumb_key(&key)?;
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let path = base.join("PDFReader").join("thumbs").join(format!("{key}.png"));
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    Ok(Some(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(&bytes)
    )))
}

/// 缩略图键白名单校验：仅小写十六进制、长度 ≤ 32，防止路径穿越。
fn validate_thumb_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 32 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("无效的缩略图键".to_string());
    }
    Ok(())
}

/// 把当前 PDF 的指定页（1-based）抽取成一个新的 PDF 文件，弹出保存对话框并返回保存路径。
///
/// 用 lopdf 删除不在保留集合里的页：保留的页及其字体/图片等资源原样保留（矢量无损），
/// 删除的页所独有的资源随后被 `prune_objects` 清掉，避免抽取两页却导出整份体积。
#[tauri::command]
pub async fn extract_pages(
    path: String,
    pages: Vec<u32>,
    suggested_name: String,
) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败：{e}"))?;
    let mut doc = lopdf::Document::load_mem(&bytes).map_err(|e| format!("解析 PDF 失败：{e}"))?;

    let total = doc.get_pages().len() as u32;
    let keep: std::collections::BTreeSet<u32> = pages
        .into_iter()
        .filter(|&p| (1..=total).contains(&p))
        .collect();
    if keep.is_empty() {
        return Err(format!("页码超出范围（1–{total}）"));
    }

    // lopdf 的页号从 1 开始；删掉不保留的页，再清掉因此悬空的资源对象以缩小体积。
    let to_delete: Vec<u32> = (1..=total).filter(|p| !keep.contains(p)).collect();
    if !to_delete.is_empty() {
        doc.delete_pages(&to_delete);
        doc.prune_objects();
    }

    let handle = rfd::AsyncFileDialog::new()
        .add_filter("PDF 文件", &["pdf"])
        .set_file_name(&suggested_name)
        .save_file()
        .await
        .ok_or_else(|| "已取消导出".to_string())?;
    let out = handle.path().to_path_buf();
    doc.save(&out).map_err(|e| format!("写入失败：{e}"))?;
    Ok(out.to_string_lossy().into_owned())
}

/// 当前 UTC 时间（可读字符串，作日志前缀）。
fn utc_now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant 的 civil_from_days：把自 1970-01-01 的天数换算成公历 y/m/d。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02} {hh:02}:{mm:02}:{ss:02}Z")
}

/// 把一行日志（带时间戳）追加到 `%APPDATA%/PDFReader/diag.log`；尽力而为，失败不报错。
fn append_log(msg: &str) {
    use std::io::Write;
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let path = base.join("PDFReader").join("diag.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {}", utc_now_string(), msg);
    }
}

/// 前端渲染诊断日志：追加写入 `%APPDATA%/PDFReader/diag.log`，便于定位大页面黑块问题。
#[tauri::command]
pub fn log_diag(msg: String) -> Result<(), String> {
    append_log(&msg);
    Ok(())
}

/// 把 OCR 原始输出（云端 `extract_page` 的段落 / GLM 的 Markdown，均未做标题清洗）
/// 整体写入 `%APPDATA%/PDFReader/ocr_raw.txt`，便于定位 `##` 标题泄漏的确切格式。
/// 返回写入路径。
#[tauri::command]
pub fn save_ocr_raw(text: String) -> Result<String, String> {
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let path = base.join("PDFReader").join("ocr_raw.txt");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &text).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// 用系统默认浏览器打开外部链接（激活码获取页 / QQ 群等）。仅允许 http/https。
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("仅允许打开 http/https 链接".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        // `cmd /c start "" <url>`：用 ShellExecute 走默认浏览器，不阻塞本进程。
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &url])
            .spawn()
            .map_err(|e| format!("打开链接失败：{e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = url;
    }
    Ok(())
}

/// Rust 侧文字提取入口（占位）。前端 PDF.js 已负责文本层提取。
#[tauri::command]
pub fn extract_text(pdf_data_url: String) -> Result<Vec<extract::PageText>, String> {
    let bytes = reader::from_data_url(&pdf_data_url).map_err(|e| e.to_string())?;
    extract::extract_text(&bytes).map_err(|e| e.to_string())
}

/// 翻译一段文字。API 配置从本地 config 读取。
#[tauri::command]
pub async fn translate(
    state: State<'_, AppState>,
    request: TranslateRequest,
) -> Result<TranslateResult, String> {
    // 先把需要的数据 clone 出来，避免跨 await 持有锁。
    let (cfg, translator) = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        (guard.translate.clone(), state.translator.clone())
    };

    translator
        .translate(&request, &cfg)
        .await
        .map_err(|e| e.to_string())
}

/// 流式翻译：边生成边通过 `translate-chunk` 事件把增量译文发到前端（实时刷新），
/// 返回拼接后的完整译文。
#[tauri::command]
pub async fn translate_stream(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: TranslateRequest,
) -> Result<TranslateResult, String> {
    let (cfg, translator) = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        (guard.translate.clone(), state.translator.clone())
    };

    let text = translator
        .stream_translate(&request, &cfg, |delta| {
            let _ = app.emit("translate-chunk", delta.to_string());
        })
        .await
        .map_err(|e| e.to_string())?;

    Ok(TranslateResult {
        translated_text: text,
        detected_lang: None,
    })
}

/// 返回当前配置。
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    Ok(state.config.lock().map_err(|e| e.to_string())?.clone())
}

/// 保存配置到本地，并更新内存中的状态。
#[tauri::command]
pub fn set_config(state: State<'_, AppState>, mut config: Config) -> Result<(), String> {
    // 授权信息由激活流程单独写入，设置对话框只管模型/OCR/排版；整体替换前保留已激活票据，
    // 避免用户改个模型或排版方向就把 Pro 激活给抹掉。
    let mut guard = state.config.lock().map_err(|e| e.to_string())?;
    config.license = guard.license.clone();
    config.save().map_err(|e| e.to_string())?;
    *guard = config;
    Ok(())
}

/// 授权状态：前端据此显示「免费 / Pro」与激活提示。
#[derive(serde::Serialize)]
pub struct LicenseStatus {
    pub pro: bool,
    pub edition: String,
    pub activated_at: Option<i64>,
    pub message: String,
}

/// 返回当前授权状态（全离线：本地验票据签名 + 核对机器指纹）。
#[tauri::command]
pub fn get_license_status(state: State<'_, AppState>) -> Result<LicenseStatus, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?.clone();
    let mid = license::machine_id().unwrap_or_default();
    if config.license.receipt.trim().is_empty() {
        return Ok(LicenseStatus {
            pro: false,
            edition: "free".to_string(),
            activated_at: None,
            message: "免费版：导出 Word / 文本 / 译文需激活 Pro".to_string(),
        });
    }
    match license::verify_receipt(&config.license.receipt, &mid) {
        Ok(p) => Ok(LicenseStatus {
            pro: true,
            edition: p.edition,
            activated_at: Some(p.activated_at),
            message: "已激活 Pro".to_string(),
        }),
        Err(e) => Ok(LicenseStatus {
            pro: false,
            edition: "free".to_string(),
            activated_at: None,
            message: format!("授权无效：{e}"),
        }),
    }
}

/// 用激活码在线激活：本地预检 → Worker 验签 + 判窗口 → 存票据。
#[tauri::command]
pub async fn activate_license(
    state: State<'_, AppState>,
    code: String,
) -> Result<LicenseStatus, String> {
    // 本地先验格式，明显打错就不浪费一次网络请求（真伪由 Worker 判）。
    license::validate_code(&code)?;
    let mid = license::machine_id()?;
    let receipt = license::activate_online(&code, &mid).await?;
    // Worker 返回的票据必须验签通过且绑定本机，才落盘。
    let payload = license::verify_receipt(&receipt, &mid)?;

    {
        let mut guard = state.config.lock().map_err(|e| e.to_string())?;
        guard.license.code = code;
        guard.license.receipt = receipt;
        guard.license.machine_id = mid;
        guard.save().map_err(|e| e.to_string())?;
    }

    Ok(LicenseStatus {
        pro: true,
        edition: payload.edition,
        activated_at: Some(payload.activated_at),
        message: "已激活 Pro".to_string(),
    })
}

/// Pro 门控：导出类命令在入口处调用，未激活直接拒绝。
fn require_pro(state: &State<'_, AppState>) -> Result<(), String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    if license::is_pro(&config) {
        Ok(())
    } else {
        Err("本功能为 Pro 版专属，请先在设置中激活".to_string())
    }
}

/// `ocr_image` 的参数：一张页面的 PNG data URL。
#[derive(serde::Deserialize)]
pub struct OcrRequest {
    pub png_data_url: String,
}

/// 对一张页面图片做 OCR（扫描版 PDF 用），按配置分派到本地 OCR 或大模型 OCR。
/// 返回整页文本与逐行包围盒（本地 OCR）；大模型 OCR 仅有文本、无包围盒。
#[tauri::command]
pub async fn ocr_image(
    state: State<'_, AppState>,
    request: OcrRequest,
) -> Result<ocr::OcrPageResult, String> {
    let (cfg, translator) = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        (guard.clone(), state.translator.clone())
    };

    // 记录实际分派的 OCR 方式，便于核对「到底走的是 GLM 还是本地 OCR」。
    append_log(&format!(
        "OCR 调用：mode={}，图片 data_url {} 字节",
        cfg.ocr.mode,
        request.png_data_url.len()
    ));

    if cfg.ocr.mode == "glm" {
        let text = translator
            .ocr_glm(&request.png_data_url, &cfg)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(ocr::OcrPageResult { text, lines: Vec::new() });
    }

    if cfg.ocr.mode == "llm" {
        let text = translator
            .ocr(&request.png_data_url, &cfg)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(ocr::OcrPageResult { text, lines: Vec::new() });
    }

    // 本地 OCR 是阻塞调用（WinRT），放到阻塞线程池执行，避免卡住异步运行时。
    let bytes = reader::from_data_url(&request.png_data_url).map_err(|e| e.to_string())?;
    let lang = cfg.ocr.lang.clone();
    tauri::async_runtime::spawn_blocking(move || ocr::ocr_image(&bytes, &lang))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// 仅用本地 Windows OCR 识别一张页面图片（忽略配置的 OCR 方式），用于排版方向检测等
/// 需要离线、零成本、结果确定的场景（扫描版古籍自动判断横排/竖排）。返回整页文本与逐行包围盒。
#[tauri::command]
pub async fn ocr_image_local(
    state: State<'_, AppState>,
    request: OcrRequest,
) -> Result<ocr::OcrPageResult, String> {
    let lang = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard.ocr.lang.clone()
    };
    let bytes = reader::from_data_url(&request.png_data_url).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || ocr::ocr_image(&bytes, &lang))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// `extract_page` 的参数：一张页面的 PNG data URL。
#[derive(serde::Deserialize)]
pub struct ExtractPageRequest {
    pub png_data_url: String,
}

/// 用视觉大模型把一页图片解析成结构化内容（正文/表格/图片），用于高质量导出。
#[tauri::command]
pub async fn extract_page(
    state: State<'_, AppState>,
    request: ExtractPageRequest,
) -> Result<ExtractedPage, String> {
    let (cfg, translator) = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        (guard.clone(), state.translator.clone())
    };
    translator
        .extract_page(&request.png_data_url, &cfg)
        .await
        .map_err(|e| e.to_string())
}

/// `export_text` 的参数：要导出的文本 + 建议文件名。
#[derive(serde::Deserialize)]
pub struct ExportRequest {
    pub text: String,
    pub suggested_name: String,
}

/// 弹出保存对话框，把文本写入 `.txt` 文件，返回保存路径。
#[tauri::command]
pub async fn export_text(
    state: State<'_, AppState>,
    request: ExportRequest,
) -> Result<String, String> {
    require_pro(&state)?;
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("文本文件", &["txt"])
        .set_file_name(&request.suggested_name)
        .save_file()
        .await
        .ok_or_else(|| "已取消导出".to_string())?;

    let path = handle.path().to_path_buf();
    // 纯文本导出时把 LaTeX 公式转成可读的 Unicode 数学（.docx 走原生 OMML，不受影响）。
    let text = crate::translate::latex::latex_math_to_unicode(&request.text);
    std::fs::write(&path, &text).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Word 导出的内嵌图片：PNG data URL。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DocxImage {
    pub png_data_url: String,
}

/// Word 导出的一段内容：`level` 为大纲级别（0=正文，1/2/3=一级/二级/三级标题）。
///
/// 段落可以是三种形态之一：普通文字（`text`）、表格（`table`）、图片（`image`）。
/// 表格/图片优先于文字渲染。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DocxParagraph {
    pub level: u8,
    pub text: String,
    /// 字号（pt）；0 表示不设置、沿用 Word 默认。
    #[serde(default)]
    pub size: f32,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub center: bool,
    /// 相对正文块左缘的左缩进（pt）；0 表示不缩进。
    #[serde(default)]
    pub indent: f32,
    /// 表格（行 × 列）；有则本段渲染成 Word 表格。
    #[serde(default)]
    pub table: Option<Vec<Vec<String>>>,
    /// 图片；有则本段渲染成内嵌图片。
    #[serde(default)]
    pub image: Option<DocxImage>,
}

/// `export_docx` 的参数：结构化段落列表 + 建议文件名。
#[derive(serde::Deserialize)]
pub struct ExportDocxRequest {
    pub paragraphs: Vec<DocxParagraph>,
    pub suggested_name: String,
}

/// 把一组结构化段落翻译成目标语言，返回同结构段落（标题层级 / 字号 / 加粗 / 居中 /
/// 缩进 / 表格 / 图片原样保留，只替换文字）。
///
/// 公式用 `⟦M数字⟧` 占位符保护、不参与翻译，翻译后按原样还原（保留 `$...$` /
/// `$$...$$` 定界符），再交给 [`export_docx`] 走同一套 OMML 注入，保证翻译版 Word
/// 与原文版格式一致。
///
/// 翻译采用**有界并发**（默认 6 路），逐段/逐单元格并发请求，避免串行请求的往返延迟
/// 叠加；期间通过 `translate-progress` 事件向前端上报 `{ done, total }` 进度。
#[tauri::command]
pub async fn translate_paragraphs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    paragraphs: Vec<DocxParagraph>,
) -> Result<Vec<DocxParagraph>, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::task::JoinSet;

    require_pro(&state)?;

    let (cfg, translator) = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        (guard.translate.clone(), state.translator.clone())
    };

    // 把「需要翻译的文本单元」拍平：正文段落 + 表格单元格。
    // `cell` 为 None 表示整段正文；Some((r,c)) 表示表格第 r 行第 c 列。
    struct Unit {
        para: usize,
        cell: Option<(usize, usize)>,
        text: String,
    }
    let mut units: Vec<Unit> = Vec::new();
    for (i, p) in paragraphs.iter().enumerate() {
        if p.image.is_some() {
            continue; // 图片段落原样保留
        }
        if let Some(tbl) = &p.table {
            for (r, row) in tbl.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    if !cell.trim().is_empty() {
                        units.push(Unit {
                            para: i,
                            cell: Some((r, c)),
                            text: cell.trim().to_string(),
                        });
                    }
                }
            }
            continue;
        }
        if p.text.trim().is_empty() {
            continue; // 空段落原样保留（用于段落间距）
        }
        units.push(Unit { para: i, cell: None, text: p.text.clone() });
    }

    let total = units.len();
    const CONCURRENCY: usize = 6;

    let mut results: Vec<Option<Result<String, String>>> = (0..total).map(|_| None).collect();
    let done = Arc::new(AtomicUsize::new(0));
    let mut set = JoinSet::new();
    let mut next = 0usize;

    // 填充初始并发窗口。
    while next < total && set.len() < CONCURRENCY {
        let (translator, cfg, text) = (translator.clone(), cfg.clone(), units[next].text.clone());
        let idx = next;
        next += 1;
        set.spawn(async move {
            let r = translate_protecting_math(
                &translator,
                &cfg,
                &text,
                &cfg.source_lang,
                &cfg.target_lang,
            )
            .await;
            (idx, r)
        });
    }

    // 滑动窗口：完成一个补一个，保持并发度；结果按 idx 写回，顺序与输入一致。
    while let Some(joined) = set.join_next().await {
        let (idx, r) = match joined {
            Ok(v) => v,
            Err(e) => return Err(format!("翻译任务异常：{e}")),
        };
        results[idx] = Some(r);
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = app.emit("translate-progress", serde_json::json!({ "done": n, "total": total }));

        if next < total {
            let (translator, cfg, text) = (translator.clone(), cfg.clone(), units[next].text.clone());
            let idx = next;
            next += 1;
            set.spawn(async move {
                let r = translate_protecting_math(
                    &translator,
                    &cfg,
                    &text,
                    &cfg.source_lang,
                    &cfg.target_lang,
                )
                .await;
                (idx, r)
            });
        }
    }

    // 按原顺序写回结果，保持段落 / 表格结构不变。
    let mut out = paragraphs;
    let mut first_err: Option<String> = None;
    for (k, u) in units.iter().enumerate() {
        let r = match results[k].take() {
            Some(r) => r,
            None => return Err("内部错误：翻译结果缺失".to_string()),
        };
        match r {
            Ok(text) => {
                let p = &mut out[u.para];
                if let Some((r, c)) = u.cell {
                    if let Some(cell) = p.table.as_mut().and_then(|t| t.get_mut(r)).and_then(|row| row.get_mut(c)) {
                        *cell = text;
                    }
                } else {
                    p.text = text;
                }
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if let Some(e) = first_err {
        return Err(e);
    }
    Ok(out)
}

/// 翻译一段含 LaTeX 公式的文本：先把公式替换成 `⟦M数字⟧` 占位符，翻译后再还原成带
/// 定界符的 LaTeX（`$...$` 行内 / `$$...$$` 块级），保证 `export_docx` 能再次识别。
async fn translate_protecting_math(
    translator: &crate::translate::client::TranslateClient,
    cfg: &crate::config::TranslateConfig,
    text: &str,
    source_lang: &str,
    target_lang: &str,
) -> Result<String, String> {
    use crate::translate::latex::{self, MathSegment};

    let segments = latex::split_math(text);
    let has_math = segments.iter().any(|s| matches!(s, MathSegment::Math { .. }));
    if !has_math {
        return translator
            .translate_preserving(text, source_lang, target_lang, cfg)
            .await
            .map_err(|e| e.to_string());
    }

    let mut translatable = String::new();
    let mut math: Vec<(String, bool)> = Vec::new(); // (latex, display)
    for seg in &segments {
        match seg {
            MathSegment::Text(t) => translatable.push_str(t),
            MathSegment::Math { latex, display } => {
                let idx = math.len();
                translatable.push_str(&format!("⟦M{}⟧", idx));
                math.push((latex.clone(), *display));
            }
        }
    }

    let mut translated = translator
        .translate_preserving(&translatable, source_lang, target_lang, cfg)
        .await
        .map_err(|e| e.to_string())?;

    // 按逆序还原占位符，避免编号前缀冲突（M1 不会误替换 M10）。
    for idx in (0..math.len()).rev() {
        let (latex, display) = &math[idx];
        let placeholder = format!("⟦M{}⟧", idx);
        let rebuilt = if *display {
            format!("$${}$$", latex)
        } else {
            format!("${}$", latex)
        };
        translated = translated.replace(&placeholder, &rebuilt);
    }
    Ok(translated)
}

/// 生成带大纲级别的 `.docx`：尽量保留原 PDF 排版（每行独立成段以保留换行、
/// 应用原文字号 / 加粗 / 斜体 / 居中，标题设置大纲级别使其在导航窗格可见）。
///
/// 正文里的 LaTeX 公式（`$...$` / `$$...$$`）会在**本地**（确定性、无网络）
/// 转成 Word 原生可编辑公式（OMML，`<m:oMath>`）嵌入文档，无需安装 MathType
/// 即可在 Word 里双击编辑。
#[tauri::command]
pub async fn export_docx(
    state: State<'_, AppState>,
    request: ExportDocxRequest,
) -> Result<String, String> {
    use docx_rs::{
        AlignmentType, Docx, LineSpacing, Paragraph, Pic, Run, Style, StyleType, Table, TableCell,
        TableRow,
    };
    use crate::translate::latex::{self, MathSegment};

    require_pro(&state)?;

    let handle = rfd::AsyncFileDialog::new()
        .add_filter("Word 文档", &["docx"])
        .set_file_name(&request.suggested_name)
        .save_file()
        .await
        .ok_or_else(|| "已取消导出".to_string())?;

    // 三个内置标题样式：outline_lvl 从 0 开始（0=一级标题，1=二级，2=三级）。
    // 导航窗格 / 样式库据此识别标题；视觉格式由下面的 run 级样式覆盖，保持与原 PDF 一致。
    let mut doc = Docx::new();
    for (id, name, outline) in [
        ("Heading1", "Heading 1", 0),
        ("Heading2", "Heading 2", 1),
        ("Heading3", "Heading 3", 2),
    ] {
        doc = doc.add_style(
            Style::new(id, StyleType::Paragraph)
                .name(name)
                .outline_lvl(outline),
        );
    }

    // 公式的 OMML 片段，与占位符 @@OMML{n}@@ 一一对应；注入 document.xml 后即为
    // Word 原生可编辑公式。
    let mut omml_replacements: Vec<String> = Vec::new();
    let mut omml_index = 0usize;

    // 判断文档语言：译文/中文原文走中文排版，英文原文走西文排版。
    let is_chinese = request.paragraphs.iter().any(|p| is_cjk(&p.text));

    for p in &request.paragraphs {
        // 图片：把 PNG 内嵌为插图，居中显示（尺寸由 Pic::new 按像素自动换算）。
        if let Some(img) = &p.image {
            let bytes = reader::from_data_url(&img.png_data_url).map_err(|e| e.to_string())?;
            let pic = Pic::new(&bytes);
            doc = doc.add_paragraph(
                Paragraph::new().add_run(Run::new().add_image(pic)).align(AlignmentType::Center),
            );
            continue;
        }

        // 表格：按行列还原成 Word 表格（Table::new 默认自带全部网格线）。
        if let Some(tbl) = &p.table {
            let rows: Vec<TableRow> = tbl
                .iter()
                .map(|row| {
                    TableRow::new(
                        row.iter()
                            .map(|cell| {
                                TableCell::new().add_paragraph(
                                    Paragraph::new().add_run(
                                        academic_fonts(Run::new().add_text(cell.trim())).color("000000"),
                                    ),
                                )
                            })
                            .collect(),
                    )
                })
                .collect();
            if !rows.is_empty() {
                doc = doc.add_table(Table::new(rows));
            }
            continue;
        }

        // 空行：输出一个空段落以保留原文档的段落间距。
        if p.text.trim().is_empty() {
            doc = doc.add_paragraph(
                Paragraph::new().line_spacing(LineSpacing::new().before(0).after(0)),
            );
            continue;
        }

        // 正文：按文本/公式切分。块级公式（$$…$$ / \[…\]）单独成段并居中，
        // 行内公式与相邻文本之间补空格，再统一注入 OLE 对象。
        let segments = latex::split_math(&p.text);

        // 以块级公式为边界拆段：块级公式独占一段（居中），其余文本/行内公式合为一段。
        // 块级公式后紧跟的纯标点（strip_trailing_punct 剥离出的、或定界符外的句末标点）
        // 附着到公式同一段，避免标点独自成行。
        let mut group: Vec<MathSegment> = Vec::new();
        let mut emitted = false;
        let mut i = 0;
        while i < segments.len() {
            if matches!(&segments[i], MathSegment::Math { display: true, .. }) {
                if let Some(par) = build_math_paragraph(
                    p, &group, false, is_chinese,
                    &mut omml_replacements, &mut omml_index,
                ) {
                    doc = doc.add_paragraph(par);
                    emitted = true;
                }
                group.clear();
                // 收集公式后紧邻的纯标点 Text 段，与公式合并成同一居中段落。
                let mut disp: Vec<MathSegment> = vec![segments[i].clone()];
                let mut k = i + 1;
                while k < segments.len() {
                    if let MathSegment::Text(t) = &segments[k] {
                        if latex::is_trailing_punct(t) {
                            disp.push(segments[k].clone());
                            k += 1;
                            continue;
                        }
                    }
                    break;
                }
                if let Some(par) = build_math_paragraph(
                    p, &disp, true, is_chinese,
                    &mut omml_replacements, &mut omml_index,
                ) {
                    doc = doc.add_paragraph(par);
                    emitted = true;
                }
                i = k;
            } else {
                group.push(segments[i].clone());
                i += 1;
            }
        }
        if let Some(par) = build_math_paragraph(
            p, &group, false, is_chinese,
            &mut omml_replacements, &mut omml_index,
        ) {
            doc = doc.add_paragraph(par);
            emitted = true;
        }
        if !emitted {
            doc = doc.add_paragraph(
                Paragraph::new().line_spacing(LineSpacing::new().before(0).after(0)),
            );
        }
    }

    // 生成 OPC 部件，再注入 OMML 公式片段。
    let mut xmldoc = doc.build();
    xmldoc.document =
        inject_omml_document(std::mem::take(&mut xmldoc.document), &omml_replacements);

    pack_docx(xmldoc, handle.path())?;
    Ok(handle.path().to_string_lossy().into_owned())
}

/// 学术论文标准中西文字体：西文 Times New Roman、中文宋体。
/// Word 需在 run 级同时指定 w:ascii / w:hAnsi（西文）与 w:eastAsia（中文），
/// 中西文混排时才会各自套用对应字体。
fn academic_fonts(run: docx_rs::Run) -> docx_rs::Run {
    run.fonts(
        docx_rs::RunFonts::new()
            .ascii("Times New Roman")
            .hi_ansi("Times New Roman")
            .east_asia("宋体")
            .cs("Times New Roman"),
    )
}

/// 判断文本是否含 CJK 字符（用于区分中文 / 英文文档）。
fn is_cjk(text: &str) -> bool {
    text.chars().any(|c| (0x4E00..=0x9FFF).contains(&(c as u32)))
}

/// 按语言 + 大纲层级给 run 套字体。
/// - 中文标题（level>0）：黑体；正文 / 英文：宋体（英文里 eastAsia 无实际作用）。
/// - 西文统一 Times New Roman。
fn run_fonts(run: docx_rs::Run, is_chinese: bool, level: u8) -> docx_rs::Run {
    let east_asia = if is_chinese && level > 0 { "黑体" } else { "宋体" };
    run.fonts(
        docx_rs::RunFonts::new()
            .ascii("Times New Roman")
            .hi_ansi("Times New Roman")
            .east_asia(east_asia)
            .cs("Times New Roman"),
    )
}

/// 按语言 + 大纲层级返回字号（docx-rs 用半点：w:sz = pt × 2）。
/// - 中文：一级小四 12pt；二级/三级/正文五号 10.5pt。
/// - 英文：一级小二 18pt；二级小四 12pt；三级/正文五号 10.5pt。
fn run_size(is_chinese: bool, level: u8) -> usize {
    match (is_chinese, level) {
        (true, 1) => 24,  // 小四 12pt
        (true, _) => 21,  // 五号 10.5pt
        (false, 1) => 36, // 小二 18pt
        (false, 2) => 24, // 小四 12pt
        (false, _) => 21, // 五号 10.5pt
    }
}

/// 把一组片段渲染成一个 Word 段落。
///
/// `display` 为 true 表示本段是一个居中的块级公式；否则按 `p` 的样式（标题层级 /
/// 加粗 / 斜体 / 字号 / 居中 / 缩进）渲染，并在行内公式与相邻文本之间补一个空格。
/// 返回 `None` 表示没有任何可渲染内容。
#[allow(clippy::too_many_arguments)]
fn build_math_paragraph(
    p: &DocxParagraph,
    segments: &[crate::translate::latex::MathSegment],
    display: bool,
    is_chinese: bool,
    omml_replacements: &mut Vec<String>,
    omml_index: &mut usize,
) -> Option<docx_rs::Paragraph> {
    use crate::translate::latex::{self, MathSegment};

    let mut paragraph = docx_rs::Paragraph::new().line_spacing(match p.level {
        // 段落间距（twip，1pt = 20）：正文段后留 6pt，标题段前留空、段后留 4pt。
        1 => docx_rs::LineSpacing::new().before(240).after(80),
        2 => docx_rs::LineSpacing::new().before(200).after(80),
        3 => docx_rs::LineSpacing::new().before(160).after(80),
        _ => docx_rs::LineSpacing::new().before(0).after(120),
    });
    if display || p.center {
        paragraph = paragraph.align(docx_rs::AlignmentType::Center);
    } else {
        // 正文（含列表缩进段）两端对齐；标题保持样式默认（Heading 通常左对齐）。
        if p.level == 0 {
            paragraph = paragraph.align(docx_rs::AlignmentType::Both);
        }
        if p.indent > 0.5 {
            // 左缩进单位是 twip（1pt = 20 twip），居中时不叠加缩进。
            paragraph = paragraph.indent(Some((p.indent * 20.0).round() as i32), None, None, None);
        }
    }
    if !display {
        match p.level {
            1 => paragraph = paragraph.style("Heading1"),
            2 => paragraph = paragraph.style("Heading2"),
            3 => paragraph = paragraph.style("Heading3"),
            _ => {}
        }
    }

    // 行内公式与相邻文本之间补空格：prev_ended_space 记录「上一个 run 是否以空白结尾」，
    // 为 false 时给下一个 run 补前导空格。
    let mut prev_ended_space = true;
    let mut has_content = false;

    for seg in segments {
        match seg {
            MathSegment::Text(t) => {
                let content = t.trim();
                if content.is_empty() {
                    // 纯空白片段：作为前后内容之间的分隔，渲染一个空格。
                    if !t.is_empty() && has_content {
                        paragraph = paragraph.add_run(docx_rs::Run::new().add_text(" "));
                        prev_ended_space = true;
                    }
                    continue;
                }
                let mut s = content.to_string();
                // 纯标点（公式后剥离出的逗号/句号等）不补前导空格：`x,` 而非 `x ,`。
                if !prev_ended_space && !latex::is_trailing_punct(&s) {
                    s.insert(0, ' ');
                }
                let mut run = run_fonts(docx_rs::Run::new().add_text(s), is_chinese, p.level);
                // 英文三级标题强制加粗；其余保留原 PDF 的加粗标记。
                if p.bold || (!is_chinese && p.level == 3) {
                    run = run.bold();
                }
                if p.italic {
                    run = run.italic();
                }
                run = run.size(run_size(is_chinese, p.level));
                // 固定黑色，避免标题样式默认的蓝色；run 级样式优先于段落样式。
                run = run.color("000000");
                paragraph = paragraph.add_run(run);
                prev_ended_space = false;
                has_content = true;
            }
            MathSegment::Math { latex, .. } => {
                let key = latex.trim().to_string();
                if !prev_ended_space {
                    paragraph = paragraph.add_run(docx_rs::Run::new().add_text(" "));
                }
                let n = *omml_index;
                let omml = crate::translate::omml::latex_to_omml(&key);
                omml_replacements.push(omml);
                paragraph = paragraph.add_run(docx_rs::Run::new().add_text(format!("@@OMML{}@@", n)));
                *omml_index += 1;
                prev_ended_space = false;
                has_content = true;
            }
        }
    }

    if has_content { Some(paragraph) } else { None }
}

/// 把 `@@OMML{n}@@` 占位符替换成 `<m:oMath>` 片段。
/// 占位符位于某个 `<w:r>` 的 `<w:t>` 文本内；`<m:oMath>` 是 `<w:p>` 的子节点（run 的
/// 兄弟），故替换时先关闭该 run，插入公式片段，再重开一个空 run，保证 XML 结构合法。
/// 公式片段自带 `xmlns:m` / `xmlns:w` 命名空间声明，无需改动根元素。
fn inject_omml_document(document: Vec<u8>, replacements: &[String]) -> Vec<u8> {
    let mut xml = String::from_utf8(document).unwrap_or_default();
    for (i, repl) in replacements.iter().enumerate() {
        xml = xml.replace(
            &format!("@@OMML{}@@", i),
            &format!("</w:t></w:r>{repl}<w:r><w:t>"),
        );
    }
    xml.into_bytes()
}

/// 把 `XMLDocx` 各部件写成 `.docx`（OPC zip）。
fn pack_docx(
    xmldoc: docx_rs::XMLDocx,
    path: &std::path::Path,
) -> Result<(), String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let docx_rs::XMLDocx {
        content_type,
        rels,
        doc_props,
        styles,
        document,
        comments,
        document_rels,
        settings,
        font_table,
        numberings,
        media,
        headers,
        header_rels,
        footers,
        footer_rels,
        comments_extended,
        footnotes,
        ..
    } = xmldoc;

    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);

    let dir_opts = SimpleFileOptions::default();
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);

    zip.add_directory("word/", dir_opts).map_err(|e| e.to_string())?;
    zip.add_directory("word/_rels", dir_opts).map_err(|e| e.to_string())?;
    zip.add_directory("_rels/", dir_opts).map_err(|e| e.to_string())?;
    zip.add_directory("docProps/", dir_opts).map_err(|e| e.to_string())?;

    let parts: [(&str, &[u8]); 14] = [
        ("[Content_Types].xml", &content_type),
        ("_rels/.rels", &rels),
        ("docProps/app.xml", &doc_props.app),
        ("docProps/core.xml", &doc_props.core),
        ("docProps/custom.xml", &doc_props.custom),
        ("word/_rels/document.xml.rels", &document_rels),
        ("word/document.xml", &document),
        ("word/styles.xml", &styles),
        ("word/settings.xml", &settings),
        ("word/fontTable.xml", &font_table),
        ("word/comments.xml", &comments),
        ("word/numbering.xml", &numberings),
        ("word/commentsExtended.xml", &comments_extended),
        ("word/footnotes.xml", &footnotes),
    ];
    for (p, data) in parts {
        zip.start_file(p, opts).map_err(|e| e.to_string())?;
        zip.write_all(data).map_err(|e| e.to_string())?;
    }

    for (i, h) in headers.iter().enumerate() {
        zip.start_file(format!("word/header{}.xml", i + 1), opts).map_err(|e| e.to_string())?;
        zip.write_all(h).map_err(|e| e.to_string())?;
        if let Some(r) = header_rels.get(i) {
            zip.start_file(format!("word/_rels/header{}.xml.rels", i + 1), opts)
                .map_err(|e| e.to_string())?;
            zip.write_all(r).map_err(|e| e.to_string())?;
        }
    }
    for (i, f) in footers.iter().enumerate() {
        zip.start_file(format!("word/footer{}.xml", i + 1), opts).map_err(|e| e.to_string())?;
        zip.write_all(f).map_err(|e| e.to_string())?;
        if let Some(r) = footer_rels.get(i) {
            zip.start_file(format!("word/_rels/footer{}.xml.rels", i + 1), opts)
                .map_err(|e| e.to_string())?;
            zip.write_all(r).map_err(|e| e.to_string())?;
        }
    }

    // 已有插图（DocxParagraph.image 的 Pic）放进 word/media/。
    if !media.is_empty() {
        zip.add_directory("word/media/", dir_opts).map_err(|e| e.to_string())?;
        for (id, bytes) in media {
            zip.start_file(format!("word/media/{}.png", id), opts).map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
        }
    }

    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// 把空串转成 `None`，避免把空字符串当值传给 pdfomml 的 CLI 开关。
fn nonempty(s: String) -> Option<String> {
    let t = s.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// 按优先级收集 pdfomml 后端候选（只做静态查找，不启动进程）：
/// 1) 打进安装包的 PyInstaller sidecar（生产环境 `<resource_dir>/pdfomml/pdfomml.exe`）；
/// 2) 源码目录里的 sidecar（开发环境 `cargo tauri dev`）；
/// 3) 兜底自动发现（`PDFOMML_BIN` / `PDFOMML_PYTHON` 环境变量、PATH 上的 `pdfomml` / `python`）。
/// 前两者免 Python，「换一台电脑也能跑」。
fn pdfomml_candidates(app: &tauri::AppHandle) -> Vec<Backend> {
    let mut out = Vec::new();
    let res_dir = app.path().resource_dir().map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<不可用:{e}>"));
    let mut notes = vec![format!("resource_dir={res_dir}")];
    if let Ok(res) = app.path().resource_dir() {
        let exe = res.join("pdfomml").join("pdfomml.exe");
        if exe.is_file() {
            notes.push(format!("打包候选存在:{}", exe.display()));
            out.push(Backend::Executable(exe));
        } else {
            notes.push(format!("打包候选缺失:{}", exe.display()));
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..").join("pdfomml").join("rust").join("pdfomml")
        .join("sidecar").join("pdfomml").join("pdfomml.exe");
    if let Ok(canon) = std::fs::canonicalize(&dev) {
        if canon.is_file() {
            notes.push(format!("源码候选存在:{}", canon.display()));
            out.push(Backend::Executable(canon));
        }
    } else {
        notes.push(format!("源码候选缺失:{}", dev.display()));
    }
    out.extend(Backend::candidates());
    append_log(&format!("pdfomml 候选清单（共 {} 个）：{}", out.len(), notes.join("；")));
    out
}

/// 解析一个**可用**的 pdfomml 后端：按候选顺序逐个探测（跑 `--list-engines`），
/// 返回第一个能通过能力探测的；全部失败则返回最后一条错误。
///
/// 不能只看 `is_file()` 就返回——`target/debug/pdfomml/` 里可能残留被 Tauri 资源
/// 打包「扁平化」弄坏的 sidecar（缺 `_internal/`），看着存在实则跑不起来。
fn resolve_pdfomml_backend(app: &tauri::AppHandle) -> Result<Backend, String> {
    let candidates = pdfomml_candidates(app);
    if candidates.is_empty() {
        return Err("未找到 pdfomml 后端：请确认 sidecar 已打包，或本机已安装 pdfomml（pip install .）"
            .to_string());
    }
    let mut tried: Vec<String> = Vec::new();
    for (i, b) in candidates.iter().enumerate() {
        match b.probe() {
            Ok(_) => {
                append_log(&format!("pdfomml 后端命中第 {} 个候选", i + 1));
                return Ok(b.clone());
            }
            Err(e) => tried.push(format!("候选{}（{}）", i + 1, e)),
        }
    }
    // 逐个候选的失败原因写进 diag.log，方便离线定位（错误本身会先于写日志返回给前端）。
    append_log(&format!(
        "pdfomml 后端探测全部失败（{} 个候选）：{}",
        candidates.len(),
        tried.join("  ||  ")
    ));
    Err(format!(
        "pdfomml 后端不可用（已尝试 {} 个候选）：{}",
        candidates.len(),
        tried.join("；")
    ))
}

/// 按统一识别引擎（`ocr.mode`）组装 Converter：`vlm` → OpenAI 兼容视觉、`glm-ocr` → 智谱、
/// 其余（`null`）→ 离线不识别（公式区域保留为图片，文字/表格/插图完整）。
///
/// `translate` 非空时额外把正文翻译成目标语言（`--translate` 及接口参数追加到 CLI）。
fn build_native_converter(
    app: &tauri::AppHandle,
    cfg: &crate::config::Config,
    pages: Option<String>,
    translate: Option<&crate::config::TranslateConfig>,
) -> Result<Converter, String> {
    let backend = resolve_pdfomml_backend(app)?;
    let mut builder = Converter::builder().backend(backend).pages(pages);
    builder = match cfg.word_engine() {
        "glm-ocr" => builder.engine(Engine::GlmOcr {
            api_key: nonempty(cfg.word.glm_api_key.clone()),
            api_base: None,
            model: None,
        }),
        "vlm" => builder.engine(Engine::Vlm {
            api_base: nonempty(cfg.word.api_base.clone()),
            api_key: nonempty(cfg.word.api_key.clone()),
            model: nonempty(cfg.word.model.clone()),
        }),
        _ => builder.engine(Engine::Null),
    };
    if cfg.word.formula_format == "mathtype" {
        builder = builder.extra_args(["--formula-format".to_string(), "mathtype".to_string()]);
    }
    if let Some(t) = translate {
        let target = t.target_lang.trim().to_string();
        if !target.is_empty() {
            let mut args = vec!["--translate".to_string(), target];
            if let Some(src) = nonempty(t.source_lang.clone()) {
                args.push("--translate-source".to_string());
                args.push(src);
            }
            if let Some(base) = nonempty(t.base_url.clone()) {
                args.push("--translate-api-base".to_string());
                args.push(base);
            }
            if let Some(key) = nonempty(t.api_key.clone()) {
                args.push("--translate-api-key".to_string());
                args.push(key);
            }
            if let Some(model) = nonempty(t.model.clone()) {
                args.push("--translate-model".to_string());
                args.push(model);
            }
            builder = builder.extra_args(args);
        }
    }
    builder.build().map_err(|e| e.to_string())
}

/// 用 pdfomml 把原始 PDF 直接转成带 Word 原生可编辑公式（OMML）的 `.docx`。
///
/// 与 [`export_docx`]（前端提取文字 + docx_rs 重建）不同，这里整份 PDF 交给
/// pdfomml 处理：文字层抽取 + 公式区域裁剪识别 + 扫描页整页 OCR + 表格/插图还原，
/// 公式在 Word 里双击即可编辑。引擎由「设置 → 原生 Word」决定。
#[tauri::command]
pub async fn export_word_native(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
    suggested_name: String,
    pages: Option<String>,
) -> Result<String, String> {
    require_pro(&state)?;
    if !std::path::Path::new(&path).is_file() {
        return Err("未找到 PDF 文件".to_string());
    }
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let converter = build_native_converter(&app, &cfg, pages, None)?;

    let handle = rfd::AsyncFileDialog::new()
        .add_filter("Word 文档", &["docx"])
        .set_file_name(&suggested_name)
        .save_file()
        .await
        .ok_or_else(|| "已取消导出".to_string())?;
    let out = handle.path().to_path_buf();

    // 转换是阻塞子进程（扫描件可能数秒到数分钟），丢到 spawn_blocking 避免卡住 tokio 线程。
    // 逐页进度通过 `export-native-progress` 事件上报，前端状态栏显示「正在导出 X/Y 页」。
    let path_for_block = path.clone();
    let out_for_block = out.clone();
    let app_for_progress = app.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        converter.convert_with_progress(
            &path_for_block,
            Some(&out_for_block),
            move |done: u32, total: u32, phase: String| {
                let _ = app_for_progress.emit(
                    "export-native-progress",
                    serde_json::json!({ "done": done, "total": total, "phase": phase }),
                );
            },
        )
    })
    .await
    .map_err(|e| format!("转换线程异常：{e}"))?
    .map_err(|e| e.to_string())?;

    append_log(&format!(
        "原生 Word 导出：{}（引擎 {}，公式格式 {}，{}）",
        out.display(),
        report.engine,
        cfg.word.formula_format,
        report.summary()
    ));
    Ok(out.to_string_lossy().into_owned())
}

/// 用 pdfomml 把原始 PDF 直接转成「翻译版」`.docx`：正文翻译成目标语言，
/// 公式/插图保留，公式格式与 [`export_word_native`] 一致（omml/mathtype）。
/// 翻译走 [`crate::config::TranslateConfig`]（base_url/api_key/model），
/// 公式识别引擎仍由「设置 → 原生 Word」决定。
#[tauri::command]
pub async fn export_word_translated_native(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    path: String,
    suggested_name: String,
    pages: Option<String>,
) -> Result<String, String> {
    require_pro(&state)?;
    if !std::path::Path::new(&path).is_file() {
        return Err("未找到 PDF 文件".to_string());
    }
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    if cfg.translate.api_key.trim().is_empty() {
        return Err("尚未配置翻译 API Key，请先打开「设置」填写".to_string());
    }
    let converter = build_native_converter(&app, &cfg, pages, Some(&cfg.translate))?;

    let handle = rfd::AsyncFileDialog::new()
        .add_filter("Word 文档", &["docx"])
        .set_file_name(&suggested_name)
        .save_file()
        .await
        .ok_or_else(|| "已取消导出".to_string())?;
    let out = handle.path().to_path_buf();

    let path_for_block = path.clone();
    let out_for_block = out.clone();
    let app_for_progress = app.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        converter.convert_with_progress(
            &path_for_block,
            Some(&out_for_block),
            move |done: u32, total: u32, phase: String| {
                let _ = app_for_progress.emit(
                    "export-native-progress",
                    serde_json::json!({ "done": done, "total": total, "phase": phase }),
                );
            },
        )
    })
    .await
    .map_err(|e| format!("转换线程异常：{e}"))?
    .map_err(|e| e.to_string())?;

    append_log(&format!(
        "原生 Word 译文导出：{}（引擎 {}，公式格式 {}，译文 {}，{}）",
        out.display(),
        report.engine,
        cfg.word.formula_format,
        cfg.translate.target_lang,
        report.summary()
    ));
    Ok(out.to_string_lossy().into_owned())
}

#[cfg(test)]
mod docx_omml_tests {
    use super::*;
    use std::io::Read;

    /// 结构级验证：走一遍「注入 OMML 占位符 → 自打包」，再解包检查
    /// `<m:oMath>` 注入正确、占位符替换干净、公式结构（上标）保留。
    #[test]
    fn omml_pack_produces_valid_parts() {
        use docx_rs::{Docx, Paragraph, Run};

        let doc = Docx::new().add_paragraph(
            Paragraph::new().add_run(Run::new().add_text("前缀 @@OMML0@@ 后缀")),
        );
        let xmldoc = doc.build();

        let omml = crate::translate::omml::latex_to_omml(r"x^2");

        let mut xmldoc = xmldoc;
        xmldoc.document = inject_omml_document(std::mem::take(&mut xmldoc.document), &[omml]);

        let path = std::env::temp_dir().join(format!("pdfreader_omml_{}.docx", std::process::id()));
        pack_docx(xmldoc, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();

        let mut doc_xml = String::new();
        zip.by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut doc_xml)
            .unwrap();

        assert!(doc_xml.contains("<m:oMath"), "document.xml 应含 <m:oMath>");
        assert!(doc_xml.contains("<m:sSup>"), "x^2 应转成上标结构");
        assert!(!doc_xml.contains("@@OMML"), "占位符应被替换干净");

        let _ = std::fs::remove_file(&path);
    }
}
