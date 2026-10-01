//! LaTeX 数学公式的共享解析器：递归下降解析 LaTeX → 小型 AST（[`Node`]）。
//!
//! 从原 `omml.rs` 抽取出来，供 OMML 渲染（[`super::omml`]）与 MathType MTEF
//! 二进制编码（[`super::mtef`]）共用，避免解析逻辑重复。

use super::latex::symbol;

/// 数学节点：解析 LaTeX 得到的中间表示。
#[derive(Debug, Clone)]
pub(crate) enum Node {
    /// 一个文本 run（数字、字母、希腊字母、运算符、符号等）。
    Run { text: String, upright: bool },
    /// 花括号分组，渲染时直接拼接子节点（下标/上标作用域需要它）。
    Group(Vec<Node>),
    /// 分式 `\frac{num}{den}`。
    Frac { num: Box<Node>, den: Box<Node> },
    /// 根式 `\sqrt[n]{e}`。
    Rad { deg: Option<Box<Node>>, e: Box<Node> },
    /// 上下标 `base_sub^sup`。
    SubSup { base: Box<Node>, sub: Option<Box<Node>>, sup: Option<Box<Node>> },
    /// 大运算符 `\sum` / `\int` 等（带上下限）。
    Nary { chr: char, sub: Option<Box<Node>>, sup: Option<Box<Node>>, e: Option<Box<Node>> },
    /// 重音 `\hat` / `\bar` 等。
    Acc { chr: char, base: Box<Node> },
    /// 上下划线 `\overline` / `\underline`。
    Bar { top: bool, e: Box<Node> },
    /// 矩阵 / 分段 / 数组。
    Matrix { rows: Vec<Vec<Vec<Node>>>, left: char, right: char },
    /// 空（对齐符、样式命令、换行等）。
    Empty,
}

/// 把一段 LaTeX 公式解析成节点序列。
pub(crate) fn parse_latex(latex: &str) -> Vec<Node> {
    let chars: Vec<char> = latex.chars().collect();
    let mut p = P { chars: &chars, pos: 0 };
    p.parse_sequence()
}

// ---------------------------------------------------------------------------
// 解析器
// ---------------------------------------------------------------------------

