//! MathType MTEF 5.0 二进制编码器：把共享 AST（[`Node`]）编码成 MTEF 字节流。
//!
//! 逐字节移植自 omml-converter 的 `mtef.py`（反推自 jure/mathtype 的 fixture 文件），
//! 结构约定见文件头注释。产物由 [`super::ole::mtef_to_ole`] 包成 OLE 复合文件后
//! 嵌入 `.docx`，双击即可在 MathType 中编辑。

use super::latex_ast::{self, Node};

const END: u8 = 0x00;
const FULL: u8 = 0x0a;
const SUB: u8 = 0x0b;

const TF_FUNCTION: u8 = 2;
const TF_VARIABLE: u8 = 3;
const TF_LCGREEK: u8 = 4;
const TF_UCGREEK: u8 = 5;
const TF_SYMBOL: u8 = 6;
const TF_NUMBER: u8 = 8;

/// 12 字节 MTEF 头之后的固定前导定义块（ENCODING_DEF / FONT_DEF / EQN_PREFS 等）。
/// 来自验证过的 MathType 5 Windows fixture。
const PREAMBLE_BODY: &[u8] = b"\x13WinAllBasicCodePages\x00\x11\x05Arial\x00\x11\x05Times New Roman\x00\x11\x03Symbol\x00\x11\x04MT Extra\x00\x12\x00\x08!\x0f(\xf2\x7fAP\xf4\x10\x0fG_AP\xf2\x1f\x1eAP\xf4\x15\x0fA\x00\xf4E\xf4%\xf4\x8fB_A\x00\xf4\x10\x0fC_A\x00\xf4\x8fE\xf4*_H\xf4\x8fA\x00\xf4\x10\x0f@\xf4\x8fA\x7fH\xf4\x10\x0fA*_D_E\xf4_E\xf4_A\x0f\x0c\x01\x00\x01\x00\x02\x02\x03\x02\x03\x00\x03\x00\x01\x01\x01\x00\x02\x00\x02\x00\x04\x00\x00";

/// 把一段 LaTeX 公式编码成完整的 MTEF 5.0 字节流（不含 EQNOLEFILEHDR 前缀）。
pub fn latex_to_mtef(latex: &str) -> Vec<u8> {
    let eq_bytes = encode_nodes(&latex_ast::parse_latex(latex));
    let header: [u8; 12] = [
        0x05, // MTEF version 5
        0x01, // platform: 1 = Windows
        0x00, // product: 0 = MathType
        0x06, // product version 6
        0x00, // product subversion 0
        b'D', b'S', b'M', b'T', b'6', // app key "DSMT6"
        0x00, // app key 结束
        0x00, // equation options: 0 = display
    ];
    let mut out =
        Vec::with_capacity(header.len() + PREAMBLE_BODY.len() + eq_bytes.len() + 8);
    out.extend_from_slice(&header);
    out.extend_from_slice(PREAMBLE_BODY);
    out.push(FULL);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(&size_display());
    out.extend_from_slice(&eq_bytes);
    out.push(END);
    out
}

// ---------------------------------------------------------------------------
// 原始记录
// ---------------------------------------------------------------------------

fn line(null: bool) -> [u8; 2] {
    [0x01, if null { 0x01 } else { 0x00 }]
}

fn char_rec(typeface: u8, unicode: u32) -> [u8; 5] {
    [
        0x02,
        0x00, // options：无 NUDGE / 无 embellishment，MTCode 存在
        typeface + 128,
        (unicode & 0xFF) as u8,
        ((unicode >> 8) & 0xFF) as u8,
    ]
}

/// 带重音装饰（embellishment）的字符记录。
///
/// MTEF 5.0 用 `CHAR`（tag 0x02）的 options 第 0 位 `mtefOPT_CHAR_EMBELL` 标记「该字符
/// 带装饰」，随后紧跟一串 `EMBELL`（tag 0x06）记录，列表以单字节 `0x00` 结束。
/// 每个 `EMBELL` = `0x06, options(0x00), embell_code`。这比在基字符后接一个组合
/// Unicode 字符（如 U+20D7）可靠——后者 MathType 不渲染，导致公式打开后空白。
fn char_rec_with_embellishment(typeface: u8, unicode: u32, embell: u8) -> Vec<u8> {
    vec![
        0x02,
        0x01, // options：mtefOPT_CHAR_EMBELL
        typeface + 128,
        (unicode & 0xFF) as u8,
        ((unicode >> 8) & 0xFF) as u8,
        0x06, // EMBELL 记录
        0x00, // embell options
        embell,
        0x00, // 结束 embellishment 列表
    ]
}

