//! 大模型 API 调用（OpenAI 兼容的 `/chat/completions` 接口）。
//!
//! 默认实现走 OpenAI 兼容接口，配合 DeepSeek / 通义千问 / Kimi 等国内可访问、
//! 且提供 OpenAI 兼容端点的服务，无需改代码即可切换。
//!
//! 除文本翻译外，还提供「大模型 OCR」：把页面图片发给视觉多模态模型，让模型
//! 抄出其中的文字。两者复用同一套 base_url / api_key。
//!
//! 若要接入 DeepL / 百度 / Google 等私有协议，可在 [`TranslateClient`] 中新增
//! `translate_deepl` / `translate_baidu` 等方法，或按 config 里的 `provider`
//! 分派到不同实现。

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::{ExtractedPage, TranslateRequest, TranslateResult};
use crate::config::{Config, TranslateConfig};

/// 大模型 OCR 的系统指令：只抄文字，不翻译、不解释。
///
/// 换行指令是关键：老版本写「保持段落与换行」会被模型理解成「每行末尾都加换行」，
/// 竖排古籍就变成「一列一行」。这里明确要求：只在段落分隔处换行（空行分隔段落），
/// 竖排文本按从右到左、从上到下重排成连续横排文字。
const OCR_PROMPT: &str = "请识别图片中的所有文字，按原样输出文字内容。\
如果文字是竖排（如古籍，从上到下、从右到左排列），请按从右到左、从上到下的阅读顺序，\
把它重排成连续的横排文字输出。只保留段落结构：仅在段落分隔处换行（用空行分隔段落），\
不要在每一行的末尾都添加换行，段内的自然换行请合并到同一段。\
只输出识别到的文字本身；如果图片中没有任何文字，输出一个空字符串。\
不要翻译，不要添加任何解释、注释或额外内容。";

/// 云端版面解析的系统指令：把一页学术论文解析成结构化 JSON。
///
/// 关键要求：跳过公式（不输出 LaTeX）、跳过页眉页脚页码、表格按行列还原、
/// 图片输出 0~1000 归一化包围盒，便于前端裁剪插图。
const EXTRACT_PROMPT: &str = "你是学术论文版面解析器。识别图片中的这一页，严格输出一个 JSON 对象（不要输出 Markdown 代码块、不要任何解释或前后缀文字）：\
{\"paragraphs\":[\"正文段落1\",\"正文段落2\"],\"tables\":[{\"rows\":[[\"表头1\",\"表头2\"],[\"值\",\"值\"]]}],\"figures\":[{\"bbox\":[x0,y0,x1,y1],\"caption\":\"图题\"}]}\n\
字段要求：\
1. paragraphs：按阅读顺序的正文段落，保留正文文字。遇到数学公式/方程时，用 LaTeX 语法原样输出，并用 $...$（行内）或 $$...$$（独占一行）包裹（例如 $a^2+b^2=c^2$）；跳过页眉、页脚、页码、论文标题页的脚注与版权声明；插图与表格的图题/表题（如 \"Fig. 2. ...\" / \"TABLE I\"）也不要放进 paragraphs。\
2. tables：页面里的表格，每个表格的 rows 是二维数组（行 × 列），按原表格行列还原；没有表格就给空数组 []。\
3. figures：页面里的插图（图表、示意图、坐标图等），bbox 是插图外接框的 0~1000 归一化坐标 [x0,y0,x1,y1]（左上角 x、左上角 y、右下角 x、右下角 y，相对整页图片宽高归一化到 0~1000），caption 是图题（如 \"Fig. 2. xxx\"）；没有插图就给空数组 []。\
只输出这一页的内容。";

/// 识别 API 错误响应里的「额度 / 余额用尽」信号，返回充值提示；无则返回 `None`。
/// 各家返回格式不一：DeepSeek 402「Insufficient Balance」、OpenAI 429「exceeded quota」、
/// 智谱「余额不足，请充值」。统一按正文关键词识别，不依赖具体状态码。
fn recharge_hint(detail: &str) -> Option<&'static str> {
    let lower = detail.to_ascii_lowercase();
    let en = ["insufficient", "balance", "quota", "billing"]
        .iter().any(|k| lower.contains(k));
    let zh = ["余额", "额度", "充值", "欠费"].iter().any(|k| detail.contains(k));
    (en || zh).then_some("模型额度已用完，请前往服务商充值后重试")
}

