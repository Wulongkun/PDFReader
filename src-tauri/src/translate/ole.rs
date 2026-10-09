//! 最小 OLE 复合文件（CFB v3，512 字节扇区）写入器。
//!
//! 把 MTEF 字节流包成含单个 `Equation Native` 流的复合文档（`oleObject*.bin`），
//! 作为 MathType OLE 对象嵌入 `.docx`。逐字节移植自 omml-converter 的 `ole.py`。
//!
//! 注意：OMML 接入导出后，本模块已不再被导出路径调用，保留作为 MathType OLE 的兜底实现。

// MathType OLE 兜底实现；导出已改用 OMML（见 super::omml），故抑制 dead_code 告警。
#![allow(dead_code)]

const SECTOR: usize = 512;
const FREESECT: u32 = 0xFFFF_FFFF;
const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const FATSECT: u32 = 0xFFFF_FFFD;
const MINI_CUTOFF: usize = 4096;

/// MathType CLSID：`{0002CE02-0000-0000-C000-000000000046}`（root entry 用）。
const MATHTYPE_CLSID: [u8; 16] = [
    0x02, 0xCE, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x46,
];

/// 把 MTEF 数据包装成完整的 OLE 复合文件（先补 28 字节 `EQNOLEFILEHDR`）。
pub fn mtef_to_ole(mtef: &[u8]) -> Vec<u8> {
    // "Equation Native" 流 = EQNOLEFILEHDR(28) + MTEF 数据。
    let mut stream_data = Vec::with_capacity(28 + mtef.len());
    stream_data.extend_from_slice(&28u16.to_le_bytes()); // cbHdr
    stream_data.extend_from_slice(&0x0002_0000u32.to_le_bytes()); // version
    stream_data.extend_from_slice(&0u16.to_le_bytes()); // cf
    stream_data.extend_from_slice(&(mtef.len() as u32).to_le_bytes()); // cbObject
    stream_data.extend_from_slice(&[0u8; 16]); // reserved
    stream_data.extend_from_slice(mtef);

    wrap_cfb(&stream_data)
}

