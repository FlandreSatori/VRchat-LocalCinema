use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListenMode {
    /// 仅本机 127.0.0.1
    Loopback,
    /// 局域网 0.0.0.0
    Lan,
    /// 旧版本自定义 IP，仅用于读取兼容配置并迁移为局域网模式
    #[serde(alias = "custom")]
    Custom,
}

impl Default for ListenMode {
    fn default() -> Self {
        ListenMode::Loopback
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UrlMode {
    Localhost,
    Lan,
    /// 旧版本值，仅用于读取兼容配置并迁移到局域网模式
    #[serde(alias = "easytier")]
    Easytier,
}

impl Default for UrlMode {
    fn default() -> Self {
        UrlMode::Localhost
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseBehavior {
    Exit,
    Tray,
}

impl Default for CloseBehavior {
    fn default() -> Self {
        CloseBehavior::Exit
    }
}

fn default_port() -> u16 {
    17001
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub listen_mode: ListenMode,
    /// listen_mode = custom 时使用的本机 IP
    pub custom_ip: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    pub default_url_mode: UrlMode,
    /// 手动输入的 EasyTier 虚拟 IPv4
    pub easytier_ip: String,
    /// 默认外部播放器路径，空表示未设置
    pub default_player_path: String,
    /// 播放器附加参数模板，{path} 会被替换为文件路径；为空则仅传文件路径
    pub default_player_args: String,
    /// 封面缓存目录，空表示使用 data_dir/thumbnails
    pub thumb_dir: String,
    pub autostart: bool,
    pub close_behavior: CloseBehavior,
    /// 外部 ffmpeg/ffprobe 路径，空表示自动从 PATH 查找
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen_mode: ListenMode::Loopback,
            custom_ip: None,
            port: default_port(),
            default_url_mode: UrlMode::Localhost,
            easytier_ip: String::new(),
            default_player_path: String::new(),
            default_player_args: String::new(),
            thumb_dir: String::new(),
            autostart: false,
            close_behavior: CloseBehavior::Exit,
            ffmpeg_path: String::new(),
            ffprobe_path: String::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> AppResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        let mut settings: Settings = serde_json::from_str(&text)?;
        settings.normalize_network_mode();
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }

    pub fn normalize_network_mode(&mut self) {
        if self.listen_mode == ListenMode::Custom {
            self.listen_mode = ListenMode::Lan;
        }
        self.default_url_mode = match self.listen_mode {
            ListenMode::Loopback => UrlMode::Localhost,
            ListenMode::Lan | ListenMode::Custom => UrlMode::Lan,
        };
    }

    /// 计算绑定的监听 IP
    pub fn bind_ip(&self) -> String {
        match self.listen_mode {
            ListenMode::Loopback => "127.0.0.1".to_string(),
            ListenMode::Lan => "0.0.0.0".to_string(),
            // 旧配置中的自定义地址不再作为对外选项，统一按局域网模式监听。
            ListenMode::Custom => "0.0.0.0".to_string(),
        }
    }
}

/// 应用目录结构
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub thumb_dir: PathBuf,
    pub log_dir: PathBuf,
    pub settings_file: PathBuf,
    pub db_file: PathBuf,
    pub portable: bool,
}

/// 便携模式判定：exe 同目录存在 portable.dat 文件或 data 目录；开发模式下总是便携（数据放 exe 目录的 data 下）
pub fn resolve_paths(exe_path: &Path) -> AppPaths {
    let exe_dir = exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let portable_marker = exe_dir.join("portable.dat");
    let portable_data = exe_dir.join("data");
    let portable = cfg!(debug_assertions) || portable_marker.exists() || portable_data.exists();

    let data_dir = if portable {
        portable_data
    } else {
        let base = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| exe_dir.clone());
        base.join("VRchat-LocalCinema")
    };

    let _ = fs::create_dir_all(&data_dir);
    AppPaths {
        thumb_dir: data_dir.join("thumbnails"),
        log_dir: data_dir.join("logs"),
        settings_file: data_dir.join("settings.json"),
        db_file: data_dir.join("library.db"),
        data_dir,
        portable,
    }
}
