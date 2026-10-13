//! 媒体兼容性白名单模块。
//!
//! 严格白名单策略：只有「容器 + 视频编码 + 位深 + 像素格式 + 音频编码」
//! 组合完全匹配已验证列表的媒体才视为受支持；任何未知参数、探测失败
//! 或未列出的组合一律默认拒绝。规则集中在本模块，避免散落各处。

use serde::Serialize;

/// 从 ffprobe JSON 解析出的媒体关键参数
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeInfo {
    /// 归一化容器名：mp4 / mov / mkv / webm / mpegts / avi
    pub container: Option<String>,
    pub raw_format_name: Option<String>,
    pub video_codec: Option<String>,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate: Option<u64>,
    pub audio_channels: Option<u32>,
    pub video_streams: usize,
    pub audio_streams: usize,
    pub duration_ms: Option<u64>,
}

impl ProbeInfo {
    pub fn is_probed(&self) -> bool {
        self.raw_format_name.is_some()
    }
}

/// 兼容性判定结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompatResult {
    pub supported: bool,
    pub reason: Option<String>,
}

impl CompatResult {
    fn support() -> Self {
        CompatResult {
            supported: true,
            reason: None,
        }
    }
    fn reject(reason: impl Into<String>) -> Self {
        CompatResult {
            supported: false,
            reason: Some(reason.into()),
        }
    }
}

/// 白名单：(容器, 视频编码, 位深, 像素格式, 音频编码)
/// 全部为已在目标环境实测可通过 BiliPlayer 播放的组合。
const WHITELIST: &[(&str, &str, u32, &str, &str)] = &[
    ("mp4", "h264", 8, "yuv420p", "aac"),
    ("mp4", "hevc", 8, "yuv420p", "aac"),
    ("mp4", "hevc", 10, "yuv420p10le", "aac"),
    ("mp4", "vp9", 8, "yuv420p", "aac"),
    ("mp4", "av1", 8, "yuv420p", "aac"),
    ("mkv", "h264", 8, "yuv420p", "aac"),
    ("mov", "h264", 8, "yuv420p", "aac"),
    ("mpegts", "h264", 8, "yuv420p", "aac"),
    ("avi", "h264", 8, "yuv420p", "mp3"),
    ("mkv", "h264", 8, "yuv420p", "opus"),
    // 特例：H.264 8-bit + yuv420p + AC-3（仅 MP4 / MKV 已验证组合）
    ("mp4", "h264", 8, "yuv420p", "ac3"),
    ("mkv", "h264", 8, "yuv420p", "ac3"),
];

/// 各视频编码允许的像素格式（位深由像素格式推导）
const ALLOWED_PIX_FMTS: &[(&str, &[&str])] = &[
    ("h264", &["yuv420p"]),
    ("hevc", &["yuv420p", "yuv420p10le"]),
    ("vp9", &["yuv420p"]),
    ("av1", &["yuv420p"]),
];

/// 白名单中允许的音频编码（ac3 仅在特例组合下放行）
const ALLOWED_AUDIO: &[&str] = &["aac", "mp3", "opus", "ac3"];

/// 归一化容器名。MP4 与 MOV 共享 format_name，需借助 major_brand 区分。
fn normalize_container(format_name: &str, major_brand: Option<&str>) -> Option<&'static str> {
    let tokens: Vec<&str> = format_name.split(',').map(|s| s.trim()).collect();
    let has = |t: &str| tokens.iter().any(|x| *x == t);
    if has("avi") {
        return Some("avi");
    }
    if has("mpegts") {
        return Some("mpegts");
    }
    if has("matroska") || has("webm") {
        // webm 通常带有 Mimetype=video/webm 标签
        return Some("webm");
    }
    if has("mov") || has("mp4") {
        // QuickTime 文件的 major_brand 为 "qt "
        return match major_brand.map(|s| s.trim()) {
            Some("qt") => Some("mov"),
            _ => Some("mp4"),
        };
    }
    None
}

/// 从像素格式推导位深（ffprobe 的 bits_per_raw_sample 常为 0/N/A）
fn bit_depth_from_pix_fmt(pix_fmt: &str) -> Option<u32> {
    match pix_fmt {
        "yuv420p" | "yuvj420p" => Some(8),
        "yuv420p10le" => Some(10),
        "yuv420p12le" => Some(12),
        _ => None,
    }
}

