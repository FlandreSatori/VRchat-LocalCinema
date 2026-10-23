//! EasyTier 组网控制：命令行参数生成、easytier-core 进程管理与房间信息读取。
//!
//! 参数名称与含义依据 EasyTier 官方「完整配置选项」文档：
//! <https://easytier.cn/guide/network/configurations.html>

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::settings::EasytierConfig;

/// easytier-core 的 RPC 门户，easytier-cli 通过它读取运行状态
pub const RPC_PORTAL: &str = "127.0.0.1:15888";

/// 运行 easytier-core 的计划任务名称（以最高权限运行，注册一次后启停均无需 UAC）
pub const EASYTIER_TASK_NAME: &str = "LocalCinemaEasyTier";

/// 清理残留 easytier-core 的计划任务名称（`schtasks /End` 只终止任务动作进程本身、
/// 不递归终止子进程，残留的提权 core 需通过本任务以最高权限强杀，避免弹 UAC）
pub const EASYTIER_TASK_NAME_STOP: &str = "LocalCinemaEasyTierStop";

/// 支持的协议类型
pub const SUPPORTED_PROTOCOLS: [&str; 5] = ["tcp", "udp", "ws", "wss", "quic"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EasytierStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub rpc_portal: String,
    pub error: Option<String>,
    /// 当前实际使用的启动参数（便于排查）
    pub args: Vec<String>,
}

impl Default for EasytierStatus {
    fn default() -> Self {
        Self {
            running: false,
            pid: None,
            rpc_portal: RPC_PORTAL.to_string(),
            error: None,
            args: Vec::new(),
        }
    }
}

/// 房间成员信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub hostname: String,
    pub ipv4: String,
    pub latency_ms: Option<f64>,
    pub loss: Option<f64>,
    pub cost: String,
    pub nat: String,
    pub version: String,
}

/// 房间信息读取结果（含原始输出，便于排查解析失败）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomInfo {
    pub peers: Vec<PeerInfo>,
    pub raw: String,
    pub error: Option<String>,
}

/// 每个协议对应的监听端口偏移，避免多协议监听时端口冲突
fn protocol_port_offset(proto: &str) -> Option<u16> {
    match proto {
        "tcp" | "udp" => Some(0),
        "ws" => Some(1),
        "wss" => Some(2),
        "quic" => Some(3),
        _ => None,
    }
}

/// 根据配置生成 easytier-core 启动参数
pub fn build_args(cfg: &EasytierConfig) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    let mut listeners: Vec<String> = Vec::new();
    for proto in &cfg.protocols {
        let p = proto.trim().to_ascii_lowercase();
        if let Some(offset) = protocol_port_offset(&p) {
            listeners.push(format!("{}://0.0.0.0:{}", p, cfg.listen_port.saturating_add(offset)));
        }
    }
    for l in listeners {
        args.push("--listeners".into());
        args.push(l);
    }

    if !cfg.peer_url.trim().is_empty() {
        for peer in cfg
            .peer_url
            .split(|c: char| c.is_whitespace() || c == ',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push("--peers".into());
            args.push(peer.to_string());
        }
    }

    if !cfg.room_name.trim().is_empty() {
        args.push("--network-name".into());
        args.push(cfg.room_name.trim().to_string());
    }
    if !cfg.room_password.trim().is_empty() {
        args.push("--network-secret".into());
        args.push(cfg.room_password.trim().to_string());
    }
    if !cfg.member_name.trim().is_empty() {
        args.push("--hostname".into());
        args.push(cfg.member_name.trim().to_string());
    }
    if cfg.dhcp {
        // DHCP 自动分配虚拟 IP（对齐 EasytierGame 默认行为），此时忽略手动 ipv4
        args.push("-d".into());
    } else if !cfg.ipv4.trim().is_empty() {
        args.push("--ipv4".into());
        args.push(cfg.ipv4.trim().to_string());
    } else {
        // 兜底：关闭 DHCP 且未填写虚拟 IP 时节点会没有虚拟 IP（媒体共享不可用），强制走 DHCP
        tracing::info!("未开启 DHCP 且未填写虚拟 IPv4，自动回退为 DHCP 分配");
        args.push("-d".into());
    }
    if !cfg.ipv6.trim().is_empty() {
        args.push("--ipv6".into());
        args.push(cfg.ipv6.trim().to_string());
    }
    if cfg.latency_first {
        // 低延迟模式（对齐 EasytierGame「使用低延迟模式」）
        args.push("--latency-first".into());
        args.push("true".into());
    }
    let compression = cfg.compression.trim().to_ascii_lowercase();
    if !compression.is_empty() && compression != "none" {
        args.push("--compression".into());
        args.push(compression);
    }
    // KCP 与 QUIC 代理互斥（对齐 EasytierGame）；两者同时开启时仅启用 KCP
    if cfg.kcp_proxy {
        args.push("--enable-kcp-proxy".into());
        args.push("true".into());
    } else if cfg.quic_proxy {
        args.push("--enable-quic-proxy".into());
        args.push("true".into());
    }
    if cfg.kcp_input {
        args.push("--disable-kcp-input".into());
        args.push("true".into());
    }
    if cfg.quic_input {
        args.push("--disable-quic-input".into());
        args.push("true".into());
    }
    if !cfg.upnp {
        // 关闭 UPnP 端口映射（EasyTier 2.6.2+，与 EasytierGame 行为一致）
        args.push("--disable-upnp".into());
        args.push("true".into());
    }
    if cfg.multi_thread {
        args.push("--multi-thread".into());
    }
    args.push("--rpc-portal".into());
    args.push(RPC_PORTAL.into());
    args.push("--console-log-level".into());
    args.push("info".into());

    args
}

pub struct EasytierManager {
    child: Mutex<Option<tokio::process::Child>>,
    status: RwLock<EasytierStatus>,
    /// 本机 EasyTier 虚拟 IPv4（用于组网时的局域网地址）
    virtual_ip: RwLock<Option<String>>,
    /// 数据目录（启动脚本 run.ps1 的写入位置）
    data_dir: PathBuf,
}

