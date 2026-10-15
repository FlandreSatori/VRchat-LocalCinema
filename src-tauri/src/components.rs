//! 内置组件（FFmpeg / EasyTier）的检测、下载与安装。
//!
//! 解析优先级：手动配置路径 → 随包分发的内置组件 → 自动下载安装的内置组件 → 系统 PATH。
//! 也就是完整包（full）下会优先使用内置路径，而不是系统路径。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::settings::{AppPaths, Settings};

/// FFmpeg 官方下载页（手动下载入口）
pub const FFMPEG_MANUAL_URL: &str = "https://ffmpeg.org/download.html";
/// EasyTier 发布页（手动下载入口）
pub const EASYTIER_MANUAL_URL: &str = "https://github.com/EasyTier/EasyTier/releases/";

const FFMPEG_RELEASE_API: &str = "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest";
const EASYTIER_RELEASE_API: &str = "https://api.github.com/repos/EasyTier/EasyTier/releases/latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentKind {
    Ffmpeg,
    Easytier,
}

impl ComponentKind {
    pub fn parse(s: &str) -> AppResult<Self> {
        match s {
            "ffmpeg" => Ok(Self::Ffmpeg),
            "easytier" => Ok(Self::Easytier),
            other => Err(AppError::Msg(format!("未知组件: {}", other))),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Easytier => "easytier",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Ffmpeg => "FFmpeg",
            Self::Easytier => "EasyTier",
        }
    }

    pub fn manual_url(&self) -> &'static str {
        match self {
            Self::Ffmpeg => FFMPEG_MANUAL_URL,
            Self::Easytier => EASYTIER_MANUAL_URL,
        }
    }

    /// 内置安装子目录名
    fn dir_name(&self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Easytier => "easytier",
        }
    }

    /// 该组件需要的可执行文件基名（不含扩展名）
    fn binaries(&self) -> &'static [&'static str] {
        match self {
            Self::Ffmpeg => &["ffmpeg", "ffprobe"],
            Self::Easytier => &["easytier-core", "easytier-cli"],
        }
    }
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", base)
    } else {
        base.to_string()
    }
}

/// 在系统中查找可执行文件
fn which(name: &str) -> Option<PathBuf> {
    let exe = exe_name(name);
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 在目录内查找可执行文件，支持压缩包解压后多一层子目录的情况
fn find_binary(dir: &Path, base: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let name = exe_name(base);
    let direct = dir.join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let nested = p.join(&name);
                if nested.is_file() {
                    return Some(nested);
                }
            }
        }
    }
    None
}

/// 组件来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentSource {
    Custom,
    Bundled,
    Downloaded,
    System,
    None,
}

impl ComponentSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Bundled => "bundled",
            Self::Downloaded => "downloaded",
            Self::System => "system",
            Self::None => "none",
        }
    }
}

fn resolve_binary(
    kind: ComponentKind,
    base: &str,
    configured: &str,
    paths: &AppPaths,
) -> (Option<PathBuf>, ComponentSource) {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.is_file() {
            return (Some(p), ComponentSource::Custom);
        }
    }
    if let Some(p) = find_binary(&paths.bundled_dir.join(kind.dir_name()), base)
        .or_else(|| find_binary(&paths.bundled_dir, base))
    {
        return (Some(p), ComponentSource::Bundled);
    }
    if let Some(p) = find_binary(&paths.components_dir.join(kind.dir_name()), base)
        .or_else(|| find_binary(&paths.components_dir, base))
    {
        return (Some(p), ComponentSource::Downloaded);
    }
    if let Some(p) = which(base) {
        return (Some(p), ComponentSource::System);
    }
    (None, ComponentSource::None)
}

pub fn ffmpeg_binary(settings: &Settings, paths: &AppPaths) -> (Option<PathBuf>, ComponentSource) {
    resolve_binary(
        ComponentKind::Ffmpeg,
        "ffmpeg",
        &settings.ffmpeg_path,
        paths,
    )
}

pub fn ffprobe_binary(settings: &Settings, paths: &AppPaths) -> (Option<PathBuf>, ComponentSource) {
    resolve_binary(
        ComponentKind::Ffmpeg,
        "ffprobe",
        &settings.ffprobe_path,
        paths,
    )
}

pub fn easytier_core_binary(
    settings: &Settings,
    paths: &AppPaths,
) -> (Option<PathBuf>, ComponentSource) {
    resolve_binary(
        ComponentKind::Easytier,
        "easytier-core",
        &settings.easytier_core_path,
        paths,
    )
}

pub fn easytier_cli_binary(
    settings: &Settings,
    paths: &AppPaths,
) -> (Option<PathBuf>, ComponentSource) {
    resolve_binary(
        ComponentKind::Easytier,
        "easytier-cli",
        &settings.easytier_cli_path,
        paths,
    )
}

