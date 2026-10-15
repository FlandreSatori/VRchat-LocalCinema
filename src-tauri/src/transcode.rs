//! 转码选项与 ffmpeg 参数构建。
//!
//! 前端「转码设置」对话框产出一个 [`TranscodeOptions`]，本模块负责把它翻译成
//! ffmpeg 参数：视频可直拷或重编码、音频可直拷或转 AAC、字幕可烧录 / 内嵌 / 丢弃。
//! 同时负责解析 ffprobe 输出的完整流清单（供对话框展示音轨 / 字幕轨）。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 可直拷进 MP4 的视频编码（其余一律重编码）
pub const COPYABLE_VIDEO: &[&str] = &["h264", "hevc", "vp9", "av1", "mpeg4"];
/// 可直拷进 MP4 的音频编码（DTS / TrueHD 等 MP4 不支持，必须转 AAC）
pub const COPYABLE_AUDIO: &[&str] = &["aac", "mp3", "ac3", "eac3"];
/// 文本字幕编码（可烧录 / 转文本 / 内嵌）
pub const TEXT_SUBTITLE_CODECS: &[&str] = &[
    "subrip", "srt", "ass", "ssa", "mov_text", "webvtt", "text", "dvb_teletext", "eia_608",
];
/// 图形字幕编码（只能烧录，不能内嵌或外挂）
pub const GRAPHIC_SUBTITLE_CODECS: &[&str] = &[
    "hdmv_pgs_subtitle", "dvd_subtitle", "dvdsub", "pgssub", "dvb_subtitle",
];

/// 转码配置（字段缺省时按「自动」处理）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TranscodeOptions {
    /// 视频流绝对索引；None = 第一路视频
    pub video_stream: Option<usize>,
    /// 音频流绝对索引；None = 第一路音频
    pub audio_stream: Option<usize>,
    /// 是否保留音频（默认保留）
    pub include_audio: Option<bool>,
    /// 字幕流绝对索引；`subtitle_mode` 为烧录 / 内嵌时生效，None = 自动选第一条文本字幕
    pub subtitle_stream: Option<usize>,
    /// 字幕处理：burn（烧录）/ embed（内嵌）/ drop（丢弃）；None = 丢弃
    pub subtitle_mode: Option<String>,
    /// 选中的字幕流是否为图形（位图）字幕：图形字幕只能用 overlay 方式烧录
    pub subtitle_graphic: Option<bool>,
    /// 视频处理：copy / h264 / h265 / av1
    pub video_mode: Option<String>,
    /// 目标分辨率高度：original / 2160 / 1440 / 1080 / 720 / 480
    pub resolution: Option<String>,
    /// 质量（CRF，0-51，越小越清晰）
    pub crf: Option<u32>,
    /// 编码预设：ultrafast … veryslow
    pub preset: Option<String>,
    /// 视频像素格式：auto / yuv420p / yuv420p10le
    pub pix_fmt: Option<String>,
    /// 音频处理：copy / aac
    pub audio_mode: Option<String>,
    /// AAC 码率
    pub audio_bitrate: Option<String>,
    /// 音频声道数（降混）：None 或 0 = 保持原样
    pub audio_channels: Option<u32>,
    /// 音频采样率
    pub audio_sample_rate: Option<u32>,
    /// MP4 faststart（默认开启）
    pub faststart: Option<bool>,
}

impl TranscodeOptions {
    pub fn video_mode(&self) -> &str {
        self.video_mode.as_deref().unwrap_or("h264")
    }
    pub fn audio_mode(&self) -> &str {
        self.audio_mode.as_deref().unwrap_or("aac")
    }
    pub fn include_audio(&self) -> bool {
        self.include_audio.unwrap_or(true)
    }
    pub fn faststart(&self) -> bool {
        self.faststart.unwrap_or(true)
    }
    pub fn video_copy(&self) -> bool {
        self.video_mode() == "copy"
    }
    pub fn audio_copy(&self) -> bool {
        self.audio_mode() == "copy"
    }
    /// 是否需要把字幕烧进画面（需要视频重编码）
    pub fn burn_subtitles(&self) -> bool {
        self.subtitle_mode.as_deref() == Some("burn")
    }
    /// 是否需要把字幕作为独立轨道内嵌
    pub fn embed_subtitles(&self) -> bool {
        self.subtitle_mode.as_deref() == Some("embed")
    }
    /// 是否把字幕抽取为独立文件放在产物旁边（外挂字幕）
    pub fn external_subtitles(&self) -> bool {
        self.subtitle_mode.as_deref() == Some("external")
    }
    pub fn wants_subtitle(&self) -> bool {
        self.burn_subtitles() || self.embed_subtitles() || self.external_subtitles()
    }
    /// 选中的字幕轨是否为图形字幕（默认非图形）
    pub fn subtitle_graphic(&self) -> bool {
        self.subtitle_graphic.unwrap_or(false)
    }
    /// 目标分辨率是否要求重新编码（非 original 即需要缩放，不能直拷）
    pub fn resolution_requires_reencode(&self) -> bool {
        matches!(self.resolution.as_deref(), Some(r) if r != "original" && !r.is_empty())
    }
    /// 烧录字幕 / 改变分辨率都会改动画面，故视频必须重编码，不能直拷
    pub fn effective_video_mode(&self) -> &str {
        if self.video_copy() && (self.burn_subtitles() || self.resolution_requires_reencode()) {
            "h264"
        } else {
            self.video_mode()
        }
    }
}