fn size_display() -> [u8; 4] {
    [0x09, 0x65, 0x40, 0x01]
}

fn tmpl_header(selector: u8, variation: u8) -> [u8; 5] {
    [0x03, 0x00, selector, variation, 0x00]
}

// ---------------------------------------------------------------------------
// 结构模板
// ---------------------------------------------------------------------------

fn fraction(num: &[u8], den: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&tmpl_header(0x0B, 0)); // tmFRACT
    out.extend_from_slice(&line(false));
    out.extend_from_slice(num);
    out.push(END);
    out.push(FULL);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(den);
    out.push(END);
    out.push(END);
    out
}

fn sqrt_radical(rad: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&tmpl_header(0x0A, 0)); // tmROOT, tvROOT_SQ
    out.extend_from_slice(&line(false));
    out.extend_from_slice(rad);
    out.push(END);
    out.push(END);
    out
}

fn nroot_radical(deg: &[u8], rad: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&tmpl_header(0x0A, 1)); // tmROOT, n 次根
    out.extend_from_slice(&line(false));
    out.extend_from_slice(deg);
    out.push(END);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(rad);
    out.push(END);
    out.push(END);
    out
}

fn subscript(sub: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(FULL);
    out.extend_from_slice(&tmpl_header(0x1B, 0)); // tmSUB
    out.push(SUB);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(sub);
    out.push(END);
    out.extend_from_slice(&line(true)); // null 槽（上标占位）
    out.push(END);
    out
}

fn superscript(sup: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(FULL);
    out.extend_from_slice(&tmpl_header(0x1C, 0)); // tmSUP
    out.push(SUB);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(sup);
    out.push(END);
    out.extend_from_slice(&line(true));
    out.push(END);
    out
}

fn subsuperscript(sub: &[u8], sup: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(FULL);
    out.extend_from_slice(&tmpl_header(0x1D, 0)); // tmSUBSUP
    out.push(SUB);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(sub);
    out.push(END);
    out.extend_from_slice(&line(false));
    out.extend_from_slice(sup);
    out.push(END);
    out.extend_from_slice(&line(true));
    out.push(END);
    out
}

// ---------------------------------------------------------------------------
// AST → 记录
// ---------------------------------------------------------------------------

fn encode_nodes(nodes: &[Node]) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nodes {
        out.extend(encode_node(n));
    }
    out
}

fn encode_node(node: &Node) -> Vec<u8> {
    match node {
        Node::Run { text, upright } => encode_run(text, *upright),
        Node::Group(nodes) => encode_nodes(nodes),
        Node::Frac { num, den } => fraction(&encode_node(num), &encode_node(den)),
        Node::Rad { deg, e } => match deg {
            Some(d) => nroot_radical(&encode_node(d), &encode_node(e)),
            None => sqrt_radical(&encode_node(e)),
        },
        Node::SubSup { base, sub, sup } => {
            let mut out = encode_node(base);
            match (sub, sup) {
                (Some(s), Some(p)) => {
                    out.extend(subsuperscript(&encode_node(s), &encode_node(p)))
                }
                (Some(s), None) => out.extend(subscript(&encode_node(s))),
                (None, Some(p)) => out.extend(superscript(&encode_node(p))),
                (None, None) => {}
            }
            out
        }
        // 大运算符：先输出运算符字符，再用上下标近似上下限。
        // 同时有上下限时必须用单个 tmSUBSUP 模板，不能把 tmSUB 与 tmSUP 两个模板
        // 首尾相接——后者会因「空基座槽位」让 MathType 无法解析整条公式（打不开）。
        Node::Nary { chr, sub, sup, e } => {
            let mut out = char_rec(TF_SYMBOL, *chr as u32).to_vec();
            match (sub, sup) {
                (Some(s), Some(p)) => {
                    out.extend(subsuperscript(&encode_node(s), &encode_node(p)))
                }
                (Some(s), None) => out.extend(subscript(&encode_node(s))),
                (None, Some(p)) => out.extend(superscript(&encode_node(p))),
                (None, None) => {}
            }
            if let Some(e) = e {
                out.extend(encode_node(e));
            }
            out
        }
        // 重音 / 上下划线：单字符基座用 MTEF embellishment 记录叠加在字符上；
        // 多字符基座（如 \vec{AB}）无法用 embellishment 表示，退化为基字符本身。
        Node::Acc { chr, base } => match (single_char(base), embellishment_code(*chr)) {
            (Some(c), Some(code)) => char_rec_with_embellishment(classify(c).0, c as u32, code),
            _ => encode_node(base),
        },
        Node::Bar { top, e } => {
            // 上划线 embOBAR=17，下划线 embU_BAR=29。
            let code = if *top { 17u8 } else { 29u8 };
            match single_char(e) {
                Some(c) => char_rec_with_embellishment(classify(c).0, c as u32, code),
                None => encode_node(e),
            }
        }
        // 矩阵：v1 退化为定界符 + 逗号/分号分隔的线性排列。
        Node::Matrix { rows, left, right } => encode_matrix(rows, *left, *right),
        Node::Empty => Vec::new(),
    }
}

