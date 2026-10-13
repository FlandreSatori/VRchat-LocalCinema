use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::error::{AppError, AppResult};
use crate::external_player::{self, PlayerInfo};
use crate::http_server::{HttpServer, ServerStatus};
use crate::logger::LogEntry;
use crate::logger::RecentLogs;
use crate::media_library::{Db, MediaItem, MediaStatus};
use crate::network_interfaces::{self, NetInterface};
use crate::settings::{AppPaths, CloseBehavior, Settings, UrlMode};
use crate::thumbnail;
use crate::url_builder::{self, UrlInput};

pub struct AppState {
    pub paths: AppPaths,
    pub settings: RwLock<Settings>,
    pub db: Arc<Mutex<Db>>,
    pub server: HttpServer,
    pub recent_logs: RecentLogs,
}

impl AppState {
    pub fn current_settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    fn emit_status(app: &AppHandle, server: &HttpServer) {
        let _ = app.emit("server-status-changed", server.status());
    }
}

fn thumb_dir_of(state: &AppState) -> PathBuf {
    let s = state.current_settings();
    if s.thumb_dir.trim().is_empty() {
        state.paths.thumb_dir.clone()
    } else {
        PathBuf::from(s.thumb_dir.trim())
    }
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> AppResult<Settings> {
    Ok(state.current_settings())
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    mut settings: Settings,
) -> AppResult<()> {
    if settings.port == 0 {
        return Err(AppError::Msg("端口必须在 1-65535 之间".to_string()));
    }
    settings.normalize_network_mode();
    let old = state.current_settings();
    let net_changed =
        old.bind_ip() != settings.bind_ip() || old.port != settings.port;
    let autostart_changed = old.autostart != settings.autostart;

    {
        let mut guard = state.settings.write().unwrap();
        *guard = settings.clone();
    }
    settings.save(&state.paths.settings_file)?;

    if autostart_changed {
        let manager = app.autolaunch();
        let r = if settings.autostart {
            manager.enable()
        } else {
            manager.disable()
        };
        if let Err(e) = r {
            tracing::warn!("设置开机启动失败: {}", e);
        }
    }

    if net_changed {
        let server = state.server.clone();
        server.stop();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let result = server.start(&settings.bind_ip(), settings.port).await;
        AppState::emit_status(&app, &server);
        result?;
    }
    Ok(())
}

#[tauri::command]
pub async fn reset_settings(app: AppHandle, state: State<'_, AppState>) -> AppResult<Settings> {
    let defaults = Settings::default();
    save_settings(app, state, defaults.clone()).await?;
    Ok(defaults)
}

#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> AppResult<ServerStatus> {
    Ok(state.server.status())
}

#[tauri::command]
pub async fn start_server(app: AppHandle, state: State<'_, AppState>) -> AppResult<ServerStatus> {
    let settings = state.current_settings();
    let server = state.server.clone();
    let r = server.start(&settings.bind_ip(), settings.port).await;
    AppState::emit_status(&app, &server);
    r.map(|_| server.status())
}

#[tauri::command]
pub async fn stop_server(app: AppHandle, state: State<'_, AppState>) -> AppResult<ServerStatus> {
    state.server.stop();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    AppState::emit_status(&app, &state.server);
    Ok(state.server.status())
}

#[tauri::command]
pub async fn restart_server(app: AppHandle, state: State<'_, AppState>) -> AppResult<ServerStatus> {
    let settings = state.current_settings();
    let server = state.server.clone();
    server.stop();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    server.start(&settings.bind_ip(), settings.port).await?;
    AppState::emit_status(&app, &server);
    Ok(server.status())
}

#[tauri::command]
pub async fn list_media(state: State<'_, AppState>) -> AppResult<Vec<MediaItem>> {
    let db = state.db.lock().unwrap();
    let mut items = db.list()?;
    // 轻量级状态检查：仅 stat 文件存在性
    for item in items.iter_mut() {
        let exists = std::path::Path::new(&item.file_path).is_file();
        if !exists && item.status != MediaStatus::Missing {
            item.status = MediaStatus::Missing;
            let _ = db.set_status(&item.id, MediaStatus::Missing);
        } else if exists && item.status == MediaStatus::Missing {
            item.status = MediaStatus::Ok;
            let _ = db.set_status(&item.id, MediaStatus::Ok);
        }
    }
    Ok(items)
}

