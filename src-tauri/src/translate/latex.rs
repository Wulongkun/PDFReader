//! 把 GLM-OCR 输出的 Markdown 里的 LaTeX 公式转写成可读的 Unicode 数学，
//! 让导出的文字里不再出现 `\sum`、`\frac{a}{b}`、`$...$` 这类源码。
//!
//! 只处理两类内容：
//! 1. `$...$` / `$$...$$` / `\(...\)` / `\[...\]` 包裹的数学片段（完整转换，含上下标）；
//! 2. 未加定界符、散落在正文里的 LaTeX 命令（如 `\rho`、`\sum`、`\frac{a}{b}`，
//!    做「符号级」转换，不碰 `^` / `_`，避免破坏 Markdown 的 `_斜体_` 等语法）。
//!
//! 其余 Markdown（`#` 标题、`|` 表格、普通文字）原样保留。

/// 一段文本按数学定界符切分后的片段：普通文字或一处公式。
#[derive(Debug, Clone, PartialEq)]
pub enum MathSegment {
    Text(String),
    /// `latex` 为去掉定界符后的 LaTeX 源码；`display` 表示块级公式（`$$...$$` 或 `\[...\]`）。
    Math { latex: String, display: bool },
}

/// 把文本切分成「普通文字 / 公式」片段，供 `.docx` 导出时逐段处理公式。
///
/// 识别 `$...$` / `$$...$$` / `\(...\)` / `\[...\]` 定界符；`$` 与 `$$` 必须成对，
/// 未闭合时按普通文字处理。
pub fn split_math(text: &str) -> Vec<MathSegment> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out: Vec<MathSegment> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < n {
        let c = chars[i];
        if c == '$' {
            let display = i + 1 < n && chars[i + 1] == '$';
            let len = if display { 2 } else { 1 };
            let delim = if display { "$$" } else { "$" };
            if let Some(j) = find_sub(&chars, i + len, delim) {
                if !buf.is_empty() {
                    out.push(MathSegment::Text(std::mem::take(&mut buf)));
                }
                let raw: String = chars[i + len..j].iter().collect();
                let (latex, punct) = strip_trailing_punct(&raw);
                push_math(&mut out, latex, display);
                if !punct.is_empty() {
                    out.push(MathSegment::Text(punct));
                }
                i = j + len;
                continue;
            }
            buf.push(c);
            i += 1;
        } else if c == '\\' && i + 1 < n && (chars[i + 1] == '(' || chars[i + 1] == '[') {
            let close = if chars[i + 1] == '(' { "\\)" } else { "\\]" };
            if let Some(j) = find_sub(&chars, i + 2, close) {
                if !buf.is_empty() {
                    out.push(MathSegment::Text(std::mem::take(&mut buf)));
                }
                let raw: String = chars[i + 2..j].iter().collect();
                let (latex, punct) = strip_trailing_punct(&raw);
                push_math(&mut out, latex, chars[i + 1] == '[');
                if !punct.is_empty() {
                    out.push(MathSegment::Text(punct));
                }
                i = j + 2;
                continue;
            }
            buf.push(c);
            i += 1;
        } else if c == '\\' {
            // 未加定界符的裸 LaTeX 命令（\frac{a}{b}、\rho、\sum_{i}^{n} 等）也当公式处理，
            // 覆盖 GLM-OCR 偶发的无定界符公式；非数学命令仍按普通文本处理。
            if let Some((latex, next)) = read_bare_math(&chars, i) {
                if !buf.is_empty() {
                    out.push(MathSegment::Text(std::mem::take(&mut buf)));
                }
                push_math(&mut out, latex, false);
                i = next;
                continue;
            }
            buf.push(c);
            i += 1;
        } else {
            buf.push(c);
            i += 1;
        }
    }

    if !buf.is_empty() {
        out.push(MathSegment::Text(buf));
    }
    out
}

/// 把一处公式片段写入 `out`。若它是多行矩阵 / 数组 / cases / align 等环境，
/// 按行拆成多个单行公式（display 公式行间用换行 Text 分隔，行内公式用空格分隔），
/// 让每一行都成为独立、可编辑的单行 MathType OLE 对象。
fn push_math(out: &mut Vec<MathSegment>, latex: String, display: bool) {
    let rows = split_matrix_rows(&latex);
    if rows.len() <= 1 {
        out.push(MathSegment::Math { latex, display });
        return;
    }
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            out.push(MathSegment::Text(if display { "\n" } else { " " }.to_string()));
        }
        out.push(MathSegment::Math { latex: row, display });
    }
}

