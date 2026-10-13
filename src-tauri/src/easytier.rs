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
}

impl Default for EasytierManager {
    fn default() -> Self {
        Self::new()
    }
}

impl EasytierManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            status: RwLock::new(EasytierStatus::default()),
            virtual_ip: RwLock::new(None),
        }
    }

    pub fn status(&self) -> EasytierStatus {
        self.status.read().unwrap().clone()
    }

    /// 设置本机虚拟 IP（EasyTier 停止时应清空）
    pub fn set_virtual_ip(&self, ip: Option<String>) {
        *self.virtual_ip.write().unwrap() = ip;
    }

    pub fn cached_virtual_ip(&self) -> Option<String> {
        self.virtual_ip.read().unwrap().clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut EasytierStatus)) {
        let mut guard = self.status.write().unwrap();
        f(&mut guard);
    }

    /// 启动 easytier-core
    pub async fn start(&self, core: &Path, args: Vec<String>) -> AppResult<EasytierStatus> {
        self.stop();

        let mut cmd = tokio::process::Command::new(core);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::Msg(format!("无法启动 easytier-core: {}", e)))?;
        let pid = child.id();

        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, "easytier");
        }
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, "easytier");
        }

        *self.child.lock().unwrap() = Some(child);
        self.set_status(|s| {
            s.running = true;
            s.pid = pid;
            s.error = None;
            s.args = args;
        });
        Ok(self.status())
    }

    /// 停止 easytier-core
    pub fn stop(&self) {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.start_kill();
        }
        drop(guard);
        self.set_status(|s| {
            s.running = false;
            s.pid = None;
        });
        self.set_virtual_ip(None);
    }

    /// 进程是否仍然存活（用于刷新状态）
    pub fn refresh(&self) {
        let mut guard = self.child.lock().unwrap();
        let exited = match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => Some(format!("easytier-core 已退出（{:?}）", status.code())),
                Ok(None) => None,
                Err(e) => Some(format!("检测 easytier-core 状态失败: {}", e)),
            },
            None => None,
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
    }
}

fn spawn_log_reader<R>(reader: R, tag: &'static str)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                tracing::info!("[{}] {}", tag, line);
            }
        }
    });
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
}