/// 解析 ffprobe -print_format json -show_format -show_streams 的输出
pub fn parse_probe(json: &serde_json::Value) -> ProbeInfo {
    let mut info = ProbeInfo::default();

    if let Some(format) = json.get("format") {
        info.raw_format_name = format
            .get("format_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let major_brand = format
            .get("tags")
            .and_then(|t| t.get("major_brand"))
            .and_then(|v| v.as_str());
        if let Some(name) = info.raw_format_name.as_deref() {
            info.container = normalize_container(name, major_brand).map(|s| s.to_string());
        }
        if let Some(d) = format.get("duration").and_then(|v| v.as_str()) {
            if let Ok(secs) = d.parse::<f64>() {
                if secs.is_finite() && secs >= 0.0 {
                    info.duration_ms = Some((secs * 1000.0) as u64);
                }
            }
        }
    }

    if let Some(streams) = json.get("streams").and_then(|v| v.as_array()) {
        for s in streams {
            let codec_type = s.get("codec_type").and_then(|v| v.as_str()).unwrap_or("");
            match codec_type {
                "video" => {
                    info.video_streams += 1;
                    if info.video_codec.is_none() {
                        info.video_codec = s
                            .get("codec_name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_ascii_lowercase());
                        info.pix_fmt = s
                            .get("pix_fmt")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_ascii_lowercase());
                        info.width = s.get("width").and_then(|v| v.as_u64()).map(|v| v as u32);
                        info.height = s.get("height").and_then(|v| v.as_u64()).map(|v| v as u32);
                        let depth = s
                            .get("bits_per_raw_sample")
                            .and_then(|v| v.as_str())
                            .and_then(|v| v.parse::<u32>().ok())
                            .filter(|v| *v > 0)
                            .or_else(|| s.get("bits_per_raw_sample").and_then(|v| v.as_u64()).map(|v| v as u32).filter(|v| *v > 0));
                        info.bit_depth = info
                            .pix_fmt
                            .as_deref()
                            .and_then(bit_depth_from_pix_fmt)
                            .or(depth);
                    }
                }
                "audio" => {
                    info.audio_streams += 1;
                    if info.audio_codec.is_none() {
                        info.audio_codec = s
                            .get("codec_name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_ascii_lowercase());
                        info.audio_sample_rate = s
                            .get("sample_rate")
                            .and_then(|v| v.as_str())
                            .and_then(|v| v.parse::<u64>().ok());
                        info.audio_channels = s
                            .get("channels")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u32);
                    }
                }
                _ => {}
            }
        }
    }

    info
}

/// 执行兼容性判定。未知参数、多轨道、未列出组合一律拒绝。
pub fn evaluate(info: &ProbeInfo) -> CompatResult {
    if !info.is_probed() {
        return CompatResult::reject("无法识别媒体参数，默认不支持");
    }
    if info.video_streams > 1 {
        return CompatResult::reject("包含多个视频轨道，默认不支持");
    }
    if info.audio_streams > 1 {
        return CompatResult::reject("包含多个音频轨道，默认不支持");
    }
    let Some(container) = info.container.as_deref() else {
        return CompatResult::reject("无法识别的容器格式");
    };
    let Some(vcodec) = info.video_codec.as_deref() else {
        return CompatResult::reject("未检测到视频流");
    };
    let Some(pix_fmt) = info.pix_fmt.as_deref() else {
        return CompatResult::reject("无法识别视频像素格式");
    };
    if info.audio_streams == 0 {
        return CompatResult::reject("未检测到音频流");
    }
    let Some(acodec) = info.audio_codec.as_deref() else {
        return CompatResult::reject("无法识别音频编码");
    };

    // 视频编码白名单
    if !ALLOWED_PIX_FMTS.iter().any(|(c, _)| *c == vcodec) {
        return CompatResult::reject(format!("不支持 {} 视频编码", vcodec));
    }
    // H.264 10-bit 明确不支持
    if vcodec == "h264"
        && (pix_fmt == "yuv420p10le" || info.bit_depth == Some(10))
    {
        return CompatResult::reject("不支持 H.264 10-bit");
    }
    // 位深 / 像素格式
    let allowed = ALLOWED_PIX_FMTS
        .iter()
        .find(|(c, _)| *c == vcodec)
        .map(|(_, pfs)| *pfs)
        .unwrap_or(&[]);
    if !allowed.contains(&pix_fmt) {
        return CompatResult::reject(format!(
            "视频位深或像素格式不在白名单（{}）",
            pix_fmt
        ));
    }
    // 音频编码
    if !ALLOWED_AUDIO.contains(&acodec) {
        return CompatResult::reject(format!("音频编码未验证（{}）", acodec));
    }
    // AC-3 特例只对 H.264 8-bit + yuv420p 放行，且容器仅限 MP4 / MKV
    if acodec == "ac3" && !(vcodec == "h264" && pix_fmt == "yuv420p") {
        return CompatResult::reject(
            "AC-3 仅支持 H.264 8-bit（yuv420p）组合，该视频编码与音频编码组合尚未验证",
        );
    }
    // 组合匹配
    if WHITELIST
        .iter()
        .any(|(c, v, _, p, a)| *c == container && *v == vcodec && *p == pix_fmt && *a == acodec)
    {
        return CompatResult::support();
    }
    CompatResult::reject(format!(
        "该容器与编码组合不在兼容性白名单（{} + {} + {}）",
        container, vcodec, acodec
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp4_h264_aac_supported() {
        let info = ProbeInfo {
            container: Some("mp4".into()),
            raw_format_name: Some("mov,mp4,m4a,3gp,3g2,mj2".into()),
            video_codec: Some("h264".into()),
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(8),
            audio_codec: Some("aac".into()),
            audio_sample_rate: Some(48000),
            audio_channels: Some(2),
            video_streams: 1,
            audio_streams: 1,
            ..Default::default()
        };
        assert!(evaluate(&info).supported);
    }

    #[test]
    fn h264_10bit_rejected() {
        let info = ProbeInfo {
            container: Some("mkv".into()),
            raw_format_name: Some("matroska,webm".into()),
            video_codec: Some("h264".into()),
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: Some(10),
            audio_codec: Some("aac".into()),
            audio_sample_rate: Some(48000),
            audio_channels: Some(2),
            video_streams: 1,
            audio_streams: 1,
            ..Default::default()
        };
        let r = evaluate(&info);
        assert!(!r.supported);
        assert_eq!(r.reason.as_deref(), Some("不支持 H.264 10-bit"));
    }

    #[test]
    fn mp4_hevc_10bit_aac_supported() {
        let info = ProbeInfo {
            container: Some("mp4".into()),
            raw_format_name: Some("mov,mp4,m4a,3gp,3g2,mj2".into()),
            video_codec: Some("hevc".into()),
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: Some(10),
            audio_codec: Some("aac".into()),
            video_streams: 1,
            audio_streams: 1,
            ..Default::default()
        };
        assert!(evaluate(&info).supported);
    }

    #[test]
    fn hevc_ac3_rejected() {
        let info = ProbeInfo {
            container: Some("mp4".into()),
            raw_format_name: Some("mov,mp4".into()),
            video_codec: Some("hevc".into()),
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(8),
            audio_codec: Some("ac3".into()),
            video_streams: 1,
            audio_streams: 1,
            ..Default::default()
        };
        assert!(!evaluate(&info).supported);
    }

    #[test]
    fn mp4_h264_ac3_special_case_supported() {
        let info = ProbeInfo {
            container: Some("mp4".into()),
            raw_format_name: Some("mov,mp4".into()),
            video_codec: Some("h264".into()),
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(8),
            audio_codec: Some("ac3".into()),
            video_streams: 1,
            audio_streams: 1,
            ..Default::default()
        };
        assert!(evaluate(&info).supported);
    }

    #[test]
    fn multi_audio_rejected() {
        let info = ProbeInfo {
            container: Some("mp4".into()),
            raw_format_name: Some("mov,mp4".into()),
            video_codec: Some("h264".into()),
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(8),
            audio_codec: Some("aac".into()),
            video_streams: 1,
            audio_streams: 2,
            ..Default::default()
        };
        assert!(!evaluate(&info).supported);
    }

    #[test]
    fn mov_brand_detection() {
        assert_eq!(
            normalize_container("mov,mp4,m4a,3gp,3g2,mj2", Some("qt ")),
            Some("mov")
        );
        assert_eq!(
            normalize_container("mov,mp4,m4a,3gp,3g2,mj2", Some("isom")),
            Some("mp4")
        );
        assert_eq!(normalize_container("avi", None), Some("avi"));
        assert_eq!(normalize_container("mpegts", None), Some("mpegts"));
        assert_eq!(normalize_container("matroska,webm", None), Some("webm"));
    }
}