#[derive(Clone)]
pub struct TranslateClient {
    http: Client,
}

impl TranslateClient {
    pub fn new() -> Self {
        Self {
            // 全局 60s 超时：避免某个请求挂起（服务器不回包）时导出/翻译无限期卡住。
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// 通过 OpenAI 兼容接口翻译文本。
    pub async fn translate(
        &self,
        request: &TranslateRequest,
        cfg: &TranslateConfig,
    ) -> Result<TranslateResult> {
        let system = format!(
            "你是一名专业翻译引擎。只输出译文本身，不要任何解释、注释或额外文字。将用户输入的{source}内容翻译成{target}。",
            source = lang_label(&request.source_lang),
            target = request.target_lang,
        );
        let content = self.translate_text(&request.text, &system, cfg).await?;
        Ok(TranslateResult {
            translated_text: content,
            detected_lang: None,
        })
    }

    /// 翻译正文段落：与 [`translate`](Self::translate) 相同，但额外要求模型保留
    /// 公式占位符 `⟦M0⟧`、`⟦M1⟧` 原样（导出翻译版 Word 时，公式不翻译、只翻译正文）。
    pub async fn translate_preserving(
        &self,
        text: &str,
        source_lang: &str,
        target_lang: &str,
        cfg: &TranslateConfig,
    ) -> Result<String> {
        let system = format!(
            "你是一名专业翻译引擎。将用户输入的{source}内容翻译成{target}，只输出译文本身，不要任何解释、注释或额外文字。\
             文中形如 ⟦M0⟧、⟦M1⟧ 的符号是公式占位符，必须原样保留、不得翻译或改动，也不要改变它们的顺序。",
            source = lang_label(source_lang),
            target = target_lang,
        );
        self.translate_text(text, &system, cfg).await
    }

    /// 发送单条翻译请求的公共实现：`translate` 与 `translate_preserving` 复用。
    async fn translate_text(
        &self,
        text: &str,
        system: &str,
        cfg: &TranslateConfig,
    ) -> Result<String> {
        if cfg.api_key.trim().is_empty() {
            return Err(anyhow!("尚未配置 API Key，请先打开「设置」填写"));
        }

        let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model: cfg.model.clone(),
            messages: vec![
                Message { role: "system".into(), content: MessageContent::Text(system.into()) },
                Message { role: "user".into(), content: MessageContent::Text(text.into()) },
            ],
            temperature: 0.2,
            stream: false,
        };

        self.chat(&url, &cfg.api_key, &body).await
    }

    /// 流式翻译：边生成边通过 `on_chunk` 回调把增量文本发出去，返回拼接后的完整译文。
    /// 前端据此实时刷新翻译结果，避免「等待整段生成」的观感延迟。
    pub async fn stream_translate(
        &self,
        request: &TranslateRequest,
        cfg: &TranslateConfig,
        mut on_chunk: impl FnMut(&str),
    ) -> Result<String> {
        if cfg.api_key.trim().is_empty() {
            return Err(anyhow!("尚未配置 API Key，请先打开「设置」填写"));
        }

        let system = format!(
            "你是一名专业翻译引擎。只输出译文本身，不要任何解释、注释或额外文字。将用户输入的{source}内容翻译成{target}。",
            source = lang_label(&request.source_lang),
            target = request.target_lang,
        );
        let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model: cfg.model.clone(),
            messages: vec![
                Message { role: "system".into(), content: MessageContent::Text(system) },
                Message { role: "user".into(), content: MessageContent::Text(request.text.clone()) },
            ],
            temperature: 0.2,
            stream: true,
        };

        self.stream_chat(&url, &cfg.api_key, &body, &mut on_chunk).await
    }

