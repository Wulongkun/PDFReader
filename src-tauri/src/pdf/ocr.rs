//! 扫描版 PDF 的 OCR，基于 Windows 系统自带的 OCR 引擎（Windows.Media.Ocr）。
//!
//! 通过 `windows` crate 调用 WinRT API，无需捆绑外部 OCR 引擎，离线可用；
//! 支持系统已安装的语言包（中文 / 英文等）。
//!
//! 流程：前端用 PDF.js 把扫描页渲染成 PNG → 这里解码成灰度图 →
//! 写入 `SoftwareBitmap`（Gray8）→ `OcrEngine::RecognizeAsync` → 返回文字。

use std::future::IntoFuture;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use windows::{
    core::{HSTRING, Interface},
    Foundation::Rect,
    Globalization::Language,
    Graphics::Imaging::{BitmapBufferAccessMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Win32::System::{Com::{CoInitializeEx, COINIT_MULTITHREADED}, WinRT::IMemoryBufferByteAccess},
};

/// OCR 识别出的一行文字及其包围盒（像素坐标，原点在图片左上角）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrLineBox {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 一页 OCR 的结果：整页文本 + 逐行包围盒。
/// 前端用 `lines`（有则优先）还原排版（字号 / 行距 / 居中 / 双栏 / 标题层级），
/// `text` 作为无包围盒时的回退（如大模型 OCR）。
#[derive(Clone, Debug, Serialize)]
pub struct OcrPageResult {
    pub text: String,
    pub lines: Vec<OcrLineBox>,
}

/// 对一张 PNG 图片（原始字节）做 OCR，返回整页文本与逐行包围盒。
/// `lang` 为 BCP-47 语言标签（如 `zh-Hans` / `en-US`），空或 `auto` 表示跟随系统首选语言。
/// 空白页 / 无可识别文字时返回空文本（而非错误），由调用方决定如何处理。
pub fn ocr_image(png_bytes: &[u8], lang: &str) -> Result<OcrPageResult> {
    let (width, height, gray) = decode_to_gray(png_bytes)?;
    let engine = create_engine(lang)?;
    let bitmap = to_software_bitmap(width, height, &gray)?;
    let (text, lines) = recognize(&engine, &bitmap)?;
    Ok(OcrPageResult {
        text: text.trim().to_string(),
        lines,
    })
}

/// PNG 解码为 8 位灰度图，返回（宽, 高, 灰度字节）。
fn decode_to_gray(png_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(png_bytes).context("PNG 图片解码失败")?;
    let gray = img.to_luma8();
    Ok((gray.width(), gray.height(), gray.into_raw()))
}

/// 创建 OCR 引擎：优先使用指定语言，其次系统首选语言，最后回退到常见语言。
fn create_engine(lang: &str) -> Result<OcrEngine> {
    ensure_com()?;

    // 用户显式指定的语言优先。
    let explicit = lang.trim();
    if !explicit.is_empty() && !explicit.eq_ignore_ascii_case("auto") {
        let language = Language::CreateLanguage(&HSTRING::from(explicit))?;
        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
            return Ok(engine);
        }
    }

    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(engine);
    }
    for lang in ["en-US", "zh-Hans", "zh-Hant"] {
        let language = Language::CreateLanguage(&HSTRING::from(lang))?;
        if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&language) {
            return Ok(engine);
        }
    }
    Err(anyhow!(
        "系统未安装可用的 OCR 语言包，请在 Windows「设置 → 时间和语言 → 语言」中安装中文或英文语言"
    ))
}

/// 命令运行在 Tauri 的阻塞线程上，调用 WinRT 前需先初始化 COM。
/// `RPC_E_CHANGED_MODE`（0x80010106）表示线程已以其它模式初始化，忽略即可。
fn ensure_com() -> Result<()> {
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_ok() || hr.0 == RPC_E_CHANGED_MODE {
            Ok(())
        } else {
            Err(anyhow!("初始化 COM 失败：{hr}"))
        }
    }
}