impl Default for EasytierManager {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

impl EasytierManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            child: Mutex::new(None),
            status: RwLock::new(EasytierStatus::default()),
            virtual_ip: RwLock::new(None),
            data_dir,
        }
    }

    pub fn status(&self) -> EasytierStatus {
        self.status.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 设置本机虚拟 IP（EasyTier 停止时应清空）
    pub fn set_virtual_ip(&self, ip: Option<String>) {
        *self.virtual_ip.write().unwrap_or_else(|e| e.into_inner()) = ip;
    }

    pub fn cached_virtual_ip(&self) -> Option<String> {
        self.virtual_ip.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut EasytierStatus)) {
        let mut guard = self.status.write().unwrap_or_else(|e| e.into_inner());
        f(&mut guard);
    }

    /// 启动 easytier-core。
    ///
    /// 创建虚拟网卡（Wintun）需要管理员权限，因此通过 Windows 计划任务以最高权限
    /// 运行：任务只注册一次（首次 UAC），之后每次启动/停止均通过 `schtasks /Run`
    /// 与 `schtasks /End` 完成，无需再次授权。任务动作固定指向 run-hidden.vbs（由其
    /// 隐藏窗口运行同目录 run.ps1），参数变化只需重写脚本内容，不会触发重新注册。
    pub async fn start(&self, core: &Path, args: Vec<String>) -> AppResult<EasytierStatus> {
        self.stop().await;

        // 生成启动脚本（UTF-8 BOM，PowerShell 5.1 可正确解析中文参数）
        let ps1 = run_ps1_path(&self.data_dir);
        write_run_ps1(&ps1, core, &args)?;

        // 生成 wscript 隐藏窗口包装脚本：任务动作指向 vbs，运行期完全不显示 PowerShell 窗口
        let vbs = run_hidden_vbs_path(&self.data_dir);
        write_run_hidden_vbs(&vbs)?;
        write_stop_hidden_vbs(&stop_hidden_vbs_path(&self.data_dir))?;

        ensure_task_registered(&self.data_dir).await?;
        // 注册完成后若仍有旧 core 残留（如升级前残留、停止任务未注册时的遗留），
        // 此时停止任务已可用，先清理再启动，避免新旧实例并存
        if !core_processes().is_empty() {
            tracing::warn!("启动前检测到 easytier-core 残留，通过停止任务清理");
            let _ = task_stop().await;
            wait_processes_gone(10).await;
        }
        task_run().await?;

        // 等待 core 进程拉起
        let mut pid = None;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if let Some(proc_pid) = core_processes().first().map(|p| p.pid) {
                pid = Some(proc_pid);
                break;
            }
        }
        let pid = pid.ok_or_else(|| {
            AppError::Msg("计划任务已触发但未检测到 easytier-core 进程，请查看 easytier-core 日志".to_string())
        })?;

        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.set_status(|s| {
            s.running = true;
            s.pid = Some(pid);
            s.error = None;
            s.args = args.clone();
        });
        tracing::info!("easytier-core 已通过计划任务启动，PID={}", pid);
        Ok(self.status())
    }

    /// 停止 easytier-core（手动停止：计划任务终止 + 停止任务免 UAC 清理残留）。
    pub async fn stop(&self) {
        {
            let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
        self.set_status(|s| {
            s.running = false;
            s.pid = None;
            s.error = None;
        });
        self.set_virtual_ip(None);

        // 停止计划任务（无需 UAC；任务不存在时忽略错误）
        task_end().await;
        wait_processes_gone(6).await;
        // /End 只终止任务动作进程（wscript/powershell），不递归终止子进程；
        // 残留的提权 core 通过停止任务强杀（免 UAC、无窗口）
        if !core_processes().is_empty() {
            tracing::warn!("计划任务 /End 后 easytier-core 残留，触发停止任务清理");
            if let Err(e) = task_stop().await {
                tracing::warn!("触发停止任务失败: {}", e);
            }
            wait_processes_gone(10).await;
        }
        if !core_processes().is_empty() {
            self.set_status(|s| {
                s.error = Some(
                    "easytier-core 仍在后台运行；若停止任务未注册，请先启动一次组网以完成升级注册"
                        .to_string(),
                )
            });
        }
    }

    /// 停止 easytier-core（应用退出场景：计划任务终止无需 UAC，同步快速执行）。
    pub fn stop_detached(&self) {
        {
            let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
        let _ = tauri::async_runtime::block_on(task_end());
        if !core_processes().is_empty() {
            // 残留提权进程：通过停止任务免 UAC 清理，异步执行不阻塞退出
            tauri::async_runtime::spawn(async {
                let _ = task_stop().await;
            });
        }
        self.set_status(|s| {
            s.running = false;
            s.pid = None;
        });
        self.set_virtual_ip(None);
    }

    /// 刷新运行状态：计划任务模式下按进程存在性判断（同时可检测意外退出）。
    pub fn refresh(&self) {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            // 兼容旧本地子进程模式
            let exited = match guard.as_mut().unwrap().try_wait() {
                Ok(Some(status)) => Some(format!("easytier-core 已退出（{:?}）", status.code())),
                Ok(None) => None,
                Err(e) => Some(format!("检测 easytier-core 状态失败: {}", e)),
            };
            if let Some(msg) = exited {
                *guard = None;
                drop(guard);
                self.set_status(|s| {
                    s.running = false;
                    s.pid = None;
                    s.error = Some(msg);
                });
            }
            return;
        }
        drop(guard);

        let was_running = self.status.read().unwrap_or_else(|e| e.into_inner()).running;
        match core_processes().first() {
            Some(proc) => {
                self.set_status(|s| {
                    s.running = true;
                    s.pid = Some(proc.pid);
                });
            }
            None => {
                if was_running {
                    self.set_status(|s| {
                        s.running = false;
                        s.pid = None;
                        s.error = Some("easytier-core 已退出".to_string());
                    });
                }
            }
        }
    }
}

/// PowerShell 单引号字符串转义（内部单引号翻倍）
pub(crate) fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 提权子进程写回失败原因的结果文件（UTF-8）。
/// 由提权后的脚本写入，外层脚本读取后以十六进制回传，避免 PowerShell 以本地
/// 代码页（GBK）写 stdout、Rust 再按 UTF-8 解码导致中文乱码。
pub(crate) fn elevation_result_path(data_dir: &Path) -> PathBuf {
    data_dir.join("elevate-result.txt")
}

