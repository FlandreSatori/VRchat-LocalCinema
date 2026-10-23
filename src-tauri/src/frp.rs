//! frp 公网隧道（frpc，SakuraFrp 等服务）：进程启停、状态与运行输出。
//!
//! 与 Cloudflare 快速隧道不同，frp 的隧道由用户在服务面板创建（隧道回源的本地地址
//! 也在面板上填写），本程序只负责按「访问密钥:隧道ID」启动/停止 frpc 并上报状态。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};

use crate::error::{AppError, AppResult};
use crate::settings::FrpConfig;

/// 启动后若在此时限内仍未建立全部隧道，视为连接失败并回收进程
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// 曾经连接成功后的异常退出，等待一段时间再自动拉起，避免失败时密集重试
const RELAUNCH_DELAY: Duration = Duration::from_secs(5);
/// 运行输出的环形缓冲上限（字节）
const OUTPUT_LIMIT: usize = 120 * 1024;
/// 失败诊断附带的最近日志行数
const LOG_LINES: usize = 12;

type StatusEmitter = Arc<dyn Fn(FrpStatus) + Send + Sync>;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrpStatus {
    /// frpc 进程存在
    pub running: bool,
    /// 启动参数中的隧道都已建立
    pub ready: bool,
    /// 本次启动参数包含的隧道数量
    pub tunnel_count: usize,
    /// 隧道回源的 HTTP 端口（启动时的当前端口）
    pub port: Option<u16>,
    pub error: Option<String>,
    /// 当前启动参数（已清洗）
    pub params: String,
    /// 防止较迟返回的 IPC 快照覆盖较新的事件
    pub revision: u64,
}

/// 一次启动所需的全部信息（自动重试时复用）
#[derive(Clone)]
struct LaunchRequest {
    binary: PathBuf,
    params: String,
    port: u16,
    tunnel_count: usize,
}

#[derive(Default)]
struct FrpInner {
    child: Option<Child>,
    status: FrpStatus,
    /// 进程代次：监控任务据此判断自己的事件是否已过期
    generation: u64,
    /// 应用退出中：阻止新的启动与自动重试
    shutting_down: bool,
    /// 是否允许异常退出后自动重试（来自设置）
    auto_restart: bool,
    /// frpc 运行输出（按行）
    output: VecDeque<String>,
    output_bytes: usize,
}

impl FrpInner {
    fn publish(&mut self, emit: &StatusEmitter) {
        self.status.revision += 1;
        emit(self.status.clone());
    }

    fn clear(&mut self, error: Option<String>) {
        self.status.running = false;
        self.status.ready = false;
        self.status.port = None;
        self.status.error = error;
    }

    fn push_output(&mut self, line: &str) {
        let line: String = line.chars().take(512).collect();
        self.output_bytes += line.len() + 1;
        self.output.push_back(line);
        while self.output_bytes > OUTPUT_LIMIT {
            match self.output.pop_front() {
                Some(front) => self.output_bytes = self.output_bytes.saturating_sub(front.len() + 1),
                None => break,
            }
        }
    }
}

#[derive(Default)]
pub struct FrpManager {
    inner: Arc<Mutex<FrpInner>>,
    /// 串行化整个启停流程（含自动重试与等待旧进程退出）
    operation: Arc<tokio::sync::Mutex<()>>,
}

