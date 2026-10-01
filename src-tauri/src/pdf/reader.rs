//! PDF 文件的读取与基本校验。
//!
//! 渲染与文本层提取由前端 PDF.js 完成；本模块负责字节校验与 data URL 编解码，
//! 并为后续 Rust 侧提取（见 [`crate::pdf::extract`]）提供统一入口。

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};

/// 最小校验：PDF 文件头必须以 `%PDF-` 开头。
pub fn validate_pdf(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 5 || &bytes[0..5] != b"%PDF-" {
        return Err(anyhow!("不是有效的 PDF 文件（缺少 %PDF- 文件头）"));
    }
    Ok(())
}

/// 把字节转换为 `data:application/pdf;base64,...`，便于前端交给 PDF.js。
pub fn to_data_url(bytes: &[u8]) -> String {
    format!("data:application/pdf;base64,{}", STANDARD.encode(bytes))
}

/// 反向：把 data URL 解码回字节（供 Rust 侧提取逻辑使用）。
pub fn from_data_url(data_url: &str) -> Result<Vec<u8>> {
    let payload = data_url
        .split_once(',')
        .map(|(_, b64)| b64)
        .ok_or_else(|| anyhow!("无效的 data URL"))?;
    STANDARD.decode(payload.trim()).context("Base64 解码失败")
}

/// 修复扫描版 PDF 里「1-bit /Decode [1 0]」位图被 PDF.js 渲染成整片黑色的问题。
///
/// 部分扫描版 PDF（如 PE_eng.pdf）的页面图像是 1-bit DeviceGray 位图，且用反转解码
/// `/Decode [1 0]`（位 1 = 墨迹）。PDF.js 对这类图像渲染错误，整页发黑
/// （见 pdf.js #7869、pdf_oxide #860）。这里在 Rust 侧把这类图像无损展开成
/// 8-bit 灰度（白底黑字），并改为标准 `/Decode [0 1]`，绕过 PDF.js 的 1-bit 解码缺陷。
///
/// 若 PDF 不含此类图像则原样返回，避免无谓重写。
pub fn normalize_scanned_pdf(bytes: &[u8]) -> Result<Vec<u8>> {
    use lopdf::{Document, Object};

    let mut doc = Document::load_mem(bytes).context("PDF 解析失败")?;
    let ids: Vec<lopdf::ObjectId> = doc.objects.keys().copied().collect();
    let mut changed = false;

    for id in ids {
        let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) else {
            continue;
        };

        let is_image = matches!(
            stream.dict.get(b"Subtype"),
            Ok(Object::Name(n)) if n.as_slice() == b"Image"
        );
        if !is_image {
            continue;
        }
        let bpc = match stream.dict.get(b"BitsPerComponent") {
            Ok(Object::Integer(v)) => *v,
            _ => continue,
        };
        if bpc != 1 {
            continue;
        }
        let is_flate = matches!(
            stream.dict.get(b"Filter"),
            Ok(Object::Name(n)) if n.as_slice() == b"FlateDecode"
        );
        if !is_flate {
            continue;
        }
        let (w, h) = match (stream.dict.get(b"Width"), stream.dict.get(b"Height")) {
            (Ok(Object::Integer(w)), Ok(Object::Integer(h))) => (*w as usize, *h as usize),
            _ => continue,
        };
        if w == 0 || h == 0 {
            continue;
        }
        let inverted = matches!(
            stream.dict.get(b"Decode"),
            Ok(Object::Array(a))
                if a.len() == 2 && matches!(&a[0], Object::Integer(1)) && matches!(&a[1], Object::Integer(0))
        );
        if !inverted {
            continue;
        }

        // 解压 FlateDecode（zlib 格式），得到 1-bit 位图原始字节。
        let plain = {
            use std::io::Read;
            let mut dec = flate2::read::ZlibDecoder::new(&stream.content[..]);
            let mut buf = Vec::new();
            dec.read_to_end(&mut buf).context("位图流解压失败")?;
            buf
        };
        let row_bytes = w / 8;
        let needed = row_bytes * h;
        if plain.len() < needed {
            continue;
        }

        // 展开成 8-bit 灰度：位 0（背景）→ 255（白），位 1（墨迹）→ 0（黑）。
        let mut gray = vec![0u8; w * h];
        for (i, &byte) in plain[..needed].iter().enumerate() {
            let base = (i / row_bytes) * w + (i % row_bytes) * 8;
            for bit in 0..8 {
                gray[base + bit] = if (byte >> (7 - bit)) & 1 == 1 { 0 } else { 255 };
            }
        }

        // 重新压成 zlib 格式，写回流并更新字典。
        let compressed = {
            use std::io::Write;
            let mut enc =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&gray).context("位图流压缩失败")?;
            enc.finish().context("位图流压缩收尾失败")?
        };
        stream.content = compressed;
        stream.dict.set("Length", stream.content.len() as i64);
        stream.dict.set("BitsPerComponent", 8i64);
        stream.dict.set("Decode", vec![Object::Integer(0), Object::Integer(1)]);
        changed = true;
    }

    if !changed {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).context("PDF 重写失败")?;
    Ok(out)
}
