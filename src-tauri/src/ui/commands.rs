//! Tauri 命令：前端 `invoke(...)` 调用的 Rust 侧入口。
//!
//! 约定：所有命令的错误都统一映射成 `String` 返回给前端，
//! 便于直接展示为可读的提示信息。

use tauri::State;
use tauri::Emitter;

use crate::{
    app::AppState,
    config::Config,
    library::{self, Library},
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
pub fn set_config(state: State<'_, AppState>, config: Config) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())?;
    *state.config.lock().map_err(|e| e.to_string())? = config;
    Ok(())
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
pub async fn export_text(request: ExportRequest) -> Result<String, String> {
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

/// 公式预览图：前端 MathJax 渲染的 PNG 及其自然尺寸（CSS 像素）。
#[derive(serde::Deserialize)]
pub struct MathPreview {
    pub latex: String,
    pub png_data_url: String,
    /// 预览图自然宽度（CSS 像素，用于换算成 Word 里的显示尺寸）。
    #[serde(default)]
    pub width: f32,
    /// 预览图自然高度（CSS 像素）。
    #[serde(default)]
    pub height: f32,
}

/// `export_docx` 的参数：结构化段落列表 + 建议文件名 + 公式预览图。
#[derive(serde::Deserialize)]
pub struct ExportDocxRequest {
    pub paragraphs: Vec<DocxParagraph>,
    pub suggested_name: String,
    #[serde(default)]
    pub previews: Vec<MathPreview>,
}

/// 汇总正文里所有去重后的 LaTeX 公式源码，供前端批量渲染预览图。
///
/// 与 `export_docx` 使用同一套 `split_math` 切分，保证两边公式集合一致。
#[tauri::command]
pub fn collect_math(paragraphs: Vec<DocxParagraph>) -> Result<Vec<String>, String> {
    use std::collections::BTreeSet;
    let mut set = BTreeSet::new();
    for p in &paragraphs {
        for seg in crate::translate::latex::split_math(&p.text) {
            if let crate::translate::latex::MathSegment::Math { latex, .. } = seg {
                let key = latex.trim().to_string();
                if !key.is_empty() {
                    set.insert(key);
                }
            }
        }
    }
    Ok(set.into_iter().collect())
}

/// 把一组结构化段落翻译成目标语言，返回同结构段落（标题层级 / 字号 / 加粗 / 居中 /
/// 缩进 / 表格 / 图片原样保留，只替换文字）。
///
/// 公式用 `⟦M数字⟧` 占位符保护、不参与翻译，翻译后按原样还原（保留 `$...$` /
/// `$$...$$` 定界符），再交给 [`export_docx`] 走同一套 OLE 注入，保证翻译版 Word
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
/// 转成 MathType OLE 对象（ProgID `Equation.DSMT4`，内含 MTEF 二进制）嵌入文档，
/// 并附带一张前端 MathJax 渲染的 PNG 预览，使 Word 在未装 MathType 时也能正确显示。
#[tauri::command]
pub async fn export_docx(request: ExportDocxRequest) -> Result<String, String> {
    use docx_rs::{
        AlignmentType, Docx, LineSpacing, Paragraph, Pic, Run, Style, StyleType, Table, TableCell,
        TableRow,
    };
    use crate::translate::latex::{self, MathSegment};

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

    // 预览图查表：latex（去定界符、trim 后）→ (PNG 字节, 宽 pt, 高 pt)。
    let mut preview_map: std::collections::HashMap<String, (Vec<u8>, f32, f32)> =
        std::collections::HashMap::new();
    for pv in &request.previews {
        let bytes = match reader::from_data_url(&pv.png_data_url) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // CSS 像素 → 点（1px = 0.75pt，96dpi 约定）。
        let w_pt = (pv.width * 0.75).max(8.0);
        let h_pt = (pv.height * 0.75).max(8.0);
        preview_map.insert(pv.latex.trim().to_string(), (bytes, w_pt, h_pt));
    }

    let mut ole_replacements: Vec<String> = Vec::new(); // 与占位符 @@OLE{n}@@ 一一对应
    let mut ole_bins: Vec<Vec<u8>> = Vec::new(); // word/embeddings/oleObject{n}.bin
    let mut pngs: Vec<Vec<u8>> = Vec::new(); // word/media/image{n}.png
    let mut ole_index = 0usize;

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
                    p, &group, false, is_chinese, &preview_map,
                    &mut ole_replacements, &mut ole_bins, &mut pngs, &mut ole_index,
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
                    p, &disp, true, is_chinese, &preview_map,
                    &mut ole_replacements, &mut ole_bins, &mut pngs, &mut ole_index,
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
            p, &group, false, is_chinese, &preview_map,
            &mut ole_replacements, &mut ole_bins, &mut pngs, &mut ole_index,
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

    // 生成各 OPC 部件，再注入 OLE 对象、关系与内容类型。
    let mut xmldoc = doc.build();
    xmldoc.document =
        inject_ole_document(std::mem::take(&mut xmldoc.document), &ole_replacements);
    xmldoc.document_rels =
        inject_ole_rels(std::mem::take(&mut xmldoc.document_rels), ole_bins.len());
    xmldoc.content_type = inject_ole_content_type(std::mem::take(&mut xmldoc.content_type));

    pack_docx(xmldoc, &ole_bins, &pngs, handle.path())?;
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
    preview_map: &std::collections::HashMap<String, (Vec<u8>, f32, f32)>,
    ole_replacements: &mut Vec<String>,
    ole_bins: &mut Vec<Vec<u8>>,
    pngs: &mut Vec<Vec<u8>>,
    ole_index: &mut usize,
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

    // 公式显示尺寸随正文字号等比缩放：MathJax 默认 16px em ≈ 12pt；
    // 但 MathJax 的 SVG 高度含整行 ascender/descender，视觉上比同字号正文略高，
    // 故分母取 16（介于「过大的 14」与「过小的 18」之间）。正文字号未知时按 12pt 缩放。
    let scale = if p.size > 0.0 { (p.size / 16.0).clamp(0.5, 3.0) } else { 12.0 / 16.0 };

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
                let need_lead = !prev_ended_space;
                match preview_map.get(&key) {
                    Some((png_bytes, w_pt, h_pt)) => {
                        if need_lead {
                            paragraph = paragraph.add_run(docx_rs::Run::new().add_text(" "));
                        }
                        let n = *ole_index;
                        let mtef = crate::translate::mtef::latex_to_mtef(&key);
                        let ole = crate::translate::ole::mtef_to_ole(&mtef);
                        let w_scaled = (w_pt * scale).max(6.0);
                        let h_scaled = (h_pt * scale).max(6.0);
                        // 显示尺寸：pt → twip（1pt = 20 twip）。
                        let twip_w = (w_scaled * 20.0).round().max(20.0) as i64;
                        let twip_h = (h_scaled * 20.0).round().max(20.0) as i64;
                        let shape_id = format!("_x0000_i{}", 1025 + n);
                        let object_id = format!("_{}", 1523517050u64 + n as u64);
                        let img_rid = format!("rId{}", 1000 + n * 2);
                        let ole_rid = format!("rId{}", 1000 + n * 2 + 1);
                        // <w:object> 是 <w:r> 的子节点：只关闭 <w:t>、对象后重开 <w:t>，
                        // 使对象落在占位符所在的 run 内部。
                        let repl = format!(
                            "</w:t><w:object w:dxaOrig=\"{twip_w}\" w:dyaOrig=\"{twip_h}\"><v:shape id=\"{shape_id}\" type=\"#_x0000_t75\" style=\"width:{w_scaled:.2}pt;height:{h_scaled:.2}pt\" o:ole=\"\"><v:imagedata r:id=\"{img_rid}\" o:title=\"\"/></v:shape><o:OLEObject Type=\"Embed\" ProgID=\"Equation.DSMT4\" ShapeID=\"{shape_id}\" DrawAspect=\"Content\" ObjectID=\"{object_id}\" r:id=\"{ole_rid}\"/></w:object><w:t>"
                        );
                        ole_replacements.push(repl);
                        ole_bins.push(ole);
                        pngs.push(png_bytes.clone());
                        paragraph = paragraph.add_run(docx_rs::Run::new().add_text(format!("@@OLE{}@@", n)));
                        *ole_index += 1;
                    }
                    // 无预览（MathJax 渲染失败）：回退为纯文本 run，不丢内容。
                    None => {
                        if need_lead {
                            paragraph = paragraph.add_run(docx_rs::Run::new().add_text(" "));
                        }
                        let mut run = academic_fonts(docx_rs::Run::new().add_text(&key));
                        if p.bold {
                            run = run.bold();
                        }
                        if p.italic {
                            run = run.italic();
                        }
                        run = run.color("000000");
                        paragraph = paragraph.add_run(run);
                    }
                }
                prev_ended_space = false;
                has_content = true;
            }
        }
    }

    if has_content { Some(paragraph) } else { None }
}