/// 一条媒体流的关键信息（供「转码设置」对话框展示与选择）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    /// ffprobe 报告流的绝对索引
    pub index: usize,
    /// video / audio / subtitle
    pub kind: String,
    pub codec: String,
    pub profile: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    // 视频
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pix_fmt: Option<String>,
    // 音频
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<u32>,
    /// 是否可直接直拷进 MP4（视频 / 音频轨用；字幕轨恒为 false）
    pub copyable: bool,
    /// 是否为文本字幕（可烧录 / 内嵌 / 外挂）
    pub text_subtitle: bool,
    /// 是否为图形字幕（只能烧录，不能内嵌或外挂）
    pub graphic_subtitle: bool,
}

impl StreamInfo {
    /// 供对话框展示的单行描述，如 `H.265 Main10 1920×816 10bit`、`DTS-HD MA 5.1ch 48kHz`
    pub fn describe(&self) -> String {
        match self.kind.as_str() {
            "video" => {
                let mut parts = vec![self.codec.to_uppercase()];
                if let Some(p) = &self.profile {
                    parts.push(p.clone());
                }
                if let (Some(w), Some(h)) = (self.width, self.height) {
                    parts.push(format!("{}×{}", w, h));
                }
                if let Some(pf) = &self.pix_fmt {
                    parts.push(pf.clone());
                }
                parts.join(" ")
            }
            "audio" => {
                let mut parts = vec![self.codec.to_uppercase()];
                if let Some(p) = &self.profile {
                    parts.push(p.clone());
                }
                if let Some(c) = self.channels {
                    parts.push(format!("{}ch", c));
                }
                if let Some(sr) = self.sample_rate {
                    parts.push(format!("{}kHz", sr / 1000));
                }
                parts.join(" ")
            }
            _ => {
                let mut parts = vec![self.codec.to_uppercase()];
                if let Some(t) = &self.title {
                    parts.push(t.clone());
                }
                parts.join(" ")
            }
        }
    }
}

fn json_u32(v: Option<&serde_json::Value>) -> Option<u32> {
    v.and_then(|x| x.as_u64()).map(|n| n as u32)
}

