//! 有界、可取消的短样本估算；只返回可信的帧差分吞吐，不承诺整片精度。
use crate::{
    commands::AppState,
    components,
    error::{AppError, AppResult},
    transcode::{self, StreamInfo, TranscodeOptions},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::State;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::Mutex as AsyncMutex,
};

#[derive(Default)]
pub struct EstimateState {
    gate: AsyncMutex<()>,
    active: Mutex<Option<(String, Arc<AtomicBool>)>>,
    cache: Mutex<HashMap<String, Value>>,
}
impl EstimateState {
    fn begin(&self, id: &str) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some((_, old)) = self
            .active
            .lock()
            .unwrap()
            .replace((id.into(), cancel.clone()))
        {
            old.store(true, Ordering::SeqCst);
        }
        cancel
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceProfile {
    pub video_stream: usize,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub total_frames: Option<f64>,
    pub video_codec: String,
    pub bitrate: Option<f64>,
    pub pix_fmt: Option<String>,
    pub file_size: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateSource {
    pub profile: SourceProfile,
    pub streams: Vec<StreamInfo>,
    pub effective_options: TranscodeOptions,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeEstimate {
    pub fps: Option<f64>,
    pub samples: Vec<f64>,
    pub method: &'static str,
    pub reason: Option<String>,
    pub effective_options: TranscodeOptions,
}
fn positive(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
}
fn rate(v: &Value) -> Option<f64> {
    let text = v.as_str()?;
    if let Some((a, b)) = text.split_once('/') {
        let n = a.parse::<f64>().ok()? / b.parse::<f64>().ok()?;
        (n.is_finite() && n > 0.0).then_some(n)
    } else {
        positive(v)
    }
}
fn source(json: &Value, size: u64, requested: &TranscodeOptions) -> AppResult<EstimateSource> {
    let streams = transcode::parse_streams(json);
    let selected = requested
        .video_stream
        .or_else(|| transcode::default_video_stream(&streams));
    let video = streams
        .iter()
        .find(|s| s.kind == "video" && Some(s.index) == selected)
        .ok_or_else(|| AppError::Msg("所选视频轨不存在".into()))?;
    let raw = json["streams"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|s| s["index"].as_u64() == Some(video.index as u64))
        })
        .ok_or_else(|| AppError::Msg("缺少视频轨信息".into()))?;
    let duration = positive(&raw["duration"]).or_else(|| positive(&json["format"]["duration"]));
    let fps = rate(&raw["avg_frame_rate"]).or_else(|| rate(&raw["r_frame_rate"]));
    let total_frames = positive(&raw["nb_frames"]).or_else(|| {
        duration
            .zip(fps)
            .map(|(d, f)| d * f)
            .filter(|n| n.is_finite())
    });
    let profile = SourceProfile {
        video_stream: video.index,
        duration,
        width: video.width,
        height: video.height,
        fps,
        total_frames,
        video_codec: video.codec.clone(),
        bitrate: positive(&raw["bit_rate"]).or_else(|| positive(&json["format"]["bit_rate"])),
        pix_fmt: video.pix_fmt.clone(),
        file_size: size,
    };
    let effective_options = effective_options(requested, &streams);
    Ok(EstimateSource {
        profile,
        streams,
        effective_options,
    })
}
// 与 commands::sanitize_options 保持一致；不因估算而启用新的执行功能。
fn effective_options(requested: &TranscodeOptions, streams: &[StreamInfo]) -> TranscodeOptions {
    let mut out = requested.clone();
    if out.video_copy()
        && !out
            .video_stream
            .and_then(|i| streams.iter().find(|s| s.index == i))
            .or_else(|| streams.iter().find(|s| s.kind == "video"))
            .map(|s| s.copyable)
            .unwrap_or(false)
    {
        out.video_mode = Some("h264".into());
    }
    if out.include_audio()
        && out.audio_copy()
        && !out
            .audio_stream
            .and_then(|i| streams.iter().find(|s| s.index == i))
            .or_else(|| streams.iter().find(|s| s.kind == "audio"))
            .map(|s| s.copyable)
            .unwrap_or(false)
    {
        out.audio_mode = Some("aac".into());
    }
    if out.wants_subtitle() {
        // 与 commands::sanitize_options 一致：显式优先，其次文本字幕，烧录模式可退而选图形字幕
        let chosen = out
            .subtitle_stream
            .and_then(|i| {
                streams
                    .iter()
                    .find(|s| s.index == i && s.kind == "subtitle")
            })
            .or_else(|| {
                streams
                    .iter()
                    .find(|s| s.kind == "subtitle" && s.text_subtitle)
            })
            .or_else(|| {
                if out.burn_subtitles() {
                    streams
                        .iter()
                        .find(|s| s.kind == "subtitle" && s.graphic_subtitle)
                } else {
                    None
                }
            });
        let usable = chosen.filter(|s| {
            if out.embed_subtitles() || out.external_subtitles() {
                s.text_subtitle
            } else {
                s.text_subtitle || s.graphic_subtitle
            }
        });
        match usable {
            Some(s) => {
                out.subtitle_stream = Some(s.index);
                out.subtitle_graphic = Some(s.graphic_subtitle);
            }
            None => {
                out.subtitle_stream = None;
                out.subtitle_mode = None;
                out.subtitle_graphic = None;
            }
        }
    }
    out
}

struct ProcessResult {
    stdout: String,
    elapsed: f64,
}
/// ffprobe 读取流信息的超时；大文件/网络盘可能较慢，留出足够余量。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 抽取内嵌字幕的超时。
const SUBTITLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 单次视频采样编码的超时；慢编码器（如 AV1）编码几十帧可能明显超过 3 秒。
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(60);
async fn run(
    binary: &Path,
    args: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<ProcessResult, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("估算已取消".into());
    }
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // 持续排空两个管道，避免 FFmpeg 日志填满后阻塞；错误不带源文件路径回传。
    let read_out = tokio::spawn(async move {
        let mut text = String::new();
        BufReader::new(stdout)
            .take(2 * 1024 * 1024)
            .read_to_string(&mut text)
            .await
            .map(|_| text)
    });
    let read_err = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                // 到达流末尾或读取出错都终止排空，避免无谓占用。
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    let result = loop {
        if cancel.load(Ordering::SeqCst) {
            break Err("估算已取消".to_string());
        }
        if started.elapsed() >= timeout {
            break Err(format!("探测进程超过 {} 秒，回落经验估算", timeout.as_secs()));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err("FFmpeg/ffprobe 探测失败，回落经验估算".into())
                }
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(e) => break Err(e.to_string()),
        }
    };
    let elapsed = started.elapsed().as_secs_f64();
    if result.is_err() {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
    let stdout = read_out
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let _ = read_err.await;
    result?;
    Ok(ProcessResult { stdout, elapsed })
}