/// 组件整体状态，供前端展示
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub kind: String,
    pub label: String,
    pub installed: bool,
    pub source: String,
    pub path: Option<String>,
    pub version: Option<String>,
    pub manual_url: String,
    /// 缺失的可执行文件名，例如 "ffmpeg.exe, ffprobe.exe"
    pub missing: Vec<String>,
    /// 是否为内置来源（随包分发或自动下载）
    pub builtin: bool,
}

fn probe_version(args: &[&str], path: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new(path);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let first = text.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        None
    } else {
        Some(first.chars().take(120).collect())
    }
}

fn primary_version_args(kind: ComponentKind) -> &'static [&'static str] {
    match kind {
        ComponentKind::Ffmpeg => &["-version"],
        ComponentKind::Easytier => &["--version"],
    }
}

pub fn status(kind: ComponentKind, settings: &Settings, paths: &AppPaths) -> ComponentStatus {
    let mut missing = Vec::new();
    let mut resolved: Option<(PathBuf, ComponentSource)> = None;
    for base in kind.binaries() {
        let configured = match (kind, *base) {
            (ComponentKind::Ffmpeg, "ffmpeg") => settings.ffmpeg_path.as_str(),
            (ComponentKind::Ffmpeg, "ffprobe") => settings.ffprobe_path.as_str(),
            (ComponentKind::Easytier, "easytier-core") => settings.easytier_core_path.as_str(),
            (ComponentKind::Easytier, "easytier-cli") => settings.easytier_cli_path.as_str(),
            _ => "",
        };
        let (path, source) = resolve_binary(kind, base, configured, paths);
        match path {
            Some(p) => {
                if resolved.is_none() {
                    resolved = Some((p, source));
                }
            }
            None => missing.push(exe_name(base)),
        }
    }

    let (path, source, version) = match resolved {
        Some((p, s)) => {
            let v = probe_version(primary_version_args(kind), &p);
            (Some(p), s, v)
        }
        None => (None, ComponentSource::None, None),
    };

    let installed = missing.is_empty() && path.is_some();
    ComponentStatus {
        kind: kind.id().to_string(),
        label: kind.label().to_string(),
        installed,
        source: source.as_str().to_string(),
        path: path.map(|p| p.to_string_lossy().to_string()),
        version,
        manual_url: kind.manual_url().to_string(),
        missing,
        builtin: matches!(
            source,
            ComponentSource::Bundled | ComponentSource::Downloaded
        ),
    }
}

/// 解析结果：最新版本号 + 直链 + 文件名
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    pub version: String,
    pub name: String,
    pub url: String,
    /// 发布资产字节数（下载后据此校验完整性），API 未提供时为 None
    pub size: Option<u64>,
}

fn arch_tag(kind: ComponentKind) -> &'static str {
    match (kind, std::env::consts::ARCH) {
        (ComponentKind::Ffmpeg, "aarch64") => "winarm64",
        (ComponentKind::Ffmpeg, "x86") => "win32",
        (ComponentKind::Ffmpeg, _) => "win64",
        (ComponentKind::Easytier, "aarch64") => "aarch64",
        (ComponentKind::Easytier, "x86") => "i686",
        (ComponentKind::Easytier, _) => "x86_64",
    }
}

/// 从发布资产里挑选当前平台可用的 Windows 压缩包
fn pick_asset(kind: ComponentKind, assets: &[Value]) -> Option<(String, String, Option<u64>)> {
    let arch = arch_tag(kind);
    let mut best: Option<(i32, String, String, Option<u64>)> = None;
    for a in assets {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = a
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let size = a.get("size").and_then(|v| v.as_u64());
        if name.is_empty() || url.is_empty() {
            continue;
        }
        let n = name.to_ascii_lowercase();
        if !n.ends_with(".zip") || !n.contains(arch) {
            continue;
        }
        let score = match kind {
            ComponentKind::Ffmpeg => {
                // 排除 LGPL 共享版，优先静态 GPL 版
                if n.contains("lgpl") || !n.contains("gpl") {
                    continue;
                }
                if n.contains("-shared") {
                    10
                } else {
                    20
                }
            }
            ComponentKind::Easytier => {
                if !n.contains("windows") || n.contains("gui") || n.contains("-web") {
                    continue;
                }
                20
            }
        };
        if best.as_ref().map(|(s, _, _, _)| score > *s).unwrap_or(true) {
            best = Some((score, name.to_string(), url.to_string(), size));
        }
    }
    best.map(|(_, n, u, sz)| (n, u, sz))
}

fn release_api(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Ffmpeg => FFMPEG_RELEASE_API,
        ComponentKind::Easytier => EASYTIER_RELEASE_API,
    }
}

fn http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("VRchat-LocalCinema")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::Msg(format!("创建网络客户端失败: {}", e)))
}