    /// 大模型 OCR：把图片（data URL）发给视觉多模态模型，返回识别出的文字。
    pub async fn ocr(&self, image_data_url: &str, cfg: &Config) -> Result<String> {
        let t = &cfg.translate;
        if t.api_key.trim().is_empty() {
            return Err(anyhow!("尚未配置 API Key，请先打开「设置」填写"));
        }

        let model = if cfg.ocr.model.trim().is_empty() {
            t.model.clone()
        } else {
            cfg.ocr.model.clone()
        };
        let url = format!("{}/chat/completions", t.base_url.trim_end_matches('/'));

        // `auto` 或留空时不传 detail（用服务端默认，即原图），`high`/`low` 显式指定。
        let detail = match cfg.ocr.detail.as_str() {
            "" | "auto" => None,
            d => Some(d.to_string()),
        };

        let body = ChatRequest {
            model,
            messages: vec![Message {
                role: "user".into(),
                content: MessageContent::Parts(vec![
                    ContentPart { kind: "text".into(), text: Some(OCR_PROMPT.into()), image_url: None },
                    ContentPart {
                        kind: "image_url".into(),
                        text: None,
                        image_url: Some(ImageUrl { url: image_data_url.into(), detail }),
                    },
                ]),
            }],
            temperature: 0.0,
            stream: false,
        };

        let content = self.chat(&url, &t.api_key, &body).await?;
        Ok(collapse_soft_newlines(&content))
    }

    /// GLM-OCR（智谱 BigModel）版面解析：把图片（data URL）发给专用 `layout_parsing` 接口。
    /// 返回 `md_results`（Markdown 文本），保留标题 / 表格 / 公式等版面结构。
    pub async fn ocr_glm(&self, image_data_url: &str, cfg: &Config) -> Result<String> {
        let key = cfg.ocr.glm_api_key.trim();
        if key.is_empty() {
            return Err(anyhow!("尚未配置 GLM-OCR API Key，请先打开「设置」填写智谱 BigModel 的 API Key"));
        }

        let body = GlmOcrRequest {
            model: "glm-ocr".to_string(),
            file: image_data_url.to_string(),
        };

        // 429 限流 / 5xx 服务端错误自动指数退避重试（与 chat 一致）。
        const MAX_RETRIES: u32 = 4;
        let mut attempt: u32 = 0;
        let resp = loop {
            let resp = self
                .http
                .post("https://open.bigmodel.cn/api/paas/v4/layout_parsing")
                .header("Authorization", format!("Bearer {key}"))
                .json(&body)
                .send()
                .await
                .context("GLM-OCR 请求失败（网络错误或无法连接服务器）")?;

            let status = resp.status().as_u16();
            if matches!(status, 429 | 500..=599) && attempt < MAX_RETRIES {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
                continue;
            }
            break resp;
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let hint = recharge_hint(&detail).unwrap_or_else(|| match status.as_u16() {
                429 => "请求过于频繁（限流），请稍后重试",
                401 => "GLM-OCR API Key 无效或未授权",
                400 => "请求被拒绝（图片格式或大小不符合要求）",
                _ => "服务返回错误",
            });
            return Err(anyhow!("{hint}（HTTP {status}）：{detail}"));
        }

        let parsed: GlmOcrResponse = resp.json().await.context("解析 GLM-OCR 响应失败")?;
        // 保留原始 Markdown（含 $...$ / $$...$$ LaTeX 公式）：.docx 导出时需要
        // 原样 LaTeX 交给大模型转 OMML；Unicode 转换放到 .txt 导出时再做。
        if !parsed.md_results.trim().is_empty() {
            return Ok(collapse_soft_newlines(&parsed.md_results));
        }
        if let Some(code) = parsed.code {
            return Err(anyhow!(
                "{}（code {code}）：{}",
                recharge_hint(&parsed.message).unwrap_or("GLM-OCR 返回错误"),
                parsed.message
            ));
        }
        Err(anyhow!("GLM-OCR 返回结果为空"))
    }