async fn load(
    state: &AppState,
    estimator: &EstimateState,
    id: &str,
    options: &TranscodeOptions,
    cancel: &AtomicBool,
) -> AppResult<(EstimateSource, PathBuf)> {
    let item = state
        .db
        .lock()
        .unwrap()
        .get(id)?
        .ok_or_else(|| AppError::Msg("媒体条目不存在".into()))?;
    let path = PathBuf::from(&item.file_path);
    let metadata = std::fs::metadata(&path)?;
    let settings = state.current_settings();
    let (probe, _) = components::ffprobe_binary(&settings, &state.paths);
    let probe = probe.ok_or_else(|| AppError::Msg("未检测到 ffprobe".into()))?;
    let key = format!(
        "{}:{:?}:{}:{:?}",
        item.file_path,
        metadata.modified().ok(),
        metadata.len(),
        probe
    );
    let cached = estimator.cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
    let json = if let Some(json) = cached {
        json
    } else {
        let args = vec![
            "-v".into(),
            "error".into(),
            "-show_streams".into(),
            "-show_format".into(),
            "-of".into(),
            "json".into(),
            item.file_path,
        ];
        let result = run(&probe, &args, None, PROBE_TIMEOUT, cancel)
            .await
            .map_err(AppError::Msg)?;
        let json: Value =
            serde_json::from_str(&result.stdout).map_err(|e| AppError::Msg(e.to_string()))?;
        let mut cache = estimator.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, json.clone());
        json
    };
    Ok((source(&json, metadata.len(), options)?, path))
}