impl FrpManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, FrpInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> FrpStatus {
        self.lock().status.clone()
    }

    /// frpc 运行输出（超出上限时保留尾部内容）
    pub fn read_output(&self) -> String {
        let inner = self.lock();
        inner.output.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// 启动 frpc：params 形如「访问密钥:隧道ID[,隧道ID...]」
    pub async fn start(
        &self,
        app: &AppHandle,
        binary: &Path,
        cfg: &FrpConfig,
        port: u16,
    ) -> AppResult<FrpStatus> {
        let _operation = self.operation.lock().await;
        let emit = app_emitter(app);
        self.terminate(&emit).await?;
        let params = normalize_params(&cfg.launch_params)?;
        let tunnel_count = count_tunnels(&params);
        {
            let mut inner = self.lock();
            if inner.shutting_down {
                return Err(AppError::Msg("应用正在退出，不能启动 frp 隧道".to_string()));
            }
            inner.auto_restart = cfg.auto_restart;
            inner.status.tunnel_count = tunnel_count;
            inner.status.params = params.clone();
            inner.status.error = None;
            inner.output.clear();
            inner.output_bytes = 0;
        }
        let request = LaunchRequest {
            binary: binary.to_path_buf(),
            params,
            port,
            tunnel_count,
        };
        spawn_request(self.inner.clone(), emit, request, self.operation.clone())
    }

    pub async fn stop(&self, app: &AppHandle) -> AppResult<FrpStatus> {
        let _operation = self.operation.lock().await;
        self.terminate(&app_emitter(app)).await
    }

    /// 退出路径不等待异步锁；关闭标记可阻止已排队的 start 与自动重试再次拉起进程
    pub fn stop_detached(&self) {
        let mut inner = self.lock();
        inner.shutting_down = true;
        inner.generation += 1;
        inner.auto_restart = false;
        if let Some(mut child) = inner.child.take() {
            let _ = child.start_kill();
        }
        inner.clear(None);
    }

    // 调用者持有 operation 锁；先使监控任务失效，再杀死并回收子进程。
    async fn terminate(&self, emit: &StatusEmitter) -> AppResult<FrpStatus> {
        let (child, generation) = {
            let mut inner = self.lock();
            inner.generation += 1;
            let child = inner.child.take();
            inner.auto_restart = false;
            inner.clear(None);
            inner.publish(emit);
            (child, inner.generation)
        };
        if let Some(mut child) = child {
            if let Err(e) = child.kill().await {
                let message = format!("停止 frpc 失败: {}", e);
                let mut inner = self.lock();
                if inner.generation == generation {
                    inner.clear(Some(message.clone()));
                    // 终止失败时保留句柄供下次重试，不能丢弃仍可能存活的进程。
                    inner.status.running = true;
                    inner.child = Some(child);
                    inner.publish(emit);
                }
                return Err(AppError::Msg(message));
            }
        }
        Ok(self.status())
    }
}

/// 拉起 frpc 并交给监控任务；返回启动瞬间的快照（隧道建立情况随后由事件推送）
///
/// 刻意保留为同步函数：监控任务在失败时会回调本函数完成自动重试，
/// 若本函数是 async 并等待监控任务，会造成 future 类型递归（非 Send）。
fn spawn_request(
    shared: Arc<Mutex<FrpInner>>,
    emit: StatusEmitter,
    request: LaunchRequest,
    operation: Arc<tokio::sync::Mutex<()>>,
) -> AppResult<FrpStatus> {
    let mut command = Command::new(&request.binary);
    command.args(["-f", &request.params, "--log_level", "info"]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let message = format!("启动 frpc 失败: {}", e);
            let mut inner = shared.lock().unwrap_or_else(|e| e.into_inner());
            inner.clear(Some(message.clone()));
            inner.publish(&emit);
            return Err(AppError::Msg(message));
        }
    };
    let stdout = child.stdout.take().expect("stdout configured as piped");
    let stderr = child.stderr.take().expect("stderr configured as piped");

    let (generation, status) = {
        let mut inner = shared.lock().unwrap_or_else(|e| e.into_inner());
        inner.generation += 1;
        inner.child = Some(child);
        inner.status.running = true;
        inner.status.ready = false;
        inner.status.port = Some(request.port);
        inner.status.error = None;
        inner.publish(&emit);
        (inner.generation, inner.status.clone())
    };

    // stdout 通常是 frpc 的常规日志，合并进同一份运行输出
    let out_shared = shared.clone();
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut inner = out_shared.lock().unwrap_or_else(|e| e.into_inner());
            if inner.generation != generation {
                return;
            }
            inner.push_output(&line);
            drop(inner);
        }
    });

    tokio::spawn(monitor(
        shared,
        operation,
        stderr,
        generation,
        request,
        emit,
    ));
    Ok(status)
}

