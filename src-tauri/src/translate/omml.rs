//! 把 LaTeX 数学公式**本地**（确定性、无网络）转成 Word 原生公式 OMML。
//!
//! 与 [`super::latex`] 里「转成 Unicode 文本」不同，这里输出的是 `<m:oMath>…</m:oMath>`
//! XML，注入 `.docx` 后就是 Word 里可编辑的原生公式（分式、根号、上下标、求和积分
//! 等都有对应结构）。只覆盖学术论文常见子集，未识别的内容也绝不会输出反斜杠命令，
//! 而是尽力转成可读的 run。
//!
//! 设计：递归下降解析 LaTeX（见 [`super::latex_ast`]）→ 小型 AST（`Node`）→ 渲染成
//! OMML 字符串。解析器与 MathType MTEF 编码共用，避免重复。

// OMML 渲染已不再接入导出（导出改用 MathType OLE），此处保留作为可本地验证的参考实现。
#![allow(dead_code)]

use super::latex_ast::{self, Node};

/// 把一段 LaTeX 公式转成完整的 `<m:oMath>…</m:oMath>` OMML 元素（自含命名空间声明）。
pub fn latex_to_omml(latex: &str) -> String {
    let inner: String = latex_ast::parse_latex(latex).iter().map(render).collect();
    format!(
        "<m:oMath xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\" xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{inner}</m:oMath>"
    )
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

fn render(node: &Node) -> String {
    match node {
        Node::Run { text, upright } => run_xml(text, *upright),
        Node::Group(nodes) => nodes.iter().map(render).collect(),
        Node::Frac { num, den } => format!(
            "<m:f><m:num>{}</m:num><m:den>{}</m:den></m:f>",
            render(num),
            render(den)
        ),
        Node::Rad { deg, e } => match deg {
            Some(d) => format!(
                "<m:rad><m:deg>{}</m:deg><m:e>{}</m:e></m:rad>",
                render(d),
                render(e)
            ),
            None => format!(
                "<m:rad><m:radPr><m:degHide m:val=\"1\"/></m:radPr><m:deg/><m:e>{}</m:e></m:rad>",
                render(e)
            ),
        },
        Node::SubSup { base, sub, sup } => match (sub, sup) {
            (Some(s), Some(p)) => format!(
                "<m:sSubSup><m:e>{}</m:e><m:sub>{}</m:sub><m:sup>{}</m:sup></m:sSubSup>",
                render(base),
                render(s),
                render(p)
            ),
            (Some(s), None) => format!(
                "<m:sSub><m:e>{}</m:e><m:sub>{}</m:sub></m:sSub>",
                render(base),
                render(s)
            ),
            (None, Some(p)) => format!(
                "<m:sSup><m:e>{}</m:e><m:sup>{}</m:sup></m:sSup>",
                render(base),
                render(p)
            ),
            (None, None) => render(base),
        },
        Node::Nary { chr, sub, sup, e } => {
            // 裸大运算符（无上下限、无被积/求和表达式）直接当普通符号输出，
            // 避免 Word 里出现一个空的操作数占位框。
            if sub.is_none() && sup.is_none() && e.is_none() {
                return run_xml(&chr.to_string(), false);
            }
            let loc = if "∫∬∭∮".contains(*chr) { "subSup" } else { "undOvr" };
            let sub_xml = sub.as_ref().map(|s| format!("<m:sub>{}</m:sub>", render(s))).unwrap_or_default();
            let sup_xml = sup.as_ref().map(|s| format!("<m:sup>{}</m:sup>", render(s))).unwrap_or_default();
            let e_xml = e.as_ref().map(|s| format!("<m:e>{}</m:e>", render(s))).unwrap_or_else(|| "<m:e/>".to_string());
            format!(
                "<m:nary><m:naryPr><m:chr m:val=\"{chr}\"/><m:limLoc m:val=\"{loc}\"/></m:naryPr>{sub_xml}{sup_xml}{e_xml}</m:nary>"
            )
        }
        Node::Acc { chr, base } => format!(
            "<m:acc><m:accPr><m:chr m:val=\"{chr}\"/></m:accPr><m:e>{}</m:e></m:acc>",
            render(base)
        ),
        Node::Bar { top, e } => {
            let pos = if *top { "top" } else { "bot" };
            format!(
                "<m:bar><m:barPr><m:pos m:val=\"{pos}\"/></m:barPr><m:e>{}</m:e></m:bar>",
                render(e)
            )
        }
        Node::Matrix { rows, left, right } => {
            let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            let body: String = rows
                .iter()
                .map(|row| {
                    let cells: String = row
                        .iter()
                        .map(|cell| {
                            let inner: String = cell.iter().map(render).collect();
                            format!("<m:e>{inner}</m:e>")
                        })
                        .collect();
                    format!("<m:mr>{cells}</m:mr>")
                })
                .collect();
            let m = format!(
                "<m:m><m:mPr><m:mcs><m:mc><m:mcPr><m:count m:val=\"{cols}\"/><m:mcJc m:val=\"center\"/></m:mcPr></m:mc></m:mcs></m:mPr>{body}</m:m>"
            );
            if *left != '\0' || *right != '\0' {
                let beg = if *left != '\0' {
                    format!("<m:begChr m:val=\"{left}\"/>")
                } else {
                    String::new()
                };
                let end = if *right != '\0' {
                    format!("<m:endChr m:val=\"{right}\"/>")
                } else {
                    String::new()
                };
                format!("<m:d><m:dPr>{beg}{end}</m:dPr><m:e>{m}</m:e></m:d>")
            } else {
                m
            }
        }
        Node::Empty => String::new(),
    }
}

fn run_xml(text: &str, upright: bool) -> String {
    let sty = if upright { "<m:sty m:val=\"p\"/>" } else { "" };
    format!(
        "<m:r><m:rPr>{sty}<w:rPr><w:rFonts w:ascii=\"Cambria Math\" w:hAnsi=\"Cambria Math\"/></w:rPr></m:rPr><m:t xml:space=\"preserve\">{}</m:t></m:r>",
        esc(text)
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_and_sqrt() {
        assert!(latex_to_omml(r"\frac{a}{b}").contains("<m:f>"));
        assert!(latex_to_omml(r"\frac{a}{b}").contains("<m:num>"));
        assert!(latex_to_omml(r"\sqrt{x}").contains("<m:rad>"));
        assert!(latex_to_omml(r"\sqrt[3]{x}").contains("<m:deg>"));
    }

    #[test]
    fn sub_sup_and_nary() {
        let s = latex_to_omml(r"x_i^2");
        assert!(s.contains("<m:sSubSup>"));
        let s = latex_to_omml(r"\sum_{i=1}^{n} x_i");
        assert!(s.contains("<m:nary>"));
        assert!(s.contains("<m:sub>"));
        assert!(s.contains("<m:sup>"));
        assert!(s.contains('∑'));
    }

    #[test]
    fn symbols_and_functions() {
        let s = latex_to_omml(r"\rho \cdot \sin\theta");
        assert!(s.contains('ρ'));
        assert!(s.contains('·'));
        assert!(s.contains("sin"));
        assert!(s.contains("<m:sty m:val=\"p\"/>")); // sin 正体
    }

    #[test]
    fn no_backslash_leaks() {
        assert!(!latex_to_omml(r"\frac{a}{b}").contains('\\'));
        assert!(!latex_to_omml(r"x_i^2 + \sum_{i=1}^n i").contains('\\'));
    }

    #[test]
    fn matrix_env() {
        let s = latex_to_omml(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}");
        assert!(s.contains("<m:m>"));
        assert!(s.contains("<m:mr>"));
        assert!(s.contains("a"));
        assert!(s.contains("d"));
        assert!(!s.contains('\\'));
    }
}
