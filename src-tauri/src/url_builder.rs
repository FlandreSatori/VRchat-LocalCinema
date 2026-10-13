use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::media_library::MediaItem;
use crate::settings::UrlMode;

pub fn encode_segment(s: &str) -> String {
    const SEGMENT: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
    utf8_percent_encode(s, SEGMENT).to_string()
}

pub fn valid_basename(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".."
        && !s.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' ))
}

pub struct UrlInput {
    pub mode: UrlMode,
    pub port: u16,
    pub lan_ip: Option<String>,
}

pub fn build_media_url(item: &MediaItem, input: &UrlInput) -> Result<String, String> {
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
