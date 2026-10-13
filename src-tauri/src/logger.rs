use std::collections::VecDeque;
use std::fs::{self, File};
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
        let mut guard = self.inner.lock().unwrap();
        if guard.len() >= MAX_RECENT {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    pub fn list(&self, limit: usize) -> Vec<LogEntry> {
        let guard = self.inner.lock().unwrap();
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

/// 初始化结构化日志：写入 data_dir/logs/app-YYYY-MM-DD.log，同时保留最近 200 条到内存。
pub fn init(log_dir: &std::path::Path) -> RecentLogs {
    let _ = fs::create_dir_all(log_dir);
    let file_name = format!(
        "app-{}.log",
        time_now_date()
    );
    let log_path: PathBuf = log_dir.join(file_name);
    let file = Arc::new(std::sync::Mutex::new(
        File::options()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok(),
    ));

    let recent = RecentLogs::new();

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(move || FileWriter(file.clone()));
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

fn time_now_date() -> String {
    // 简易本地日期，避免引入 chrono
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

struct FileWriter(Arc<std::sync::Mutex<Option<File>>>);

impl std::io::Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self.0.lock().map_err(|_| std::io::ErrorKind::Other)?;
        match guard.as_mut() {
            Some(f) => f.write(buf),
            None => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut guard = self.0.lock().map_err(|_| std::io::ErrorKind::Other)?;
        if let Some(f) = guard.as_mut() {
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