    /// 云端版面解析：把一页图片发给视觉大模型，返回结构化内容
    /// （正文去公式/去页眉页脚、表格按行列、图片带位置框）。
    ///
    /// 复用 `ocr` 同款的多模态请求体与 `chat` 请求路径，只是换成解析 prompt，
    /// 并把返回的 JSON 解析成 [`ExtractedPage`]。
    pub async fn extract_page(&self, image_data_url: &str, cfg: &Config) -> Result<ExtractedPage> {
        let t = &cfg.translate;
        if t.api_key.trim().is_empty() {
            return Err(anyhow!("尚未配置 API Key，请先打开「设置」填写"));
        }

        let model = if cfg.ocr.model.trim().is_empty() {
            t.model.clone()
        } else {
            cfg.ocr.model.clone()
        };
        let url = format!("{}/chat/completions", t.base_url.trim_end_matches('/'));

        let detail = match cfg.ocr.detail.as_str() {
            "" | "auto" => None,
            d => Some(d.to_string()),
        };

        let body = ChatRequest {
            model,
            messages: vec![Message {
                role: "user".into(),
                content: MessageContent::Parts(vec![
                    ContentPart {
                        kind: "text".into(),
                        text: Some(EXTRACT_PROMPT.into()),
                        image_url: None,
                    },
                    ContentPart {
                        kind: "image_url".into(),
                        text: None,
                        image_url: Some(ImageUrl { url: image_data_url.into(), detail }),
                    },
                ]),
            }],
            temperature: 0.0,
            stream: false,
        };

        let raw = self.chat(&url, &t.api_key, &body).await?;
        Ok(parse_extracted_page(&raw))
    }

    /// 用大模型把一批 LaTeX 公式**一次性**转成 OMML（比逐公式调用快一个数量级，
    /// 尤其对推理型模型：一次请求、一次「思考」搞定整批）。
    ///
    /// 返回与输入等长的 `Vec`，每个元素是 `Some(OMML)`（成功）或 `None`（该公式
    /// 转换失败，由调用方回退为 Unicode 文本）。
    ///
    /// 注意：此方法已被本地确定性转换 [`crate::translate::omml::latex_to_omml`]
    /// 取代，目前仅作保留。
    #[allow(dead_code)]
    pub async fn latex_to_omml_batch(
        &self,
        formulas: &[String],
        cfg: &Config,
    ) -> Result<Vec<Option<String>>> {
        let t = &cfg.translate;
        if t.api_key.trim().is_empty() {
            return Err(anyhow!("尚未配置 API Key，请先打开「设置」填写"));
        }

        let model = if cfg.ocr.model.trim().is_empty() {
            t.model.clone()
        } else {
            cfg.ocr.model.clone()
        };
        let url = format!("{}/chat/completions", t.base_url.trim_end_matches('/'));

        // 给出两个最小但完整的 OMML 例子（分式、下标），让模型照着结构输出，
        // 避免它「自由发挥」成 LaTeX 源码或纯文本。
        let system = "你是 LaTeX 到 OMML（Office Math Markup Language，Word 原生公式）的转换器。\
用户会给出一组 LaTeX 公式（每行一个），你要逐个转成 OMML。对每个公式只输出一个 <m:oMath>…</m:oMath> \
XML 元素（使用 m: 与 w: 命名空间前缀），各结果之间用单独一行 ===OMATH=== 分隔，顺序与输入一致。\
不要输出任何解释、Markdown 代码块或额外文字。\
示例：\\frac{a}{b} → <m:oMath><m:f><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b</m:t></m:r></m:den></m:f></m:oMath>；\
x_i → <m:oMath><m:sSub><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sub><m:r><m:t>i</m:t></m:r></m:sub></m:sSub></m:oMath>";

        let user = formulas
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {f}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let body = ChatRequest {
            model,
            messages: vec![
                Message {
                    role: "system".into(),
                    content: MessageContent::Text(system.to_string()),
                },
                Message {
                    role: "user".into(),
                    content: MessageContent::Text(user),
                },
            ],
            temperature: 0.0,
            stream: false,
        };

        let content = self.chat(&url, &t.api_key, &body).await?;
        let extracted = extract_all_omml(&content);
        // 诊断：模型成功响应却没给 OMML 时，把原始输出打到控制台，便于排查。
        if extracted.is_empty() && !content.trim().is_empty() {
            eprintln!("[omml] 模型未返回 OMML，原始输出：\n{content}");
        }
        let mut out: Vec<Option<String>> = extracted.into_iter().map(Some).collect();
        out.resize(formulas.len(), None);
        out.truncate(formulas.len());
        Ok(out)
    }