fn encode_run(text: &str, upright: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in text.chars() {
        if ch == ' ' {
            continue;
        }
        let (tf, cp) = if upright {
            (TF_FUNCTION, ch as u32)
        } else {
            classify(ch)
        };
        out.extend_from_slice(&char_rec(tf, cp));
    }
    out
}

/// 若节点正好表示一个单字符（Run 或仅含单个子节点的 Group），返回该字符。
fn single_char(node: &Node) -> Option<char> {
    match node {
        Node::Run { text, .. } if text.chars().count() == 1 => text.chars().next(),
        Node::Group(nodes) if nodes.len() == 1 => single_char(&nodes[0]),
        _ => None,
    }
}

/// 组合重音字符 → MTEF 5.0 的 embellishment 代码（见 jure/mathtype 的 embell.rb）。
fn embellishment_code(chr: char) -> Option<u8> {
    Some(match chr {
        '\u{0302}' => 9,  // \hat     → embHAT
        '\u{0303}' => 8,  // \tilde   → embTILDE
        '\u{0304}' => 17, // \bar     → embOBAR
        '\u{20D7}' => 11, // \vec     → embRARROW
        '\u{0307}' => 2,  // \dot     → emb1DOT
        '\u{0308}' => 3,  // \ddot    → emb2DOT
        _ => return None,
    })
}

/// 矩阵 / 数组：按 MTEF 5.0 的 MATRIX 记录编码成真正的二维矩阵（而非线性化）。
///
/// 结构：MATRIX（tag 0x05）→ options / valign / h_just / v_just / rows / cols /
/// row_parts / col_parts → 每格一个 PILE（tag 0x04，内含一条 LINE 装内容）→ 末尾 END。
/// row_parts / col_parts 是「分隔线」的 2-bit 打包（每字节 4 条），无分隔线时全 0。
fn encode_matrix(rows: &[Vec<Vec<Node>>], left: char, right: char) -> Vec<u8> {
    let n_rows = rows.len();
    let n_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut out = Vec::new();

    // 定界符（pmatrix / bmatrix 等的括号）用 CHAR 输出在矩阵两侧。
    if left != '\0' {
        out.extend_from_slice(&char_rec(TF_SYMBOL, left as u32));
    }

    // MATRIX 记录（tag 0x05）。
    out.push(0x05);
    out.push(0x00); // options（无 nudge / ruler）
    out.push(0x01); // valign = center_baseline
    out.push(0x01); // h_just = left
    out.push(0x01); // v_just = center_baseline
    out.push(n_rows as u8);
    out.push(n_cols as u8);
    // row_parts / col_parts：无分隔线，全部 0。
    for _ in 0..((n_rows + 4) / 4) {
        out.push(0x00);
    }
    for _ in 0..((n_cols + 4) / 4) {
        out.push(0x00);
    }

    // 各单元格：行优先，每格一个 PILE。
    for row in rows {
        for cell in row {
            out.push(0x04); // PILE
            out.push(0x00); // options
            out.push(0x01); // halign = left
            out.push(0x01); // valign = center_baseline
            out.extend_from_slice(&line(false)); // 单元格内容包在一条 LINE 里
            out.extend(encode_nodes(cell));
            out.push(END); // 结束 LINE
            out.push(END); // 结束 PILE
        }
    }

    // 结束 MATRIX 的 object_list。
    out.push(END);

    if right != '\0' {
        out.extend_from_slice(&char_rec(TF_SYMBOL, right as u32));
    }
    out
}