/// 把多行公式按行拆成多个单行公式，返回空或单元素 `Vec` 表示无需拆分。
///
/// MathType 打不开含 MATRIX / PILE 记录的公式（报「公式超出了允许的大小和（或）高度」），
/// 而单行公式都正常；因此导出 Word 时把多行公式拆成逐行的单行 OLE 对象。
///
/// 拆行范围：
/// - 堆叠式环境（`array` / `cases` / `align` / `gather` / `eqnarray` / `split` 等）——
///   `&` 只是对齐点，直接去掉、按 `\\` 分行。
/// - 真矩阵（`matrix` / `pmatrix` / `bmatrix` 等）——仅当**单列**时才拆（列向量），
///   多列矩阵保持原样（仍走 MATRIX 记录，尊重「真正的矩阵」需求）。
pub fn split_matrix_rows(latex: &str) -> Vec<String> {
    let trimmed = latex.trim();
    // 只拆「整段就是一个矩阵环境」的情况（前后无其它内容），其它保守不动。
    let Some(begin_pos) = trimmed.find("\\begin{") else {
        return Vec::new();
    };
    if !trimmed[..begin_pos].trim().is_empty() {
        return Vec::new();
    }
    let after_begin = &trimmed[begin_pos + "\\begin{".len()..];
    let Some(name_end) = after_begin.find('}') else {
        return Vec::new();
    };
    let env = after_begin[..name_end].trim().to_string();
    let stack = is_stack_env(&env);
    let matrix = is_true_matrix(&env);
    if !stack && !matrix {
        return Vec::new();
    }
    let rest = &after_begin[name_end + 1..]; // 跳过环境名后的 `}`
    let end_marker = format!("\\end{{{}}}", env);
    let Some(end_off) = rest.find(&end_marker) else {
        return Vec::new();
    };
    if !rest[end_off + end_marker.len()..].trim().is_empty() {
        return Vec::new();
    }

    // 矩阵体：去掉 aligned/gathered 的可选 `[t]`/`[b]` 标签与 array 的列格式说明 `{ll}`。
    let mut body = rest[..end_off].trim_start();
    if let Some(r) = body.strip_prefix('[') {
        if let Some(e) = r.find(']') {
            body = r[e + 1..].trim_start();
        }
    }
    if env == "array" {
        if let Some(r) = body.strip_prefix('{') {
            if let Some(e) = r.find('}') {
                body = r[e + 1..].trim_start();
            }
        }
    }

    let mut rows: Vec<String> = Vec::new();
    let mut multi_col = false;
    for raw_row in body.split("\\\\") {
        let row = raw_row.trim();
        if row.is_empty() {
            continue;
        }
        let cells: Vec<&str> = row
            .split('&')
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        if cells.is_empty() {
            continue;
        }
        if cells.len() > 1 {
            multi_col = true;
        }
        rows.push(cells.join(" "));
    }
    // 多列真矩阵保持 MATRIX 记录，不拆成单行。
    if matrix && multi_col {
        return Vec::new();
    }
    if rows.len() <= 1 {
        return Vec::new();
    }
    rows
}

/// 堆叠式多行环境（`&` 只是对齐点，行就是独立的公式）。
fn is_stack_env(env: &str) -> bool {
    matches!(
        env,
        "array" | "cases" | "align" | "align*" | "aligned" | "gather" | "gather*"
            | "gathered" | "eqnarray" | "eqnarray*" | "split"
    )
}

/// 真矩阵环境（`&` 是列分隔；仅单列时按行拆）。
fn is_true_matrix(env: &str) -> bool {
    matches!(env, "matrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix")
}

/// 判断一个 `\命令` 是否是数学命令（用于把无定界符的裸 LaTeX 也识别成公式）。
fn is_math_command(cmd: &str) -> bool {
    if symbol(cmd).is_some() {
        return true;
    }
    matches!(
        cmd,
        "frac" | "dfrac" | "tfrac" | "sqrt" | "mathbf" | "mathbb" | "mathcal" | "mathfrak"
            | "boldsymbol" | "bm" | "mathit" | "mathrm" | "mathsf" | "mathtt"
            | "operatorname" | "text" | "textrm" | "mbox" | "hbox" | "hat" | "widehat"
            | "bar" | "vec" | "dot" | "ddot" | "tilde" | "widetilde" | "overline"
            | "underline" | "left" | "right" | "begin" | "not" | "textstyle"
            | "displaystyle" | "limits" | "nolimits"
    )
}