async fn monitor(
    shared: Arc<Mutex<FrpInner>>,
    operation: Arc<tokio::sync::Mutex<()>>,
    stderr: tokio::process::ChildStderr,
    generation: u64,
    request: LaunchRequest,
    emit: StatusEmitter,
) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let mut log_open = true;
    let mut tail = VecDeque::new();
    let mut started = 0usize;
    let mut ever_ready = false;
    let mut waiting_since = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        let line = tokio::select! {
            biased;
            line = lines.next_line(), if log_open => Some(line),
            _ = tick.tick() => None,
        };
        // 在同一临界区校验代次、读取进程状态、更新状态并发布事件。
        let reaped = {
            let mut inner = shared.lock().unwrap_or_else(|e| e.into_inner());
            if inner.generation != generation || inner.child.is_none() {
                return;
            }
            let mut failure = None;
            if let Some(line) = line {
                match line {
                    Ok(Some(line)) => {
                        tracing::info!(target: "frpc", "{}", line);
                        push_log(&mut tail, &line);
                        inner.push_output(&line);
                        match classify_line(&line) {
                            LineKind::TunnelStarted => {
                                started += 1;
                                let ready = started >= request.tunnel_count;
                                if inner.status.ready != ready || inner.status.error.is_some() {
                                    inner.status.ready = ready;
                                    if ready {
                                        ever_ready = true;
                                        inner.status.error = None;
                                    }
                                    inner.publish(&emit);
                                }
                            }
                            LineKind::Lost => {
                                started = 0;
                                waiting_since = Instant::now();
                                if inner.status.ready {
                                    inner.status.ready = false;
                                    inner.status.error =
                                        Some("frp 隧道已断开，frpc 正在重连…".to_string());
                                    inner.publish(&emit);
                                }
                            }
                            LineKind::Failure(text) => failure = Some(text),
                            LineKind::Other => {}
                        }
                    }
                    Ok(None) => log_open = false, // EOF 不等于进程退出，仍需检查退出码。
                    Err(e) => failure = Some(format!("读取 frpc 日志失败: {}", e)),
                }
            } else {
                match inner.child.as_mut().unwrap().try_wait() {
                    Ok(Some(exit)) => failure = Some(format!("frpc 已退出（{}）", exit)),
                    Err(e) => failure = Some(format!("检查 frpc 进程失败: {}", e)),
                    Ok(None) => {}
                }
            }
            if failure.is_none() && !inner.status.ready && waiting_since.elapsed() >= READY_TIMEOUT
            {
                failure = Some(format!(
                    "frp 隧道启动超时（{} 秒），请检查访问密钥、隧道 ID 与网络后重试",
                    READY_TIMEOUT.as_secs()
                ));
            }
            if let Some(message) = failure {
                let message = with_logs(&message, &tail);
                tracing::warn!(target: "frpc", "{}", message);
                let mut child = inner.child.take();
                // 在释放状态锁前发送终止信号，避免新实例启动后旧实例仍运行。
                if let Some(child) = child.as_mut() {
                    let _ = child.start_kill();
                }
                // 仅在「曾经连接成功」后的异常退出才自动重试：密钥错误/网络不通等
                // 首次即失败的情况直接报错，避免无意义的重试风暴。
                let restart = inner.auto_restart && ever_ready && !inner.shutting_down;
                inner.auto_restart = false;
                inner.clear(Some(message));
                inner.publish(&emit);
                Some((child, restart))
            } else {
                None
            }
        };

        let Some((mut child, restart)) = reaped else {
            continue;
        };
        if let Some(child) = child.as_mut() {
            let _ = child.wait().await;
        }
        if !restart {
            return;
        }
        tracing::info!("frpc 中断，{} 秒后自动重试", RELAUNCH_DELAY.as_secs());
        tokio::time::sleep(RELAUNCH_DELAY).await;
        // 与手动启停串行，并确认仍是同一次运行的自动重试
        let _guard = operation.lock().await;
        {
            let inner = shared.lock().unwrap_or_else(|e| e.into_inner());
            if inner.shutting_down || inner.generation != generation {
                return;
            }
        }
        let _ = spawn_request(shared.clone(), emit.clone(), request.clone(), operation.clone());
        return;
    }
}

enum LineKind {
    /// 单条隧道建立成功
    TunnelStarted,
    Lost,
    Failure(String),
    Other,
}