/// 以管理员权限运行 PowerShell 子进程的包装脚本。
///
/// 输出协议为**纯 ASCII**（`ERR|KIND|HEX` 中 HEX 是 UTF-8 字节的十六进制），
/// 因此不受控制台代码页影响：
/// - `OK`
/// - `ERR|ELEVATE|<hex>`：提权启动失败（UAC 被取消/拒绝等）
/// - `ERR|EXIT|<code>[|<hex>]`：子进程非零退出，hex 为结果文件中的原因
/// - `ERR|MSG|<hex>`：子进程退出码为 0，但结果文件给出了失败原因
pub(crate) fn elevation_wrapper(child_args: &[String], result_file: &Path) -> String {
    let template = r#"$ErrorActionPreference = 'Stop'
$resultFile = '@@RESULT@@'
try {
  Remove-Item -LiteralPath $resultFile -Force -ErrorAction SilentlyContinue
  $p = Start-Process -FilePath 'powershell' -ArgumentList @(@@ARGS@@) -Verb RunAs -WindowStyle Hidden -Wait -PassThru
  $code = 0
  if ($null -ne $p) { $code = $p.ExitCode }
  $detail = ''
  if (Test-Path -LiteralPath $resultFile) { $detail = [IO.File]::ReadAllText($resultFile, [Text.Encoding]::UTF8).Trim() }
  $hx = ''
  if ($detail.Length -gt 0) { $hx = [BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($detail)).Replace('-','') }
  if ($code -ne 0) {
    if ($hx.Length -gt 0) { Write-Output ('ERR|EXIT|' + $code + '|' + $hx) } else { Write-Output ('ERR|EXIT|' + $code) }
  } elseif ($hx.Length -gt 0) {
    Write-Output ('ERR|MSG|' + $hx)
  } else {
    Write-Output 'OK'
  }
} catch {
  $hx = [BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($_.Exception.Message)).Replace('-','')
  Write-Output ('ERR|ELEVATE|' + $hx)
  exit 1
}
"#;
    // Start-Process 的 -ArgumentList 用逗号分隔的 @(...) 数组传参，逐项单引号转义
    let args = child_args
        .iter()
        .map(|a| ps_quote(a))
        .collect::<Vec<_>>()
        .join(",");
    template
        .replace("@@RESULT@@", &result_file.to_string_lossy().replace('\'', "''"))
        .replace("@@ARGS@@", &args)
}

/// 十六进制串还原为文本（非法十六进制返回空串）
fn decode_hex_detail(hex_str: &str) -> String {
    match hex::decode(hex_str.trim()) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(_) => String::new(),
    }
}

/// 是否为用户主动取消授权（中英文系统提示不一致，做多重匹配）
fn is_user_cancel(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("取消")
        || lower.contains("cancel")
        || lower.contains("1223")
        || lower.contains("0x800704c7")
}

/// 解析 PowerShell 脚本输出（OK / ERR 协议），返回可读的中文错误
fn parse_ps_output(stdout: &str) -> AppResult<()> {
    for line in stdout.lines() {
        let line = line.trim();
        if line == "OK" {
            return Ok(());
        }
        let Some(rest) = line.strip_prefix("ERR|") else {
            continue;
        };
        let mut parts = rest.splitn(3, '|');
        let kind = parts.next().unwrap_or("").trim();
        match kind {
            "ELEVATE" => {
                let detail = decode_hex_detail(parts.next().unwrap_or(""));
                return Err(AppError::Msg(if is_user_cancel(&detail) {
                    "已取消管理员授权，无法继续（该操作需要管理员权限）".to_string()
                } else if detail.is_empty() {
                    "无法以管理员权限启动（未获取到具体原因）；若始终没有弹出 UAC 授权窗口，请检查系统 UAC 设置，或以管理员身份运行本程序".to_string()
                } else {
                    format!(
                        "无法完成管理员授权：{}；若始终没有弹出 UAC 授权窗口，请检查系统 UAC 设置，或以管理员身份运行本程序",
                        detail
                    )
                }));
            }
            "EXIT" => {
                let code = parts.next().unwrap_or("").trim();
                let detail = decode_hex_detail(parts.next().unwrap_or(""));
                return Err(AppError::Msg(if detail.is_empty() {
                    format!("管理员授权执行失败（退出码 {}），未获取到具体原因", code)
                } else {
                    format!("管理员授权执行失败：{}（退出码 {}）", detail, code)
                }));
            }
            "MSG" => {
                let detail = decode_hex_detail(parts.next().unwrap_or(""));
                return Err(AppError::Msg(format!("管理员授权执行失败：{}", detail)));
            }
            "CANCEL" => {
                return Err(AppError::Msg(
                    "已取消管理员授权，无法继续（该操作需要管理员权限）".to_string(),
                ));
            }
            // 兼容旧格式 ERR|文本（无类型前缀）
            _ => {
                let msg = rest.trim();
                return Err(AppError::Msg(if is_user_cancel(msg) {
                    "已取消管理员授权，无法继续（该操作需要管理员权限）".to_string()
                } else {
                    format!("管理员授权执行失败: {}", msg)
                }));
            }
        }
    }
    Err(AppError::Msg(
        "未收到管理员授权结果：若桌面上没有出现 UAC 授权窗口，请按 Alt+Tab 或查看任务栏（窗口可能被主界面遮挡）；也可以改用「以管理员身份运行」后重试"
            .to_string(),
    ))
}

/// 运行外层 PowerShell 脚本并解析 OK/ERR 输出
pub(crate) async fn run_powershell_script(script: &str) -> AppResult<()> {
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| AppError::Msg(format!("执行 PowerShell 失败: {}", e)))?;
    parse_ps_output(&String::from_utf8_lossy(&output.stdout))
}

/// run.ps1 路径（数据目录下 easytier/run.ps1，参数变化仅重写此文件）
fn run_ps1_path(data_dir: &Path) -> PathBuf {
    data_dir.join("easytier").join("run.ps1")
}

/// 写入 UTF-8 BOM 文件（PowerShell 5.1 对无 BOM 的 UTF-8 会按本地代码页解析，中文会乱码）
pub(crate) fn write_utf8_bom(path: &Path, content: &str) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(content.as_bytes());
    std::fs::write(path, bytes)?;
    Ok(())
}

/// 生成 easytier-core 启动脚本（参数全部单引号包裹，含空格/中文均安全）
fn write_run_ps1(path: &Path, core: &Path, args: &[String]) -> AppResult<()> {
    let mut line = String::from("& ");
    line.push_str(&ps_quote(&core.to_string_lossy()));
    for a in args {
        line.push(' ');
        line.push_str(&ps_quote(a));
    }
    line.push('\n');
    write_utf8_bom(path, &format!("# 由 VRchat-LocalCinema 自动生成，请勿手动修改\n{}\n", line))
}

/// run-hidden.vbs 路径（数据目录下 easytier/run-hidden.vbs）
fn run_hidden_vbs_path(data_dir: &Path) -> PathBuf {
    data_dir.join("easytier").join("run-hidden.vbs")
}

/// stop-hidden.vbs 路径（数据目录下 easytier/stop-hidden.vbs）
fn stop_hidden_vbs_path(data_dir: &Path) -> PathBuf {
    data_dir.join("easytier").join("stop-hidden.vbs")
}

