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
    /// 直接退出程序（先停止后台服务）
    Exit,
    /// 最小化到托盘，服务继续运行
    Tray,
    /// 每次关闭主窗口时弹窗询问
    Ask,
}

impl Default for CloseBehavior {
    fn default() -> Self {
        // 关闭主窗口时先询问用户，避免误关闭导致服务停止；用户可在设置中固定为某一动作
        CloseBehavior::Ask
    }
}

/// 配置结构版本号：用于对历史配置做一次性迁移（见 [`Settings::normalize_legacy`]）
const SETTINGS_REVISION: u32 = 1;

fn default_port() -> u16 {
    17001
}

fn default_easytier_protocols() -> Vec<String> {
    vec!["tcp".to_string(), "ws".to_string()]
}

fn default_room_name() -> String {
    "VRchat".to_string()
}

fn default_room_password() -> String {
    "123".to_string()
}

fn default_easytier_listen_port() -> u16 {
    11010
}

fn default_stream_buffer_kb() -> u32 {
    256
}

fn default_disable_power_throttling() -> bool {
    true
}

fn default_streaming_720p_bitrate_mbps() -> f64 {
    3.0
}

fn default_streaming_1080p_bitrate_mbps() -> f64 {
    6.0
}

/// EasyTier 组网配置，对应 easytier-core 的命令行参数
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EasytierConfig {
    /// 启用的协议类型：tcp / udp / ws / wss / quic
    pub protocols: Vec<String>,
    /// 中转服务器（对等节点）地址，为空表示仅监听
    pub peer_url: String,
    /// 房间名，对应 --network-name
    pub room_name: String,
    /// 房间密码，对应 --network-secret
    pub room_password: String,
    /// 成员名，对应 --hostname
    pub member_name: String,
    /// 虚拟 IPv4，对应 --ipv4（DHCP 开启时忽略）
    pub ipv4: String,
    /// 虚拟 IPv6，对应 --ipv6
    pub ipv6: String,
    /// 自动分配虚拟 IP（DHCP），对应 -d
    pub dhcp: bool,
    /// KCP 代理，对应 --enable-kcp-proxy
    pub kcp_proxy: bool,
    /// 禁用 KCP 输入，对应 --disable-kcp-input
    pub kcp_input: bool,
    /// QUIC 代理，对应 --enable-quic-proxy
    pub quic_proxy: bool,
    /// 禁用 QUIC 输入，对应 --disable-quic-input
    pub quic_input: bool,
    /// 低延迟模式，对应 --latency-first
    pub latency_first: bool,
    /// 传输数据压缩算法：none / zstd，对应 --compression
    pub compression: String,
    /// UPnP / NAT 打洞，关闭时禁用 UDP 打洞
    pub upnp: bool,
    /// 多线程，对应 --multi-thread
    pub multi_thread: bool,
    /// 监听起始端口
    pub listen_port: u16,
}