/// 从 `start`（指向 `\`）读取一个裸 LaTeX 命令及其参数（`{...}`、`[...]`、`_{...}`、`^{...}`），
/// 返回 (LaTeX 源码, 下一个位置)。若 `start` 处不是数学命令则返回 `None`。
///
/// 参数前的空白仅在有参数跟随时才被消费，否则还给正文，避免把 `\rho 的` 里的空格吃掉。
fn read_bare_math(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut j = start + 1;
    while j < chars.len() && chars[j].is_ascii_alphabetic() {
        j += 1;
    }
    let cmd: String = chars[start + 1..j].iter().collect();
    if !is_math_command(&cmd) {
        return None;
    }

    let mut k = j;
    loop {
        // 记录跳过空白前的位置：若后面没有参数，这段空白要还给正文。
        let saved = k;
        while k < chars.len() && chars[k].is_whitespace() {
            k += 1;
        }
        let arg_end: Option<usize> = match chars.get(k) {
            Some('{') => read_brace(chars, k).map(|(_, next)| next),
            Some('[') => {
                let mut m = k + 1;
                while m < chars.len() && chars[m] != ']' {
                    m += 1;
                }
                if m < chars.len() { Some(m + 1) } else { None }
            }
            Some('_') | Some('^') => {
                let mut m = k + 1;
                while m < chars.len() && chars[m].is_whitespace() {
                    m += 1;
                }
                if m < chars.len() && chars[m] == '{' {
                    read_brace(chars, m).map(|(_, next)| next)
                } else if m < chars.len() {
                    Some(m + 1)
                } else {
                    None
                }
            }
            _ => None,
        };
        match arg_end {
            Some(next) => k = next,
            None => {
                k = saved;
                break;
            }
        }
    }
    Some((chars[start..k].iter().collect(), k))
}