fn json_str(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// 解析 `ffprobe -show_streams -print_format json` 输出中的流清单
pub fn parse_streams(json: &serde_json::Value) -> Vec<StreamInfo> {
    let Some(streams) = json.get("streams").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in streams {
        let Some(kind) = json_str(s.get("codec_type")) else {
            continue;
        };
        if kind != "video" && kind != "audio" && kind != "subtitle" {
            continue; // 跳过 attachment / data 等
        }
        // mjpeg 内嵌封面不算视频轨
        if kind == "video" && s.get("disposition").and_then(|d| d.get("attached_pic")).and_then(|v| v.as_u64()) == Some(1) {
            continue;
        }
        let codec = json_str(s.get("codec_name")).unwrap_or_else(|| "unknown".into());
        let tags = s.get("tags");
        let language = tags.and_then(|t| json_str(t.get("language")));
        let title = tags.and_then(|t| json_str(t.get("title")));
        let is_default = s
            .get("disposition")
            .and_then(|d| d.get("default"))
            .and_then(|v| v.as_u64())
            == Some(1);
        let copyable = match kind.as_str() {
            "video" => COPYABLE_VIDEO.contains(&codec.as_str()),
            "audio" => COPYABLE_AUDIO.contains(&codec.as_str()),
            _ => false,
        };
        let text_subtitle = kind == "subtitle" && TEXT_SUBTITLE_CODECS.contains(&codec.as_str());
        let graphic_subtitle = kind == "subtitle" && GRAPHIC_SUBTITLE_CODECS.contains(&codec.as_str());
        out.push(StreamInfo {
            index: json_u32(s.get("index")).unwrap_or(0) as usize,
            kind,
            codec,
            profile: json_str(s.get("profile")),
            language,
            title,
            is_default,
            width: json_u32(s.get("width")),
            height: json_u32(s.get("height")),
            pix_fmt: json_str(s.get("pix_fmt")),
            channels: json_u32(s.get("channels")),
            channel_layout: json_str(s.get("channel_layout")),
            sample_rate: json_str(s.get("sample_rate")).and_then(|v| v.parse::<u32>().ok()),
            copyable,
            text_subtitle,
            graphic_subtitle,
        });
    }
    out
}

/// 选取默认字幕流：优先第一条文本字幕
pub fn first_text_subtitle(streams: &[StreamInfo]) -> Option<usize> {
    streams
        .iter()
        .find(|s| s.kind == "subtitle" && s.text_subtitle)
        .map(|s| s.index)
}

/// 选取默认音频流：优先标记 default 的，否则第一条
pub fn default_audio_stream(streams: &[StreamInfo]) -> Option<usize> {
    streams
        .iter()
        .find(|s| s.kind == "audio" && s.is_default)
        .or_else(|| streams.iter().find(|s| s.kind == "audio"))
        .map(|s| s.index)
}

/// 选取默认视频流：第一条
pub fn default_video_stream(streams: &[StreamInfo]) -> Option<usize> {
    streams.iter().find(|s| s.kind == "video").map(|s| s.index)
}

/// 烧录字幕时传给 `subtitles` 滤镜的取值：**只取文件名**（不含目录）。
///
/// ffmpeg 的滤镜参数先把整段值剥离外层引号，再按 `:` 切分成一个个选项，
/// 因此直接传 `C:/.../sub.srt` 时会被解析成 filename=`C` + 选项 `original_size=/...`
/// 而报错（实测退出码 -22）；文件路径中的空格、逗号、括号、单引号也有各自的转义坑，
/// 极难穷举。改为「裸文件名 + 把 ffmpeg 工作目录设为字幕所在目录」（见
/// [`subtitle_filter_cwd`]），可一次性绕开全部转义问题：
/// 文件名由本程序生成（形如 `sub-<pid>-<idx>.srt`），只含字母数字与 `.`/`-`。
pub fn subtitle_filter_value(sub: &Path) -> String {
    sub.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| sub.to_string_lossy().replace('\\', "/"))
}

/// 烧录字幕时 ffmpeg 进程应设置的工作目录（字幕文件所在目录），配合 [`subtitle_filter_value`] 使用
pub fn subtitle_filter_cwd(sub: &Path) -> Option<std::path::PathBuf> {
    sub.parent().map(|p| p.to_path_buf())
}