    /// 发送 chat 请求并提取返回的文本内容（翻译与 OCR 共用）。
    async fn chat(&self, url: &str, api_key: &str, body: &ChatRequest) -> Result<String> {
        // 429 限流 / 5xx 服务端错误自动指数退避重试（1s、2s、4s、8s，最多 4 次）：
        // 避免单个请求撞上瞬时限流就导致整篇导出失败。
        const MAX_RETRIES: u32 = 4;
        let mut attempt: u32 = 0;
        let resp = loop {
            let resp = self
                .http
                .post(url)
                .bearer_auth(api_key)
                .json(body)
                .send()
                .await
                .context("请求失败（网络错误或无法连接服务器）")?;

            let status = resp.status().as_u16();
            if matches!(status, 429 | 500..=599) && attempt < MAX_RETRIES {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
                continue;
            }
            break resp;
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let hint = recharge_hint(&detail).unwrap_or_else(|| match status.as_u16() {
                429 => "请求过于频繁（限流），请稍后重试",
                401 => "API Key 无效或未授权",
                404 => "接口地址或模型名不正确",
                400 => "请求被拒绝（可能是该模型不支持图片输入）",
                _ => "服务返回错误",
            });
            return Err(anyhow!("{hint}（HTTP {status}）：{detail}"));
        }

        let parsed: ChatResponse = resp.json().await.context("解析响应失败")?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message)
            .map(|m| m.content)
            .unwrap_or_default()
            .trim()
            .to_string();

        if content.is_empty() {
            return Err(anyhow!("返回结果为空"));
        }
        Ok(content)
    }

    /// 流式发送 chat 请求：解析 SSE（`data: {...}`），逐段回调增量内容，返回拼接后的完整文本。
    async fn stream_chat<F>(
        &self,
        url: &str,
        api_key: &str,
        body: &ChatRequest,
        on_chunk: &mut F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        use futures_util::StreamExt;

        // 重试逻辑与 `chat` 一致。
        const MAX_RETRIES: u32 = 4;
        let mut attempt: u32 = 0;
        let resp = loop {
            let resp = self
                .http
                .post(url)
                .bearer_auth(api_key)
                .json(body)
                .send()
                .await
                .context("请求失败（网络错误或无法连接服务器）")?;

            let status = resp.status().as_u16();
            if matches!(status, 429 | 500..=599) && attempt < MAX_RETRIES {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
                continue;
            }
            break resp;
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            let hint = recharge_hint(&detail).unwrap_or_else(|| match status.as_u16() {
                429 => "请求过于频繁（限流），请稍后重试",
                401 => "API Key 无效或未授权",
                404 => "接口地址或模型名不正确",
                400 => "请求被拒绝（可能是该模型不支持图片输入）",
                _ => "服务返回错误",
            });
            return Err(anyhow!("{hint}（HTTP {status}）：{detail}"));
        }

        let mut full = String::new();
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("读取流式响应失败")?;
            // 先按原始字节累积：网络分块可能把多字节 UTF-8 字符（中文）切半，
            // 逐个 chunk 做 lossy 解码会把半个字符变成 �（乱码）。
            buf.extend_from_slice(&chunk);
            // SSE 按行解析：只对完整的行（以 \n 结尾）解码，保证多字节字符不被切开。
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&buf[..nl]).trim().to_string();
                buf.drain(..=nl);
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                if let Ok(parsed) = serde_json::from_str::<StreamResponse>(data) {
                    for c in parsed.choices {
                        if !c.delta.content.is_empty() {
                            on_chunk(&c.delta.content);
                            full.push_str(&c.delta.content);
                        }
                    }
                }
            }
        }

        if full.is_empty() {
            return Err(anyhow!("返回结果为空"));
        }
        Ok(full)
    }
}

