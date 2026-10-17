//! B 站在线视频解析：把 BV 号解析为可被 VRChat 直接播放的单文件 MP4 官方 CDN 直链。
//!
//! 事实核查（本机实测）：
//! - `x/web-interface/view` 无 UA 会 403，必须带浏览器 UA 与 `Referer`。
//! - `x/player/playurl` 使用 `platform=html5&fnval=1` 时返回**单文件 MP4**（durl），
//!   免登录上限 720P（API 实测 `data.quality=64`）；带 `Referer` 访问官方 CDN 可正常
//!   返回 `206 Partial Content`，因此可 seek。
//! - 直链 URL 内含 `deadline` 签名参数，约 2 小时后过期，故解析结果必须缓存并在过期时刷新。
//!
//! 说明：直链来自 B 站官方 CDN（`upos-*.bilivideo.com`），并非第三方解析站缓存。
//! 本模块全部直接调用 B 站官方接口，不依赖任何第三方解析服务。

use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::error::{AppError, AppResult};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const REFERER: &str = "https://www.bilibili.com/";

/// 免登录可直接获取的直链最长有效时间（秒）。无法解析 `deadline` 时使用的保守值（90 分钟）。
const FALLBACK_TTL_SECS: i64 = 90 * 60;

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 全应用共享的 HTTP 客户端（固定浏览器 UA，避免 B 站接口 403）
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(UA)
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 转发视频流用的客户端：不设整体超时（视频可达数百 MB），仅限制连接建立时间。
/// 注意不能用上面的 `client()`——其 20 秒整体超时会中断长下载。
pub fn stream_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(UA)
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 从用户输入（完整分享链接 / 含 BV 号的文本 / 纯 BV 号）中提取 BV 号
pub fn parse_bvid(input: &str) -> Option<String> {
    let s = input.trim();
    let idx = s.find("BV")?;
    let rest = &s[idx..];
    // BV 号 = "BV" + 10 位 base58 字符，共 12 位
    if rest.len() < 12 {
        return None;
    }
    let cand = &rest[..12];
    if cand[2..].bytes().all(|b| b.is_ascii_alphanumeric()) {
        Some(cand.to_string())
    } else {
        None
    }
}

/// 在线条目的唯一伪路径（同一视频多 P 用 cid 区分）
pub fn online_key(bvid: &str, cid: i64) -> String {
    format!("online://{}/{}", bvid, cid)
}

/// 从在线伪路径反解出 (bvid, cid)
pub fn parse_online_key(key: &str) -> Option<(String, i64)> {
    let rest = key.strip_prefix("online://")?;
    let (bvid, cid) = rest.rsplit_once('/')?;
    Some((bvid.to_string(), cid.parse::<i64>().ok()?))
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub bvid: String,
    pub cid: i64,
    pub title: String,
    pub cover_url: String,
    pub duration_ms: u64,
    /// 分 P 数量（用于标题提示）
    pub page_count: usize,
}

#[derive(Debug, Clone)]
pub struct PlayLink {
    pub url: String,
    pub size: u64,
    pub quality: i64,
    /// Unix 秒；到此时间后直链失效需重新解析
    pub expires_at: i64,
}

#[derive(Deserialize)]
struct ViewResp {
    code: i64,
    message: Option<String>,
    data: Option<ViewData>,
}

#[derive(Deserialize)]
struct ViewData {
    bvid: String,
    #[serde(default)]
    cid: i64,
    title: String,
    #[serde(default)]
    pic: String,
    #[serde(default)]
    duration: i64,
    #[serde(default)]
    pages: Vec<ViewPage>,
}

#[derive(Deserialize)]
struct ViewPage {
    cid: i64,
}

#[derive(Deserialize)]
struct PlayResp {
    code: i64,
    message: Option<String>,
    data: Option<PlayData>,
}

#[derive(Deserialize)]
struct PlayData {
    #[serde(default)]
    quality: i64,
    #[serde(default)]
    durl: Vec<Durl>,
}

#[derive(Deserialize)]
struct Durl {
    url: String,
    #[serde(default)]
    size: i64,
}

/// 获取视频基本信息（标题 / 封面 / 时长 / cid）
pub async fn fetch_video_info(bvid: &str) -> AppResult<VideoInfo> {
    let resp = client()
        .get("https://api.bilibili.com/x/web-interface/view")
        .query(&[("bvid", bvid)])
        .header(reqwest::header::REFERER, REFERER)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求 B 站视频信息失败: {}", e)))?
        .error_for_status()
        .map_err(|e| AppError::Msg(format!("B 站视频信息请求被拒绝: {}", e)))?
        .json::<ViewResp>()
        .await
        .map_err(|e| AppError::Msg(format!("解析 B 站视频信息失败: {}", e)))?;
    if resp.code != 0 {
        return Err(AppError::Msg(format!(
            "B 站返回错误（{}）：{}",
            resp.code,
            resp.message.unwrap_or_else(|| "视频不存在或不可访问".into())
        )));
    }
    let data = resp
        .data
        .ok_or_else(|| AppError::Msg("B 站返回数据为空".into()))?;
    let cid = if data.cid != 0 {
        data.cid
    } else {
        data.pages.first().map(|p| p.cid).unwrap_or(0)
    };
    if cid == 0 {
        return Err(AppError::Msg("未获取到分 P 信息（cid）".into()));
    }
    Ok(VideoInfo {
        bvid: data.bvid,
        cid,
        title: data.title,
        cover_url: data.pic,
        duration_ms: (data.duration.max(0) as u64) * 1000,
        page_count: data.pages.len().max(1),
    })
}