/// 后台探针：读取时长 + 提取封面，完成后更新 DB 并通知前端
fn spawn_probe_task(app: AppHandle, id: String) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let db = state.db.clone();
        let settings = state.current_settings();
        let thumb_dir = thumb_dir_of(&state);

        let item = {
            let db = db.lock().unwrap();
            match db.get(&id) {
                Ok(Some(v)) => v,
                _ => return,
            }
        };
        let file_path = PathBuf::from(&item.file_path);

        if !file_path.is_file() {
            let db = db.lock().unwrap();
            let _ = db.set_status(&id, MediaStatus::Missing);
            if let Ok(Some(item)) = db.get(&id) {
                let _ = app.emit("media-updated", &item);
            }
            return;
        }

        let duration_ms = if let Some(ffprobe) =
            thumbnail::resolve_tool(&settings.ffprobe_path, "ffprobe")
        {
            thumbnail::probe_duration(&ffprobe, &file_path).await
        } else {
            tracing::warn!("未找到 ffprobe，跳过时长获取: {}", file_path.display());
            None
        };

        let file_size = std::fs::metadata(&file_path).ok().map(|m| m.len());

        let mut thumb_name: Option<String> = None;
        if let Some(ffmpeg) = thumbnail::resolve_tool(&settings.ffmpeg_path, "ffmpeg") {
            let out = thumb_dir.join(format!("{}.jpg", id));
            match thumbnail::extract_thumbnail(&ffmpeg, &file_path, duration_ms, &out).await {
                Ok(_) => thumb_name = Some(format!("{}.jpg", id)),
                Err(e) => tracing::warn!("封面提取失败 {}: {}", file_path.display(), e),
            }
        } else {
            tracing::warn!("未找到 ffmpeg，跳过封面提取: {}", file_path.display());
        }

        let item = {
            let db = db.lock().unwrap();
            match db.update_after_probe(&id, duration_ms, thumb_name, MediaStatus::Ok, file_size) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("更新媒体信息失败: {}", e);
                    None
                }
            }
        };
        if let Some(item) = item {
            let _ = app.emit("media-updated", &item);
        }
    });
}