/// 把 `@@OLE{n}@@` 占位符替换成 `<w:object>` 片段。
/// 根元素已自带 `xmlns:v` / `xmlns:o` / `xmlns:r`，无需额外命名空间声明。
fn inject_ole_document(document: Vec<u8>, replacements: &[String]) -> Vec<u8> {
    let mut xml = String::from_utf8(document).unwrap_or_default();
    for (i, repl) in replacements.iter().enumerate() {
        xml = xml.replace(&format!("@@OLE{}@@", i), repl);
    }
    xml.into_bytes()
}

/// 往 `document.xml.rels` 追加 OLE 对象与预览图的关系。
fn inject_ole_rels(document_rels: Vec<u8>, ole_count: usize) -> Vec<u8> {
    let mut xml = String::from_utf8(document_rels).unwrap_or_default();
    let mut extra = String::new();
    for i in 0..ole_count {
        let n = i + 1;
        extra.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image{}.png\"/>",
            1000 + i * 2,
            n
        ));
        extra.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject\" Target=\"embeddings/oleObject{}.bin\"/>",
            1000 + i * 2 + 1,
            n
        ));
    }
    if !extra.is_empty() {
        xml = xml.replacen("</Relationships>", &format!("{}</Relationships>", extra), 1);
    }
    xml.into_bytes()
}