/// 对整段文本做 LaTeX → Unicode 数学转换（详见模块说明）。
pub fn latex_math_to_unicode(input: &str) -> String {
    // 没有任何反斜杠命令、也没有数学定界符时，直接原样返回，避免无谓遍历。
    if !input.contains('\\') && !input.contains('$') {
        return input.to_string();
    }

    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(input.len() + input.len() / 4);
    let mut i = 0;

    while i < n {
        let c = chars[i];
        if c == '$' {
            let display = i + 1 < n && chars[i + 1] == '$';
            let len = if display { 2 } else { 1 };
            let delim = if display { "$$" } else { "$" };
            if let Some(j) = find_sub(&chars, i + len, delim) {
                let inner: String = chars[i + len..j].iter().collect();
                // 内容含反斜杠才当作数学；纯文字（如金额 $5）只去掉 $ 符号。
                let converted = if inner.contains('\\') {
                    convert_math(&inner)
                } else {
                    inner.trim().to_string()
                };
                if display {
                    // 块级公式独立成段。
                    out.push('\n');
                    out.push_str(converted.trim());
                    out.push('\n');
                } else {
                    out.push_str(converted.trim());
                }
                i = j + len;
            } else {
                out.push_str(delim);
                i += len;
            }
        } else if c == '\\' && i + 1 < n && (chars[i + 1] == '(' || chars[i + 1] == '[') {
            let close = if chars[i + 1] == '(' { "\\)" } else { "\\]" };
            if let Some(j) = find_sub(&chars, i + 2, close) {
                let inner: String = chars[i + 2..j].iter().collect();
                out.push_str(convert_math(&inner).trim());
                i = j + 2;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }

    // 未加定界符的 LaTeX 命令再做一遍符号级转换（保留换行，不碰 ^/_）。
    convert_symbols_only(&out)
}

/// 数学片段的完整转换（含上下标），输出单行文本。
fn convert_math(s: &str) -> String {
    let mut t = replace_scripts(s);
    t = replace_frac(&t);
    t = replace_sqrt(&t);
    t = strip_wrappers(&t);
    t = replace_accent(&t);
    t = replace_symbols(&t);
    let t = cleanup(&t);
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 符号级转换：不处理上下标（`^` / `_`），用于正文里的裸 LaTeX 命令。
fn convert_symbols_only(s: &str) -> String {
    let mut t = s.to_string();
    t = replace_frac(&t);
    t = replace_sqrt(&t);
    t = strip_wrappers(&t);
    t = replace_accent(&t);
    t = replace_symbols(&t);
    cleanup(&t)
}

/// `\frac{a}{b}` / `\dfrac{a}{b}` / `\tfrac{a}{b}` → `(a)/(b)`。
fn replace_frac(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > i + 1 {
                let cmd: String = chars[i + 1..j].iter().collect();
                if cmd == "frac" || cmd == "dfrac" || cmd == "tfrac" {
                    if let Some((num, after_num)) = read_brace(&chars, j) {
                        if let Some((den, after_den)) = read_brace(&chars, after_num) {
                            let num = convert_math(&num);
                            let den = convert_math(&den);
                            out.push_str(&format!("({})/({})", num, den));
                            i = after_den;
                            continue;
                        }
                    }
                }
                out.push_str(&chars[i..j].iter().collect::<String>());
                i = j;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `\sqrt{x}` → `√(x)`；`\sqrt[n]{x}` → `n√(x)`。
fn replace_sqrt(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > i + 1 {
                let cmd: String = chars[i + 1..j].iter().collect();
                if cmd == "sqrt" {
                    let mut k = j;
                    let mut root = String::new();
                    if k < chars.len() && chars[k] == '[' {
                        if let Some(e) = find_sub(&chars, k + 1, "]") {
                            root = chars[k + 1..e].iter().collect();
                            k = e + 1;
                        }
                    }
                    if let Some((arg, next)) = read_brace(&chars, k) {
                        let arg = convert_math(&arg);
                        if root.is_empty() {
                            out.push_str(&format!("√({})", arg));
                        } else {
                            out.push_str(&format!("{}√({})", convert_math(&root), arg));
                        }
                        i = next;
                        continue;
                    }
                }
                out.push_str(&chars[i..j].iter().collect::<String>());
                i = j;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 文本/样式包装命令：去掉命令名，保留 `{...}` 内容。
///
/// 注意顺序：`\text` 系列必须排在 `\text` 之前（它们以 `\text` 为前缀）。
fn strip_wrappers(s: &str) -> String {
    let mut t = s.to_string();
    for cmd in [
        "\\operatorname", "\\boldsymbol", "\\textnormal", "\\textsc", "\\textsl", "\\textmd",
        "\\textup", "\\textsf", "\\texttt", "\\textbf", "\\textit", "\\textrm", "\\mathsf",
        "\\mathtt", "\\mathbf", "\\mathcal", "\\mathbb", "\\mathfrak", "\\mathnormal",
        "\\mathit", "\\mathrm", "\\text", "\\mbox", "\\overline", "\\underline", "\\widehat",
        "\\widetilde", "\\overrightarrow", "\\overleftarrow", "\\overbrace", "\\underbrace",
        "\\boxed", "\\displaystyle", "\\textstyle", "\\scriptstyle", "\\scriptscriptstyle",
        "\\phantom", "\\hphantom", "\\vphantom",
    ] {
        t = t.replace(cmd, "");
    }
    t
}

/// 重音命令：`\hat{x}` → `x̂`、`\bar{x}` → `x̄`、`\vec{x}` → `x⃗` 等。
fn replace_accent(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > i + 1 {
                let cmd: String = chars[i + 1..j].iter().collect();
                let comb = match cmd.as_str() {
                    "hat" => Some('\u{0302}'),
                    "bar" => Some('\u{0304}'),
                    "vec" => Some('\u{20d7}'),
                    "dot" => Some('\u{0307}'),
                    "ddot" => Some('\u{0308}'),
                    "tilde" => Some('\u{0303}'),
                    _ => None,
                };
                if let Some(comb) = comb {
                    let (arg, next) = read_arg(&chars, j);
                    let sym = replace_symbols(&arg);
                    if let Some(base) = sym.chars().next() {
                        out.push(base);
                        out.push(comb);
                    } else {
                        out.push_str(&sym);
                    }
                    i = next;
                } else {
                    out.push_str(&chars[i..j].iter().collect::<String>());
                    i = j;
                }
            } else {
                out.push(chars[i]);
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 上标 `^{...}` / 下标 `_{...}` → Unicode 上下标；无法逐字转换时用 `^(...)` / `_(...)`。
fn replace_scripts(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '^' || c == '_' {
            let is_sup = c == '^';
            let (arg, next) = read_arg(&chars, i + 1);
            out.push_str(&to_script(&arg, is_sup));
            i = next;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// 把脚本参数转成 Unicode 上下标（参数先做符号转换，再逐字符映射）。
fn to_script(arg: &str, is_sup: bool) -> String {
    if arg.is_empty() {
        return String::new();
    }
    // 先转命令（^\top → ^⊤、_\infty → _∞），不含上下标步骤，避免递归。
    let converted = convert_symbols_only(arg);
    if converted.chars().all(|c| script_char(c, is_sup).is_some()) {
        return converted.chars().map(|c| script_char(c, is_sup).unwrap()).collect();
    }
    if is_sup {
        format!("^({})", converted)
    } else {
        format!("_({})", converted)
    }
}

/// 单个字符的上标/下标 Unicode（无对应字符时返回 None）。
fn script_char(c: char, is_sup: bool) -> Option<char> {
    if c.is_ascii_digit() {
        let digits: &str = if is_sup { "⁰¹²³⁴⁵⁶⁷⁸⁹" } else { "₀₁₂₃₄₅₆₇₈₉" };
        return digits.chars().nth((c as u8 - b'0') as usize);
    }
    let (keys, pairs): (&[char], &[char]) = if is_sup {
        (
            &['+', '-', '=', '(', ')', 'n', 'i', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'l', 'm', 'o', 'p', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'T'],
            &['⁺', '⁻', '⁼', '⁽', '⁾', 'ⁿ', 'ⁱ', 'ᵃ', 'ᵇ', 'ᶜ', 'ᵈ', 'ᵉ', 'ᶠ', 'ᵍ', 'ʰ', 'ʲ', 'ᵏ', 'ˡ', 'ᵐ', 'ᵒ', 'ᵖ', 'ʳ', 'ˢ', 'ᵗ', 'ᵘ', 'ᵛ', 'ʷ', 'ˣ', 'ʸ', 'ᶻ', 'ᵀ'],
        )
    } else {
        (
            &['+', '-', '=', '(', ')', 'a', 'e', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'r', 's', 't', 'u', 'v', 'x'],
            &['₊', '₋', '₌', '₍', '₎', 'ₐ', 'ₑ', 'ₕ', 'ᵢ', 'ⱼ', 'ₖ', 'ₗ', 'ₘ', 'ₙ', 'ₒ', 'ₚ', 'ᵣ', 'ₛ', 'ₜ', 'ᵤ', 'ᵥ', 'ₓ'],
        )
    };
    keys.iter().position(|&k| k == c).and_then(|idx| pairs.get(idx).copied())
}

/// 希腊字母 / 运算符 / 关系符 / 函数名等命令 → Unicode（未知命令丢弃）。
fn replace_symbols(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > i + 1 {
                let cmd: String = chars[i + 1..j].iter().collect();
                if let Some(rep) = symbol(&cmd) {
                    out.push_str(rep);
                }
                // 未知命令：丢弃命令名，保留其后的花括号内容。
                i = j;
            } else {
                // 反斜杠 + 非字母（如 \, \; \!），原样保留交给 cleanup。
                out.push('\\');
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 命令 → Unicode 符号的查找表。
pub(crate) fn symbol(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        // 希腊字母（小写）
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ",
        "epsilon" | "varepsilon" => "ε", "zeta" => "ζ", "eta" => "η",
        "theta" => "θ", "vartheta" => "ϑ", "iota" => "ι", "kappa" => "κ",
        "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ",
        "omicron" => "ο", "pi" => "π", "varpi" => "ϖ", "rho" => "ρ",
        "varrho" => "ϱ", "sigma" => "σ", "varsigma" => "ς", "tau" => "τ",
        "upsilon" => "υ", "phi" | "varphi" => "φ", "chi" => "χ",
        "psi" => "ψ", "omega" => "ω",
        // 希腊字母（大写）
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ",
        "Xi" => "Ξ", "Pi" => "Π", "Sigma" => "Σ", "Upsilon" => "Υ",
        "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        // 大运算符
        "sum" => "∑", "prod" => "∏", "coprod" => "∐", "int" => "∫",
        "iint" => "∬", "iiint" => "∭", "oint" => "∮", "bigcup" => "⋃",
        "bigcap" => "⋂", "bigvee" => "⋁", "bigwedge" => "⋀", "bigoplus" => "⨁",
        "bigotimes" => "⨂", "bigodot" => "⨀",
        // 关系符
        "leq" | "le" => "≤", "geq" | "ge" => "≥", "neq" | "ne" => "≠",
        "equiv" => "≡", "approx" => "≈", "sim" => "∼", "simeq" => "≃",
        "cong" => "≅", "propto" => "∝", "ll" => "≪", "gg" => "≫",
        "prec" => "≺", "succ" => "≻", "preceq" => "⪯", "succeq" => "⪰",
        "subset" => "⊂", "supset" => "⊃", "subseteq" => "⊆", "supseteq" => "⊇",
        "in" => "∈", "notin" => "∉", "ni" => "∋", "mid" => "∣",
        "parallel" => "∥", "perp" => "⊥", "asymp" => "≍", "doteq" => "≐",
        // 箭头
        "to" | "rightarrow" => "→", "longrightarrow" => "⟶",
        "leftarrow" => "←", "longleftarrow" => "⟵", "leftrightarrow" => "↔",
        "Rightarrow" => "⇒", "Leftarrow" => "⇐", "Leftrightarrow" => "⇔",
        "mapsto" => "↦", "uparrow" => "↑", "downarrow" => "↓",
        "updownarrow" => "↕", "nearrow" => "↗", "searrow" => "↘",
        "swarrow" => "↙", "nwarrow" => "↖", "hookrightarrow" => "↪",
        // 二元运算符
        "pm" => "±", "mp" => "∓", "times" => "×", "div" => "÷",
        "cdot" => "·", "ast" => "∗", "star" => "⋆", "circ" => "∘",
        "bullet" => "•", "cap" => "∩", "cup" => "∪", "uplus" => "⊎",
        "sqcap" => "⊓", "sqcup" => "⊔", "vee" => "∨", "wedge" => "∧",
        "setminus" => "∖", "diamond" => "⋄", "oplus" => "⊕", "ominus" => "⊖",
        "otimes" => "⊗", "oslash" => "⊘", "odot" => "⊙", "dagger" => "†",
        "ddagger" => "‡",
        // 杂项符号
        "infty" => "∞", "partial" => "∂", "nabla" => "∇", "forall" => "∀",
        "exists" => "∃", "emptyset" | "varnothing" => "∅", "Re" => "ℜ",
        "Im" => "ℑ", "aleph" => "ℵ", "hbar" => "ℏ", "ell" => "ℓ",
        "angle" => "∠", "top" => "⊤", "prime" => "′", "surd" => "√",
        "ldots" | "dots" | "dotsc" => "…", "cdots" => "⋯", "vdots" => "⋮",
        "ddots" => "⋱", "therefore" => "∴", "because" => "∵", "neg" | "lnot" => "¬",
        // 定界符
        "langle" => "⟨", "rangle" => "⟩", "lceil" => "⌈", "rceil" => "⌉",
        "lfloor" => "⌊", "rfloor" => "⌋", "vert" | "lvert" | "rvert" => "|",
        "Vert" | "lVert" | "rVert" => "‖",
        // 函数名（保留字母）
        "sin" => "sin", "cos" => "cos", "tan" => "tan", "cot" => "cot",
        "sec" => "sec", "csc" => "csc", "arcsin" => "arcsin", "arccos" => "arccos",
        "arctan" => "arctan", "sinh" => "sinh", "cosh" => "cosh", "tanh" => "tanh",
        "log" => "log", "ln" => "ln", "lg" => "lg", "exp" => "exp",
        "lim" => "lim", "liminf" => "liminf", "limsup" => "limsup", "sup" => "sup",
        "inf" => "inf", "min" => "min", "max" => "max", "arg" => "arg",
        "deg" => "deg", "det" => "det", "gcd" => "gcd", "Pr" => "Pr",
        _ => return None,
    })
}

/// 清理：去掉分组花括号、还原转义字符、去掉间距命令与对齐符。
fn cleanup(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    // 分组花括号丢弃；转义花括号 \{ \} 还原为字面量。
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && (chars[i + 1] == '{' || chars[i + 1] == '}') {
            out.push(chars[i + 1]);
            i += 2;
        } else if chars[i] == '{' || chars[i] == '}' {
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }

    let mut t = out;
    // 转义字符还原
    for (from, to) in [
        ("\\%", "%"), ("\\&", "&"), ("\\#", "#"), ("\\_", "_"), ("\\$", "$"),
    ] {
        t = t.replace(from, to);
    }
    // 间距命令 → 空格
    for from in ["\\quad", "\\qquad", "\\,", "\\;", "\\!", "\\:", "\\ ", "\\\\"] {
        t = t.replace(from, " ");
    }
    // 对齐符 → 空格
    t.replace('&', " ")
}

/// 读取 `{...}` 分组、单个字符或单个 `\命令`，返回 (内容, 下一个位置)。
fn read_arg(chars: &[char], i: usize) -> (String, usize) {
    if i >= chars.len() {
        return (String::new(), i);
    }
    if chars[i] == '{' {
        return match read_brace(chars, i) {
            Some((inner, next)) => (inner, next),
            // 未闭合分组：跳过左花括号，保证前进。
            None => (String::new(), i + 1),
        };
    }
    // 反斜杠 + 字母组成的命令：整词作为参数（如 `^\top`）。
    if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic() {
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        return (chars[i..j].iter().collect(), j);
    }
    (chars[i].to_string(), i + 1)
}

/// 读取从位置 `i` 开始的 `{...}` 分组（`chars[i]` 应为 `{`），返回 (内容, 下一个位置)。
fn read_brace(chars: &[char], i: usize) -> Option<(String, usize)> {
    if i >= chars.len() || chars[i] != '{' {
        return None;
    }
    let mut depth = 0usize;
    let mut j = i;
    while j < chars.len() {
        match chars[j] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[i + 1..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// 从 `from` 位置起查找字面量 `needle`（连续字符），返回其起始下标。
fn find_sub(haystack: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    let last = haystack.len().saturating_sub(needle.len());
    (from..=last).find(|&i| haystack[i..i + needle.len()] == needle[..])
}

/// 从数学片段末尾剥离「句子标点」（逗号 / 句号 / 分号 / 冒号，含全角），
/// 返回 (剩余 LaTeX, 被剥离的标点)。GLM / 大模型常把句末标点误包进 `$...$`
/// （如 `$x_i，$`），这些标点应还给正文，否则会随公式一起进入 MathType 对象。
/// 不含 `!` / `?`：它们常是公式本身的一部分（阶乘等）。
fn strip_trailing_punct(latex: &str) -> (String, String) {
    let trimmed = latex.trim_end();
    let mut end = trimmed.len();
    let mut punct = String::new();
    for ch in trimmed.chars().rev() {
        if matches!(ch, ',' | '，' | '.' | '。' | ';' | '；' | ':' | '：') {
            punct.push(ch);
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    (trimmed[..end].to_string(), punct.chars().rev().collect())
}

/// 判断字符串是否只由「句末标点」构成（逗号 / 句号 / 分号 / 冒号，含全角，与
/// `strip_trailing_punct` 一致的集合）。用于把块级公式后剥离出的标点附着回公式同一行
/// （避免标点独自成行），以及行内公式后的标点不补前导空格（`x,` 而非 `x ,`）。
pub fn is_trailing_punct(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|ch| matches!(ch, ',' | '，' | '.' | '。' | ';' | '；' | ':' | '：'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_inline_math() {
        assert_eq!(latex_math_to_unicode("$\\rho_i$"), "ρᵢ");
        assert_eq!(latex_math_to_unicode("$\\sum_{i=1}^n x_i$"), "∑ᵢ₌₁ⁿ xᵢ");
    }

    #[test]
    fn converts_fraction_and_sqrt() {
        assert_eq!(latex_math_to_unicode("$\\frac{a}{b}$"), "(a)/(b)");
        assert_eq!(latex_math_to_unicode("$\\sqrt{x}$"), "√(x)");
        assert_eq!(latex_math_to_unicode("$\\sqrt[3]{x}$"), "3√(x)");
    }

    #[test]
    fn converts_bare_commands_outside_delimiters() {
        assert_eq!(latex_math_to_unicode("使用 \\rho 和 \\lambda"), "使用 ρ 和 λ");
    }

    #[test]
    fn preserves_markdown_structure() {
        let md = "## 标题\n| a | b |\n|---|\n| 1 | $\\alpha$ |";
        let out = latex_math_to_unicode(md);
        assert!(out.contains("## 标题"));
        assert!(out.contains("| a | b |"));
        assert!(out.contains("α"));
        assert!(!out.contains('\\'));
    }

    #[test]
    fn display_math_gets_own_line() {
        let out = latex_math_to_unicode("正文 $$\\int_0^1 x dx$$ 结束");
        assert!(out.contains("∫₀¹ x dx"));
        assert!(!out.contains("$$"));
    }

    #[test]
    fn splits_inline_and_display_math() {
        use super::MathSegment;
        let segs = split_math("前面 $x^2$ 中间 $$\\frac{a}{b}$$ 后面");
        assert_eq!(
            segs,
            vec![
                MathSegment::Text("前面 ".into()),
                MathSegment::Math { latex: "x^2".into(), display: false },
                MathSegment::Text(" 中间 ".into()),
                MathSegment::Math { latex: "\\frac{a}{b}".into(), display: true },
                MathSegment::Text(" 后面".into()),
            ]
        );
    }

    #[test]
    fn unbalanced_dollar_stays_text() {
        use super::MathSegment;
        let segs = split_math("金额 $5 元");
        assert_eq!(segs, vec![MathSegment::Text("金额 $5 元".into())]);
    }

    #[test]
    fn bare_latex_commands_split_as_math() {
        use super::MathSegment;
        // 无定界符的裸 LaTeX 命令也要识别成公式，且不吞掉命令后面的空格。
        let segs = split_math("密度 \\rho 与 \\frac{a}{b} 的关系");
        assert_eq!(
            segs,
            vec![
                MathSegment::Text("密度 ".into()),
                MathSegment::Math { latex: "\\rho".into(), display: false },
                MathSegment::Text(" 与 ".into()),
                MathSegment::Math { latex: "\\frac{a}{b}".into(), display: false },
                MathSegment::Text(" 的关系".into()),
            ]
        );
    }

    #[test]
    fn trailing_punctuation_stays_out_of_math() {
        use super::MathSegment;
        // 句末逗号 / 句号误包进 $...$ 时应还给正文，不要进入公式。
        let segs = split_math("令 $x_i，$。");
        assert_eq!(
            segs,
            vec![
                MathSegment::Text("令 ".into()),
                MathSegment::Math { latex: "x_i".into(), display: false },
                MathSegment::Text("，".into()),
                MathSegment::Text("。".into()),
            ]
        );
    }

    #[test]
    fn is_trailing_punct_recognizes_only_sentence_punct() {
        assert!(is_trailing_punct(","));
        assert!(is_trailing_punct("，。"));
        assert!(is_trailing_punct(".;:"));
        assert!(!is_trailing_punct(""));
        assert!(!is_trailing_punct(", which"));
        assert!(!is_trailing_punct("!")); // 感叹号不剥离（可能是阶乘）
        assert!(!is_trailing_punct("abc"));
    }

    #[test]
    fn splits_multiline_matrix_into_rows() {
        let rows = split_matrix_rows(
            "\\begin{array}{l} (R/N)T = \\vec{E}, \\\\ \\rho_{\\nu} = (R/N) T. \\\\ \\end{array}",
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("(R/N)T"));
        assert!(!rows[0].contains("\\begin"));
        assert!(rows[1].contains("\\rho"));
        // 单行矩阵不拆。
        assert!(split_matrix_rows("\\begin{pmatrix} a \\end{pmatrix}").is_empty());
        // 多列真矩阵保持原样（仍是 MATRIX 记录）。
        assert!(split_matrix_rows("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}").is_empty());
        // 单列真矩阵（列向量）拆成两行。
        assert_eq!(
            split_matrix_rows("\\begin{pmatrix} a \\\\ b \\end{pmatrix}"),
            vec!["a".to_string(), "b".to_string()]
        );
        // 非矩阵环境不拆。
        assert!(split_matrix_rows("\\frac{a}{b}").is_empty());
    }

    #[test]
    fn multiline_math_splits_into_segments() {
        use super::MathSegment;
        let segs = split_math("$$\\begin{array}{l} a \\\\ b \\end{array}$$");
        let math: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s, MathSegment::Math { .. }))
            .collect();
        assert_eq!(math.len(), 2);
        assert!(matches!(&segs[0], MathSegment::Math { latex, display: true } if latex == "a"));
    }
}