fn lang_label(lang: &str) -> String {
    if lang.is_empty() || lang.eq_ignore_ascii_case("auto") {
        "源语言".to_string()
    } else {
        lang.to_string()
    }
}

/// 合并 OCR 输出里的「行内换行」：把相邻的非结构行拼成一段（用空格连接），
/// 仅在空行（段落分隔）或结构行（标题 / 表格 / 列表 / 块级公式 / HTML 块 / 分隔线）处断行。
///
/// 竖排古籍经版面解析后常「一列一行」——每列都是一个单独的行，段内并没有语义换行，
/// 直接输出就会每列都换行。此函数把这些软换行合并，还原成连续段落。
/// GLM 的 `layout_parsing` 本身已按段落用空行分隔，此函数对其基本是空操作；
/// 主要兜底大模型 `ocr`（视觉抄字）那种「每行末尾都加换行」的输出。
fn collapse_soft_newlines(md: &str) -> String {
    let mut out = String::new();
    let mut para: Vec<String> = Vec::new();
    let mut in_math = false;

    for line in md.lines() {
        let t = line.trim();

        // 块级公式 $$…$$：保持原样，块内多行 LaTeX 不做合并。
        if t.starts_with("$$") {
            flush_para(&mut out, &mut para);
            push_line(&mut out, t);
            // 单行 `$$…$$` 立即闭合；否则进入块模式，直到遇到含闭合 `$$` 的行。
            in_math = !(t.len() > 2 && t[2..].contains("$$"));
            continue;
        }
        if in_math {
            push_line(&mut out, t);
            if t == "$$" || t.ends_with("$$") {
                in_math = false;
            }
            continue;
        }

        if t.is_empty() {
            flush_para(&mut out, &mut para);
            out.push('\n'); // 空行 = 段落分隔
        } else if is_structural_line(t) {
            flush_para(&mut out, &mut para);
            push_line(&mut out, t);
        } else {
            para.push(t.to_string());
        }
    }
    flush_para(&mut out, &mut para);
    out
}

/// 追加一行到输出（行间用单个 `\n` 分隔）。
fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

/// 把累积的段落文字落盘（行内用空格连接），并清空缓存。
fn flush_para(out: &mut String, para: &mut Vec<String>) {
    if para.is_empty() {
        return;
    }
    push_line(out, &para.join(" "));
    para.clear();
}

/// 判断一行是否属于「结构行」：标题 / 表格 / 列表 / HTML 块 / 分隔线 / 代码围栏。
/// 结构行必须原样保留（不参与软换行合并）。
fn is_structural_line(t: &str) -> bool {
    if t.starts_with('#')
        || t.starts_with('|')
        || t.starts_with('<')
        || t.starts_with("---")
        || t.starts_with("```")
    {
        return true;
    }
    let b = t.as_bytes();
    // 无序列表：`- ` / `* ` / `+ `。
    if matches!(b.first(), Some(b'-' | b'*' | b'+')) && b.get(1) == Some(&b' ') {
        return true;
    }
    // 有序列表：`1. ` / `1) `（最多 3 位数字）。
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0
        && digits <= 3
        && matches!(b.get(digits), Some(b'.' | b')'))
        && b.get(digits + 1) == Some(&b' ')
}

/// 稳健解析大模型返回的 JSON：剥掉 ```json 围栏与任何前后缀说明文字，
/// 只取首个 `{` 到末个 `}` 之间的内容。
///
/// 解析失败时回退为「整段文本当单段落」，保证模型偶尔不按 JSON 输出时也能拿到文字。
fn parse_extracted_page(raw: &str) -> ExtractedPage {
    let json = raw
        .find('{')
        .and_then(|s| raw.rfind('}').map(|e| &raw[s..=e]));
    if let Some(json) = json {
        if let Ok(page) = serde_json::from_str::<ExtractedPage>(json) {
            return page;
        }
    }

    let text = raw.trim().to_string();
    if text.is_empty() {
        return ExtractedPage::default();
    }
    ExtractedPage { paragraphs: vec![text], ..ExtractedPage::default() }
}