#[tauri::command]
pub async fn import_files(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> AppResult<crate::media_library::ImportResult> {
    let result = state.db.lock().unwrap().import_paths(&paths);
    let mut probed = std::collections::HashSet::new();
    for item in &result.items {
        if probed.insert(item.id.clone()) {
            spawn_probe_task(app.clone(), item.id.clone());
        }
    }
    Ok(result)
}

async fn refresh_media_internal(
    app: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
) -> AppResult<MediaItem> {
    let item = {
        let db = state.db.lock().unwrap();
        db.get(id)?.ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    let path = PathBuf::from(&item.file_path);
    if !path.is_file() {
        let updated = {
            let db = state.db.lock().unwrap();
            db.set_status(id, MediaStatus::Missing)?;
            db.get(id)?.unwrap()
        };
        let _ = app.emit("media-updated", &updated);
        return Ok(updated);
    }
    spawn_probe_task(app.clone(), id.to_string());
    // 立即返回当前条目，探针完成后通过事件推送更新
    Ok(item)
}

#[tauri::command]
pub async fn refresh_media(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<MediaItem> {
    refresh_media_internal(&app, &state, &id).await
}

#[tauri::command]
pub async fn remove_media(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let removed = {
        let db = state.db.lock().unwrap();
        db.remove(&id)?
    };
    if let Some(item) = removed {
        // 清理封面缓存
        if let Some(thumb) = &item.thumbnail {
            let thumb_path = thumb_dir_of(&state).join(thumb);
            let _ = std::fs::remove_file(thumb_path);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn set_custom_thumbnail(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    image_path: String,
) -> AppResult<MediaItem> {
    let src = PathBuf::from(&image_path);
    if !src.is_file() {
        return Err(AppError::Msg("封面图片不存在".to_string()));
    }
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("jpg")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return Err(AppError::Msg("封面必须是 jpg/png/webp 图片".to_string()));
    }
    let thumb_name = format!("{}.{}", id, ext);
    let out = thumb_dir_of(&state).join(&thumb_name);
    thumbnail::import_custom_thumbnail(&src, &out)?;
    let updated = {
        let db = state.db.lock().unwrap();
        db.update_after_probe(&id, None, Some(thumb_name), MediaStatus::Ok, None)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    let _ = app.emit("media-updated", &updated);
    Ok(updated)
}

#[tauri::command]
pub async fn get_network_interfaces() -> AppResult<Vec<NetInterface>> {
    Ok(network_interfaces::list_ipv4_interfaces())
}

#[tauri::command]
pub async fn build_media_url(
    state: State<'_, AppState>,
    id: String,
    mode: UrlMode,
) -> AppResult<String> {
    let settings = state.current_settings();
    let item = {
        let db = state.db.lock().unwrap();
        let item = db.get(&id)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?;
        db.resolve_basename(&item.file_name)?
            .ok_or_else(|| AppError::Msg("文件名无法映射".into()))?;
        item
    };
    let lan_ip = network_interfaces::first_lan_ipv4();
    let input = UrlInput {
        mode,
        port: settings.port,
        lan_ip,
    };
    url_builder::build_media_url(&item, &input).map_err(AppError::Msg)
}

#[tauri::command]
pub async fn detect_players() -> AppResult<Vec<PlayerInfo>> {
    Ok(external_player::detect_players())
}

#[tauri::command]
pub async fn open_in_player(
    state: State<'_, AppState>,
    id: String,
    player_path: Option<String>,
) -> AppResult<()> {
    let item = get_item_checked(&state, &id)?;
    let settings = state.current_settings();
    let player = player_path
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(settings.default_player_path);
    if player.trim().is_empty() {
        return Err(AppError::Msg(
            "未设置默认播放器，请在设置中选择或使用系统默认打开".to_string(),
        ));
    }
    external_player::launch_player(
        &PathBuf::from(player.trim()),
        &settings.default_player_args,
        &PathBuf::from(&item.file_path),
    )
}

#[tauri::command]
pub async fn open_with_system_default(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let item = get_item_checked(&state, &id)?;
    external_player::open_with_default(&PathBuf::from(&item.file_path))
}

#[tauri::command]
pub async fn open_in_folder(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let item = get_item_checked(&state, &id)?;
    external_player::reveal_in_folder(&PathBuf::from(&item.file_path))
}

fn get_item_checked(state: &State<'_, AppState>, id: &str) -> AppResult<MediaItem> {
    let db = state.db.lock().unwrap();
    let item = db
        .get(id)?
        .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?;
    if !std::path::Path::new(&item.file_path).is_file() {
        return Err(AppError::Msg("文件已丢失或无法访问".to_string()));
    }
    Ok(item)
}

#[tauri::command]
pub async fn open_url(app: AppHandle, url: String) -> AppResult<()> {
    app.opener()
        .open_url(&url, None::<&str>)
        .map_err(|e| AppError::Msg(format!("打开链接失败: {}", e)))
}

#[tauri::command]
pub async fn get_recent_logs(state: State<'_, AppState>, limit: usize) -> AppResult<Vec<LogEntry>> {
    Ok(state.recent_logs.list(limit.min(200)))
}

#[tauri::command]
pub async fn get_app_paths(state: State<'_, AppState>) -> AppResult<AppPaths> {
    Ok(state.paths.clone())
}

#[tauri::command]
pub async fn open_data_dir(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let dir = state.paths.data_dir.clone();
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| AppError::Msg(format!("打开配置目录失败: {}", e)))
}

#[tauri::command]
pub async fn clear_thumbnail_cache(state: State<'_, AppState>) -> AppResult<u64> {
    let dir = thumb_dir_of(&state);
    let mut removed: u64 = 0;
    // 仅清理当前媒体库未引用的缓存
    let used: std::collections::HashSet<String> = {
        let db = state.db.lock().unwrap();
        db.list()?
            .into_iter()
            .filter_map(|m| m.thumbnail)
            .collect()
    };
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !used.contains(&name) {
                if std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    Ok(removed)
}

#[tauri::command]
pub async fn get_close_behavior(state: State<'_, AppState>) -> AppResult<CloseBehavior> {
    Ok(state.current_settings().close_behavior)
}
