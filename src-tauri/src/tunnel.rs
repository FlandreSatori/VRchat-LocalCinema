//! Cloudflare 快速隧道：进程启停、连接状态与失败诊断。

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};

use crate::error::{AppError, AppResult};

const START_TIMEOUT: Duration = Duration::from_secs(60);
const LOG_LINES: usize = 12;
type StatusEmitter = Arc<dyn Fn(TunnelStatus) + Send + Sync>;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatus {
    /// 子进程存在（包括申请地址、连接及重连阶段）。
    pub running: bool,
    /// 已收到 cloudflared 的连接注册成功日志。
    pub ready: bool,
    /// 仅在连接就绪时发布地址；停止、断线和失败都会清空。
    pub url: Option<String>,
    pub port: Option<u16>,
    pub error: Option<String>,
    /// 防止较迟返回的 IPC 快照覆盖较新的事件。
    pub revision: u64,
}

impl TunnelStatus {
    pub fn playback_base(&self, port: u16) -> AppResult<String> {
        if !self.running || !self.ready {
            return Err(AppError::Msg(self.error.clone().unwrap_or_else(|| {
                "公网隧道尚未连接，请在「HTTP 服务」页启动隧道并等待连接成功".to_string()
            })));
        }
        if self.port != Some(port) {
            return Err(AppError::Msg(
                "HTTP 端口已变化，请重新启动公网隧道".to_string(),
            ));
        }
        self.url
            .clone()
            .ok_or_else(|| AppError::Msg("公网隧道地址尚未就绪".to_string()))
    }
}

#[derive(Default)]
struct TunnelInner {
    child: Option<Child>,
    status: TunnelStatus,
    generation: u64,
    shutting_down: bool,
}

impl TunnelInner {
    // 所有代次检查、状态修改与事件发布均在同一把锁内完成。
    fn publish(&mut self, emit: &StatusEmitter) {
        self.status.revision += 1;
        emit(self.status.clone());
    }

    fn clear(&mut self, error: Option<String>) {
        self.status.running = false;
        self.status.ready = false;
        self.status.url = None;
        self.status.port = None;
        self.status.error = error;
    }
}

#[derive(Default)]
pub struct TunnelManager {
    inner: Arc<Mutex<TunnelInner>>,
    /// 串行化整个启停流程，包括等待旧进程退出。
    operation: tokio::sync::Mutex<()>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> TunnelStatus {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }

    pub async fn start(
        &self,
        app: &AppHandle,
        binary: &Path,
        port: u16,
    ) -> AppResult<TunnelStatus> {
        let mut command = Command::new(binary);
        let target = format!("http://127.0.0.1:{}", port);
        command.args(["tunnel", "--no-autoupdate", "--url", &target]);
        self.launch(command, port, START_TIMEOUT, app_emitter(app))
            .await
    }