/// 单个字符 → `(typeface, unicode)` 分类（与 mathml_to_mtef.py 的 `_enc_mi` 一致）。
fn classify(ch: char) -> (u8, u32) {
    let cp = ch as u32;
    // MathType 的减号是 U+2212（MINUS SIGN），ASCII 连字符 U+002D 在 MathType 里
    // 会被当成未知字符显示为乱码；这里统一映射成 U+2212。
    if ch == '-' {
        return (TF_SYMBOL, 0x2212);
    }
    if ch.is_ascii_digit() {
        return (TF_NUMBER, cp);
    }
    if (0x03B1..=0x03C9).contains(&cp) {
        return (TF_LCGREEK, cp);
    }
    if (0x0391..=0x03A9).contains(&cp) {
        return (TF_UCGREEK, cp);
    }
    if is_operator(ch) || cp > 0x2000 {
        return (TF_SYMBOL, cp);
    }
    if ch.is_ascii_alphabetic() {
        return (TF_VARIABLE, cp);
    }
    (TF_SYMBOL, cp)
}

fn is_operator(ch: char) -> bool {
    matches!(
        ch,
        '+' | '-' | '=' | '<' | '>' | '*' | '/' | '÷' | '×' | '·' | '±' | '∓' | '∞' | '∂'
            | '∇' | '∑' | '∏' | '∫' | '√' | '∧' | '∨' | '∩' | '∪' | '≤' | '≥' | '≠'
            | '≈' | '≡' | '←' | '→' | '↑' | '↓' | '↔' | '⟨' | '⟩'
    )
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_dump() {
        for latex in [
            r"\vec{E}",
            r"L^{3}",
            r"v^{2}",
            r"\rho_{\nu}",
            r"(R / N) T = \vec {E} = \left(\vec {E} _ {\nu}\right) = \left(L ^ {3} / 8 \pi v ^ {2}\right) \rho_ {\nu},",
        ] {
            let m = latex_to_mtef(latex);
            println!("LATEX: {}", latex);
            println!("MTEF ({} bytes): {}", m.len(), hex(&m));
            println!("---");
        }
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn header_is_mtef5_dsmt6() {
        let m = latex_to_mtef(r"a");
        assert_eq!(&m[..6], &[0x05, 0x01, 0x00, 0x06, 0x00, b'D']);
        assert_eq!(&m[5..12], b"DSMT6\x00\x00");
    }

    #[test]
    fn fraction_layout() {
        // \frac{a}{b} → TMPL(0x03) selector 0x0B 开头，末尾以 END 收尾。
        let m = latex_to_mtef(r"\frac{a}{b}");
        // 前导头 + 前导块之后是 FULL + LINE + SIZE，随后是公式记录。
        // 直接检查整个字节流里存在 tmFRACT 头（03 00 0b 00 00）。
        let needle = [0x03, 0x00, 0x0b, 0x00, 0x00];
        assert!(m.windows(needle.len()).any(|w| w == needle));
        assert_eq!(*m.last().unwrap(), END);
    }

    #[test]
    fn subscript_layout() {
        let m = latex_to_mtef(r"x_i");
        // tmSUB 头 03 00 1b 00 00，且其后紧跟 SUB(0x0b)。
        let needle = [0x03, 0x00, 0x1b, 0x00, 0x00, 0x0b];
        assert!(m.windows(needle.len()).any(|w| w == needle));
    }

    #[test]
    fn no_backslash_leaks() {
        let m = latex_to_mtef(r"\frac{a}{b} + \sum_{i=1}^n i");
        assert!(!m.contains(&b'\\'));
    }

    #[test]
    fn greek_uses_lc_greek_typeface() {
        // ρ → typeface 4（LCGREEK），字节 = 4 + 128 = 0x84。
        let m = latex_to_mtef(r"\rho");
        let needle = [0x02, 0x00, 0x84, 0xC1, 0x03]; // 0x03C1 = ρ
        assert!(m.windows(needle.len()).any(|w| w == needle));
    }

    #[test]
    fn digit_uses_number_typeface() {
        // '1' → typeface 8（NUMBER），字节 = 8 + 128 = 0x88。
        let m = latex_to_mtef(r"1");
        let needle = [0x02, 0x00, 0x88, 0x31, 0x00];
        assert!(m.windows(needle.len()).any(|w| w == needle));
    }

    #[test]
    fn minus_uses_unicode_minus_sign() {
        // '-'（U+002D）必须映射成 U+2212（0x12 0x22），否则 MathType 里乱码。
        let m = latex_to_mtef(r"a-b");
        let needle = [0x02, 0x00, 0x86, 0x12, 0x22]; // typeface 6 + MTCode 0x2212
        assert!(m.windows(needle.len()).any(|w| w == needle));
        // 不应再出现 0x002D 的 MTCode（0x2D 0x00）。
        let hyphen = [0x02, 0x00, 0x86, 0x2D, 0x00];
        assert!(!m.windows(hyphen.len()).any(|w| w == hyphen));
    }

    #[test]
    fn nary_uses_single_subsup_template() {
        // \sum_{i=1}^{n} 同时有上下限时用 tmSUBSUP（0x1D）单个模板，
        // 不能把 tmSUB(0x1B) 与 tmSUP(0x1C) 首尾相接。
        let m = latex_to_mtef(r"\sum_{i=1}^{n} x_i");
        let subsup = [0x03, 0x00, 0x1D, 0x00, 0x00];
        assert!(m.windows(subsup.len()).any(|w| w == subsup));
    }

    #[test]
    fn matrix_uses_matrix_record() {
        // 2×2 矩阵应编码成 MATRIX(0x05) + 4 个 PILE(0x04)，而不是逗号/分号线性化。
        let m = latex_to_mtef(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}");
        // MATRIX 记录头：tag + options + valign + h_just + v_just + rows(2) + cols(2)。
        let header = [0x05, 0x00, 0x01, 0x01, 0x01, 0x02, 0x02];
        assert!(m.windows(header.len()).any(|w| w == header));
        // 每格一个 PILE。
        let pile = [0x04, 0x00, 0x01, 0x01];
        assert_eq!(m.windows(pile.len()).filter(|w| *w == pile).count(), 4);
        // 不应再出现分号（0x3B）线性化。
        let semicolon = [0x02, 0x00, 0x86, 0x3B, 0x00];
        assert!(!m.windows(semicolon.len()).any(|w| w == semicolon));
    }

    #[test]
    fn accent_uses_embellishment_not_combining_char() {
        // \vec{E} → CHAR(0x02) options=0x01(EMBELL) typeface=0x83(variable) 'E'(0x45)
        // + EMBELL(0x06 0x00 0x0B) + 结束 0x00。
        let m = latex_to_mtef(r"\vec{E}");
        let embell = [0x02, 0x01, 0x83, 0x45, 0x00, 0x06, 0x00, 0x0B, 0x00];
        assert!(m.windows(embell.len()).any(|w| w == embell));
        // 不能再出现组合字符 U+20D7（0xD7 0x20）——那是导致 MathType 空白的旧实现。
        let combining = [0x02, 0x00, 0x86, 0xD7, 0x20];
        assert!(!m.windows(combining.len()).any(|w| w == combining));
    }

    #[test]
    fn bar_uses_over_under_embellishment() {
        // \bar{x} → embOBAR(17)；\underline{x} → embU_BAR(29)。
        let over = latex_to_mtef(r"\bar{x}");
        assert!(over
            .windows(9)
            .any(|w| w == [0x02, 0x01, 0x83, 0x78, 0x00, 0x06, 0x00, 0x11, 0x00]));
        let under = latex_to_mtef(r"\underline{x}");
        assert!(under
            .windows(9)
            .any(|w| w == [0x02, 0x01, 0x83, 0x78, 0x00, 0x06, 0x00, 0x1D, 0x00]));
    }
}