/// 构建转码参数。
///
/// `subtitle_file` 为烧录模式时预先抽出的字幕文件路径；`None` 表示不烧录。
/// **注意**：烧录模式下本函数只把字幕「文件名」写进 `subtitles` 滤镜，调用方必须同时
/// 以 [`subtitle_filter_cwd`] 的返回值作为 ffmpeg 进程的工作目录，否则滤镜会找不到字幕文件。
pub fn build_args(
    input: &str,
    dest: &Path,
    o: &TranscodeOptions,
    subtitle_file: Option<&Path>,
    start_sec: Option<f64>,
    dur_sec: Option<f64>,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-y".into()];
    if let Some(s) = start_sec {
        args.extend(["-ss".into(), format!("{:.3}", s)]);
    }
    args.extend(["-i".into(), input.to_string()]);

    // ===== 视频轨 =====
    // 图形（位图）字幕烧录需要用 filter_complex 把字幕画面 overlay 到视频上，
    // 此时视频走滤镜输出 [v]，不能再单独 -map 原视频流。
    let scale_filter: Option<String> = match o.resolution.as_deref() {
        Some(res) if res != "original" && !res.is_empty() => {
            // 按高度缩放，宽度取偶数保持宽高比
            Some(format!("scale=-2:{}", res))
        }
        _ => None,
    };
    let graphic_burn =
        o.burn_subtitles() && o.subtitle_graphic() && o.subtitle_stream.is_some();
    if graphic_burn {
        let idx = o.subtitle_stream.unwrap();
        // 先 overlay（两者尺寸一致）再缩放，避免字幕位图与缩放后画面错位
        let fc = match &scale_filter {
            Some(sc) => format!("[0:v][0:{}]overlay[ov];[ov]{}[v]", idx, sc),
            None => format!("[0:v][0:{}]overlay[v]", idx),
        };
        args.extend(["-filter_complex".into(), fc]);
        args.extend(["-map".into(), "[v]".into()]);
    } else {
        match o.video_stream {
            Some(i) => args.extend(["-map".into(), format!("0:{}", i)]),
            None => args.extend(["-map".into(), "0:v:0?".into()]),
        }
    }

    // ===== 音频轨 =====
    if o.include_audio() {
        match o.audio_stream {
            Some(i) => args.extend(["-map".into(), format!("0:{}", i)]),
            None => args.extend(["-map".into(), "0:a:0?".into()]),
        }
    }

    // ===== 字幕轨（内嵌模式；烧录 / 外挂模式不映射字幕流）=====
    if o.embed_subtitles() {
        if let Some(i) = o.subtitle_stream {
            args.extend(["-map".into(), format!("0:{}", i)]);
        }
    }

    // ===== 视频编码 =====
    let vmode = o.effective_video_mode();
    let reencode = vmode != "copy";
    match vmode {
        "h265" | "hevc" => {
            args.extend(["-c:v".into(), "libx265".into()]);
        }
        "av1" => {
            args.extend(["-c:v".into(), "libaom-av1".into(), "-cpu-used".into(), "6".into()]);
        }
        "copy" => {
            args.extend(["-c:v".into(), "copy".into()]);
        }
        _ => {
            args.extend(["-c:v".into(), "libx264".into()]);
        }
    }
    if reencode {
        let preset = o.preset.as_deref().unwrap_or("medium");
        args.extend(["-preset".into(), preset.to_string()]);
        let crf = o.crf.unwrap_or(20).min(51);
        args.extend(["-crf".into(), crf.to_string()]);
        let pix = o.pix_fmt.as_deref().unwrap_or(if vmode == "h265" || vmode == "hevc" {
            "yuv420p10le"
        } else {
            "yuv420p"
        });
        if pix != "auto" {
            args.extend(["-pix_fmt".into(), pix.to_string()]);
        }
    }

    // ===== 视频滤镜：缩放 + 文本字幕烧录（图形字幕已在上面的 filter_complex 中处理）=====
    if !graphic_burn {
        let mut vf: Vec<String> = Vec::new();
        if let Some(sc) = &scale_filter {
            vf.push(sc.clone());
        }
        if o.burn_subtitles() {
            if let Some(p) = subtitle_file {
                vf.push(format!("subtitles={}", subtitle_filter_value(p)));
            }
        }
        if !vf.is_empty() {
            args.extend(["-vf".into(), vf.join(",")]);
        }
    }

    // ===== 音频编码 =====
    if o.include_audio() {
        if o.audio_copy() {
            args.extend(["-c:a".into(), "copy".into()]);
        } else {
            args.extend(["-c:a".into(), "aac".into()]);
            let br = o.audio_bitrate.as_deref().unwrap_or("192k");
            args.extend(["-b:a".into(), br.to_string()]);
        }
        // 直拷时不能再改声道 / 采样率
        if !o.audio_copy() {
            if let Some(ch) = o.audio_channels.filter(|c| *c > 0) {
                args.extend(["-ac".into(), ch.to_string()]);
            }
            if let Some(sr) = o.audio_sample_rate.filter(|s| *s > 0) {
                args.extend(["-ar".into(), sr.to_string()]);
            }
        }
    } else {
        args.push("-an".into());
    }

    // ===== 字幕编码（内嵌模式）=====
    if o.embed_subtitles() {
        args.extend(["-c:s".into(), "mov_text".into()]);
    } else if !o.burn_subtitles() {
        args.push("-sn".into());
    }

    if o.faststart() {
        args.extend(["-movflags".into(), "+faststart".into()]);
    }
    args.extend(["-progress".into(), "pipe:1".into(), "-nostats".into()]);
    if let Some(d) = dur_sec {
        args.extend(["-t".into(), format!("{:.3}", d)]);
    }
    args.push(dest.to_string_lossy().to_string());
    args
}