/// wscript 隐藏窗口包装脚本内容。
/// wscript 为 GUI 程序（无控制台），`Run(cmd, 0, True)` 中 0 = SW_HIDE（PowerShell 窗口
/// 从创建起即隐藏，无闪现），True = 等待 run.ps1 退出（保持任务进程树完整，便于
/// `schtasks /End` 一并终止）。内容保持纯 ASCII，run.ps1 路径从脚本自身位置推导，
/// 避免在脚本内嵌入非 ASCII 路径导致 wscript 编码问题。
const RUN_HIDDEN_VBS: &str = r#"Set sh = CreateObject("WScript.Shell")
folder = Left(WScript.ScriptFullName, InStrRev(WScript.ScriptFullName, "\") - 1)
sh.Run "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File """ & folder & "\run.ps1""", 0, True
"#;

/// 写入 run-hidden.vbs（纯 ASCII，无需 BOM）
fn write_run_hidden_vbs(path: &Path) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, RUN_HIDDEN_VBS)?;
    Ok(())
}

/// wscript 隐藏窗口包装的停止脚本：以最高权限强杀 easytier-core。
/// 由 LocalCinemaEasyTierStop 计划任务触发（`schtasks /Run` 无需 UAC），
/// 用于清理 `/End` 无法递归终止的残留 core 进程，全程无窗口。
const STOP_HIDDEN_VBS: &str = r#"Set sh = CreateObject("WScript.Shell")
sh.Run "powershell.exe -NoProfile -NonInteractive -Command ""Stop-Process -Name 'easytier-core' -Force -ErrorAction SilentlyContinue""", 0, True
"#;

/// 写入 stop-hidden.vbs（纯 ASCII，无需 BOM）
fn write_stop_hidden_vbs(path: &Path) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, STOP_HIDDEN_VBS)?;
    Ok(())
}

/// 运行 powershell 并捕获输出（普通权限）
async fn powershell_output(script: &str) -> AppResult<std::process::Output> {
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    cmd.output()
        .await
        .map_err(|e| AppError::Msg(format!("执行 PowerShell 失败: {}", e)))
}

/// 检查运行/停止两个计划任务是否存在且动作指向当前 vbs
/// 返回 Some(true)=匹配 / Some(false)=需重新注册 / None=查询失败
async fn task_action_matches(data_dir: &Path) -> Option<bool> {
    let run_vbs = run_hidden_vbs_path(data_dir);
    let stop_vbs = stop_hidden_vbs_path(data_dir);
    let script = format!(
        "$r = Get-ScheduledTask -TaskName '{}' -ErrorAction SilentlyContinue; $s = Get-ScheduledTask -TaskName '{}' -ErrorAction SilentlyContinue; if ($r -and $s) {{ $ra = @($r.Actions)[0]; $sa = @($s.Actions)[0]; if ($null -ne $ra -and $null -ne $sa -and $ra.Arguments -and $sa.Arguments -and $ra.Arguments.Contains({}) -and $sa.Arguments.Contains({})) {{ Write-Output 'MATCH' }} else {{ Write-Output 'STALE' }} }} else {{ Write-Output 'MISS' }}",
        EASYTIER_TASK_NAME,
        EASYTIER_TASK_NAME_STOP,
        ps_quote(&run_vbs.to_string_lossy()),
        ps_quote(&stop_vbs.to_string_lossy())
    );
    let output = powershell_output(&script).await.ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        match line.trim() {
            "MATCH" => return Some(true),
            "STALE" | "MISS" => return Some(false),
            _ => {}
        }
    }
    None
}

/// 以管理员权限注册/覆盖运行与停止两个计划任务（首次 UAC 一次，之后不再需要）
async fn register_task_elevated(data_dir: &Path) -> AppResult<()> {
    // 注册脚本通过 -EncodedCommand 内联传入提权进程，不落地到用户可写目录，
    // 避免「先写文件、再提权执行」之间的替换窗口（TOCTOU / 本地提权）。
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n$rv = {}\n$sv = {}\n$u = \"$env:USERDOMAIN\\$env:USERNAME\"\n$principal = New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Highest\n$set = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew\n$a1 = New-ScheduledTaskAction -Execute 'wscript.exe' -Argument ('//B \"' + $rv + '\"')\nRegister-ScheduledTask -TaskName '{}' -Action $a1 -Principal $principal -Settings $set -Force | Out-Null\n$a2 = New-ScheduledTaskAction -Execute 'wscript.exe' -Argument ('//B \"' + $sv + '\"')\nRegister-ScheduledTask -TaskName '{}' -Action $a2 -Principal $principal -Settings $set -Force | Out-Null\nWrite-Output 'OK'\n",
        ps_quote(&run_hidden_vbs_path(data_dir).to_string_lossy()),
        ps_quote(&stop_hidden_vbs_path(data_dir).to_string_lossy()),
        EASYTIER_TASK_NAME,
        EASYTIER_TASK_NAME_STOP
    );
    let encoded = encode_powershell_command(&script);

    let wrapper = elevation_wrapper(
        &[
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-EncodedCommand".to_string(),
            encoded,
        ],
        &elevation_result_path(data_dir),
    );
    run_powershell_script(&wrapper).await
}

/// 将脚本编码为 PowerShell `-EncodedCommand` 所需的 Base64（UTF-16LE，含 BOM 无关）。
/// 内联传参可完全规避引号/编码问题，且无需落地脚本文件。
fn encode_powershell_command(script: &str) -> String {
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    base64_encode(&utf16)
}

/// 标准 Base64 编码（无外部依赖）
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 确保运行/停止两个计划任务已注册且动作指向当前 vbs
async fn ensure_task_registered(data_dir: &Path) -> AppResult<()> {
    match task_action_matches(data_dir).await {
        Some(true) => return Ok(()),
        Some(false) => tracing::info!("计划任务不存在或动作过期，重新注册（可能需要管理员授权）"),
        None => tracing::info!("无法确认计划任务状态，尝试重新注册"),
    }
    register_task_elevated(data_dir).await
}

/// 触发计划任务启动 easytier-core（无需 UAC）
async fn task_run() -> AppResult<()> {
    let out = schtasks(&["/Run", "/TN", EASYTIER_TASK_NAME]).await?;
    if !out.status.success() {
        return Err(AppError::Msg(
            "触发计划任务失败，请重新启动组网以重新注册任务".to_string(),
        ));
    }
    Ok(())
}