pub(crate) struct P<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    /// 跳过空白与 LaTeX 间距命令（`\,` `\;` `\!` `\:` `\ ` `\quad` `\qquad` `~`）。
    fn skip_spaces(&mut self) {
        loop {
            if self.at_end() {
                break;
            }
            let c = self.peek().unwrap();
            if c.is_whitespace() || c == '~' {
                self.bump();
            } else if c == '\\' && self.pos + 1 < self.chars.len() {
                let nxt = self.chars[self.pos + 1];
                if nxt == ' ' || nxt == ',' || nxt == ';' || nxt == '!' || nxt == ':' {
                    self.pos += 2;
                } else {
                    let mut j = self.pos + 1;
                    while j < self.chars.len() && self.chars[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    let cmd: String = self.chars[self.pos + 1..j].iter().collect();
                    if cmd == "quad" || cmd == "qquad" {
                        self.pos = j;
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// 向前看一个 `\命令` 名（不消费）。
    fn peek_command(&self) -> Option<String> {
        if self.peek() == Some('\\') {
            let mut j = self.pos + 1;
            while j < self.chars.len() && self.chars[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > self.pos + 1 {
                Some(self.chars[self.pos + 1..j].iter().collect())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// 解析到末尾（或遇到 `}`）的节点序列。
    fn parse_sequence(&mut self) -> Vec<Node> {
        let mut out = Vec::new();
        loop {
            self.skip_spaces();
            if self.at_end() || self.peek() == Some('}') {
                break;
            }
            out.push(self.parse_node());
        }
        out
    }

    /// 解析一个「原子 + 其后的下标/上标/撇号」。
    fn parse_node(&mut self) -> Node {
        let mut node = self.parse_primary();
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('_') => {
                    self.bump();
                    let a = self.parse_script_arg();
                    node = attach(node, true, a);
                }
                Some('^') => {
                    self.bump();
                    let a = self.parse_script_arg();
                    node = attach(node, false, a);
                }
                Some('\'') => {
                    self.bump();
                    node = attach(node, false, Node::Run { text: "′".into(), upright: false });
                }
                _ => break,
            }
        }
        node
    }

    /// 解析单个「基原子」（不含其后的上下标）。
    fn parse_primary(&mut self) -> Node {
        self.skip_spaces();
        let Some(c) = self.peek() else {
            return Node::Empty;
        };
        match c {
            '{' => {
                let inner = self.read_group();
                Node::Group(parse_text(&inner))
            }
            '}' => {
                self.bump();
                Node::Empty
            }
            '\\' => self.parse_command(),
            '&' => {
                // 对齐符：普通公式里忽略
                self.bump();
                Node::Empty
            }
            _ => {
                self.bump();
                Node::Run { text: c.to_string(), upright: false }
            }
        }
    }

    /// 解析下标/上标参数：`{...}` 分组，或单个 token。
    fn parse_script_arg(&mut self) -> Node {
        self.skip_spaces();
        if self.peek() == Some('{') {
            let inner = self.read_group();
            Node::Group(parse_text(&inner))
        } else {
            self.parse_primary()
        }
    }

    /// 读取 `{...}` 分组内容（调用前 `peek() == '{'`），返回内层字符串。
    fn read_group(&mut self) -> String {
        self.bump(); // consume '{'
        let start = self.pos;
        let mut depth = 1;
        while self.pos < self.chars.len() {
            match self.chars[self.pos] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner: String = self.chars[start..self.pos].iter().collect();
                        self.pos += 1;
                        return inner;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// 读取到 `stop` 字符前的内容（不消费 `stop`）。
    fn read_until(&mut self, stop: char) -> String {
        let start = self.pos;
        while self.pos < self.chars.len() && self.chars[self.pos] != stop {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// 解析 `\命令`（调用前 `peek() == '\\'`）。
    fn parse_command(&mut self) -> Node {
        self.bump(); // consume '\'
        let Some(c) = self.peek() else {
            return Node::Empty;
        };
        if !c.is_ascii_alphabetic() {
            self.bump();
            // 单字符命令：\{ \} \% \& \# \_ \$ \\ 等
            let text = match c {
                '{' => "{",
                '}' => "}",
                '%' => "%",
                '&' => "&",
                '#' => "#",
                '_' => "_",
                '$' => "$",
                '\\' => return Node::Empty, // \\ 换行（公式内忽略）
                _ => return Node::Run { text: c.to_string(), upright: false },
            };
            return Node::Run { text: text.to_string(), upright: false };
        }

        let mut j = self.pos;
        while j < self.chars.len() && self.chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        let cmd: String = self.chars[self.pos..j].iter().collect();
        self.pos = j;
        self.dispatch(&cmd)
    }

    fn dispatch(&mut self, cmd: &str) -> Node {
        // —— 结构命令（带参数） ——
        match cmd {
            "frac" | "dfrac" | "tfrac" => {
                let num = self.parse_script_arg();
                let den = self.parse_script_arg();
                return Node::Frac { num: Box::new(num), den: Box::new(den) };
            }
            "sqrt" => {
                let deg = if self.peek() == Some('[') {
                    self.bump();
                    let inner = self.read_until(']');
                    self.bump();
                    Some(Box::new(Node::Group(parse_text(&inner))))
                } else {
                    None
                };
                let e = self.parse_script_arg();
                return Node::Rad { deg, e: Box::new(e) };
            }
            // 正体 / 文本
            "text" | "textrm" | "mbox" | "hbox" | "mathrm" | "mathsf" | "mathtt" | "operatorname" => {
                let inner = self.read_group();
                return Node::Run { text: inner, upright: true };
            }
            "mathbf" | "mathbb" | "mathcal" | "mathfrak" | "boldsymbol" | "bm" | "mathit" | "emph" => {
                let inner = self.read_group();
                return Node::Run { text: inner, upright: false };
            }
            // 重音
            "hat" | "widehat" => return self.accent('\u{0302}'),
            "bar" => return self.accent('\u{0304}'),
            "vec" => return self.accent('\u{20D7}'),
            "dot" => return self.accent('\u{0307}'),
            "ddot" => return self.accent('\u{0308}'),
            "tilde" | "widetilde" => return self.accent('\u{0303}'),
            "check" => return self.accent('\u{030C}'),
            "breve" => return self.accent('\u{0306}'),
            "acute" => return self.accent('\u{0301}'),
            "overline" => {
                let inner = self.read_group();
                return Node::Bar { top: true, e: Box::new(Node::Group(parse_text(&inner))) };
            }
            "underline" => {
                let inner = self.read_group();
                return Node::Bar { top: false, e: Box::new(Node::Group(parse_text(&inner))) };
            }
            "left" | "right" => return self.parse_delimiter(),
            "begin" => return self.parse_environment(),
            "end" | "label" | "nonumber" | "tag" | "notag" => {
                if self.peek() == Some('{') {
                    let _ = self.read_group();
                }
                return Node::Empty;
            }
            // 样式 / 间距（忽略）
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" | "rm" | "bf" | "it" => {
                return Node::Empty;
            }
            "hspace" | "vspace" | "phantom" | "hphantom" | "vphantom" | "kern" | "mkern" => {
                if self.peek() == Some('{') {
                    let _ = self.read_group();
                }
                return Node::Empty;
            }
            "not" => return self.parse_not(),
            _ => {}
        }

        // —— 大运算符 ——
        if let Some(ch) = nary_char(cmd) {
            return Node::Nary { chr: ch, sub: None, sup: None, e: None };
        }

        // —— 函数名（正体） ——
        if FN_NAMES.contains(&cmd) {
            return Node::Run { text: cmd.to_string(), upright: true };
        }

        // —— 符号表 ——
        if let Some(sym) = symbol(cmd) {
            return Node::Run { text: sym.to_string(), upright: false };
        }

        // 未知命令：原样输出命令名（正体），避免丢失信息。
        Node::Run { text: cmd.to_string(), upright: false }
    }

    fn accent(&mut self, chr: char) -> Node {
        let base = self.parse_script_arg();
        Node::Acc { chr, base: Box::new(base) }
    }

    /// `\left` / `\right` 定界符：v1 只输出定界符字符本身（不自动伸缩）。
    fn parse_delimiter(&mut self) -> Node {
        self.skip_spaces();
        if self.at_end() {
            return Node::Empty;
        }
        let c = self.peek().unwrap();
        if c == '\\' {
            return self.parse_command(); // \langle 等
        }
        self.bump();
        let text = match c {
            '(' => "(",
            ')' => ")",
            '[' => "[",
            ']' => "]",
            '{' => "{",
            '}' => "}",
            '|' => "|",
            '.' => return Node::Empty, // \left. / \right.
            other => return Node::Run { text: other.to_string(), upright: false },
        };
        Node::Run { text: text.to_string(), upright: false }
    }

    /// `\begin{...}` 环境。
    fn parse_environment(&mut self) -> Node {
        if self.peek() != Some('{') {
            return Node::Empty;
        }
        let name = self.read_group().trim().to_string();
        if !is_matrix_env(&name) {
            // equation / align 等：跳过标记，内容由上层正常解析。
            return Node::Empty;
        }
        if name == "array" && self.peek() == Some('{') {
            let _ = self.read_group(); // 列说明 {cc}
        }
        self.parse_matrix(&name)
    }

    /// 解析矩阵内容，直到 `\end{...}`。
    fn parse_matrix(&mut self, name: &str) -> Node {
        let mut rows: Vec<Vec<Vec<Node>>> = Vec::new();
        let mut row: Vec<Vec<Node>> = vec![Vec::new()];
        loop {
            self.skip_spaces();
            if self.at_end() {
                break;
            }
            if self.peek_command().as_deref() == Some("end") {
                self.parse_command(); // 消费 \end{...}
                break;
            }
            if self.peek() == Some('\\') && self.peek2() == Some('\\') {
                self.pos += 2; // 行分隔 \\
                rows.push(std::mem::take(&mut row));
                row = vec![Vec::new()];
                continue;
            }
            if self.peek() == Some('&') {
                self.bump(); // 列分隔
                row.push(Vec::new());
                continue;
            }
            let node = self.parse_node();
            row.last_mut().unwrap().push(node);
        }
        if !row.is_empty() {
            rows.push(row);
        }
        // 过滤掉 `\\` 分隔产生的空行（常见：数组 / 分段尾部多余的 `\\`，
        // 会生成一行空 PILE，MathType 读成异常尺寸）。
        let rows: Vec<Vec<Vec<Node>>> = rows
            .into_iter()
            .filter(|r| r.iter().any(|cell| !cell.is_empty()))
            .collect();
        let (left, right) = env_fence(name);
        Node::Matrix { rows, left, right }
    }

    /// `\not` 组合：`\not=` → ≠、`\not\in` → ∉ 等。
    fn parse_not(&mut self) -> Node {
        if self.peek() == Some('=') {
            self.bump();
            return Node::Run { text: "≠".into(), upright: false };
        }
        if let Some(nxt) = self.peek_command() {
            if let Some(neg) = negated_symbol(&nxt) {
                self.parse_command(); // 消费被否定的命令
                return Node::Run { text: neg.to_string(), upright: false };
            }
        }
        Node::Empty
    }
}

/// 用独立解析器解析一段（通常是 `{...}` 内层）文本。
pub(crate) fn parse_text(s: &str) -> Vec<Node> {
    let chars: Vec<char> = s.chars().collect();
    let mut p = P { chars: &chars, pos: 0 };
    p.parse_sequence()
}

// ---------------------------------------------------------------------------
// 辅助表
// ---------------------------------------------------------------------------

pub(crate) fn nary_char(cmd: &str) -> Option<char> {
    Some(match cmd {
        "sum" => '∑',
        "prod" => '∏',
        "coprod" => '∐',
        "int" => '∫',
        "iint" => '∬',
        "iiint" => '∭',
        "oint" => '∮',
        "bigcup" => '⋃',
        "bigcap" => '⋂',
        "bigvee" => '⋁',
        "bigwedge" => '⋀',
        "bigoplus" => '⨁',
        "bigotimes" => '⨂',
        "bigodot" => '⨀',
        _ => return None,
    })
}

pub(crate) const FN_NAMES: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan",
    "sinh", "cosh", "tanh", "log", "ln", "lg", "exp", "lim", "liminf", "limsup",
    "sup", "inf", "min", "max", "arg", "deg", "det", "gcd", "Pr",
];

fn negated_symbol(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "in" => "∉",
        "subset" => "⊄",
        "subseteq" => "⊈",
        "supset" => "⊅",
        "supseteq" => "⊉",
        "sim" => "≁",
        "simeq" => "≄",
        "equiv" => "≢",
        "parallel" => "∦",
        "mid" => "∤",
        "leq" | "le" => "≰",
        "geq" | "ge" => "≱",
        _ => return None,
    })
}

fn is_matrix_env(name: &str) -> bool {
    matches!(
        name,
        "matrix" | "pmatrix" | "bmatrix" | "Bmatrix" | "vmatrix" | "Vmatrix" | "cases" | "array"
    )
}

fn env_fence(name: &str) -> (char, char) {
    match name {
        "pmatrix" => ('(', ')'),
        "bmatrix" => ('[', ']'),
        "Bmatrix" => ('{', '}'),
        "vmatrix" => ('|', '|'),
        "Vmatrix" => ('‖', '‖'),
        "cases" => ('{', '\0'),
        _ => ('\0', '\0'),
    }
}

/// 把已解析的基原子套上 `_`/`^`（合并到已有 SubSup/Nary，避免嵌套）。
fn attach(node: Node, is_sub: bool, arg: Node) -> Node {
    match node {
        Node::SubSup { base, sub, sup } => {
            if is_sub && sub.is_none() {
                Node::SubSup { base, sub: Some(Box::new(arg)), sup }
            } else if !is_sub && sup.is_none() {
                Node::SubSup { base, sub, sup: Some(Box::new(arg)) }
            } else {
                Node::SubSup { base, sub, sup }
            }
        }
        Node::Nary { chr, sub, sup, e } => {
            if is_sub && sub.is_none() {
                Node::Nary { chr, sub: Some(Box::new(arg)), sup, e }
            } else if !is_sub && sup.is_none() {
                Node::Nary { chr, sub, sup: Some(Box::new(arg)), e }
            } else {
                Node::Nary { chr, sub, sup, e }
            }
        }
        other => {
            if is_sub {
                Node::SubSup { base: Box::new(other), sub: Some(Box::new(arg)), sup: None }
            } else {
                Node::SubSup { base: Box::new(other), sub: None, sup: Some(Box::new(arg)) }
            }
        }
    }
}
