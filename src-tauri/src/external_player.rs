use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Serialize;

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerInfo {
    pub name: String,
    pub path: String,
}

/// 常见播放器候选（名称 + PATH 可执行名 + 常见安装路径）
fn candidates() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        (
            "VLC media player",
            "vlc",
            vec![
                r"C:\Program Files\VideoLAN\VLC\vlc.exe",
                r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
            ],
        ),
        (
            "PotPlayer",
            "potplayermini64",
            vec![
                r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
                r"C:\Program Files (x86)\DAUM\PotPlayer\PotPlayerMini.exe",
            ],
        ),
        (
            "mpv",
            "mpv",
            vec![r"C:\Program Files\mpv\mpv.exe"],
        ),
        (
            "MPC-HC",
            "mpc-hc64",
            vec![
                r"C:\Program Files\MPC-HC\mpc-hc64.exe",
                r"C:\Program Files (x86)\MPC-HC\mpc-hc.exe",
            ],
        ),
        (
            "MPC-BE",
            "mpc-be64",
            vec![r"C:\Program Files\MPC-BE x64\mpc-be64.exe"],
        ),
    ]
}

fn which(name: &str) -> Option<PathBuf> {
    let exe = format!("{}.exe", name);
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let c = dir.join(&exe);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// 自动检测常见播放器
pub fn detect_players() -> Vec<PlayerInfo> {
    let mut found: Vec<PlayerInfo> = Vec::new();
    for (name, exe_name, paths) in candidates() {
        let mut detected: Option<PathBuf> = None;
        for p in &paths {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                detected = Some(pb);
                break;
            }
        }
        if detected.is_none() {
            detected = which(exe_name);
        }
        if let Some(p) = detected {
            let ps = p.to_string_lossy().to_string();
            if !found.iter().any(|f| f.path == ps) {
                found.push(PlayerInfo {
                    name: name.to_string(),
                    path: ps,
                });
            }
        }
    }
    found
}

/// 用指定播放器打开文件。
/// args_template: 为空则仅传文件路径；否则按空格切分，{path} 占位符替换为文件路径。
pub fn launch_player(player: &Path, args_template: &str, file: &Path) -> AppResult<()> {
    if !player.is_file() {
        return Err(AppError::Msg(format!(
            "播放器不存在: {}",
            player.display()
        )));
    }
    let file_str = file.to_string_lossy().to_string();
    let mut cmd = std::process::Command::new(player);
    if args_template.trim().is_empty() {
        cmd.arg(&file_str);
    } else {
        for part in args_template.split_whitespace() {
            if part == "{path}" {
                cmd.arg(&file_str);
            } else if part.contains("{path}") {
                cmd.arg(part.replace("{path}", &file_str));
            } else {
                cmd.arg(part);
            }
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
        .map_err(|e| AppError::Msg(format!("启动播放器失败: {}", e)))?;
    Ok(())
}

/// 用系统默认关联程序打开文件
pub fn open_with_default(file: &Path) -> AppResult<()> {
    opener::open(file).map_err(|e| AppError::Msg(format!("打开文件失败: {}", e)))
}

/// 在文件管理器中定位文件
pub fn reveal_in_folder(file: &Path) -> AppResult<()> {
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("explorer.exe");
        cmd.arg("/select,").arg(file);
        cmd.spawn()
            .map_err(|e| AppError::Msg(format!("打开目录失败: {}", e)))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    open_with_default(file.parent().unwrap_or(file))
}