impl Default for EasytierConfig {
    fn default() -> Self {
        Self {
            protocols: default_easytier_protocols(),
            peer_url: String::new(),
            room_name: default_room_name(),
            room_password: default_room_password(),
            member_name: String::new(),
            ipv4: String::new(),
            ipv6: String::new(),
            // 对齐 EasytierGame：DHCP 与 KCP 代理默认开启
            dhcp: true,
            kcp_proxy: true,
            quic_proxy: false,
            kcp_input: false,
            quic_input: false,
            latency_first: false,
            compression: "none".to_string(),
            upnp: true,
            multi_thread: true,
            listen_port: default_easytier_listen_port(),
        }
    }
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
    /// 手动指定的局域网网卡 IP；空表示自动（EasyTier 运行时优先虚拟 IP，其次物理网卡）
    #[serde(default)]
    pub lan_adapter_ip: String,
    /// 是否允许其他人（对等节点）查看本机媒体库；开启时会自动放行入站防火墙
    #[serde(default)]
    pub allow_peer_view: bool,
    /// 默认外部播放器路径，空表示未设置
    pub default_player_path: String,
    /// 播放器附加参数模板，{path} 会被替换为文件路径；为空则仅传文件路径
    pub default_player_args: String,
    /// 封面缓存目录，空表示使用 data_dir/thumbnails
    pub thumb_dir: String,
    pub autostart: bool,
    pub close_behavior: CloseBehavior,
    /// 配置结构版本号：用于历史配置的一次性迁移，请勿手动修改
    #[serde(default)]
    pub settings_revision: u32,
    /// 外部 ffmpeg/ffprobe 路径，空表示自动从 PATH 查找
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    /// easytier-core 路径，空表示自动检测（内置优先，其次系统 PATH）
    pub easytier_core_path: String,
    /// easytier-cli 路径，空表示自动检测（内置优先，其次系统 PATH）
    pub easytier_cli_path: String,
    /// cloudflared 路径，空表示自动检测（内置优先，其次系统 PATH）
    #[serde(default)]
    pub cloudflared_path: String,
    /// 从对等节点下载的媒体存放目录，空表示 data_dir/downloads
    pub download_dir: String,
    /// 媒体流读缓冲（KB）：排查播放卡顿时按开发者建议调整，平时不推荐修改
    #[serde(default = "default_stream_buffer_kb")]
    pub stream_buffer_kb: u32,
    /// 禁用电源节流（Windows EcoQoS 豁免）：开启后避免媒体流传输被系统降速，默认开启
    #[serde(default = "default_disable_power_throttling")]
    pub disable_power_throttling: bool,
    /// 一键压制 720p 峰值码率（Mbps，范围 1-100，保留两位小数）
    #[serde(default = "default_streaming_720p_bitrate_mbps", rename = "streaming720pBitrateMbps")]
    pub streaming_720p_bitrate_mbps: f64,
    /// 一键压制 1080p 峰值码率（Mbps，范围 1-100，保留两位小数）
    #[serde(default = "default_streaming_1080p_bitrate_mbps", rename = "streaming1080pBitrateMbps")]
    pub streaming_1080p_bitrate_mbps: f64,
    /// EasyTier 组网配置
    pub easytier: EasytierConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen_mode: ListenMode::Loopback,
            custom_ip: None,
            port: default_port(),
            default_url_mode: UrlMode::Localhost,
            easytier_ip: String::new(),
            lan_adapter_ip: String::new(),
            allow_peer_view: false,
            default_player_path: String::new(),
            default_player_args: String::new(),
            thumb_dir: String::new(),
            autostart: false,
            close_behavior: CloseBehavior::Ask,
            settings_revision: SETTINGS_REVISION,
            ffmpeg_path: String::new(),
            ffprobe_path: String::new(),
            easytier_core_path: String::new(),
            easytier_cli_path: String::new(),
            cloudflared_path: String::new(),
            download_dir: String::new(),
            stream_buffer_kb: default_stream_buffer_kb(),
            disable_power_throttling: default_disable_power_throttling(),
            streaming_720p_bitrate_mbps: default_streaming_720p_bitrate_mbps(),
            streaming_1080p_bitrate_mbps: default_streaming_1080p_bitrate_mbps(),
            easytier: EasytierConfig::default(),
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
        settings.normalize_legacy();
        settings.normalize_network_mode();
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        // 先写临时文件再原子替换，避免写入过程中断导致配置损坏
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 对历史配置做一次性迁移：早期版本把「关闭动作」的默认值写成最小化到托盘，
    /// 且没有版本号字段（revision = 0）。这类配置无法与用户显式选择区分，
    /// 统一迁移为新版默认「每次都询问」，让用户自己决定；用户后续的选择会记入更高的版本号而不再被改动。
    pub fn normalize_legacy(&mut self) {
        if self.settings_revision < SETTINGS_REVISION {
            if self.close_behavior == CloseBehavior::Tray {
                self.close_behavior = CloseBehavior::Ask;
            }
            self.settings_revision = SETTINGS_REVISION;
        }
    }

    pub fn normalize_network_mode(&mut self) {
        if self.listen_mode == ListenMode::Custom {
            self.listen_mode = ListenMode::Lan;
        }
        // 局域网监听只有在「允许其他人查看我的媒体库」开启时才真正绑定 0.0.0.0；
        // 未开启时绑定逻辑会强制退回环回地址，为避免出现「局域网模式却只监听 127.0.0.1」
        // 的矛盾状态，这里统一把监听模式回落为本地模式。
        if self.listen_mode == ListenMode::Lan && !self.allow_peer_view {
            self.listen_mode = ListenMode::Loopback;
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
    /// 随发布包分发的内置组件目录（exe 同级 components）
    pub bundled_dir: PathBuf,
    /// 可写的内置组件安装目录（data_dir/components），自动下载的组件安装到这里
    pub components_dir: PathBuf,
    /// 默认的对等节点媒体下载目录
    pub download_dir: PathBuf,
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
        bundled_dir: exe_dir.join("components"),
        components_dir: data_dir.join("components"),
        download_dir: data_dir.join("downloads"),
        data_dir,
        portable,
    }
}