#[tauri::command]
pub async fn load_estimate_source(
    state: State<'_, AppState>,
    estimator: State<'_, EstimateState>,
    id: String,
    options: Option<TranscodeOptions>,
    request_id: Option<String>,
) -> AppResult<EstimateSource> {
    let cancel = estimator.begin(&request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()));
    let _guard = estimator.gate.lock().await;
    if cancel.load(Ordering::SeqCst) {
        return Err(AppError::Msg("估算已取消".into()));
    }
    let (source, _) = load(
        &state,
        &estimator,
        &id,
        &options.unwrap_or_default(),
        &cancel,
    )
    .await?;
    Ok(source)
}
#[tauri::command]
pub fn cancel_transcode_estimate(estimator: State<'_, EstimateState>, request_id: String) {
    if let Some((id, cancel)) = estimator.active.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *id == request_id {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}
fn sample_positions(duration: f64) -> Vec<f64> {
    if duration < 15.0 {
        vec![duration * 0.3]
    } else {
        vec![duration * 0.2, duration * 0.5, duration * 0.8]
    }
}
fn probe_args(
    input: &str,
    options: &TranscodeOptions,
    sub: Option<&Path>,
    start: f64,
    frames: u32,
) -> Vec<String> {
    let mut video = options.clone();
    video.include_audio = Some(false);
    if !video.burn_subtitles() {
        video.subtitle_mode = None;
        video.subtitle_stream = None;
    }
    video.faststart = Some(false);
    let mut args = transcode::build_args(input, Path::new("-"), &video, sub, Some(start), None);
    args.pop();
    // 恢复原片字幕时间，再恢复样本内时间，与分段执行路径相同。
    if video.burn_subtitles() {
        if let Some(i) = args.iter().position(|a| a == "-vf") {
            args[i + 1] = format!(
                "setpts=PTS+{start:.3}/TB,{},setpts=PTS-{start:.3}/TB",
                args[i + 1]
            );
        }
    }
    args.extend([
        "-frames:v".into(),
        frames.to_string(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ]);
    args
}
fn completed_frames(output: &str) -> u32 {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("frame=")?.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}
fn differential(a: &ProcessResult, b: &ProcessResult) -> Option<f64> {
    let dt = b.elapsed - a.elapsed;
    if completed_frames(&a.stdout) != 5 || completed_frames(&b.stdout) != 45 || dt < 0.05 {
        return None;
    }
    let fps = 40.0 / dt;
    (fps.is_finite() && fps > 0.0).then_some(fps)
}

#[tauri::command]
pub async fn probe_transcode_estimate(
    state: State<'_, AppState>,
    estimator: State<'_, EstimateState>,
    id: String,
    request_id: String,
    options: TranscodeOptions,
) -> AppResult<ProbeEstimate> {
    let cancel = estimator.begin(&request_id);
    // 新请求先取消旧请求，旧子进程 kill + wait 完成后才释放 gate。
    let _guard = estimator.gate.lock().await;
    if cancel.load(Ordering::SeqCst) {
        return Err(AppError::Msg("估算已取消".into()));
    }
    let (source, path) = load(&state, &estimator, &id, &options, &cancel).await?;
    let options = source.effective_options;
    let mut result = ProbeEstimate {
        fps: None,
        samples: Vec::new(),
        method: "frame-difference",
        reason: None,
        effective_options: options.clone(),
    };
    let fail = |mut r: ProbeEstimate, reason: &str| {
        r.reason = Some(reason.into());
        r
    };
    if options.effective_video_mode() == "copy" {
        return Ok(fail(result, "直拷按磁盘吞吐估算，不运行视频编码探测"));
    }
    let Some((duration, _fps)) = source.profile.duration.zip(source.profile.fps) else {
        return Ok(fail(result, "缺少有效时长或帧率，无法获得可信探测样本"));
    };
    let settings = state.current_settings();
    let (ffmpeg, _) = components::ffmpeg_binary(&settings, &state.paths);
    let Some(ffmpeg) = ffmpeg else {
        return Ok(fail(result, "未检测到 FFmpeg，回落经验估算"));
    };
    let temp = tempfile::tempdir().map_err(|e| AppError::Msg(e.to_string()))?;
    let input = path.to_string_lossy().to_string();
    let subtitle = if options.burn_subtitles() {
        let s = source
            .streams
            .iter()
            .find(|s| Some(s.index) == options.subtitle_stream)
            .unwrap();
        let dest = temp
            .path()
            .join(format!("sub.{}", transcode::subtitle_ext(&s.codec)));
        let args = vec![
            "-v".into(),
            "error".into(),
            "-y".into(),
            "-i".into(),
            input.clone(),
            "-map".into(),
            format!("0:{}", s.index),
            "-c:s".into(),
            if matches!(s.codec.as_str(), "ass" | "ssa") {
                "ass".into()
            } else {
                "srt".into()
            },
            dest.to_string_lossy().to_string(),
        ];
        if let Err(e) = run(&ffmpeg, &args, None, SUBTITLE_TIMEOUT, &cancel).await {
            return Ok(fail(result, &format!("字幕抽取未完成：{e}")));
        }
        Some(dest)
    } else {
        None
    };
    for position in sample_positions(duration) {
        let mut pair = Vec::new();
        for frames in [5, 45] {
            let args = probe_args(&input, &options, subtitle.as_deref(), position, frames);
            match run(
                &ffmpeg,
                &args,
                subtitle.as_deref().and_then(Path::parent),
                SAMPLE_TIMEOUT,
                &cancel,
            )
            .await
            {
                Ok(sample) => pair.push(sample),
                Err(e) => return Ok(fail(result, &e)),
            }
        }
        if let Some(fps) = differential(&pair[0], &pair[1]) {
            result.samples.push(fps);
        }
    }
    let required = if duration < 15.0 { 1 } else { 2 };
    if result.samples.len() < required {
        return Ok(fail(
            result,
            "有效差分样本不足（可能片长过短或编码器启动过慢），回落经验估算",
        ));
    }
    let lo = result.samples.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = result.samples.iter().copied().fold(0.0, f64::max);
    if hi / lo > 3.0 {
        return Ok(fail(result, "样本吞吐差异过大，暂不校准，回落经验估算"));
    }
    result.fps =
        Some(result.samples.len() as f64 / result.samples.iter().map(|f| 1.0 / f).sum::<f64>());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newer_request_cancels_old_token() {
        let state = EstimateState::default();
        let first = state.begin("first");
        let second = state.begin("second");
        assert!(first.load(Ordering::SeqCst));
        assert!(!second.load(Ordering::SeqCst));
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn process_timeout_kills_and_reaps() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Start-Sleep -Seconds 10".into(),
        ];
        let error = run(
            Path::new("powershell.exe"),
            &args,
            None,
            Duration::from_secs(1),
            &cancel,
        )
        .await
        .err()
        .unwrap();
        assert!(error.contains("1 秒"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn process_cancel_kills_and_reaps() {
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Start-Sleep -Seconds 10".into(),
        ];
        let error = run(
            Path::new("powershell.exe"),
            &args,
            None,
            SAMPLE_TIMEOUT,
            &cancel,
        )
        .await
        .err()
        .unwrap();
        assert!(error.contains("取消"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn unknown_rates_are_not_nan() {
        assert_eq!(rate(&Value::String("0/0".into())), None);
        assert_eq!(
            rate(&Value::String("30000/1001".into())).unwrap().round(),
            30.0
        );
        assert_eq!(positive(&Value::String("NaN".into())), None);
    }
    #[test]
    fn selected_profile_and_effective_subtitles() {
        let json = serde_json::json!({"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":1920},
            {"index":1,"codec_type":"video","codec_name":"vp8","width":640,"avg_frame_rate":"25/1","duration":"10"},
            {"index":2,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle"}
        ]});
        let options = TranscodeOptions {
            video_stream: Some(1),
            video_mode: Some("copy".into()),
            subtitle_mode: Some("burn".into()),
            subtitle_stream: Some(2),
            ..Default::default()
        };
        let s = source(&json, 100, &options).unwrap();
        assert_eq!(s.profile.width, Some(640));
        assert_eq!(s.profile.total_frames, Some(250.0));
        assert_eq!(s.effective_options.video_mode(), "h264");
        // 1.4：图形字幕也纳入可烧录字幕轨（走 overlay 方式）
        assert!(s.effective_options.burn_subtitles());
        assert!(s.effective_options.subtitle_graphic());
    }
    #[test]
    fn probe_reuses_real_encoder_and_burn_filter() {
        let o = TranscodeOptions {
            video_mode: Some("av1".into()),
            subtitle_mode: Some("burn".into()),
            subtitle_stream: Some(3),
            audio_stream: Some(2),
            ..Default::default()
        };
        let a = probe_args("movie.mkv", &o, Some(Path::new("sub.ass")), 20.0, 45);
        let text = a.join(" ");
        assert!(text.contains("libaom-av1"));
        assert!(text.contains("subtitles=sub.ass"));
        assert!(text.contains("-f null -"));
        assert!(!text.contains("-map 0:2"));
        assert!(!text.contains("-map 0:3"));
        assert!(a.iter().position(|s| s == "-ss") < a.iter().position(|s| s == "-i"));
    }
    #[test]
    fn only_completed_differential_samples_calibrate() {
        let a = ProcessResult {
            stdout: "frame=5\n".into(),
            elapsed: 0.4,
        };
        let mut b = ProcessResult {
            stdout: "frame=45\n".into(),
            elapsed: 1.4,
        };
        assert!((differential(&a, &b).unwrap() - 40.0).abs() < 0.001);
        b.stdout = "frame=20\n".into();
        assert_eq!(differential(&a, &b), None);
        assert_eq!(sample_positions(10.0), vec![3.0]);
        assert_eq!(sample_positions(100.0), vec![20.0, 50.0, 80.0]);
    }
}