/// 从大模型返回里按顺序提取所有 `<m:oMath>…</m:oMath>` 元素。
///
/// 兼容多种模型输出：可能带代码围栏、解释文字、`===OMATH===` 分隔符、
/// 甚至多个 `<m:oMath>` 直接相邻；只按标签对精确截取，不受这些干扰。
///
/// 注意：仅被已保留的 [`TranslateClient::latex_to_omml_batch`] 使用。
#[allow(dead_code)]
fn extract_all_omml(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(start) = find_omath_open(rest) {
        let after = &rest[start..];
        let Some(end_rel) = after.find("</m:oMath") else { break };
        let Some(close_rel) = after[end_rel..].find('>').map(|k| end_rel + k + 1) else { break };
        let end = start + close_rel;
        out.push(rest[start..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// 定位 `<m:oMath` 起始位置，排除 `<m:oMathPara` / `<m:oMathPr` 这类前缀误匹配。
#[allow(dead_code)]
fn find_omath_open(s: &str) -> Option<usize> {
    let mut i = 0;
    while let Some(pos) = s[i..].find("<m:oMath") {
        let p = i + pos;
        let next = s[p + "<m:oMath".len()..].chars().next().unwrap_or('>');
        if !(next.is_ascii_alphanumeric() || next == ':' || next == '_') {
            return Some(p);
        }
        i = p + 1;
    }
    None
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    #[serde(skip_serializing_if = "is_false")]
    stream: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: MessageContent,
}

/// 请求消息内容：纯文本，或多模态（文本 + 图片）片段。
#[derive(Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize)]
struct ContentPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<ImageUrl>,
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Option<ResponseMessage>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

/// 流式响应：SSE 每帧 `data:` 里的 JSON（`choices[0].delta.content` 为增量文本）。
#[derive(Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: String,
}

#[derive(Serialize)]
struct GlmOcrRequest {
    model: String,
    file: String,
}

/// GLM-OCR `layout_parsing` 的响应：文本在 `md_results`（Markdown），
/// 失败时（部分实现以 200 + code 返回）落到 `code` / `message`。
#[derive(Deserialize)]
struct GlmOcrResponse {
    #[serde(default)]
    md_results: String,
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: String,
}

#[cfg(test)]
mod tests {
    use super::collapse_soft_newlines;

    #[test]
    fn merges_soft_linebreaks_into_paragraphs() {
        let md = "第一章\n第一列文字\n第二列文字\n第三列文字\n\n第二章\n甲\n乙\n丙";
        let out = collapse_soft_newlines(md);
        assert_eq!(
            out,
            "第一章 第一列文字 第二列文字 第三列文字\n\n第二章 甲 乙 丙"
        );
    }

    #[test]
    fn preserves_headings_and_tables() {
        let md = "# 标题\n\n正文第一行\n正文第二行\n\n| 列1 | 列2 |\n| --- | --- |\n| 1 | 2 |";
        let out = collapse_soft_newlines(md);
        assert_eq!(
            out,
            "# 标题\n\n正文第一行 正文第二行\n\n| 列1 | 列2 |\n| --- | --- |\n| 1 | 2 |"
        );
    }

    #[test]
    fn preserves_display_math_verbatim() {
        let md = "公式如下\n$$\n\\frac{a}{b}\n= c\n$$\n结束";
        let out = collapse_soft_newlines(md);
        assert_eq!(out, "公式如下\n$$\n\\frac{a}{b}\n= c\n$$\n结束");
    }

    #[test]
    fn no_op_for_already_paragraph_separated_text() {
        // GLM layout_parsing 已用空行分隔段落：应原样返回（不改动）。
        let md = "第一段。\n\n第二段。\n\n第三段。";
        assert_eq!(collapse_soft_newlines(md), md);
    }
}