/// 往 `[Content_Types].xml` 追加 `bin`（oleObject）Default 扩展名。
fn inject_ole_content_type(content_type: Vec<u8>) -> Vec<u8> {
    let mut xml = String::from_utf8(content_type).unwrap_or_default();
    if !xml.contains("Extension=\"bin\"") {
        xml = xml.replacen(
            "</Types>",
            "<Default Extension=\"bin\" ContentType=\"application/vnd.openxmlformats-officedocument.oleObject\"/></Types>",
            1,
        );
    }
    xml.into_bytes()
}

/// 把 `XMLDocx` 各部件写成 `.docx`（OPC zip），额外追加 OLE 对象与公式预览 PNG。
fn pack_docx(
    xmldoc: docx_rs::XMLDocx,
    ole_bins: &[Vec<u8>],
    pngs: &[Vec<u8>],
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

    // 已有插图（DocxParagraph.image 的 Pic）+ 公式预览 PNG，都放进 word/media/。
    if !media.is_empty() || !pngs.is_empty() {
        zip.add_directory("word/media/", dir_opts).map_err(|e| e.to_string())?;
        for (id, bytes) in media {
            zip.start_file(format!("word/media/{}.png", id), opts).map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        for (i, png) in pngs.iter().enumerate() {
            zip.start_file(format!("word/media/image{}.png", i + 1), opts)
                .map_err(|e| e.to_string())?;
            zip.write_all(png).map_err(|e| e.to_string())?;
        }
    }

    // OLE 对象二进制。
    if !ole_bins.is_empty() {
        zip.add_directory("word/embeddings/", dir_opts).map_err(|e| e.to_string())?;
        for (i, ole) in ole_bins.iter().enumerate() {
            zip.start_file(format!("word/embeddings/oleObject{}.bin", i + 1), opts)
                .map_err(|e| e.to_string())?;
            zip.write_all(ole).map_err(|e| e.to_string())?;
        }
    }

    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod docx_ole_tests {
    use super::*;
    use std::io::Read;

    /// 结构级验证：走一遍「注入 OLE 占位符 → 追加关系/内容类型 → 自打包」，
    /// 再解包检查各部件齐全、占位符替换干净、OLE 二进制以 CFB 魔数开头。
    #[test]
    fn ole_pack_produces_valid_parts() {
        use docx_rs::{Docx, Paragraph, Run};

        let doc = Docx::new().add_paragraph(
            Paragraph::new().add_run(Run::new().add_text("前缀 @@OLE0@@ 后缀")),
        );
        let xmldoc = doc.build();

        let mtef = crate::translate::mtef::latex_to_mtef(r"x^2");
        let ole = crate::translate::ole::mtef_to_ole(&mtef);
        let png: Vec<u8> = b"\x89PNG\r\n\x1a\nfake-png".to_vec();
        let repl = concat!(
            "</w:t><w:object w:dxaOrig=\"600\" w:dyaOrig=\"300\">",
            "<v:shape id=\"s1\" type=\"#_x0000_t75\" style=\"width:30pt;height:15pt\" o:ole=\"\">",
            "<v:imagedata r:id=\"rId1000\" o:title=\"\"/></v:shape>",
            "<o:OLEObject Type=\"Embed\" ProgID=\"Equation.DSMT4\" ShapeID=\"s1\" ",
            "DrawAspect=\"Content\" ObjectID=\"_1\" r:id=\"rId1001\"/></w:object><w:t>",
        )
        .to_string();

        let mut xmldoc = xmldoc;
        xmldoc.document = inject_ole_document(std::mem::take(&mut xmldoc.document), &[repl]);
        xmldoc.document_rels =
            inject_ole_rels(std::mem::take(&mut xmldoc.document_rels), 1);
        xmldoc.content_type = inject_ole_content_type(std::mem::take(&mut xmldoc.content_type));

        let path = std::env::temp_dir().join(format!("pdfreader_ole_{}.docx", std::process::id()));
        pack_docx(xmldoc, &[ole], &[png], &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();

        let read_str = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| -> String {
            let mut s = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
            s
        };

        let doc_xml = read_str(&mut zip, "word/document.xml");
        assert!(doc_xml.contains("<w:object"), "document.xml 应含 <w:object>");
        assert!(doc_xml.contains("Equation.DSMT4"));
        assert!(!doc_xml.contains("@@OLE"), "占位符应被替换干净");

        let rels = read_str(&mut zip, "word/_rels/document.xml.rels");
        assert!(rels.contains("media/image1.png"), "应含预览图关系");
        assert!(rels.contains("embeddings/oleObject1.bin"), "应含 OLE 关系");
        assert!(rels.contains("relationships/oleObject"), "OLE 关系类型正确");

        let ct = read_str(&mut zip, "[Content_Types].xml");
        assert!(ct.contains("Extension=\"bin\""), "应声明 bin 内容类型");

        let mut ole_bytes = Vec::new();
        zip.by_name("word/embeddings/oleObject1.bin")
            .unwrap()
            .read_to_end(&mut ole_bytes)
            .unwrap();
        assert_eq!(
            &ole_bytes[..8],
            &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
            "OLE 对象应以 CFB 魔数开头"
        );
        assert!(zip.by_name("word/media/image1.png").is_ok(), "预览 PNG 应存在");

        let _ = std::fs::remove_file(&path);
    }
}