/// 终止计划任务（无需 UAC；任务不存在时忽略失败）
async fn task_end() {
    let _ = schtasks(&["/End", "/TN", EASYTIER_TASK_NAME]).await;
}

/// 触发停止任务强杀残留 easytier-core（免 UAC、无窗口；任务未注册时报错）
async fn task_stop() -> AppResult<()> {
    let out = schtasks(&["/Run", "/TN", EASYTIER_TASK_NAME_STOP]).await?;
    if !out.status.success() {
        return Err(AppError::Msg("停止任务未注册或触发失败".to_string()));
    }
    Ok(())
}

/// 调用 schtasks（仅以退出码判断结果，规避本地化输出编码问题）
async fn schtasks(args: &[&str]) -> AppResult<std::process::Output> {
    let mut cmd = tokio::process::Command::new("schtasks");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    cmd.output()
        .await
        .map_err(|e| AppError::Msg(format!("执行 schtasks 失败: {}", e)))
}

struct CoreProc {
    pid: u32,
}

/// 枚举 easytier-core 进程（tasklist 按进程名过滤，CSV 第二列为 PID）
fn core_processes() -> Vec<CoreProc> {
    let mut cmd = std::process::Command::new("tasklist");
    cmd.args(["/FI", "IMAGENAME eq easytier-core.exe", "/FO", "CSV", "/NH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut result = Vec::new();
    if let Ok(out) = cmd.output() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() >= 2 {
                if let Ok(pid) = cols[1].trim_matches('"').trim().parse::<u32>() {
                    result.push(CoreProc { pid });
                }
            }
        }
    }
    result
}

/// 轮询等待 easytier-core 进程全部退出
async fn wait_processes_gone(rounds: usize) {
    for _ in 0..rounds {
        if core_processes().is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// 按 2 个以上空格切分表格列
fn split_columns(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut current = String::new();
    let mut spaces = 0usize;
    for ch in line.chars() {
        if ch == ' ' {
            spaces += 1;
            continue;
        }
        if spaces >= 2 && !current.is_empty() {
            cols.push(current.trim().to_string());
            current = String::new();
        } else if spaces > 0 {
            current.push(' ');
        }
        spaces = 0;
        current.push(ch);
    }
    if !current.trim().is_empty() {
        cols.push(current.trim().to_string());
    }
    cols
}

fn parse_num(s: &str) -> Option<f64> {
    let cleaned: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if cleaned.is_empty() || cleaned == "-" {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// 解析 `easytier-cli peer` 的表格输出
pub fn parse_peers(text: &str) -> Vec<PeerInfo> {
    let mut header: Option<Vec<String>> = None;
    let mut peers = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if header.is_none() {
            if trimmed.to_ascii_lowercase().contains("hostname") {
                header = Some(
                    split_columns(trimmed)
                        .into_iter()
                        .map(|c| c.to_ascii_lowercase())
                        .collect(),
                );
            }
            continue;
        }
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '+' || c == '=' || c == '|' || c.is_whitespace())
        {
            continue;
        }
        let Some(h) = header.as_ref() else { continue };
        let cols = split_columns(trimmed);
        if cols.len() < 2 {
            continue;
        }
        let find = |key: &str| -> Option<String> {
            h.iter()
                .position(|c| c.contains(key))
                .and_then(|i| cols.get(i).cloned())
        };
        let hostname = find("hostname").unwrap_or_default();
        let ipv4 = find("ipv4").unwrap_or_default();
        if hostname.is_empty() && ipv4.is_empty() {
            continue;
        }
        peers.push(PeerInfo {
            hostname,
            ipv4,
            latency_ms: find("lat").as_deref().and_then(parse_num),
            loss: find("loss").as_deref().and_then(parse_num),
            cost: find("cost").unwrap_or_default(),
            nat: find("nat").unwrap_or_default(),
            version: find("version").unwrap_or_default(),
        });
    }
    peers
}

async fn run_cli_args(cli: &Path, cli_args: &[&str]) -> AppResult<String> {
    let mut cmd = tokio::process::Command::new(cli);
    cmd.arg("--rpc-portal")
        .arg(RPC_PORTAL)
        .args(cli_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output())
        .await
        .map_err(|_| AppError::Msg("easytier-cli 执行超时".to_string()))?
        .map_err(|e| AppError::Msg(format!("无法运行 easytier-cli: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Err(AppError::Msg(format!(
        "easytier-cli {} 执行失败：{}",
        cli_args.join(" "),
        stderr.lines().next().unwrap_or("").trim()
    )))
}

async fn run_cli(cli: &Path, subcommand: &str) -> AppResult<String> {
    run_cli_args(cli, &[subcommand]).await
}

/// 解析 `easytier-cli -o json peer list` 输出（对齐 EasytierGame 的成员读取方式）。
/// 解析失败（旧版 CLI 不支持该参数）时返回 None，由调用方回退到表格解析。
fn parse_peers_json(text: &str) -> Option<Vec<PeerInfo>> {
    let arr = serde_json::from_str::<Vec<serde_json::Value>>(text.trim()).ok()?;
    let mut peers = Vec::new();
    for v in arr {
        let get_str = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
        };
        let get_f64 = |k: &str| -> Option<f64> {
            match v.get(k) {
                Some(serde_json::Value::Number(n)) => n.as_f64(),
                Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
        };
        let hostname = get_str("hostname").unwrap_or_default();
        let ipv4 = get_str("ipv4").unwrap_or_default();
        if hostname.is_empty() && ipv4.is_empty() {
            continue;
        }
        // 对齐 EasytierGame：公共中转节点名显示为「服务器」
        let hostname = if hostname.to_ascii_lowercase().contains("publicserver") {
            hostname.replace("PublicServer", "服务器")
        } else {
            hostname
        };
        peers.push(PeerInfo {
            hostname,
            ipv4,
            latency_ms: get_f64("lat_ms"),
            // loss_rate 为小数（0.05 = 5%），统一换算为百分比
            loss: get_f64("loss_rate").map(|r| r * 100.0),
            cost: get_str("cost").unwrap_or_default(),
            nat: get_str("nat_type").unwrap_or_default(),
            version: get_str("version").unwrap_or_default(),
        });
    }
    Some(peers)
}

/// 解析 `easytier-cli route` 输出，返回 (虚拟 IP, 主机名) 列表（包含本机节点）。
/// route 表包含虚拟网络内的全部节点，即使未经直连也能拿到主机名。
pub fn parse_route_hosts(text: &str) -> Vec<(String, String)> {
    let mut header: Option<Vec<String>> = None;
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if header.is_none() {
            if trimmed.to_ascii_lowercase().contains("hostname") {
                header = Some(
                    split_columns(trimmed)
                        .into_iter()
                        .map(|c| c.to_ascii_lowercase())
                        .collect(),
                );
            }
            continue;
        }
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '+' || c == '=' || c == '|' || c.is_whitespace())
        {
            continue;
        }
        let Some(h) = header.as_ref() else { continue };
        let cols = split_columns(trimmed);
        let find = |key: &str| -> Option<String> {
            h.iter()
                .position(|c| c.contains(key))
                .and_then(|i| cols.get(i).cloned())
        };
        let raw_ip = find("ipv4").unwrap_or_default();
        let hostname = find("hostname").unwrap_or_default();
        let ip = raw_ip.split('/').next().unwrap_or("").trim().to_string();
        let is_ip = ip.split('.').count() == 4
            && ip
                .split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        if is_ip {
            out.push((ip, hostname));
        }
    }
    out
}