fn classify_line(line: &str) -> LineKind {
    let lower = line.to_ascii_lowercase();
    if lower.contains("start proxy success")
        || lower.contains("start tunnel success")
        || line.contains("启动成功")
    {
        return LineKind::TunnelStarted;
    }
    if lower.contains("connection closed")
        || lower.contains("disconnected")
        || line.contains("连接中断")
        || line.contains("隧道关闭")
    {
        return LineKind::Lost;
    }
    if lower.contains("start error")
        || lower.contains("login to server failed")
        || line.contains("启动失败")
    {
        return LineKind::Failure(format!("frp 隧道启动失败：{}", line.trim()));
    }
    LineKind::Other
}

/// 清洗并校验启动参数，返回可直接传给 frpc -f 的形式
fn normalize_params(raw: &str) -> AppResult<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err(AppError::Msg("请先填写启动参数：访问密钥:隧道ID".to_string()));
    }
    let (token, ids) = cleaned.split_once(':').ok_or_else(|| {
        AppError::Msg(
            "启动参数格式应为「访问密钥:隧道ID」，例如 wdnmdtoken666666:114514".to_string(),
        )
    })?;
    if token.is_empty() || ids.is_empty() {
        return Err(AppError::Msg("访问密钥与隧道 ID 都不能为空".to_string()));
    }
    if !ids
        .split(',')
        .all(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(AppError::Msg(
            "隧道 ID 必须是数字，多条隧道用半角逗号分隔".to_string(),
        ));
    }
    Ok(format!("{}:{}", token, ids))
}

fn count_tunnels(params: &str) -> usize {
    params
        .split_once(':')
        .map(|(_, ids)| ids.split(',').filter(|s| !s.is_empty()).count())
        .unwrap_or(0)
}

fn push_log(tail: &mut VecDeque<String>, line: &str) {
    if tail.len() == LOG_LINES {
        tail.pop_front();
    }
    tail.push_back(line.chars().take(512).collect());
}

fn with_logs(message: &str, tail: &VecDeque<String>) -> String {
    if tail.is_empty() {
        return message.to_string();
    }
    format!(
        "{}\n最近日志：\n{}",
        message,
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    )
}

fn app_emitter(app: &AppHandle) -> StatusEmitter {
    let app = app.clone();
    Arc::new(move |status| {
        let _ = app.emit("frp-status-changed", status);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_counts_tunnels() {
        assert_eq!(
            normalize_params(" wdnmdtoken666666:114514 ").unwrap(),
            "wdnmdtoken666666:114514"
        );
        assert_eq!(
            normalize_params("wdnmdtoken666666 : 114514,114515,114516").unwrap(),
            "wdnmdtoken666666:114514,114515,114516"
        );
        assert_eq!(count_tunnels("wdnmdtoken666666:114514,114515"), 2);
        assert_eq!(count_tunnels(""), 0);
    }

    #[test]
    fn rejects_invalid_launch_params() {
        for raw in ["", "   ", "wdnmdtoken666666", ":114514", "wdnmdtoken666666:", "token:abc"] {
            assert!(normalize_params(raw).is_err(), "应拒绝: {raw:?}");
        }
        assert!(normalize_params("wdnmdtoken666666:114514,,").is_err());
    }

    #[test]
    fn classifies_tunnel_lifecycle_lines() {
        assert!(matches!(
            classify_line("INFO start proxy success"),
            LineKind::TunnelStarted
        ));
        assert!(matches!(
            classify_line("隧道 114514 启动成功"),
            LineKind::TunnelStarted
        ));
        assert!(matches!(
            classify_line("ERROR start error: port already used"),
            LineKind::Failure(_)
        ));
        assert!(matches!(
            classify_line("login to server failed: token not valid"),
            LineKind::Failure(_)
        ));
        assert!(matches!(
            classify_line("WARN connection closed, reconnecting"),
            LineKind::Lost
        ));
        assert!(matches!(classify_line("INFO 一些普通日志"), LineKind::Other));
    }

    #[test]
    fn output_buffer_is_bounded_and_keeps_tail() {
        let mut inner = FrpInner::default();
        for i in 0..4000 {
            inner.push_output(&format!("第 {} 行，填充一些内容以便超过上限", i));
        }
        assert!(inner.output_bytes <= OUTPUT_LIMIT);
        assert!(inner.output.len() < 4000);
    }
}
