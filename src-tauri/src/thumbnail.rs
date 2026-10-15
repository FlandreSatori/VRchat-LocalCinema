use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::error::{AppError, AppResult};

/// 在 PATH 中查找可执行文件
fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    };
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn resolve_tool(configured: &str, default_name: &str) -> Option<PathBuf> {
    if !configured.trim().is_empty() {
        let p = PathBuf::from(configured.trim());
        if p.is_file() {
            return Some(p);
        }
        tracing::warn!("配置的工具路径不可用: {}", configured);
    }
    which(default_name)
}

/// 隐藏控制台窗口（tokio 版）
fn hide_console_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

/// 用 ffprobe 获取完整媒体流参数（JSON），用于兼容性判定与视频信息展示
pub async fn probe_media_json(ffprobe: &Path, file: &Path) -> AppResult<serde_json::Value> {
    let output = run_with_timeout(
        ffprobe,
        &[
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ],
        file,
        Duration::from_secs(30),
    )
    .await?;
    serde_json::from_slice(&output)
        .map_err(|e| AppError::Msg(format!("解析 ffprobe 输出失败: {}", e)))
}

/// 用 ffprobe 获取视频时长（毫秒）
pub async fn probe_duration(ffprobe: &Path, file: &Path) -> Option<u64> {
    let output = run_with_timeout(
        ffprobe,
        &[
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ],
        file,
        Duration::from_secs(20),
    )
    .await
    .ok()?;
    let text = String::from_utf8_lossy(&output);
    let secs: f64 = text.trim().parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        Some((secs * 1000.0) as u64)
    } else {
        None
    }
}

async fn run_with_timeout(
    tool: &Path,
    args: &[&str],
    file: &Path,
    timeout: Duration,
) -> AppResult<Vec<u8>> {
    let mut cmd = tokio::process::Command::new(tool);
    cmd.args(args)
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 超时后 future 被丢弃时自动结束子进程，避免遗留僵尸进程
        .kill_on_drop(true);
    hide_console_tokio(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| AppError::Msg(format!("无法启动 {}: {}", tool.display(), e)))?;

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(AppError::Io(e)),
        Err(_) => return Err(AppError::Msg(format!("{} 执行超时", tool.display()))),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Msg(format!(
            "{} 退出码 {:?}: {}",
            tool.display(),
            output.status.code(),
            stderr.chars().take(300).collect::<String>()
        )));
    }
    Ok(output.stdout)
}

/// 从视频提取一帧作为封面，输出为 jpg。优先取 10% 处（最长不超过 60s），失败回退到第 1 秒。
pub async fn extract_thumbnail(
    ffmpeg: &Path,
    file: &Path,
    duration_ms: Option<u64>,
    out_path: &Path,
) -> AppResult<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let seek_secs = duration_ms
        .map(|d| ((d / 1000) / 10).min(60).max(1))
        .unwrap_or(3);

    for seek in [seek_secs, 1] {
        let out_str = out_path.to_string_lossy().to_string();
        let args: Vec<String> = vec![
            "-y".into(),
            "-ss".into(),
            seek.to_string(),
            "-i".into(),
            file.to_string_lossy().to_string(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            "scale='min(480,iw)':-2".into(),
            "-q:v".into(),
            "4".into(),
            out_str,
        ];
        let mut cmd = tokio::process::Command::new(ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // 超时后自动结束子进程
            .kill_on_drop(true);
        hide_console_tokio(&mut cmd);
        let child = cmd.spawn();
        let ok = match child {
            Ok(c) => match tokio::time::timeout(Duration::from_secs(30), c.wait_with_output()).await {
                Ok(Ok(o)) => o.status.success() && out_path.exists(),
                _ => false,
            },
            Err(e) => {
                return Err(AppError::Msg(format!("无法启动 ffmpeg: {}", e)));
            }
        };
        if ok {
            return Ok(());
        }
        let _ = std::fs::remove_file(out_path);
    }
    Err(AppError::Msg("ffmpeg 封面提取失败".to_string()))
}

/// 复制用户指定的自定义封面到缓存目录
pub fn import_custom_thumbnail(src: &Path, out_path: &Path) -> AppResult<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, out_path)?;
    Ok(())
}