/// 读取房间成员信息。
/// 优先使用 `easytier-cli -o json peer list`（对齐 EasytierGame 的读取方式，结构化且稳定）；
/// 解析失败（旧版 CLI）时回退为 peer 表格 + route 主机名合并。
pub async fn fetch_room_info(cli: &Path) -> RoomInfo {
    if let Ok(json) = run_cli_args(cli, &["-o", "json", "peer", "list"]).await {
        if let Some(mut peers) = parse_peers_json(&json) {
            peers.sort_by(|a, b| a.ipv4.cmp(&b.ipv4));
            return RoomInfo {
                peers,
                raw: json,
                error: None,
            };
        }
    }

    let (peer_res, route_res) = tokio::join!(run_cli(cli, "peer"), run_cli(cli, "route"));
    let route_raw = route_res.unwrap_or_default();
    let hosts = parse_route_hosts(&route_raw);

    let peer_raw = match peer_res {
        Ok(raw) => raw,
        Err(e) => {
            // peer 失败时退化为仅列出 route 中的成员名
            return RoomInfo {
                peers: hosts
                    .into_iter()
                    .map(|(ip, hostname)| PeerInfo {
                        hostname,
                        ipv4: ip,
                        latency_ms: None,
                        loss: None,
                        cost: String::new(),
                        nat: String::new(),
                        version: String::new(),
                    })
                    .collect(),
                raw: route_raw,
                error: Some(e.to_string()),
            };
        }
    };

    let mut peers = parse_peers(&peer_raw);

    // 中转服务器连接没有虚拟 IP，不属于房间成员
    peers.retain(|p| p.ipv4.split('.').count() == 4);

    // 用 route 的主机名补全（peer 表在经中转等场景下可能拿不到主机名）
    for p in &mut peers {
        if p.hostname.is_empty() {
            let ip = p.ipv4.split('/').next().unwrap_or("").trim();
            if let Some((_, h)) = hosts.iter().find(|(hi, _)| hi == ip) {
                p.hostname = h.clone();
            }
        }
    }
    // route 中存在但 peer 表没有的成员（未建立连接/仅经中转可达）
    for (ip, hostname) in &hosts {
        let exists = peers
            .iter()
            .any(|p| p.ipv4.split('/').next().unwrap_or("").trim() == ip.as_str());
        if !exists {
            peers.push(PeerInfo {
                hostname: hostname.clone(),
                ipv4: ip.clone(),
                latency_ms: None,
                loss: None,
                cost: String::new(),
                nat: String::new(),
                version: String::new(),
            });
        }
    }
    peers.sort_by(|a, b| a.ipv4.cmp(&b.ipv4));

    let raw = format!("-- peer --\n{peer_raw}\n-- route --\n{route_raw}");
    RoomInfo {
        peers,
        raw,
        error: None,
    }
}

/// 重置网卡：清理本地 EasyTier 运行缓存，下次启动会重新创建虚拟网卡
pub fn clear_local_state(data_dir: &Path) -> AppResult<()> {
    let cache = data_dir.join("easytier");
    if cache.exists() {
        std::fs::remove_dir_all(&cache)?;
    }
    Ok(())
}

/// 日志过滤的黑名单片段（基于真实运行日志归纳的刷屏来源）
const LOG_DENY_PATTERNS: [&str; 11] = [
    // 连接尝试按所有网卡逐个 bind 刷屏（bind addr / bind addr fail ... AddrNotAvailable）
    "bind addr",
    // tracing span 生命周期收尾行（close time.busy=... time.idle=...）
    "time.busy=",
    "time.idle=",
    // STUN/NAT 探测细节
    "get txt record",
    "detect_nat_type",
    // ospf 路由内部事件（关键事件已有 CORE::INSTANCE 行）
    "global event received",
    "ospf sync",
    "new_initiator",
    // 外网客户端转发刷屏
    "send msg success",
    // ring 隧道内部连接
    "ring://",
    // Wintun 无害警告
    "Failed to find matching adapter name",
];

/// 行首时间戳判断（YYYY-MM-DDTHH...）；无时间戳视为上一行的续行（如启动 TOML 配置块）
fn log_line_has_timestamp(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 10
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
}

/// 过滤 easytier-core 日志，只保留有价值的内容：
/// - 保留：CORE::INSTANCE 关键事件（tun device ready / dhcp ip changed / new peer connection
///   added 等）、WinTun 适配器生命周期、启动配置块（Starting easytier from cli 及其续行）、
///   其余 WARN/ERROR（黑名单刷屏除外）
/// - 丢弃：其余 INFO 噪音（连接 bind 尝试、STUN 探测、span 生命周期、ospf 内部事件等）
pub fn filter_valuable_log(text: &str, max_lines: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        if LOG_DENY_PATTERNS.iter().any(|p| line.contains(p)) {
            continue;
        }
        let valuable = !log_line_has_timestamp(line)
            || line.contains("CORE::INSTANCE")
            || line.contains("WinTun:")
            || line.contains("Starting easytier from cli")
            || line.contains(" WARN ")
            || line.contains(" ERROR ");
        if valuable {
            kept.push(line);
        }
    }
    if kept.len() > max_lines {
        let start = kept.len() - max_lines;
        kept = kept.split_off(start);
    }
    kept.join("\n")
}

/// 读取日志文件末尾（最多 max_bytes 字节，UTF-8 lossy），丢弃可能被截断的首行
pub fn read_log_tail(path: &Path, max_bytes: u64) -> AppResult<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        if let Some(pos) = text.find('\n') {
            text.drain(..=pos);
        }
    }
    Ok(text)
}