    async fn launch(
        &self,
        mut command: Command,
        port: u16,
        timeout: Duration,
        emit: StatusEmitter,
    ) -> AppResult<TunnelStatus> {
        let _operation = self.operation.lock().await;
        self.terminate(&emit).await?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.shutting_down {
            return Err(AppError::Msg("应用正在退出，不能启动公网隧道".to_string()));
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let message = format!("启动 cloudflared 失败: {}", e);
                inner.clear(Some(message.clone()));
                inner.publish(&emit);
                return Err(AppError::Msg(message));
            }
        };
        let stderr = child.stderr.take().expect("stderr configured as piped");
        inner.child = Some(child);
        inner.status.running = true;
        inner.status.port = Some(port);
        inner.publish(&emit);
        let generation = inner.generation;
        let status = inner.status.clone();
        tokio::spawn(monitor(
            self.inner.clone(),
            stderr,
            generation,
            timeout,
            emit,
        ));
        Ok(status)
    }

    // 调用者持有 operation；先使监控任务失效，再杀死并回收子进程。
    async fn terminate(&self, emit: &StatusEmitter) -> AppResult<TunnelStatus> {
        let (child, generation) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.generation += 1;
            let child = inner.child.take();
            inner.clear(None);
            inner.publish(emit);
            (child, inner.generation)
        };
        if let Some(mut child) = child {
            if let Err(e) = child.kill().await {
                let message = format!("停止 cloudflared 失败: {}", e);
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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

    pub async fn stop(&self, app: &AppHandle) -> AppResult<TunnelStatus> {
        let _operation = self.operation.lock().await;
        self.terminate(&app_emitter(app)).await
    }

    /// 退出路径不等待异步锁；关闭标记可阻止已排队的 start 再次拉起进程。
    pub fn stop_detached(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.shutting_down = true;
        inner.generation += 1;
        if let Some(mut child) = inner.child.take() {
            let _ = child.start_kill();
        }
        inner.clear(None);
    }
}

fn app_emitter(app: &AppHandle) -> StatusEmitter {
    let app = app.clone();
    Arc::new(move |status| {
        let _ = app.emit("tunnel-status-changed", status);
    })
}

async fn monitor(
    shared: Arc<Mutex<TunnelInner>>,
    stderr: tokio::process::ChildStderr,
    generation: u64,
    timeout: Duration,
    emit: StatusEmitter,
) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let mut log_open = true;
    let mut tail = VecDeque::new();
    let mut candidate_url = None;
    let mut connected = false;
    let mut waiting_since = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        let line = tokio::select! {
            biased;
            line = lines.next_line(), if log_open => Some(line),
            _ = tick.tick() => None,
        };
        // 在同一临界区校验代次、读取进程状态、更新状态并发布事件。
        let child_to_reap = {
            let mut inner = shared.lock().unwrap_or_else(|e| e.into_inner());
            if inner.generation != generation || inner.child.is_none() {
                return;
            }
            let mut failure = None;
            if let Some(line) = line {
                match line {
                    Ok(Some(line)) => {
                        tracing::info!(target: "cloudflared", "{}", line);
                        push_log(&mut tail, &line);
                        if let Some(url) = extract_url(&line) {
                            candidate_url = Some(url);
                        }
                        if line.contains("Registered tunnel connection") {
                            connected = true;
                        } else if connection_lost(&line) {
                            if connected {
                                waiting_since = Instant::now();
                            }
                            connected = false;
                        }
                        let ready = connected && candidate_url.is_some();
                        if inner.status.ready != ready {
                            inner.status.ready = ready;
                            inner.status.url = if ready { candidate_url.clone() } else { None };
                            inner.status.error = if ready {
                                None
                            } else {
                                Some("隧道连接中断，正在重连…".to_string())
                            };
                            inner.publish(&emit);
                        }
                    }
                    Ok(None) => log_open = false, // EOF 不等于进程退出，仍需检查退出码。
                    Err(e) => failure = Some(format!("读取 cloudflared 日志失败: {}", e)),
                }
            } else {
                match inner.child.as_mut().unwrap().try_wait() {
                    Ok(Some(exit)) => failure = Some(format!("cloudflared 已退出（{}）", exit)),
                    Err(e) => failure = Some(format!("检查 cloudflared 进程失败: {}", e)),
                    Ok(None) => {}
                }
            }
            if failure.is_none() && !inner.status.ready && waiting_since.elapsed() >= timeout {
                failure = Some(format!(
                    "隧道连接超时（{} 秒），请检查网络后重试",
                    timeout.as_secs()
                ));
            }
            if let Some(message) = failure {
                let message = with_logs(&message, &tail);
                tracing::warn!(target: "cloudflared", "{}", message);
                let mut child = inner.child.take();
                // 在释放状态锁前发送终止信号，避免新实例启动后旧实例仍运行。
                if let Some(child) = child.as_mut() {
                    let _ = child.start_kill();
                }
                inner.clear(Some(message));
                inner.publish(&emit);
                child
            } else {
                continue;
            }
        };
        if let Some(mut child) = child_to_reap {
            let _ = child.wait().await;
        }
        return;
    }
}

