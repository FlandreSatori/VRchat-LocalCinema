use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write as _;
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

