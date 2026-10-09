# PDFReader

Windows 上的 PDF 阅读与翻译桌面应用（Rust + Tauri 2 + PDF.js）。

## 下载

到 [Releases](https://github.com/Wulongkun/PDFReader/releases/latest) 下载最新版安装包（解压即用，绿色免安装，无需 Python）。

> 国内网络访问 GitHub Releases 慢的，可改从个人服务器快速下载：**http://yuanjingzh.cn/app/**

| 文件 | 适用 |
| --- | --- |
| `PDFReader-online-x64.zip` | 64 位系统，联网版（需系统已装 WebView2） |
| `PDFReader-online-x86.zip` | 32 位系统，联网版（需系统已装 WebView2） |
| `PDFReader-standalone-x64.zip` | 64 位系统，自包含版（内置 WebView2 安装器） |
| `PDFReader-standalone-x86.zip` | 32 位系统，自包含版（内置 WebView2 安装器） |

> 仅 x64 版本包含「原生 Word 导出」的 pdfomml 后端；导出 Word / 翻译需联网或本地模型。

解压后运行 `PDFReader.exe` 即可。激活码见应用内「获取激活码」入口。

## 技术栈

- **Tauri 2** + 系统 WebView2 渲染前端
- **前端**：原生 HTML/JS/CSS + PDF.js（渲染 + 文本层提取）
- **Rust 后端**：文件读取、翻译 API 调用、本地配置持久化
- **OCR**：两种方式可选 —— ① 本地：Windows 系统自带 OCR（`Windows.Media.Ocr`），离线可用；② 大模型：视觉多模态模型识别（复用 OpenAI 兼容接口），需联网
- **翻译**：OpenAI 兼容接口（DeepSeek / 通义千问 / Kimi 等）

## 功能

- 打开 / 渲染 / 缩放 / 适应宽度
- **垂直连续翻页**：所有页纵向堆叠、滚动阅读（默认），懒加载渲染节省内存；上一页/下一页/页码输入平滑滚动到对应页顶部
- **Ctrl + 滚轮缩放**：按住 Ctrl 滚动鼠标滚轮缩放，带平滑过渡动画并保持阅读位置；工具栏缩放按钮同样平滑
- **目录（左侧侧边栏）**：可展开常驻左侧、也可收起；优先读取 PDF 内置书签目录，无书签时可从正文标题自动生成（扫描版需 OCR），点击目录项跳页，随滚动高亮当前章节
- 文字提取（PDF.js 文本层，自动识别单栏 / 双栏并正确排序）
- **导出文本**：文本层 + 扫描页 OCR（本地 / 大模型二选一）混合提取整篇文档，保存为 `.txt`
- **导出 Word**：生成带大纲级别的 `.docx`，标题层级由字号启发式识别（与原文一致），可在导航窗格中显示；同时尽量保留原排版——每行独立成段（保留换行与段落间距），并应用原文字号 / 加粗 / 斜体 / 居中 / 左缩进
- 选中 / 全文翻译
- **OCR 两种方式**：本地（Windows 自带，可指定语言）/ 大模型（视觉多模态）

## 目录结构

```
src/                    前端（index.html / main.js / styles.css）
  vendor/               PDF.js 本地构建（pdf.min.mjs / pdf.worker.min.mjs）
src-tauri/
  src/
    main.rs / lib.rs    Tauri 入口与组装
    app.rs              应用状态
    pdf/                文件读取 / 文字提取(占位) / 本地 OCR（Windows.Media.Ocr）
    translate/          翻译 + 大模型 OCR API 调用
    config/             配置读写（%APPDATA%/PDFReader/config.json）
    ui/                 Rust 侧命令（invoke 入口）
  tauri.conf.json       应用配置
  capabilities/         权限声明
```

## 运行

> ⚠️ **依赖说明**：本仓库通过相对路径 `../../pdfomml/rust/pdfomml` 引用 pdfomml（PDF→Word 原生公式引擎），该源码为私有、**不在本仓库内**。仅克隆本仓库无法直接编译——需在同级目录放置 pdfomml 仓库后，`cargo tauri build` 才能通过。

```bash
# 1. 安装 Tauri CLI（二选一）
cargo install tauri-cli --version "^2"
# 或
npm i -D @tauri-apps/cli

# 2. 生成图标（仓库里已生成，可跳过）
node scripts/gen-icons.mjs

# 3. 开发运行
cargo tauri dev

# 4. 打包
cargo tauri build
```

## 配置

API Key / Base URL / 模型等在应用内「设置」中填写，保存到
`%APPDATA%/PDFReader/config.json`。默认走 **OpenAI 兼容接口**，
DeepSeek / 通义千问 / Kimi 等国内可访问的服务均可直接使用。

- **翻译**：用「模型」字段指定的文本模型。
- **OCR 方式**：
  - `local`（默认）：Windows 自带 OCR，离线、无需 Key。「OCR 语言」可选跟随系统或显式指定（zh-Hans / zh-Hant / en-US / ja-JP / ko-KR）。
  - `llm`：把页面图片发给视觉多模态模型识别，需在「OCR 模型」填一个支持图片输入的模型（如通义 VL、Kimi 视觉、GPT-4V、`deepseek-v4-flash-vision-exp`），留空则回退到「模型」字段；复用同一 base_url / api_key，需联网。「OCR 清晰度」可选 `low` 把图片降采样到 512px 以省 token（对极小字号可能略降准确度）。

## 双栏与标题识别（启发式）

- **双栏**：按行 x 区间并集检测页面中段的垂直栏缝，识别为双栏后按「通栏标题 → 左栏 → 右栏」排序；未检测到栏缝则按单栏自上而下。
- **标题层级**：以按文字量加权的字号中位数为正文基准，字号 1.4× / 1.2× / 1.1×（或加粗短行）分别映射为一 / 二 / 三级标题，导出 Word 时对应 `Heading 1/2/3`。
- **排版还原（Word 导出）**：逐行输出、行距明显大于正常行距时插入空行以保留段落间距；字号取 PDF 文本项高度（近似 pt，0.5pt 精度、限制 4–72pt）；`fontName` 含 Bold/Italic 时套用加粗/斜体；左右留白接近且较宽时判定为居中；相对正文块左缘的偏移保留为左缩进。扫描页（无文本层）走 OCR，用逐行包围盒还原字号 / 行距 / 居中 / 双栏 / 标题层级 / 缩进，但加粗 / 斜体无法从 OCR 恢复。字体族、颜色、首行缩进等暂未还原。
- 这些阈值针对常见排版设定，遇到特殊版式可调整 `src/main.js` 中的 `detectColumnSplit` / `headingLevel` / `isCentered`。

## 升级 PDF.js

PDF.js 已本地化到 `src/vendor/`，运行与打包均不依赖外网。如需升级：

```bash
curl -L -o src/vendor/pdf.min.mjs \
  https://cdn.jsdelivr.net/npm/pdfjs-dist@4/build/pdf.min.mjs
curl -L -o src/vendor/pdf.worker.min.mjs \
  https://cdn.jsdelivr.net/npm/pdfjs-dist@4/build/pdf.worker.min.mjs
```

当前内置版本：**4.10.38**。