fn connection_lost(line: &str) -> bool {
    [
        "Unregistered tunnel connection",
        "Connection terminated",
        "Serve tunnel error",
        "failed to serve tunnel connection",
    ]
    .iter()
    .any(|text| line.contains(text))
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

fn extract_url(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| matches!(c, '|' | '"' | '\''));
        let host = word
            .strip_prefix("https://")?
            .strip_suffix(".trycloudflare.com")?;
        if host.is_empty() || !host.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return None;
        }
        Some(word.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_quick_tunnel_hostnames() {
        assert_eq!(
            extract_url("INF | https://random-words.trycloudflare.com |"),
            Some("https://random-words.trycloudflare.com".to_string())
        );
        assert!(
            extract_url("https://example.com https://bad.trycloudflare.com.evil.test").is_none()
        );
        assert!(extract_url("https://bad.trycloudflare.com/path").is_none());
        assert!(extract_url("https://.trycloudflare.com").is_none());
    }

    #[test]
    fn refuses_inactive_unready_and_wrong_port_links() {
        let mut status = TunnelStatus {
            url: Some("https://test.trycloudflare.com".to_string()),
            port: Some(17001),
            ..Default::default()
        };
        assert!(status.playback_base(17001).is_err());
        status.running = true;
        assert!(status.playback_base(17001).is_err());
        status.ready = true;
        assert!(status.playback_base(17001).is_ok());
        assert!(status.playback_base(17002).is_err());
    }

    #[test]
    fn stopped_state_clears_all_playback_fields() {
        let mut inner = TunnelInner::default();
        inner.status.running = true;
        inner.status.ready = true;
        inner.status.url = Some("https://test.trycloudflare.com".into());
        inner.status.port = Some(17001);
        inner.clear(Some("exited".into()));
        assert!(!inner.status.running && !inner.status.ready);
        assert!(inner.status.url.is_none() && inner.status.port.is_none());
    }

    #[test]
    fn diagnostic_tail_is_bounded() {
        let mut tail = VecDeque::new();
        for _ in 0..100 {
            push_log(&mut tail, &"字".repeat(1024));
        }
        assert_eq!(tail.len(), LOG_LINES);
        assert_eq!(tail.back().unwrap().chars().count(), 512);
    }

    // 使用本地假进程测试真实管道、退出码和并发启停；不会创建公网隧道。
    #[cfg(windows)]
    mod processes {
        use super::*;

        fn command(script: &str) -> Command {
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
            command
        }

        fn ready_script(host: &str) -> String {
            format!(
                "[Console]::Error.WriteLine('https://{}.trycloudflare.com'); \
                 [Console]::Error.WriteLine('INF Registered tunnel connection'); \
                 Start-Sleep -Seconds 30",
                host
            )
        }

        fn recorder() -> (StatusEmitter, Arc<Mutex<Vec<TunnelStatus>>>) {
            let events = Arc::new(Mutex::new(Vec::new()));
            let captured = events.clone();
            let emit: StatusEmitter = Arc::new(move |status| {
                captured.lock().unwrap().push(status);
            });
            (emit, events)
        }

        async fn wait_for(manager: &TunnelManager, predicate: impl Fn(&TunnelStatus) -> bool) {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if predicate(&manager.status()) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("fake tunnel did not reach expected state");
        }

        async fn stop(manager: &TunnelManager, emit: &StatusEmitter) -> AppResult<TunnelStatus> {
            let _operation = manager.operation.lock().await;
            manager.terminate(emit).await
        }

        #[tokio::test]
        async fn url_alone_is_not_ready_and_connection_timeout_reaps_process() {
            let manager = TunnelManager::new();
            let (emit, events) = recorder();
            manager
                .launch(
                    command(
                        "[Console]::Error.WriteLine('https://pending.trycloudflare.com'); \
                 [Console]::Error.WriteLine('connection blocked'); Start-Sleep -Seconds 30",
                    ),
                    17001,
                    Duration::from_secs(3),
                    emit,
                )
                .await
                .unwrap();
            wait_for(&manager, |status| !status.running).await;
            let status = manager.status();
            assert!(!status.ready && status.url.is_none());
            let error = status.error.unwrap();
            assert!(error.contains("超时") && error.contains("connection blocked"));
            assert!(events.lock().unwrap().iter().all(|status| !status.ready));
            assert!(manager.inner.lock().unwrap().child.is_none());
        }

        #[tokio::test]
        async fn unexpected_exit_clears_url_and_reports_exit_code_and_logs() {
            let manager = TunnelManager::new();
            let (emit, _) = recorder();
            manager
                .launch(
                    command(
                        "[Console]::Error.WriteLine('https://exit-test.trycloudflare.com'); \
                 [Console]::Error.WriteLine('INF Registered tunnel connection'); \
                 Start-Sleep -Milliseconds 700; \
                 [Console]::Error.WriteLine('simulated failure'); exit 17",
                    ),
                    17001,
                    Duration::from_secs(10),
                    emit,
                )
                .await
                .unwrap();
            wait_for(&manager, |status| status.ready).await;
            wait_for(&manager, |status| !status.running).await;
            let status = manager.status();
            assert!(status.url.is_none() && !status.ready);
            let error = status.error.unwrap();
            assert!(error.contains("17") && error.contains("simulated failure"));
            assert!(manager.inner.lock().unwrap().child.is_none());
        }

        #[tokio::test]
        async fn disconnect_hides_url_and_reconnect_restores_it() {
            let manager = TunnelManager::new();
            let (emit, _) = recorder();
            manager
                .launch(
                    command(
                        "[Console]::Error.WriteLine('https://reconnect.trycloudflare.com'); \
                 [Console]::Error.WriteLine('INF Registered tunnel connection'); \
                 Start-Sleep -Milliseconds 700; \
                 [Console]::Error.WriteLine('ERR Connection terminated'); \
                 Start-Sleep -Milliseconds 700; \
                 [Console]::Error.WriteLine('INF Registered tunnel connection'); \
                 Start-Sleep -Seconds 30",
                    ),
                    17001,
                    Duration::from_secs(10),
                    emit.clone(),
                )
                .await
                .unwrap();
            wait_for(&manager, |status| status.ready).await;
            wait_for(&manager, |status| status.running && !status.ready).await;
            assert!(manager.status().url.is_none());
            wait_for(&manager, |status| status.ready).await;
            assert!(manager.status().error.is_none());
            stop(&manager, &emit).await.unwrap();
        }

        #[tokio::test]
        async fn concurrent_starts_and_stop_do_not_leave_a_process_or_late_events() {
            let manager = TunnelManager::new();
            let (emit, events) = recorder();
            let (first, second, stopped) = tokio::join!(
                manager.launch(
                    command(&ready_script("first")),
                    17001,
                    START_TIMEOUT,
                    emit.clone()
                ),
                manager.launch(
                    command(&ready_script("second")),
                    17002,
                    START_TIMEOUT,
                    emit.clone()
                ),
                stop(&manager, &emit),
            );
            first.unwrap();
            second.unwrap();
            stopped.unwrap();
            let revision = manager.status().revision;
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!manager.status().running && manager.status().url.is_none());
            assert!(manager.inner.lock().unwrap().child.is_none());
            assert_eq!(manager.status().revision, revision);
            let events = events.lock().unwrap();
            assert!(events
                .windows(2)
                .all(|pair| pair[0].revision < pair[1].revision));
        }

        #[tokio::test]
        async fn spawn_failure_clears_previous_state_and_shutdown_prevents_restart() {
            let manager = TunnelManager::new();
            let (emit, _) = recorder();
            manager
                .launch(
                    command(&ready_script("first")),
                    17001,
                    START_TIMEOUT,
                    emit.clone(),
                )
                .await
                .unwrap();
            wait_for(&manager, |status| status.ready).await;
            let temp = tempfile::tempdir().unwrap();
            assert!(manager
                .launch(
                    Command::new(temp.path().join("missing.exe")),
                    17001,
                    START_TIMEOUT,
                    emit.clone()
                )
                .await
                .is_err());
            assert!(!manager.status().running && manager.status().url.is_none());
            assert!(manager.status().error.is_some());
            manager.stop_detached();
            assert!(manager
                .launch(
                    command(&ready_script("blocked")),
                    17001,
                    START_TIMEOUT,
                    emit
                )
                .await
                .is_err());
            assert!(manager.inner.lock().unwrap().child.is_none());
        }
    }
}
