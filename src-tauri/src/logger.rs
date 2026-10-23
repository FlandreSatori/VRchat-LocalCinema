use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const MAX_RECENT: usize = 200;
/// 单个日志文件超过该大小即轮转，避免长期运行把磁盘写满
const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
/// 日志保留天数（按文件修改时间清理更早的文件）
const LOG_RETENTION_DAYS: u64 = 14;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub ts: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Clone)]
pub struct RecentLogs {
    inner: Arc<Mutex<VecDeque<LogEntry>>>,
}

impl RecentLogs {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_RECENT))),
        }
    }

    pub fn push(&self, entry: LogEntry) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= MAX_RECENT {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    pub fn list(&self, limit: usize) -> Vec<LogEntry> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}

struct RecentLayer {
    logs: RecentLogs,
}

impl<S> Layer<S> for RecentLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor {
            message: String::new(),
        };
        event.record(&mut visitor);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.logs.push(LogEntry {
            ts,
            level: event.metadata().level().to_string(),
            target: event.metadata().target().to_string(),
            message: visitor.message,
        });
    }
}

/// 初始化结构化日志：写入 data_dir/logs/app-YYYY-MM-DD.log（本地日期），
/// 超过 MAX_LOG_BYTES 自动轮转为 app-YYYY-MM-DD.N.log，并清理超过保留天数的旧日志；
/// 同时保留最近 200 条到内存。
pub fn init(log_dir: &std::path::Path) -> RecentLogs {
    let _ = fs::create_dir_all(log_dir);
    prune_old_logs(log_dir);
    let file_name = format!("app-{}.log", time_now_date());
    let log_path: PathBuf = log_dir.join(file_name);
    let sink = Arc::new(Mutex::new(FileSink::open(log_path)));

    let recent = RecentLogs::new();

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(move || FileWriter(sink.clone()));
    let recent_layer = RecentLayer {
        logs: recent.clone(),
    };
    let stdout_layer = tracing_subscriber::fmt::layer().with_filter(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    );

    tracing_subscriber::registry()
        .with(file_layer.with_filter(EnvFilter::new("info")))
        .with(recent_layer.with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .with(stdout_layer)
        .init();

    recent
}

/// 删除修改时间早于保留期的历史日志（仅处理 app-*.log，避免误删其他文件）
fn prune_old_logs(log_dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };
    let Some(cutoff) = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(LOG_RETENTION_DAYS * 86400))
    else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let looks_like_log = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("app-") && n.ends_with(".log"))
            .unwrap_or(false);
        if !looks_like_log {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            if modified < cutoff {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn time_now_date() -> String {
    #[cfg(windows)]
    {
        // 取本地时区日期，避免跨时区/跨午夜时日志文件名与用户感知不一致
        use windows::Win32::System::SystemInformation::GetLocalTime;
        let st = unsafe { GetLocalTime() };
        return format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay);
    }
    #[cfg(not(windows))]
    {
        // 简易 UTC 日期，避免引入 chrono
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let days = secs / 86400;
        // civil-from-days 算法
        let z = days as i64 + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!("{:04}-{:02}-{:02}", y, m, d)
    }
}

/// 当前日志文件的写入句柄与已写入字节数，支持按大小轮转
struct FileSink {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    rotation: u32,
}

impl FileSink {
    fn open(path: PathBuf) -> Self {
        let file = File::options().create(true).append(true).open(&path).ok();
        let written = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);
        Self {
            path,
            file,
            written,
            rotation: 0,
        }
    }

    /// 关闭当前文件并改名为 app-<date>.N.log，然后重新创建空文件继续写入
    fn rotate(&mut self) {
        if let Some(mut f) = self.file.take() {
            let _ = f.flush();
        }
        self.rotation += 1;
        let rotated = self.path.with_extension(format!("{}.log", self.rotation));
        let _ = fs::remove_file(&rotated);
        // 改名（Rust 打开文件默认共享删除权限，Windows 下可改名）；失败则退化为截断
        if fs::rename(&self.path, &rotated).is_err() {
            let _ = fs::remove_file(&self.path);
        }
        self.file = File::options().create(true).append(true).open(&self.path).ok();
        self.written = 0;
    }
}

struct FileWriter(Arc<Mutex<FileSink>>);

impl std::io::Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut sink = self.0.lock().map_err(|_| std::io::ErrorKind::Other)?;
        if sink.written > 0 && sink.written + buf.len() as u64 > MAX_LOG_BYTES {
            sink.rotate();
        }
        let n = match sink.file.as_mut() {
            Some(f) => f.write(buf)?,
            None => return Ok(buf.len()),
        };
        sink.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut sink = self.0.lock().map_err(|_| std::io::ErrorKind::Other)?;
        if let Some(f) = sink.file.as_mut() {
            f.flush()?;
        }
        Ok(())
    }
}

impl Clone for FileWriter {
    fn clone(&self) -> Self {
        FileWriter(self.0.clone())
    }
}

/// VRChat 客户端日志中的关键行：AVPro 打开流时的 API/解码路径，以及播放失败信息。
/// 这些行只在 VRChat 侧产生，抓到这里后可在本应用日志内直接对比「能播/不能播」的差异。
const VRC_LOG_KEYWORDS: [&str; 4] = [
    "[AVProVideo]",
    "Using playback path",
    "Video Error",
    "Loading failed",
];

/// VRChat 客户端日志目录（%USERPROFILE%\AppData\LocalLow\VRChat\VRChat）
fn vrchat_log_dir() -> Option<PathBuf> {
    let profile = std::env::var_os("USERPROFILE")?;
    Some(PathBuf::from(profile).join("AppData/LocalLow/VRChat/VRChat"))
}