/// 把灰度字节写入 `SoftwareBitmap`（Gray8 是 OCR 推荐格式）。
fn to_software_bitmap(width: u32, height: u32, gray: &[u8]) -> Result<SoftwareBitmap> {
    let bitmap = SoftwareBitmap::Create(BitmapPixelFormat::Gray8, width as i32, height as i32)
        .context("创建 OCR 位图失败")?;
    {
        let buffer = bitmap
            .LockBuffer(BitmapBufferAccessMode::Write)
            .context("锁定位图缓冲区失败")?;
        // stride 可能因对齐而大于 width，必须按行拷贝，否则整幅图会错位。
        let desc = buffer
            .GetPlaneDescription(0)
            .context("获取位图平面信息失败")?;
        let reference = buffer.CreateReference().context("获取缓冲区引用失败")?;
        let byte_access: IMemoryBufferByteAccess = reference.cast().context("访问缓冲区失败")?;

        let mut data = std::ptr::null_mut();
        let mut capacity = 0u32;
        unsafe {
            byte_access.GetBuffer(&mut data, &mut capacity)?;
            let stride = desc.Stride.max(0) as usize;
            let w = width as usize;
            let h = height as usize;
            let need = stride * h;
            if (capacity as usize) < need {
                // 极少数情况下 GetBuffer 报告的容量小于按 stride 计算的整块，
                // 此时回退为按 stride 截断的紧凑拷贝（仍按行拷贝，避免整图错位）。
                let cap = capacity as usize;
                for row in 0..h {
                    let src = &gray[row * w..(row + 1) * w];
                    let dst = data.add(row * stride);
                    let n = w.min(cap.saturating_sub(row * stride));
                    std::ptr::copy_nonoverlapping(src.as_ptr(), dst, n);
                }
            } else {
                for row in 0..h {
                    let src = &gray[row * w..(row + 1) * w];
                    let dst = data.add(row * stride);
                    std::ptr::copy_nonoverlapping(src.as_ptr(), dst, w);
                }
            }
        }
    } // 离开作用域释放缓冲区的写锁
    Ok(bitmap)
}

/// 调用 OCR 引擎识别，返回（整段文字，逐行包围盒）。
///
/// 行包围盒取该行所有词包围盒的并集，供前端换算字号 / 行距 / 居中 / 双栏 / 标题层级，
/// 从而让扫描版 PDF 的 Word 导出也能尽量贴近原排版。
fn recognize(engine: &OcrEngine, bitmap: &SoftwareBitmap) -> Result<(String, Vec<OcrLineBox>)> {
    let op = engine
        .RecognizeAsync(bitmap)
        .context("OCR 识别请求失败")?;
    // 同步命令运行在阻塞线程上，这里就地等待 WinRT 异步完成。
    let result = tauri::async_runtime::block_on(op.into_future())
        .context("等待 OCR 结果失败")?;

    // `IVectorView` 没有 IntoIterator，只能用 Size()/GetAt() 手动索引遍历。
    let line_view = result.Lines().context("读取 OCR 行失败")?;
    let n = line_view.Size().context("读取 OCR 行数失败")?;
    let mut lines = Vec::with_capacity(n as usize);
    for i in 0..n {
        let line = line_view.GetAt(i).context("读取 OCR 行失败")?;
        let text = line.Text().map(|t| t.to_string()).unwrap_or_default();

        let words = line.Words().context("读取 OCR 词失败")?;
        let m = words.Size().context("读取 OCR 词数失败")?;
        let mut rect: Option<Rect> = None;
        for j in 0..m {
            let word = words.GetAt(j).context("读取 OCR 词失败")?;
            let r = word.BoundingRect().context("读取 OCR 词包围盒失败")?;
            rect = Some(match rect {
                None => r,
                Some(acc) => {
                    let x0 = acc.X.min(r.X);
                    let y0 = acc.Y.min(r.Y);
                    let x1 = (acc.X + acc.Width).max(r.X + r.Width);
                    let y1 = (acc.Y + acc.Height).max(r.Y + r.Height);
                    Rect { X: x0, Y: y0, Width: x1 - x0, Height: y1 - y0 }
                }
            });
        }

        if let Some(r) = rect {
            lines.push(OcrLineBox {
                text,
                x: r.X,
                y: r.Y,
                w: r.Width,
                h: r.Height,
            });
        }
    }

    let text = result.Text().map(|t| t.to_string()).unwrap_or_default();
    Ok((text, lines))
}