fn wrap_cfb(stream_data: &[u8]) -> Vec<u8> {
    // 补到 >= mini-stream 阈值并对齐扇区；多余零字节无害（EQNOLEFILEHDR 已记录真实长度）。
    let mut data = stream_data.to_vec();
    if data.len() < MINI_CUTOFF {
        data.resize(MINI_CUTOFF, 0);
    }
    pad(&mut data, SECTOR);
    let n_data_sectors = data.len() / SECTOR;

    // ── FAT ──
    let mut fat = vec![FREESECT; SECTOR / 4];
    fat[0] = FATSECT;
    fat[1] = ENDOFCHAIN;
    for i in 0..n_data_sectors {
        fat[2 + i] = if i + 1 < n_data_sectors { 2 + i as u32 + 1 } else { ENDOFCHAIN };
    }
    let mut fat_bytes = Vec::with_capacity(SECTOR);
    for v in &fat {
        fat_bytes.extend_from_slice(&v.to_le_bytes());
    }

    // ── DIFAT ──
    let mut difat = [FREESECT; 109];
    difat[0] = 0; // 唯一的 FAT 扇区位于扇区 0

    // ── CFB 头（512 字节） ──
    let mut header = vec![0u8; 512];
    header[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    header[24..26].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor version
    header[26..28].copy_from_slice(&0x0003u16.to_le_bytes()); // major version v3
    header[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes()); // byte order LE
    header[30..32].copy_from_slice(&9u16.to_le_bytes()); // sector shift 2^9=512
    header[32..34].copy_from_slice(&6u16.to_le_bytes()); // mini sector shift 2^6=64
    header[44..48].copy_from_slice(&1u32.to_le_bytes()); // NumFATSectors = 1
    header[48..52].copy_from_slice(&1u32.to_le_bytes()); // FirstDirectorySector = 1
    header[56..60].copy_from_slice(&0x1000u32.to_le_bytes()); // MiniStreamCutoff = 4096
    header[60..64].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // FirstMiniFATSector = none
    header[64..68].copy_from_slice(&0u32.to_le_bytes()); // NumMiniFATSectors = 0
    header[68..72].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // FirstDIFATSector = none
    header[72..76].copy_from_slice(&0u32.to_le_bytes()); // NumDIFATSectors = 0
    for (i, v) in difat.iter().enumerate() {
        header[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
    }

    // ── 目录扇区（扇区 1） ──
    let root_entry = dir_entry(
        "Root Entry",
        5,
        1,
        FREESECT,
        FREESECT,
        1,
        ENDOFCHAIN,
        0,
        &MATHTYPE_CLSID,
    );
    let stream_entry = dir_entry(
        "Equation Native",
        2,
        1,
        FREESECT,
        FREESECT,
        FREESECT,
        2,
        data.len() as u32,
        &[0u8; 16],
    );
    let unused = [0u8; 128];
    let mut dir_sector = Vec::with_capacity(SECTOR);
    dir_sector.extend_from_slice(&root_entry);
    dir_sector.extend_from_slice(&stream_entry);
    dir_sector.extend_from_slice(&unused);
    dir_sector.extend_from_slice(&unused);

    let mut out = Vec::with_capacity(SECTOR + SECTOR + SECTOR + data.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&fat_bytes);
    out.extend_from_slice(&dir_sector);
    out.extend_from_slice(&data);
    out
}

fn pad(data: &mut Vec<u8>, align: usize) {
    let rem = data.len() % align;
    if rem != 0 {
        data.resize(data.len() + align - rem, 0);
    }
}

fn dir_entry(
    name: &str,
    entry_type: u8,
    color: u8,
    left: u32,
    right: u32,
    child: u32,
    start_sect: u32,
    size: u32,
    clsid: &[u8; 16],
) -> Vec<u8> {
    let mut name_utf16: Vec<u8> = name
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    name_utf16.extend_from_slice(&[0, 0]); // 结尾空 UTF-16 字符
    let name_len = name_utf16.len() as u16;
    let mut entry = vec![0u8; 128];
    entry[..name_utf16.len()].copy_from_slice(&name_utf16);
    entry[64..66].copy_from_slice(&name_len.to_le_bytes());
    entry[66] = entry_type;
    entry[67] = color;
    entry[68..72].copy_from_slice(&left.to_le_bytes());
    entry[72..76].copy_from_slice(&right.to_le_bytes());
    entry[76..80].copy_from_slice(&child.to_le_bytes());
    entry[80..96].copy_from_slice(clsid);
    entry[116..120].copy_from_slice(&start_sect.to_le_bytes());
    entry[120..124].copy_from_slice(&size.to_le_bytes());
    entry
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_cfb_magic() {
        let ole = mtef_to_ole(&[0x00, 0x01]);
        assert_eq!(&ole[..8], &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    }

    #[test]
    fn total_size_is_sector_multiple() {
        let ole = mtef_to_ole(&[0x00, 0x01]);
        // header + FAT + directory + data 均为 512 的整数倍。
        assert_eq!(ole.len() % SECTOR, 0);
        assert!(ole.len() >= SECTOR * 3 + MINI_CUTOFF);
    }

    #[test]
    fn contains_equation_native_stream() {
        let ole = mtef_to_ole(&[0x00, 0x01, 0x02]);
        let needle: Vec<u8> = "Equation Native"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert!(ole.windows(needle.len()).any(|w| w == needle.as_slice()));
    }

    #[test]
    fn eqn_header_prefix_is_present() {
        let mtef = [0xAAu8; 10];
        let ole = mtef_to_ole(&mtef);
        // EQNOLEFILEHDR：cbHdr=28, version=0x00020000, cf=0, cbObject=10。
        let hdr = [28u8, 0, 0x00, 0x00, 0x02, 0x00, 0, 0, 10, 0, 0, 0];
        assert!(ole.windows(hdr.len()).any(|w| w == hdr));
    }
}
