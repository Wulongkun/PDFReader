#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 大页面首帧黑块已在内容层修复，无需再禁用 GPU：
    //  - 前端 MAX_RENDER_EDGE 限制画布长边，避开 GPU 纹理上限导致的合成黑块；
    //  - 渲染前铺白底 + 清理页用 1×1 画布，避免透明/0×0 画布合成成黑块；
    //  - Rust 侧 normalize_scanned_pdf 修正 1-bit /Decode[1 0] 位图整页反相。
    // 保留 GPU 合成可让缩放预览与滚动更流畅。
    pdfreader_lib::run()
}