/// 查找日志目录下最近修改的日志文件
pub fn newest_log_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// 卸载 EasyTier 虚拟网卡设备（Wintun，FriendlyName 以 et_ 开头）。
/// 重启 easytier-core 时会自动重新创建，即"重装网卡"。
/// 返回成功卸载的设备数量。
pub async fn uninstall_tun_adapter() -> AppResult<usize> {
    // EasyTier 通过 tun crate 创建 Wintun 适配器，设备名形如 et_xxxx_yyyy
    const SCRIPT: &str = r#"
$devs = Get-PnpDevice -Class Net -ErrorAction SilentlyContinue | Where-Object {
    $_.InstanceId -like 'ROOT\WINTUN*' -and
    ($_.FriendlyName -like 'et_*' -or $_.FriendlyName -like 'EasyTier*' -or $_.FriendlyName -like 'eton*')
}
if (-not $devs) { Write-Output 'NO_DEVICE'; exit 0 }
$devs | ForEach-Object {
    $null = & pnputil.exe /remove-device $_.InstanceId 2>&1 | Out-String
    Write-Output ("DEV|" + $_.InstanceId + "|" + $LASTEXITCODE)
}
"#;
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| AppError::Msg(format!("执行网卡卸载脚本失败: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.lines().any(|l| l.trim() == "NO_DEVICE") {
        tracing::info!("未找到 EasyTier 虚拟网卡设备，跳过卸载");
        return Ok(0);
    }

    let mut removed = 0usize;
    let mut denied = 0usize;
    for line in stdout.lines().filter(|l| l.starts_with("DEV|")) {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() >= 3 {
            if parts[2].trim() == "0" {
                removed += 1;
                tracing::info!("已卸载虚拟网卡设备: {}", parts[1]);
            } else {
                denied += 1;
                tracing::warn!("卸载虚拟网卡设备失败（{}）: {}", parts[2].trim(), parts[1]);
            }
        }
    }
    if removed == 0 && denied > 0 {
        return Err(AppError::Msg(
            "卸载虚拟网卡需要管理员权限，请以管理员身份运行本应用后重试".to_string(),
        ));
    }
    Ok(removed)
}

/// 从 `easytier-cli route` 输出解析本机虚拟 IPv4（路由表包含自身节点）
pub async fn probe_virtual_ip(cli: &Path) -> Option<String> {
    let raw = run_cli(cli, "route").await.ok()?;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.to_ascii_lowercase().contains("ipv4") {
            continue;
        }
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '+' || c == '=' || c == '|' || c.is_whitespace())
        {
            continue;
        }
        let cols = split_columns(trimmed);
        for col in &cols {
            // 形如 10.126.126.1 或 10.126.126.1/24
            let ip = col.split('/').next().unwrap_or(col);
            if ip.split('.').count() == 4
                && ip
                    .split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                return Some(ip.to_string());
            }
        }
    }
    None
}

/// 生成默认成员名（主机名）
pub fn default_member_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "VRchat-LocalCinema".to_string())
}