/// 抽出字幕流到临时文件时，按字幕编码选择容器扩展名（文本字幕统一用 srt / ass）
pub fn subtitle_ext(codec: &str) -> &'static str {
    match codec {
        "ass" | "ssa" => "ass",
        _ => "srt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base() -> TranscodeOptions {
        TranscodeOptions {
            video_mode: Some("h264".into()),
            audio_mode: Some("aac".into()),
            ..Default::default()
        }
    }

    #[test]
    fn burn_forces_video_reencode() {
        let mut o = base();
        o.video_mode = Some("copy".into());
        o.subtitle_mode = Some("burn".into());
        assert_eq!(o.effective_video_mode(), "h264");
        assert!(o.burn_subtitles());
    }

    #[test]
    fn args_for_audio_only_copy() {
        let mut o = TranscodeOptions::default();
        o.video_mode = Some("copy".into());
        o.audio_mode = Some("aac".into());
        o.audio_bitrate = Some("256k".into());
        let dest = PathBuf::from("out.mp4");
        let args = build_args("in.mkv", &dest, &o, None, None, None);
        let joined = args.join(" ");
        assert!(joined.contains("-c:v copy"));
        assert!(joined.contains("-c:a aac"));
        assert!(joined.contains("-b:a 256k"));
        assert!(joined.contains("-movflags +faststart"));
        assert!(args.last().unwrap() == "out.mp4");
    }

    #[test]
    fn args_burn_subtitle_uses_vf_and_reencodes() {
        let mut o = base();
        o.subtitle_mode = Some("burn".into());
        o.video_mode = Some("copy".into());
        let dest = PathBuf::from("out.mp4");
        let sub = PathBuf::from(r"C:\Temp\sub.srt");
        let args = build_args("in.mkv", &dest, &o, Some(&sub), None, None);
        let joined = args.join(" ");
        assert!(joined.contains("-c:v libx264"), "burn 必须重编码: {}", joined);
        assert!(joined.contains("subtitles=sub.srt"), "只应传字幕文件名: {}", joined);
        assert!(!joined.contains("-c:s"), "烧录模式不内嵌字幕轨: {}", joined);
    }

    #[test]
    fn args_embed_subtitle_and_drop_when_none() {
        let mut o = base();
        o.subtitle_mode = Some("embed".into());
        o.subtitle_stream = Some(3);
        let dest = PathBuf::from("out.mp4");
        let args = build_args("in.mkv", &dest, &o, None, None, None);
        let joined = args.join(" ");
        assert!(joined.contains("-map 0:3"));
        assert!(joined.contains("-c:s mov_text"));

        let o2 = base();
        let args2 = build_args("in.mkv", &dest, &o2, None, None, None);
        assert!(args2.join(" ").contains("-sn"));
    }

    #[test]
    fn subtitle_filter_uses_basename_and_parent_cwd() {
        let p = PathBuf::from(r"C:\Temp\sub-1.srt");
        assert_eq!(subtitle_filter_value(&p), "sub-1.srt");
        assert_eq!(subtitle_filter_cwd(&p), Some(PathBuf::from(r"C:\Temp")));
    }

    #[test]
    fn parse_streams_detects_types_and_copyability() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"streams":[
              {"index":0,"codec_type":"video","codec_name":"hevc","profile":"Main 10","width":1920,"height":816,"pix_fmt":"yuv420p10le","disposition":{"default":1}},
              {"index":1,"codec_type":"audio","codec_name":"dts","profile":"DTS-HD MA","channels":6,"sample_rate":"48000","tags":{"language":"eng"},"disposition":{"default":1}},
              {"index":2,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"chi"},"disposition":{"default":0}},
              {"index":3,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle","disposition":{"default":0}},
              {"index":4,"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}
            ]}"#,
        )
        .unwrap();
        let s = parse_streams(&json);
        assert_eq!(s.len(), 4, "封面 mjpeg 应被剔除");
        assert_eq!(s[0].kind, "video");
        assert!(s[0].copyable, "hevc 可直拷进 MP4");
        assert!(!s[1].copyable, "DTS 不能直拷进 MP4");
        assert!(s[2].text_subtitle, "subrip 为文本字幕");
        assert!(!s[3].text_subtitle, "PGS 为图形字幕");
        assert_eq!(s[1].describe(), "DTS DTS-HD MA 6ch 48kHz");
        assert_eq!(first_text_subtitle(&s), Some(2));
        assert_eq!(default_audio_stream(&s), Some(1));
    }
}