/// 查询官方发布页的最新版本并解析出 Windows 直链
pub async fn latest_release_asset(kind: ComponentKind) -> AppResult<ReleaseAsset> {
    let client = http_client()?;
    let resp = client
        .get(release_api(kind))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("查询 {} 最新版本失败: {}", kind.label(), e)))?;
    if !resp.status().is_success() {
        return Err(AppError::Msg(format!(
            "查询 {} 最新版本失败（HTTP {}），可尝试手动下载",
            kind.label(),
            resp.status().as_u16()
        )));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Msg(format!("解析 {} 发布信息失败: {}", kind.label(), e)))?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("latest")
        .to_string();
    let assets = json
        .get("assets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let (name, url, size) = pick_asset(kind, &assets).ok_or_else(|| {
        AppError::Msg(format!(
            "{} 最新发布中未找到适用于当前系统的 Windows 压缩包，请手动下载",
            kind.label()
        ))
    })?;
    Ok(ReleaseAsset {
        version: tag,
        name,
        url,
        size,
    })
}

/// 下载并解压到内置组件目录，返回主程序的路径
pub async fn download_and_install(
    kind: ComponentKind,
    paths: &AppPaths,
    on_progress: impl Fn(u64, Option<u64>),
) -> AppResult<PathBuf> {
    let asset = latest_release_asset(kind).await?;
    tracing::info!("开始下载 {} {}：{}", kind.label(), asset.version, asset.url);

    let dest_dir = paths.components_dir.join(kind.dir_name());
    std::fs::create_dir_all(&dest_dir)?;

    let tmp_dir = paths.components_dir.join(".cache");
    std::fs::create_dir_all(&tmp_dir)?;
    let zip_path = tmp_dir.join(&asset.name);

    {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let client = http_client()?;
        let resp = client
            .get(&asset.url)
            .send()
            .await
            .map_err(|e| AppError::Msg(format!("下载 {} 失败: {}", kind.label(), e)))?;
        if !resp.status().is_success() {
            return Err(AppError::Msg(format!(
                "下载 {} 失败（HTTP {}）",
                kind.label(),
                resp.status().as_u16()
            )));
        }
        let total = resp.content_length();
        let mut file = tokio::fs::File::create(&zip_path).await?;
        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AppError::Msg(format!("下载中断: {}", e)))?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            on_progress(downloaded, total);
        }
        file.flush().await?;
        // 完整性校验：与发布资产声明的字节数比对，避免截断/损坏的压缩包被解压安装
        if let Some(expected) = asset.size {
            if downloaded != expected {
                let _ = std::fs::remove_file(&zip_path);
                return Err(AppError::Msg(format!(
                    "{} 下载大小校验失败（期望 {} 字节，实际 {} 字节），请重试，或改用完整版安装包（已内置组件）",
                    kind.label(),
                    expected,
                    downloaded
                )));
            }
        }
    }

    let zip_for_extract = zip_path.clone();
    let dest_for_extract = dest_dir.clone();
    let extracted =
        tokio::task::spawn_blocking(move || extract_binaries(&zip_for_extract, &dest_for_extract))
            .await
            .map_err(|e| AppError::Msg(format!("解压任务失败: {}", e)))??;
    let _ = std::fs::remove_file(&zip_path);

    // 校验必要文件是否齐全
    let mut missing = Vec::new();
    for base in kind.binaries() {
        if find_binary(&dest_dir, base).is_none() {
            missing.push(exe_name(base));
        }
    }
    if !missing.is_empty() {
        return Err(AppError::Msg(format!(
            "{} 安装不完整，缺少：{}",
            kind.label(),
            missing.join(", ")
        )));
    }

    tracing::info!(
        "{} 已安装到 {}（解压 {} 个文件）",
        kind.label(),
        dest_dir.display(),
        extracted
    );
    // 返回主程序路径
    let primary = kind.binaries()[0];
    find_binary(&dest_dir, primary)
        .ok_or_else(|| AppError::Msg(format!("{} 安装后未找到主程序", kind.label())))
}

/// 解压压缩包中的可执行文件与动态库，返回解压数量
fn extract_binaries(zip_path: &Path, dest_dir: &Path) -> AppResult<usize> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| AppError::Msg(format!("打开压缩包失败: {}", e)))?;
    let mut count = 0usize;
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.is_dir() {
            continue;
        }
        // 只取文件名，避免压缩包内路径穿越
        let raw_name = entry.name().to_string();
        let Some(file_name) = Path::new(&raw_name).file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let lower = file_name.to_ascii_lowercase();
        if !(lower.ends_with(".exe") || lower.ends_with(".dll")) {
            continue;
        }
        let out = dest_dir.join(file_name);
        let mut writer = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut writer)?;
        count += 1;
    }
    if count == 0 {
        return Err(AppError::Msg("压缩包中未找到可执行文件".to_string()));
    }
    Ok(count)
}