/// 便捷封装：返回 easytier-cli 应当使用的路径
pub fn cli_path_or_err(cli: Option<PathBuf>) -> AppResult<PathBuf> {
    cli.ok_or_else(|| AppError::Msg("未检测到 easytier-cli，请先安装 EasyTier 组件".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_route_hosts() {
        let raw = "\nipv4              hostname         cost   lat  loss  dup?\n\
                   ----------------  ---------------  -----  ----  ----  -----\n\
                   10.126.126.1/24   DESKTOP-ABC      1      0     0.0%  false\n\
                   10.126.126.2/24   LivingRoom-PC    12     3     0.1%  false\n";
        let hosts = parse_route_hosts(raw);
        assert_eq!(
            hosts,
            vec![
                ("10.126.126.1".to_string(), "DESKTOP-ABC".to_string()),
                ("10.126.126.2".to_string(), "LivingRoom-PC".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_route_hosts_empty() {
        assert!(parse_route_hosts("").is_empty());
        assert!(parse_route_hosts("no table here").is_empty());
    }

    #[test]
    fn test_parse_peers_json() {
        let raw = r#"[
        {"id": 15, "ipv4": "10.126.126.2", "hostname": "LivingRoom-PC", "cost": "p2p",
         "lat_ms": "3.12", "loss_rate": "0.05", "nat_type": "PortRestricted", "version": "2.4.3"},
        {"id": 3, "ipv4": "", "hostname": "PublicServer_aws", "cost": "relay",
         "lat_ms": null, "loss_rate": null, "nat_type": "Unknown", "version": "2.4.3"}
    ]"#;
        let peers = parse_peers_json(raw).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].hostname, "LivingRoom-PC");
        assert_eq!(peers[0].ipv4, "10.126.126.2");
        assert_eq!(peers[0].latency_ms, Some(3.12));
        assert_eq!(peers[0].loss, Some(5.0));
        assert_eq!(peers[0].cost, "p2p");
        assert_eq!(peers[1].hostname, "服务器_aws");
        assert_eq!(peers[1].latency_ms, None);
        assert_eq!(peers[1].loss, None);
    }

    #[test]
    fn test_parse_peers_json_invalid() {
        assert!(parse_peers_json("not json").is_none());
        assert!(parse_peers_json("{}").is_none());
        assert!(parse_peers_json("[]").unwrap().is_empty());
    }

    #[test]
    fn test_write_run_ps1() {
        let dir = std::env::temp_dir().join(format!("lc-ps1-test-{}", std::process::id()));
        let path = run_ps1_path(&dir);
        let core = Path::new("C:\\Program Files\\EasyTier\\easytier-core.exe");
        let args = vec![
            "--network-name".to_string(),
            "影院房间".to_string(),
            "--network-secret".to_string(),
            "p'w\"d".to_string(),
        ];
        write_run_ps1(&path, core, &args).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // UTF-8 BOM 前缀
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let text = String::from_utf8_lossy(&bytes[3..]);
        // 路径与参数单引号包裹，内部单引号翻倍
        assert!(text.contains("& 'C:\\Program Files\\EasyTier\\easytier-core.exe'"));
        assert!(text.contains("'影院房间'"));
        assert!(text.contains("'p''w\"d'"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_encode_powershell_command() {
        // 标准 Base64；UTF-16LE 编码（PowerShell -EncodedCommand 要求）
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b""), "");
        // 'A' = 0x41 → UTF-16LE 为 41 00 → Base64 "QQA="
        assert_eq!(encode_powershell_command("A"), "QQA=");
    }

    #[test]
    fn test_parse_ps_output_hex_detail_no_mojibake() {
        // PowerShell 以 GBK 写 stdout 时中文会被 Rust 按 UTF-8 解码成乱码；
        // 协议改为 ASCII + UTF-8 字节的十六进制后必须能还原出原文
        let detail = "必须为管理员";
        let hx: String = detail.as_bytes().iter().map(|b| format!("{:02X}", b)).collect();
        let err = parse_ps_output(&format!("ERR|EXIT|1|{}\r\n", hx))
            .unwrap_err()
            .to_string();
        assert!(err.contains(detail), "错误信息应还原为中文: {}", err);
        assert!(err.contains("退出码 1"), "{}", err);
        assert!(parse_ps_output("OK\r\n").is_ok());
        assert!(parse_ps_output("noise\nOK\n").is_ok());
    }

    #[test]
    fn test_parse_ps_output_user_cancel() {
        for msg in [
            "操作已被用户取消。",
            "The operation was canceled by the user",
            "0x800704C7",
        ] {
            let hx: String = msg.as_bytes().iter().map(|b| format!("{:02X}", b)).collect();
            let err = parse_ps_output(&format!("ERR|ELEVATE|{}", hx))
                .unwrap_err()
                .to_string();
            assert!(err.contains("已取消管理员授权"), "{}", err);
        }
    }

    #[test]
    fn test_elevation_wrapper_ascii_protocol() {
        let args = vec!["-File".to_string(), "C:\\data\\火\\a.ps1".to_string()];
        let w = elevation_wrapper(&args, Path::new("C:\\data\\elevate-result.txt"));
        assert!(w.contains("'C:\\data\\火\\a.ps1'"));
        assert!(w.contains("@('-File','C:\\data\\火\\a.ps1')"));
        assert!(w.contains("[BitConverter]::ToString"));
        assert!(w.contains("ERR|EXIT|"));
        // 输出协议与脚本本身都不含中文，stdout 永远是 ASCII
        assert!(w.contains("$resultFile = 'C:\\data\\elevate-result.txt'"));
    }

    #[test]
    fn test_register_task_script_shape() {
        // 注册脚本必须以 UTF-8 BOM 落盘且包含任务名（引号转义由 ps_quote 保证）
        let ps1 = Path::new("C:\\data\\easytier\\run.ps1");
        let quoted = ps_quote(&ps1.to_string_lossy());
        assert_eq!(quoted, "'C:\\data\\easytier\\run.ps1'");
        assert_eq!(ps_quote("a'b"), "'a''b'");
    }

    #[test]
    fn test_write_run_hidden_vbs() {
        let dir = std::env::temp_dir().join(format!("lc-vbs-test-{}", std::process::id()));
        let path = run_hidden_vbs_path(&dir);
        write_run_hidden_vbs(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 纯 ASCII（wscript 兼容），且从脚本自身位置推导 run.ps1 路径
        assert!(text.chars().all(|c| c.is_ascii()));
        assert!(text.contains("WScript.ScriptFullName"));
        assert!(text.contains("-WindowStyle Hidden"));
        assert!(text.contains("\\run.ps1\"\"\""));
        assert!(text.contains(", 0, True"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_write_stop_hidden_vbs() {
        let dir = std::env::temp_dir().join(format!("lc-stopvbs-test-{}", std::process::id()));
        let path = stop_hidden_vbs_path(&dir);
        write_stop_hidden_vbs(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 纯 ASCII（wscript 兼容），隐藏窗口运行并强杀 easytier-core
        assert!(text.chars().all(|c| c.is_ascii()));
        assert!(text.contains("Stop-Process -Name 'easytier-core' -Force"));
        assert!(text.contains("-WindowStyle") || text.contains(", 0, True"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_filter_valuable_log() {
        let raw = "2026-10-14T10:27:54+08:00  INFO CORE: Starting easytier from cli with config:\n\
                   hostname = \"RAINBOW\"\n\
                   dhcp = true\n\
                   2026-10-14T10:27:55.8105+08:00 ERROR easytier::tunnel::tcp: bind addr fail bind_addr=1.2.3.4:0 addr=5.6.7.8:11010 error=IOError\n\
                   2026-10-14T10:27:55.8107+08:00  INFO easytier::tunnel::tcp: bind addr bind_addr=1.2.3.4:0 addr=5.6.7.8:11010\n\
                   2026-10-14T10:27:56.0279+08:00  INFO CORE::INSTANCE::CONNECTION: [id] new peer connection added conn_info=PeerConnInfo\n\
                   2026-10-14T10:27:57.1036+08:00  INFO wintun::log: WinTun: Creating adapter\n\
                   2026-10-14T10:27:57.6086+08:00 ERROR wintun::log: WinTun: Failed to find matching adapter name: x\n\
                   2026-10-14T10:27:58.9317+08:00  INFO CORE::INSTANCE: [id] tun device ready dev=\"et_9_elby\"\n\
                   2026-10-14T10:27:59.0113+08:00  INFO CORE::INSTANCE: [id] dhcp ip changed old=None new=Some(10.126.126.1/24)\n\
                   2026-10-14T10:27:59.1+08:00  INFO try_connect_to_ip: easytier::connector::direct: close time.busy=3.21s time.idle=16.4s\n\
                   2026-10-14T10:27:59.2+08:00  INFO easytier::common::dns: get txt record txt_data=x\n";
        let filtered = filter_valuable_log(raw, 200);
        assert!(filtered.contains("Starting easytier from cli"));
        assert!(filtered.contains("hostname = \"RAINBOW\""));
        assert!(filtered.contains("dhcp = true"));
        assert!(filtered.contains("new peer connection added"));
        assert!(filtered.contains("WinTun: Creating adapter"));
        assert!(filtered.contains("tun device ready"));
        assert!(filtered.contains("dhcp ip changed"));
        assert!(!filtered.contains("bind addr fail"));
        assert!(!filtered.contains("time.busy="));
        assert!(!filtered.contains("get txt record"));
        // 黑名单错误（Wintun 无害警告）同样被丢弃
        assert!(!filtered.contains("Failed to find matching adapter name"));
    }

    #[test]
    fn test_filter_valuable_log_max_lines() {
        let raw: String = (0..300)
            .map(|i| format!("2026-10-14T10:00:{:02}+08:00  INFO CORE::INSTANCE: [id] dhcp ip changed new=10.0.0.{}\n", i % 60, i))
            .collect();
        let filtered = filter_valuable_log(&raw, 100);
        assert_eq!(filtered.lines().count(), 100);
        // 保留的是最新的行
        assert!(filtered.contains("10.0.0.299"));
        assert!(!filtered.contains("10.0.0.0\n"));
    }
}