#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 自包含版：若机器上没有 WebView2 运行时，且 exe 同目录带有 WebView2Setup.exe
    // （Evergreen 离线安装器），则先静默安装再启动。联网版无此文件，直接依赖系统 WebView2。
    if !webview2_installed() {
        if let Some(installer) = bundled_webview2_installer() {
            let _ = std::process::Command::new(&installer)
                .args(["/silent", "/install"])
                .status();
        }
    }

    // 大页面首帧黑块已在内容层修复，无需再禁用 GPU：
    //  - 前端 MAX_RENDER_EDGE 限制画布长边，避开 GPU 纹理上限导致的合成黑块；
    //  - 渲染前铺白底 + 清理页用 1×1 画布，避免透明/0×0 画布合成成黑块；
    //  - Rust 侧 normalize_scanned_pdf 修正 1-bit /Decode[1 0] 位图整页反相。
    // 保留 GPU 合成可让缩放预览与滚动更流畅。
    pdfreader_lib::run()
}

/// WebView2 Evergreen 运行时是否已安装（检查其客户端注册表项的版本值）。
fn webview2_installed() -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    const GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    let keys = [
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{GUID}")),
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{GUID}")),
        (HKEY_CURRENT_USER, format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{GUID}")),
    ];
    keys.into_iter().any(|(root, key)| {
        RegKey::predef(root)
            .open_subkey(key)
            .and_then(|k| k.get_value::<String, _>("pv"))
            .is_ok()
    })
}

/// exe 同目录下的 WebView2 离线安装器（自包含版才附带）。
fn bundled_webview2_installer() -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = exe_dir.join("WebView2Setup.exe");
    candidate.is_file().then_some(candidate)
}