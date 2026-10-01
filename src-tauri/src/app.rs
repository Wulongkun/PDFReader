//! 应用全局状态：配置 + 翻译客户端 + 书架。

use std::sync::Mutex;

use crate::{
    config::Config,
    library::Library,
    orientation::OrientationCache,
    translate::client::TranslateClient,
};

pub struct AppState {
    /// 当前配置（内存中），由 `get_config` / `set_config` 读写。
    pub config: Mutex<Config>,
    /// 翻译客户端，内部持有可复用的 HTTP 连接池。
    pub translator: TranslateClient,
    /// 书架 / 最近阅读 / 收藏（内存中），由书架相关命令读写。
    pub library: Mutex<Library>,
    /// 排版方向检测结果缓存（内存中），由 `get_orientation` / `set_orientation` 读写。
    pub orientation: Mutex<OrientationCache>,
}

impl AppState {
    pub fn new() -> Self {
        // 读取失败（例如首次运行还没有配置文件）时回退到默认值，不影响启动。
        let config = match Config::load() {
            Ok(cfg) => cfg,
            Err(err) => {
                eprintln!("[config] 读取配置失败，使用默认值：{err}");
                Config::default()
            }
        };

        let library = match Library::load() {
            Ok(lib) => lib,
            Err(err) => {
                eprintln!("[library] 读取书架失败，使用默认值：{err}");
                Library::default()
            }
        };

        let orientation = match OrientationCache::load() {
            Ok(cache) => cache,
            Err(err) => {
                eprintln!("[orientation] 读取排版缓存失败，使用空缓存：{err}");
                OrientationCache::default()
            }
        };

        Self {
            config: Mutex::new(config),
            translator: TranslateClient::new(),
            library: Mutex::new(library),
            orientation: Mutex::new(orientation),
        }
    }
}