/// 目录下最新的 output_log*.txt（VRChat 每次启动新建一个带时间戳的文件）
fn newest_vrchat_log(dir: &std::path::Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("output_log") || !name.ends_with(".txt") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// 后台跟踪 VRChat 客户端日志，把关键行同步写入本应用日志。
/// 每 2 秒轮询一次；只捕获本软件启动之后的日志：
/// - 会话文件在本软件启动前已存在（VRChat 早已在运行）→ 跳过已有内容，只读新增行；
/// - 本软件运行期间 VRChat 新开的会话（新 output_log 文件）→ 从头读取（整份都是新日志）。
pub fn spawn_vrchat_log_tailer() {
    tauri::async_runtime::spawn(async move {
        // 未安装 VRChat（无该目录）时静默空转，不影响其他功能
        let Some(dir) = vrchat_log_dir() else {
            return;
        };
        let started = std::time::SystemTime::now();
        let mut current: Option<PathBuf> = None;
        let mut offset: u64 = 0;
        let mut pending = String::new();
        loop {
            if let Some(path) = newest_vrchat_log(&dir) {
                if current.as_deref() != Some(path.as_path()) {
                    offset = initial_offset(&path, started);
                    tracing::info!(
                        "[VRChat] 开始跟踪客户端日志: {}（{}）",
                        path.display(),
                        if offset > 0 {
                            "跳过软件启动前已有内容"
                        } else {
                            "从头读取"
                        }
                    );
                    current = Some(path.clone());
                    pending.clear();
                }
                read_new_vrchat_lines(&path, &mut offset, &mut pending);
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
}

/// 新检测到的会话文件的起始读取位置：
/// 文件创建于本软件启动之前 → 返回当前文件长度（跳过已有内容）；
/// 创建于本软件启动之后 → 返回 0（从头读取）。取不到创建时间时按启动前处理。
fn initial_offset(path: &std::path::Path, started: std::time::SystemTime) -> u64 {
    let created_before_start = std::fs::metadata(path)
        .and_then(|m| m.created())
        .map(|created| created < started)
        .unwrap_or(true);
    if created_before_start {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    }
}

/// 增量读取日志文件新增内容，按行切分后筛选关键行写入应用日志
fn read_new_vrchat_lines(path: &std::path::Path, offset: &mut u64, pending: &mut String) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *offset {
        // 文件被重建或截断
        *offset = 0;
        pending.clear();
    }
    if len == *offset || file.seek(SeekFrom::Start(*offset)).is_err() {
        return;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return;
    }
    *offset += buf.len() as u64;
    pending.push_str(&String::from_utf8_lossy(&buf));
    // 只处理已完整结束的行，未结束的尾部留到下一次轮询
    while let Some(idx) = pending.find('\n') {
        let line = pending[..idx].trim_end_matches('\r').to_string();
        pending.drain(..=idx);
        if is_vrc_interesting(&line) {
            tracing::info!("[VRChat] {}", line);
        }
    }
}

/// 是否为需要同步到应用日志的 VRChat 关键行
fn is_vrc_interesting(line: &str) -> bool {
    !line.trim().is_empty() && VRC_LOG_KEYWORDS.iter().any(|k| line.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_only_vrchat_media_lines() {
        // 取自真实 output_log 的失败/成功行
        assert!(is_vrc_interesting(
            "2026.10.17 01:48:33 Debug -  [AVProVideo] Opening http://127.0.0.1:17001/x.mkv with API MediaFoundation"
        ));
        assert!(is_vrc_interesting("Using playback path: MF-MediaEngine-Hardware"));
        assert!(is_vrc_interesting("Video Error: PlayerError"));
        assert!(is_vrc_interesting(
            "[AVProVideo] Error: Loading failed.  File not found, codec not supported"
        ));
        assert!(!is_vrc_interesting("2026.10.17 01:48:33 Debug -  [Behaviour] OnPlayerJoined x"));
        assert!(!is_vrc_interesting("   "));
    }

    #[test]
    fn initial_offset_skips_preexisting_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output_log_2026-04-04_00-00-00.txt");
        std::fs::write(&path, "old lines\n").unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        // 文件创建于软件启动之前 → 跳过已有内容，只读新增
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(initial_offset(&path, future), len);
        // 文件创建于软件启动之后（运行期间新开的会话）→ 从头读取
        #[cfg(windows)]
        {
            let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
            assert_eq!(initial_offset(&path, past), 0);
        }
    }

    #[test]
    fn newest_log_file_is_picked() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("output_log_2026-01-01_00-00-00.txt");
        let newer = dir.path().join("output_log_2026-02-02_00-00-00.txt");
        std::fs::write(&older, b"x").unwrap();
        std::fs::write(&newer, b"x").unwrap();
        std::fs::write(dir.path().join("other.txt"), b"x").unwrap();
        File::options()
            .write(true)
            .open(&newer)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(newest_vrchat_log(dir.path()), Some(newer));
    }

    #[test]
    fn incremental_read_keeps_partial_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output_log_2026-03-03_00-00-00.txt");
        std::fs::write(&path, "noise\n[AVProVideo] Opening a\n未完").unwrap();
        let mut offset = 0u64;
        let mut pending = String::new();
        read_new_vrchat_lines(&path, &mut offset, &mut pending);
        assert_eq!(offset, std::fs::metadata(&path).unwrap().len());
        // 只切出完整行，未结束的尾部保留
        assert_eq!(pending, "未完");

        // 追加后续内容后再读，只读取新增部分
        let mut file = File::options().append(true).open(&path).unwrap();
        file.write_all("整\n".as_bytes()).unwrap();
        drop(file);
        read_new_vrchat_lines(&path, &mut offset, &mut pending);
        assert!(pending.is_empty());
        assert_eq!(offset, std::fs::metadata(&path).unwrap().len());
    }
}