/// 解析单文件 MP4 直链。分层兜底：html5(720P 上限) → 降码率 → 非 html5 的 MP4 接口。
pub async fn resolve_play_url(bvid: &str, cid: i64) -> AppResult<PlayLink> {
    let cid_str = cid.to_string();
    let attempts: Vec<Vec<(&str, String)>> = vec![
        vec![
            ("bvid", bvid.to_string()),
            ("cid", cid_str.clone()),
            ("qn", "80".into()),
            ("fnval", "1".into()),
            ("platform", "html5".into()),
        ],
        vec![
            ("bvid", bvid.to_string()),
            ("cid", cid_str.clone()),
            ("qn", "64".into()),
            ("fnval", "1".into()),
            ("platform", "html5".into()),
        ],
        vec![
            ("bvid", bvid.to_string()),
            ("cid", cid_str.clone()),
            ("qn", "80".into()),
            ("fnval", "1".into()),
            ("high_quality", "1".into()),
            ("try_look", "1".into()),
        ],
    ];
    let mut last_err = String::from("未获取到可用的 MP4 直链");
    for query in &attempts {
        let resp = match client()
            .get("https://api.bilibili.com/x/player/playurl")
            .query(query)
            .header(reqwest::header::REFERER, REFERER)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("请求 B 站播放地址失败: {}", e);
                continue;
            }
        };
        if !resp.status().is_success() {
            last_err = format!("B 站播放接口返回 HTTP {}", resp.status());
            continue;
        }
        let parsed = match resp.json::<PlayResp>().await {
            Ok(p) => p,
            Err(e) => {
                last_err = format!("解析 B 站播放地址失败: {}", e);
                continue;
            }
        };
        if parsed.code != 0 {
            last_err = format!(
                "B 站播放接口错误（{}）：{}",
                parsed.code,
                parsed.message.unwrap_or_default()
            );
            continue;
        }
        let Some(data) = parsed.data else { continue };
        let Some(durl) = data.durl.into_iter().find(|d| !d.url.is_empty()) else {
            last_err = "该视频暂无可直接播放的单文件 MP4（可能仅提供高清 DASH，需登录或使用下载工具）".into();
            continue;
        };
        let expires_at = parse_deadline(&durl.url).unwrap_or_else(|| now_secs() + FALLBACK_TTL_SECS);
        return Ok(PlayLink {
            url: durl.url,
            size: durl.size.max(0) as u64,
            quality: data.quality,
            expires_at,
        });
    }
    Err(AppError::Msg(last_err))
}

/// 从直链 URL 的 `deadline` 参数解析过期时间（Unix 秒）
pub fn parse_deadline(url: &str) -> Option<i64> {
    let idx = url.find("deadline=")?;
    let rest = &url[idx + "deadline=".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok()
}

/// 下载封面图片并按 jpg 保存到指定路径
pub async fn fetch_cover_to(url: &str, out: &Path) -> AppResult<()> {
    if url.trim().is_empty() {
        return Err(AppError::Msg("封面地址为空".into()));
    }
    let bytes = client()
        .get(url)
        .header(reqwest::header::REFERER, REFERER)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("下载封面失败: {}", e)))?
        .error_for_status()
        .map_err(|e| AppError::Msg(format!("下载封面被拒绝: {}", e)))?
        .bytes()
        .await
        .map_err(|e| AppError::Msg(format!("读取封面数据失败: {}", e)))?;
    if bytes.is_empty() {
        return Err(AppError::Msg("封面数据为空".into()));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bvid_from_various_inputs() {
        assert_eq!(
            parse_bvid("https://www.bilibili.com/video/BV1tFjz6eEtp/?spm_id_from=333.1387"),
            Some("BV1tFjz6eEtp".to_string())
        );
        assert_eq!(parse_bvid("BV1tFjz6eEtp"), Some("BV1tFjz6eEtp".to_string()));
        assert_eq!(parse_bvid("  分享 https://b23.tv/BV1tFjz6eEtp  "), Some("BV1tFjz6eEtp".to_string()));
        assert_eq!(parse_bvid("https://www.bilibili.com/video/av12345"), None);
        assert_eq!(parse_bvid("BV1abc"), None);
    }

    #[test]
    fn parses_deadline_from_play_url() {
        let url = "https://upos-sz-mirrorcos.bilivideo.com/upgcxcode/x.mp4?e=abc&deadline=1792256999&oi=2067347262";
        assert_eq!(parse_deadline(url), Some(1792256999));
        assert_eq!(parse_deadline("https://x/y.mp4"), None);
        assert_eq!(parse_deadline("https://x/y.mp4?deadline="), None);
    }

    #[test]
    fn online_key_is_unique_per_part() {
        assert_eq!(online_key("BV1tFjz6eEtp", 39240534069), "online://BV1tFjz6eEtp/39240534069");
    }
}
