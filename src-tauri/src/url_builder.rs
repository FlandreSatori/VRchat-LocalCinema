use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::media_library::MediaItem;
use crate::settings::UrlMode;

pub fn encode_segment(s: &str) -> String {
    const SEGMENT: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
    utf8_percent_encode(s, SEGMENT).to_string()
}

/// Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9 等，扩展名不影响判定）
fn is_reserved_device_name(stem: &str) -> bool {
    let up = stem
        .trim_end_matches(|c| c == ' ' || c == '.')
        .to_ascii_uppercase();
    if matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$") {
        return true;
    }
    for pfx in ["COM", "LPT"] {
        if let Some(rest) = up.strip_prefix(pfx) {
            if rest.len() == 1 && rest.chars().all(|c| ('1'..='9').contains(&c)) {
                return true;
            }
        }
    }
    false
}

pub fn valid_basename(s: &str) -> bool {
    if s.is_empty() || s == "." || s == ".." {
        return false;
    }
    // Windows 文件名不允许以点或空格结尾
    if s.ends_with('.') || s.ends_with(' ') {
        return false;
    }
    if s.chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return false;
    }
    // 拒绝 Windows 保留设备名（如 CON.mp4 / aux）
    let stem = s.split('.').next().unwrap_or(s);
    !is_reserved_device_name(stem)
}

pub struct UrlInput {
    pub mode: UrlMode,
    pub port: u16,
    pub lan_ip: Option<String>,
}

pub fn build_media_url(item: &MediaItem, input: &UrlInput) -> Result<String, String> {
    // 在线流媒体条目：字节由本软件经 HTTP 代理转发，这里只生成本机回环地址。
    // 局域网模式下不生成局域网地址，而是由命令层（commands::build_media_url）直接返回
    // B 站官方 CDN 直链，因此不会走到本函数。
    if item.source_kind == "online" {
        return Ok(format!("http://127.0.0.1:{}/online/{}", input.port, item.id));
    }
    if !valid_basename(&item.file_name) {
        return Err("文件名不能映射为播放 URL".to_string());
    }
    let host = match input.mode {
        UrlMode::Localhost => "127.0.0.1".to_string(),
        UrlMode::Lan => input.lan_ip.clone().filter(|s| !s.is_empty())
            .ok_or_else(|| "未检测到可用的局域网 IPv4 地址，请确认已连接局域网".to_string())?,
        // 旧配置值由设置迁移；这里保留分支避免破坏旧调用，但不再生成旧网络链接。
        UrlMode::Easytier => return Err("该地址模式已停用，请使用本地模式或局域网模式".to_string()),
    };
    Ok(format!("http://{}:{}/{}", host, input.port, encode_segment(&item.file_name)))
}
