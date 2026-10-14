use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::compat;
use crate::components::{self, ComponentKind, ComponentStatus};
use crate::easytier::{self, EasytierStatus, RoomInfo};
use crate::error::{AppError, AppResult};
use crate::external_player::{self, PlayerInfo};
use crate::http_server::{HttpServer, ServerStatus};
use crate::logger::LogEntry;
use crate::logger::RecentLogs;
use crate::media_library::{Db, MediaItem, MediaStatus, MediaSummary};
use crate::network_interfaces::{self, NetInterface};
use crate::settings::{AppPaths, CloseBehavior, ListenMode, Settings, UrlMode};
use crate::thumbnail;
use crate::url_builder::{self, UrlInput};

pub struct AppState {
    pub paths: AppPaths,
    pub settings: RwLock<Settings>,
    pub db: Arc<Mutex<Db>>,
    pub server: HttpServer,
    pub recent_logs: RecentLogs,
    pub easytier: easytier::EasytierManager,
    /// 转码任务队列（按任务序列执行）
    pub convert_queue: crate::convert_queue::ConvertQueue,
    /// 正在进行的 HLS 转码任务（媒体 id）
    pub hls_jobs: RwLock<HashSet<String>>,
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

/// 后台探针：深度探测媒体参数 + 兼容性判定 + 计算哈希 + 提取封面，完成后更新 DB 并通知前端
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

        let (ffprobe, _) = components::ffprobe_binary(&settings, &state.paths);
        // 深度探测：容器/编码/位深/像素格式/音频参数，并做兼容性判定
        let probe = if let Some(ffprobe) = &ffprobe {
            match thumbnail::probe_media_json(ffprobe, &file_path).await {
                Ok(json) => {
                    let info = compat::parse_probe(&json);
                    let result = compat::evaluate(&info);
                    Some((info, result))
                }
                Err(e) => {
                    tracing::warn!("ffprobe 深度探测失败 {}: {}", file_path.display(), e);
                    // 探测失败按不支持处理（严格白名单）
                    Some((
                        compat::ProbeInfo::default(),
                        compat::evaluate(&compat::ProbeInfo::default()),
                    ))
                }
            }
        } else {
            tracing::warn!("未找到 ffprobe，跳过媒体探测: {}", file_path.display());
            None
        };
        let duration_ms = probe.as_ref().and_then(|(i, _)| i.duration_ms);

        let file_size = std::fs::metadata(&file_path).ok().map(|m| m.len());

        // 计算内容哈希，用于房间内媒体比对
        let hash = hash_file(&file_path).await;

        let mut thumb_name: Option<String> = None;
        let (ffmpeg, _) = components::ffmpeg_binary(&settings, &state.paths);
        if let Some(ffmpeg) = ffmpeg {
            let out = thumb_dir.join(format!("{}.jpg", id));
            match thumbnail::extract_thumbnail(&ffmpeg, &file_path, duration_ms, &out).await {
                Ok(_) => thumb_name = Some(format!("{}.jpg", id)),
                Err(e) => tracing::warn!("封面提取失败 {}: {}", file_path.display(), e),
            }
        } else {
            tracing::warn!("未找到 ffmpeg，跳过封面提取: {}", file_path.display());
        }

        if let Some((info, compat_result)) = &probe {
            if !compat_result.supported {
                tracing::info!(
                    "媒体不兼容 {}（{}）: {}",
                    file_path.display(),
                    info.raw_format_name.as_deref().unwrap_or("?"),
                    compat_result.reason.as_deref().unwrap_or("")
                );
            }
        }

        let item = {
            let db = db.lock().unwrap();
            if let Some(h) = &hash {
                let _ = db.set_hash(&id, h);
            }
            match db.update_after_probe(
                &id,
                duration_ms,
                thumb_name,
                MediaStatus::Ok,
                file_size,
                probe.as_ref().map(|(i, r)| (i, r)),
            ) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("更新媒体信息失败: {}", e);
                    None
                }
            }
        };
        let item = match item {
            Some(mut it) => {
                // 不兼容条目自动归入内置「不兼容」文件夹（尊重用户手动整理）
                if it.compat_status.as_deref() == Some("unsupported")
                    && it.folder_id.is_none()
                    && !it.compat_resolved
                {
                    let updated = {
                        let db = db.lock().unwrap();
                        let _ =
                            db.set_media_folder(&id, Some(crate::media_library::BUILTIN_INCOMPATIBLE_ID));
                        db.get(&id).ok().flatten()
                    };
                    if let Some(u) = updated {
                        it = u;
                    }
                }
                Some(it)
            }
            None => None,
        };
        if let Some(item) = item {
            let _ = app.emit("media-updated", &item);
        }
    });
}

/// 递归收集目录中的视频文件，返回 (文件路径, 目录链)；
/// 目录链为相对于拖入根目录的各级文件夹名，用于按目录结构生成媒体库文件夹
fn collect_import_files(
    dir: &std::path::Path,
    rel: &[String],
    out: &mut Vec<(PathBuf, Vec<String>)>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        // 跳过隐藏目录 / 系统目录（如 $RECYCLE.BIN、System Volume Information）
        if name.starts_with('.') || name.starts_with('$') {
            continue;
        }
        let child = entry.path();
        let Ok(meta) = std::fs::metadata(&child) else {
            continue;
        };
        if meta.is_dir() {
            let mut next = rel.to_vec();
            next.push(name);
            collect_import_files(&child, &next, out, depth + 1);
        } else if meta.is_file() {
            let ext = child
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if crate::media_library::VIDEO_EXTENSIONS.contains(&ext.as_str()) {
                out.push((child, rel.to_vec()));
            }
        }
    }
}

#[tauri::command]
pub async fn import_files(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> AppResult<crate::media_library::ImportResult> {
    // 展开导入路径：目录递归收集其中的视频文件，并按目录结构生成媒体库文件夹
    //（仅映射关系，不动磁盘文件；播放链接仍基于文件名的根路径短链接）
    let mut expanded: Vec<(String, Option<String>)> = Vec::new();
    let mut folder_created = false;
    for p in &paths {
        let path = PathBuf::from(p);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            // 拖入的目录本身作为顶层文件夹，保留源目录结构
            let root_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "导入文件夹".to_string());
            let mut files = Vec::new();
            collect_import_files(&path, &[root_name], &mut files, 0);
            for (file, chain) in files {
                let mut parent_id: Option<String> = None;
                for seg in &chain {
                    let folder = state
                        .db
                        .lock()
                        .unwrap()
                        .get_or_create_folder(seg, parent_id.as_deref())?;
                    parent_id = Some(folder.id);
                }
                folder_created = true;
                expanded.push((file.to_string_lossy().to_string(), parent_id));
            }
        } else if meta.is_file() {
            expanded.push((p.clone(), None));
        }
    }

    let file_paths: Vec<String> = expanded.iter().map(|(f, _)| f.clone()).collect();
    let result = state.db.lock().unwrap().import_paths(&file_paths);
    // 将导入条目放入对应的媒体库文件夹
    if folder_created {
        // register() 内部会 canonicalize 路径（Windows 下带 \\?\ 前缀），
        // 因此映射表的键必须同样 canonicalize，否则匹配不上
        let mut folder_by_path: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        for (f, fid) in &expanded {
            let key = std::fs::canonicalize(std::path::Path::new(f))
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| f.clone());
            folder_by_path.insert(key, fid.clone());
        }
        let db = state.db.lock().unwrap();
        for item in &result.items {
            if let Some(Some(fid)) = folder_by_path.get(&item.file_path) {
                let _ = db.set_media_folder(&item.id, Some(fid));
            }
        }
    }
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
        db.update_after_probe(&id, None, Some(thumb_name), MediaStatus::Ok, None, None)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    let _ = app.emit("media-updated", &updated);
    Ok(updated)
}

#[tauri::command]
pub async fn get_network_interfaces() -> AppResult<Vec<NetInterface>> {
    Ok(network_interfaces::list_ipv4_interfaces())
}

/// 解析「局域网模式」使用的 IP：手动指定网卡优先，其次 EasyTier 虚拟 IP，最后物理网卡
async fn resolve_lan_ip(state: &State<'_, AppState>) -> Option<String> {
    let settings = state.current_settings();
    let manual = settings.lan_adapter_ip.trim().to_string();
    if !manual.is_empty() {
        return Some(manual);
    }
    if state.easytier.status().running {
        if let Some(ip) = state.easytier.cached_virtual_ip() {
            return Some(ip);
        }
        // 配置未指定虚拟 IP 时，从 easytier-cli route 查询自身并缓存
        let (cli, _) = components::easytier_cli_binary(&settings, &state.paths);
        if let Some(cli) = cli {
            if let Some(ip) = easytier::probe_virtual_ip(&cli).await {
                state.easytier.set_virtual_ip(Some(ip.clone()));
                return Some(ip);
            }
        }
    }
    network_interfaces::first_lan_ipv4().filter(|s| !s.is_empty())
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
    let lan_ip = resolve_lan_ip(&state).await;
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

// ---------- 转码缓存 ----------

/// 转码缓存概览
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertCacheInfo {
    /// 转码产物目录（data/converted）
    pub dir: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

/// 一键清理结果
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearConvertCacheResult {
    pub removed_files: u64,
    pub freed_bytes: u64,
    /// 从媒体库移除的转码产物记录数
    pub removed_records: u64,
}

/// 递归统计目录下的文件数与总字节数
fn walk_dir_stats(dir: &std::path::Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    let (c, b) = walk_dir_stats(&entry.path());
                    count += c;
                    bytes += b;
                }
                _ => {
                    if let Ok(md) = entry.metadata() {
                        count += 1;
                        bytes += md.len();
                    }
                }
            }
        }
    }
    (count, bytes)
}

#[tauri::command]
pub async fn get_convert_cache_info(state: State<'_, AppState>) -> AppResult<ConvertCacheInfo> {
    let converted = state.paths.data_dir.join("converted");
    let hls = state.paths.data_dir.join("hls");
    let (c1, b1) = walk_dir_stats(&converted);
    let (c2, b2) = walk_dir_stats(&hls);
    Ok(ConvertCacheInfo {
        dir: converted.to_string_lossy().to_string(),
        file_count: c1 + c2,
        total_bytes: b1 + b2,
    })
}

/// 在文件资源管理器中打开转码产物目录
#[tauri::command]
pub async fn open_convert_dir(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let dir = state.paths.data_dir.join("converted");
    std::fs::create_dir_all(&dir)?;
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| AppError::Msg(format!("打开转码目录失败: {}", e)))
}

/// 一键清除全部转码缓存：
/// 删除 data/converted 与 data/hls 下的文件，并从媒体库移除指向转码产物的记录（仅数据库映射，不影响源视频）。
#[tauri::command]
pub async fn clear_convert_cache(_app: AppHandle, state: State<'_, AppState>) -> AppResult<ClearConvertCacheResult> {
    // 队列中有未完成任务时拒绝清理，避免删掉正在写入的产物
    {
        let tasks = state.convert_queue.list();
        if tasks.iter().any(|t| {
            matches!(
                t.state,
                crate::convert_queue::TaskState::Queued | crate::convert_queue::TaskState::Running
            )
        }) {
            return Err(AppError::Msg(
                "转码队列中有未完成的任务，请先完成或取消后再清理".to_string(),
            ));
        }
    }

    let converted = state.paths.data_dir.join("converted");
    let hls = state.paths.data_dir.join("hls");
    let (c1, b1) = walk_dir_stats(&converted);
    let (c2, b2) = walk_dir_stats(&hls);

    // 从媒体库移除指向转码产物的记录；源视频的 HLS 标记一并清空
    let mut removed_records = 0u64;
    {
        let db = state.db.lock().unwrap();
        for item in db.list()? {
            let in_converted = std::path::Path::new(&item.file_path).starts_with(&converted);
            if in_converted {
                if db.remove(&item.id)?.is_some() {
                    removed_records += 1;
                }
            } else if item.hls_playlist.is_some() {
                db.clear_hls_playlist(&item.id)?;
            }
        }
    }

    if converted.exists() {
        std::fs::remove_dir_all(&converted)?;
    }
    if hls.exists() {
        std::fs::remove_dir_all(&hls)?;
    }
    std::fs::create_dir_all(&converted)?;

    Ok(ClearConvertCacheResult {
        removed_files: c1 + c2,
        freed_bytes: b1 + b2,
        removed_records,
    })
}

/// 计算文件 SHA-256（流式分块读取，避免占用大量内存）
async fn hash_file(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};

    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path).ok()?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = std::io::Read::read(&mut file, &mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Some(hex::encode(hasher.finalize()))
    })
    .await
    .ok()
    .flatten()
}

// ===== 内置组件（FFmpeg / EasyTier） =====

#[tauri::command]
pub async fn get_components_status(state: State<'_, AppState>) -> AppResult<Vec<ComponentStatus>> {
    let settings = state.current_settings();
    Ok(vec![
        components::status(ComponentKind::Ffmpeg, &settings, &state.paths),
        components::status(ComponentKind::Easytier, &settings, &state.paths),
    ])
}

/// 自动下载最新版组件并安装到内置组件目录
#[tauri::command]
pub async fn download_component(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
) -> AppResult<ComponentStatus> {
    let kind = ComponentKind::parse(&kind)?;
    let paths = state.paths.clone();
    let settings = state.current_settings();
    let progress_app = app.clone();
    let event_kind = kind.id().to_string();

    components::download_and_install(kind, &paths, move |downloaded, total| {
        let _ = progress_app.emit(
            "component-download-progress",
            serde_json::json!({
                "kind": event_kind,
                "downloaded": downloaded,
                "total": total,
            }),
        );
    })
    .await?;

    let status = components::status(kind, &settings, &paths);
    let _ = app.emit("component-installed", &status);
    Ok(status)
}

/// 打开组件的官方下载页
#[tauri::command]
pub async fn open_component_page(app: AppHandle, kind: String) -> AppResult<()> {
    let kind = ComponentKind::parse(&kind)?;
    app.opener()
        .open_url(kind.manual_url(), None::<&str>)
        .map_err(|e| AppError::Msg(format!("打开下载页失败: {}", e)))
}

// ===== EasyTier 组网 =====

#[tauri::command]
pub async fn get_easytier_status(state: State<'_, AppState>) -> AppResult<EasytierStatus> {
    state.easytier.refresh();
    Ok(state.easytier.status())
}

#[tauri::command]
pub async fn start_easytier(app: AppHandle, state: State<'_, AppState>) -> AppResult<EasytierStatus> {
    let mut settings = state.current_settings();
    // 组网媒体共享通过虚拟网段访问 HTTP 服务，仅本机（127.0.0.1）监听会导致
    // 成员之间（包括查看自己的媒体库）无法连接，启动组网时自动切换为局域网监听
    if settings.listen_mode == ListenMode::Loopback {
        settings.listen_mode = ListenMode::Lan;
        {
            let mut guard = state.settings.write().unwrap();
            *guard = settings.clone();
        }
        settings.save(&state.paths.settings_file)?;
        let server = state.server.clone();
        server.stop();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        server.start(&settings.bind_ip(), settings.port).await?;
        AppState::emit_status(&app, &server);
        tracing::info!("监听模式已自动切换为局域网（组网媒体共享需要）");
    }
    let (core, _) = components::easytier_core_binary(&settings, &state.paths);
    let core = core.ok_or_else(|| {
        AppError::Msg("未检测到 easytier-core，请先在设置中下载或指定 EasyTier 组件".to_string())
    })?;

    let mut cfg = settings.easytier.clone();
    if cfg.member_name.trim().is_empty() {
        cfg.member_name = easytier::default_member_name();
    }
    // KCP 与 QUIC 代理互斥（对齐 EasytierGame）
    if cfg.kcp_proxy && cfg.quic_proxy {
        return Err(AppError::Msg(
            "KCP 代理与 QUIC 代理不能同时开启，请修改组网配置后重试".to_string(),
        ));
    }
    let args = easytier::build_args(&cfg);
    // 提权启动后无法捕获 stdout，core 改写文件日志（排查虚拟网卡/组网问题）
    let core_log_dir = state.paths.log_dir.join("easytier-core");
    let _ = std::fs::create_dir_all(&core_log_dir);
    let mut args = args;
    args.push("--file-log-dir".to_string());
    args.push(core_log_dir.to_string_lossy().into_owned());
    args.push("--file-log-level".to_string());
    args.push("info".to_string());
    let status = state.easytier.start(&core, args).await?;
    // 缓存本机虚拟 IP（配置未指定时由 build URL 时按需查询 route）
    let configured_ip = cfg.ipv4.trim().split('/').next().unwrap_or("").to_string();
    state.easytier.set_virtual_ip(
        if configured_ip.is_empty() {
            None
        } else {
            Some(configured_ip)
        },
    );
    let _ = app.emit("easytier-status-changed", &status);
    Ok(status)
}

#[tauri::command]
pub async fn stop_easytier(app: AppHandle, state: State<'_, AppState>) -> AppResult<EasytierStatus> {
    state.easytier.stop().await;
    let status = state.easytier.status();
    let _ = app.emit("easytier-status-changed", &status);
    Ok(status)
}

/// 读取房间成员信息
#[tauri::command]
pub async fn get_room_info(state: State<'_, AppState>) -> AppResult<RoomInfo> {
    let settings = state.current_settings();
    let (cli, _) = components::easytier_cli_binary(&settings, &state.paths);
    let cli = easytier::cli_path_or_err(cli)?;
    Ok(easytier::fetch_room_info(&cli).await)
}

/// 读取 easytier-core 运行日志（最近一块，过滤后有价值的尾部内容），供组网页面展示。
/// 无日志文件时返回 None。
#[tauri::command]
pub fn read_easytier_log(state: State<'_, AppState>) -> Option<String> {
    let dir = state.paths.log_dir.join("easytier-core");
    let file = easytier::newest_log_file(&dir)?;
    let text = easytier::read_log_tail(&file, 512 * 1024).ok()?;
    Some(easytier::filter_valuable_log(&text, 200))
}

/// 重置网卡：停止组网并清理本地虚拟网卡缓存后重新启动
#[tauri::command]
pub async fn reset_network_adapter(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<EasytierStatus> {
    let was_running = state.easytier.status().running;
    state.easytier.stop().await;
    match easytier::uninstall_tun_adapter().await {
        Ok(0) => tracing::info!("没有需要卸载的 EasyTier 虚拟网卡"),
        Ok(n) => tracing::info!("已卸载 {} 个 EasyTier 虚拟网卡设备", n),
        Err(e) => return Err(e),
    }
    easytier::clear_local_state(&state.paths.data_dir)?;
    tracing::info!("已清理本地 EasyTier 网卡缓存");

    if was_running {
        return start_easytier(app, state).await;
    }
    Ok(state.easytier.status())
}

// ===== 房间内媒体共享 =====

fn peer_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("VRchat-LocalCinema")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Msg(format!("创建网络客户端失败: {}", e)))
}

/// 读取对等节点的媒体清单
#[tauri::command]
pub async fn fetch_peer_media(ip: String, port: u16) -> AppResult<Vec<MediaSummary>> {
    #[derive(serde::Deserialize)]
    struct ApiMediaResponse {
        items: Vec<MediaSummary>,
    }
    let url = format!("http://{}:{}/api/media", ip, port);
    let resp = peer_client()?
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("无法连接 {}：{}", ip, e)))?;
    if !resp.status().is_success() {
        return Err(AppError::Msg(format!(
            "对方媒体库不可用（HTTP {}）",
            resp.status().as_u16()
        )));
    }
    let parsed: ApiMediaResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Msg(format!("解析对方媒体库失败: {}", e)))?;
    Ok(parsed.items)
}

/// 下载专用 HTTP 客户端：大文件下载耗时不确定，不能设置总超时
/// （旧实现 peer_client 的 timeout 会中断超过 15 秒的下载，
/// 导致文件只写了一半、入库逻辑从未执行）
fn peer_download_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("VRchat-LocalCinema")
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Msg(format!("创建网络客户端失败: {}", e)))
}

/// 解析下载目录（设置优先，否则用默认数据目录）
fn download_dir_of(state: &AppState) -> PathBuf {
    let settings = state.current_settings();
    if settings.download_dir.trim().is_empty() {
        state.paths.download_dir.clone()
    } else {
        PathBuf::from(settings.download_dir.trim())
    }
}

/// 下载临时文件路径：与最终文件同目录、后缀 .part，完成后改名为最终文件
fn part_path(dest: &std::path::Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// 加入对端媒体下载任务（后台执行，可在「任务」页查看进度、暂停/继续/取消，
/// 完成后自动登记入库）
#[tauri::command]
pub async fn download_peer_media_task(
    app: AppHandle,
    state: State<'_, AppState>,
    ip: String,
    port: u16,
    id: String,
    file_name: String,
    file_size: u64,
    title: String,
) -> AppResult<crate::convert_queue::ConvertTask> {
    let safe_name = std::path::Path::new(&file_name)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| AppError::Msg("文件名非法".to_string()))?
        .to_string();
    if !url_builder::valid_basename(&safe_name) {
        return Err(AppError::Msg("文件名不能映射为播放 URL".to_string()));
    }
    if !Db::is_supported_video(std::path::Path::new(&safe_name)) {
        return Err(AppError::Msg("不支持的格式".to_string()));
    }

    // 本地已有预检：同名同大小的条目视为已拥有，避免重复下载后入库冲突
    {
        let db = state.db.lock().unwrap();
        let dup = db.list()?.into_iter().find(|m| {
            m.status != MediaStatus::Missing
                && m.file_name.to_lowercase() == safe_name.to_lowercase()
        });
        if let Some(existing) = dup {
            if existing.file_size == file_size {
                return Err(AppError::Msg(
                    "本地已有该文件（同名同大小），无需重复下载".into(),
                ));
            }
        }
    }

    let dir = download_dir_of(&state);
    std::fs::create_dir_all(&dir)?;
    let dest = unique_dest(&dir, &safe_name);

    let (task, cancel_flag, pause_flag) = state
        .convert_queue
        .register_download_task(
            id.clone(),
            title,
            safe_name.clone(),
            ip.clone(),
            port,
            dest.to_string_lossy().to_string(),
        )
        .map_err(AppError::Msg)?;
    tracing::info!("加入下载任务: {} <- {}:{}{}", safe_name, ip, port, id);
    emit_convert_tasks(&app);
    spawn_download_worker(app, task.id.clone(), cancel_flag, pause_flag);
    Ok(task)
}

/// 暂停下载任务（保留 .part 进度，可继续）
#[tauri::command]
pub async fn download_task_pause(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<()> {
    state
        .convert_queue
        .pause_download(&task_id)
        .map_err(AppError::Msg)?;
    emit_convert_tasks(&app);
    Ok(())
}

/// 继续已暂停/失败的下载任务（从 .part 断点续传）
#[tauri::command]
pub async fn download_task_resume(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<()> {
    let (_task, cancel_flag, pause_flag) = state
        .convert_queue
        .resume_download(&task_id)
        .map_err(AppError::Msg)?;
    emit_convert_tasks(&app);
    spawn_download_worker(app, task_id, cancel_flag, pause_flag);
    Ok(())
}

/// 下载会话结束原因
enum DownloadEnd {
    /// 下载完成，.part 已改名为最终文件
    Done,
    Cancelled,
    /// 已暂停（携带已接收字节）
    Paused(u64),
    Failed(String),
}

/// 后台下载执行器：从 .part 已有偏移断点续传，结束后保存进度/入库并刷新任务列表
fn spawn_download_worker(
    app: AppHandle,
    task_id: String,
    cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    pause_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    tauri::async_runtime::spawn(async move {
        run_download_task(app, task_id, cancel_flag, pause_flag).await;
    });
}

/// 执行一次下载会话（从 .part 已有偏移续传），通过 on_progress 上报进度
async fn run_download_session(
    ip: &str,
    port: u16,
    peer_media_id: &str,
    dest: &std::path::Path,
    part: &std::path::Path,
    cancel_flag: &std::sync::atomic::AtomicBool,
    pause_flag: &std::sync::atomic::AtomicBool,
    mut on_progress: impl FnMut(u64, Option<u64>, Option<f64>),
) -> DownloadEnd {
    use std::sync::atomic::Ordering;

    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let mut offset = tokio::fs::metadata(part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let url = format!("http://{}:{}/download/{}", ip, port, peer_media_id);
    let client = match peer_download_client() {
        Ok(c) => c,
        Err(e) => return DownloadEnd::Failed(e.to_string()),
    };

    // 带 Range 头续传；对方返回 416（远端文件已变化）时放弃续传从头下载
    let resp = loop {
        let mut req = client.get(&url);
        if offset > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={}-", offset));
        }
        match req.send().await {
            Ok(r) => {
                let status = r.status();
                if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && offset > 0 {
                    offset = 0;
                    continue;
                }
                if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
                    return DownloadEnd::Failed(format!("下载失败（HTTP {}）", status.as_u16()));
                }
                break r;
            }
            Err(e) => return DownloadEnd::Failed(format!("连接失败: {}", e)),
        }
    };

    let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT && offset > 0;
    if !resumed {
        offset = 0;
    }
    let total = resp.content_length().map(|len| offset + len);

    let mut file = if resumed {
        match tokio::fs::OpenOptions::new().append(true).open(part).await {
            Ok(f) => f,
            Err(e) => return DownloadEnd::Failed(format!("无法写入临时文件: {}", e)),
        }
    } else {
        match tokio::fs::File::create(part).await {
            Ok(f) => f,
            Err(e) => return DownloadEnd::Failed(format!("无法创建临时文件: {}", e)),
        }
    };

    let mut stream = resp.bytes_stream();
    let mut received = offset;
    // 每 300ms 轮询一次取消/暂停标志，避免对端停发数据时操作无响应
    let mut flag_tick = tokio::time::interval(std::time::Duration::from_millis(300));
    loop {
        tokio::select! {
            _ = flag_tick.tick() => {
                if cancel_flag.load(Ordering::Relaxed) {
                    return DownloadEnd::Cancelled;
                }
                if pause_flag.load(Ordering::Relaxed) {
                    let _ = file.flush().await;
                    return DownloadEnd::Paused(received);
                }
            }
            chunk = stream.next() => {
                match chunk {
                    None => break,
                    Some(Err(e)) => return DownloadEnd::Failed(format!("下载中断: {}", e)),
                    Some(Ok(bytes)) => {
                        if let Err(e) = file.write_all(&bytes).await {
                            return DownloadEnd::Failed(format!("写入文件失败: {}", e));
                        }
                        received += bytes.len() as u64;
                        let percent = total
                            .filter(|t| *t > 0)
                            .map(|t| (received as f64 / t as f64 * 100.0).min(100.0));
                        on_progress(received, total, percent);
                    }
                }
            }
        }
    }
    if let Err(e) = file.flush().await {
        return DownloadEnd::Failed(format!("写入文件失败: {}", e));
    }
    drop(file);

    // 改名为最终文件（若最终名期间被占用则顺延序号）
    let final_dest = if dest.exists() {
        let dir = dest.parent().unwrap_or(std::path::Path::new("."));
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "video".into());
        unique_dest(dir, &name)
    } else {
        dest.to_path_buf()
    };
    if let Err(e) = tokio::fs::rename(part, &final_dest).await {
        return DownloadEnd::Failed(format!("保存文件失败: {}", e));
    }
    DownloadEnd::Done
}

/// 下载任务执行器：运行一次下载会话并按结果收尾（完成时登记入库并启动探针）
async fn run_download_task(
    app: AppHandle,
    task_id: String,
    cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    pause_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let Some(st) = app.try_state::<AppState>() else {
        return;
    };
    let queue = st.convert_queue.clone();
    let Some(task) = queue.list().into_iter().find(|t| t.id == task_id) else {
        return;
    };
    let (Some(ip), Some(port), Some(peer_media_id), Some(dest_str)) = (
        task.peer_ip.clone(),
        task.peer_port,
        task.peer_media_id.clone(),
        task.dest_path.clone(),
    ) else {
        queue.finish(
            &task_id,
            crate::convert_queue::TaskState::Error,
            Some("下载任务信息不完整".into()),
        );
        emit_convert_tasks(&app);
        return;
    };
    let dest = PathBuf::from(&dest_str);
    let part = part_path(&dest);

    // 进度上报：更新队列并按 1% 或 300ms 节流广播，避免事件风暴
    let progress_queue = queue.clone();
    let progress_app = app.clone();
    let progress_task_id = task_id.clone();
    let mut last_emit = std::time::Instant::now();
    let mut last_pct = -1i64;
    let end = run_download_session(
        &ip,
        port,
        &peer_media_id,
        &dest,
        &part,
        &cancel_flag,
        &pause_flag,
        move |received, total, percent| {
            let pct = percent.map(|p| p as i64).unwrap_or(-1);
            if last_emit.elapsed() >= std::time::Duration::from_millis(300) || pct != last_pct {
                last_emit = std::time::Instant::now();
                last_pct = pct;
                progress_queue.set_download_progress(&progress_task_id, received, total, percent);
                emit_convert_tasks(&progress_app);
            }
        },
    )
    .await;

    // 会话已返回（文件句柄已释放），此处可安全收尾
    let was_cancel_requested =
        cancel_flag.load(std::sync::atomic::Ordering::Relaxed);
    match end {
        DownloadEnd::Done => {
            let reg = {
                let db = st.db.lock().unwrap();
                db.register(&dest)
            };
            match reg {
                Ok((item, _)) => {
                    queue.finish(&task_id, crate::convert_queue::TaskState::Done, None);
                    tracing::info!("下载完成并入库: {}", item.file_path);
                    spawn_probe_task(app.clone(), item.id.clone());
                    let _ = app.emit("media-updated", &item);
                }
                Err(e) => {
                    queue.finish(
                        &task_id,
                        crate::convert_queue::TaskState::Error,
                        Some(format!("下载完成但入库失败：{}（文件保留在下载目录）", e)),
                    );
                }
            }
        }
        DownloadEnd::Cancelled => {
            let _ = tokio::fs::remove_file(&part).await;
            queue.finish(&task_id, crate::convert_queue::TaskState::Cancelled, None);
        }
        DownloadEnd::Paused(received) => {
            // 暂停与取消请求同时到达时以取消为准
            if was_cancel_requested {
                let _ = tokio::fs::remove_file(&part).await;
                queue.finish(&task_id, crate::convert_queue::TaskState::Cancelled, None);
            } else {
                let total = queue
                    .list()
                    .into_iter()
                    .find(|t| t.id == task_id)
                    .and_then(|t| t.total);
                queue.mark_download_paused(&task_id, received, total);
            }
        }
        DownloadEnd::Failed(msg) => {
            queue.finish(&task_id, crate::convert_queue::TaskState::Error, Some(msg));
        }
    }
    emit_convert_tasks(&app);
}

/// 目标文件重名时追加序号，避免覆盖
fn unique_dest(dir: &std::path::Path, file_name: &str) -> PathBuf {
    let mut candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let path = std::path::Path::new(file_name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".to_string());
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    for i in 1..1000 {
        let name = match &ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        candidate = dir.join(name);
        if !candidate.exists() {
            break;
        }
    }
    candidate
}

// ===== 一键转码 / HLS =====

/// 转码进度事件负载
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ConvertProgress {
    id: String,
    /// transcode（转 MP4）| hls（转 m3u8）
    kind: &'static str,
    /// running | done | error
    status: &'static str,
    percent: Option<f64>,
    error: Option<String>,
    /// 转码完成时附带新登记的媒体条目
    item: Option<MediaItem>,
}

fn emit_convert(app: &AppHandle, payload: ConvertProgress) {
    let _ = app.emit("convert-progress", &payload);
}

/// HLS（边转边播）输出根目录：存放在 data 目录
fn hls_root_of(state: &AppState) -> PathBuf {
    state.paths.data_dir.join("hls")
}

/// 选择唯一的 HLS 播放列表文件名（根路径短链接）；与其它条目重名时追加 id 片段
fn pick_playlist_name(state: &AppState, id: &str, stem: &str) -> String {
    let candidate = format!("{}.m3u8", stem);
    let taken = state
        .db
        .lock()
        .unwrap()
        .resolve_hls_playlist(&candidate)
        .ok()
        .flatten()
        .map(|m| m.id != id)
        .unwrap_or(false);
    if taken {
        format!("{}-{}.m3u8", stem, &id[..8.min(id.len())])
    } else {
        candidate
    }
}

fn hide_console_tokio(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
}

/// 运行 ffmpeg 并解析 `-progress pipe:1` 输出上报进度（out_time_ms 单位为微秒）；
/// 传入 cancel 标志后可随时终止进程（转码队列取消任务用）
async fn run_ffmpeg_with_progress(
    ffmpeg: &PathBuf,
    args: &[String],
    duration_ms: Option<u64>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    mut on_progress: impl FnMut(f64),
) -> AppResult<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut cmd = tokio::process::Command::new(ffmpeg);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    hide_console_tokio(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Msg(format!("无法启动 ffmpeg: {}", e)))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Msg("无法读取 ffmpeg 输出".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Msg("无法读取 ffmpeg 错误输出".into()))?;

    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut collected = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if collected.len() < 8000 {
                collected.push_str(&line);
                collected.push('\n');
            }
        }
        collected
    });

    let mut last_percent = -1.0f64;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        if let Some(flag) = &cancel {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(AppError::Msg("任务已取消".into()));
            }
        }
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(v) = line.strip_prefix("out_time_ms=") {
                    if let Ok(us) = v.trim().parse::<u64>() {
                        if let Some(total) = duration_ms.filter(|d| *d > 0) {
                            let pct = ((us as f64 / 1000.0) / total as f64 * 100.0).min(100.0);
                            // 每 1% 上报一次，避免事件风暴
                            if pct - last_percent >= 1.0 {
                                last_percent = pct;
                                on_progress(pct);
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return Err(AppError::Msg(format!("读取 ffmpeg 进度失败: {}", e))),
        }
    }

    let status = child.wait().await?;
    let err_text = err_task
        .await
        .unwrap_or_else(|_| "无法获取错误详情".to_string());
    if !status.success() {
        let brief: String = err_text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("未知错误")
            .chars()
            .take(300)
            .collect();
        return Err(AppError::Msg(format!(
            "ffmpeg 退出码 {:?}: {}",
            status.code(),
            brief
        )));
    }
    on_progress(100.0);
    Ok(())
}

// ===== 转码任务队列 =====

fn emit_convert_tasks(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        let tasks = state.convert_queue.list();
        let _ = app.emit("convert-tasks-updated", &tasks);
    }
}

/// 构建转码为 MP4（H.264 8-bit yuv420p + AAC）的 ffmpeg 参数；
/// start_sec/dur_sec 用于分段转码（-ss 快速定位起点，-t 限定时长）
fn transcode_args(
    input: &str,
    dest: &std::path::Path,
    start_sec: Option<f64>,
    dur_sec: Option<f64>,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-y".into()];
    if let Some(s) = start_sec {
        args.extend(["-ss".into(), format!("{:.3}", s)]);
    }
    args.extend([
        "-i".into(),
        input.to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "medium".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
    ]);
    if let Some(d) = dur_sec {
        args.extend(["-t".into(), format!("{:.3}", d)]);
    }
    args.push(dest.to_string_lossy().to_string());
    args
}

/// 加入转码队列（按任务序列执行），可在「转码任务」页查看进度、开始或取消；
/// overwrite 为 true 时直接覆盖已存在的转码产物（前端已弹出覆盖确认）
#[tauri::command]
pub async fn transcode_media(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    overwrite: Option<bool>,
) -> AppResult<crate::convert_queue::ConvertTask> {
    let item = get_item_checked(&state, &id)?;
    if state.hls_jobs.read().unwrap().contains(&id) {
        return Err(AppError::Msg("该视频正在边转边播，请等待完成或先取消".into()));
    }
    let task = state
        .convert_queue
        .enqueue(id, item.title, overwrite.unwrap_or(false))
        .map_err(AppError::Msg)?;
    emit_convert_tasks(&app);
    Ok(task)
}

/// 检查该视频的转码产物是否已存在；kind = "segment" 时检查分段产物（任一段存在即为 true）
#[tauri::command]
pub async fn transcode_output_exists(
    state: State<'_, AppState>,
    id: String,
    kind: Option<String>,
) -> AppResult<bool> {
    let item = get_item_checked(&state, &id)?;
    let stem = std::path::Path::new(&item.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".into());
    let dir = state.paths.data_dir.join("converted");
    let names: Vec<String> = if kind.as_deref() == Some("segment") {
        vec![
            format!("{} (分段1).mp4", stem),
            format!("{} (分段2).mp4", stem),
        ]
    } else {
        vec![format!("{} (转码).mp4", stem)]
    };
    Ok(names.iter().any(|n| dir.join(n).exists()))
}

/// 加入分段转码队列：第一段默认总时长 10%（向上取整到 5 分钟），可由前端调整 firstMs；
/// 第一段完成后弹出「复制链接」窗口提醒
#[tauri::command]
pub async fn segment_transcode(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    first_ms: u64,
    overwrite: Option<bool>,
) -> AppResult<crate::convert_queue::ConvertTask> {
    let item = get_item_checked(&state, &id)?;
    if state.hls_jobs.read().unwrap().contains(&id) {
        return Err(AppError::Msg("该视频正在边转边播，请等待完成或先取消".into()));
    }
    if item.duration_ms.is_none() {
        return Err(AppError::Msg("视频时长未知，请先「刷新媒体信息」".into()));
    }
    if first_ms < 10_000 {
        return Err(AppError::Msg("第一段时长过短（至少 10 秒）".into()));
    }
    let task = state
        .convert_queue
        .enqueue_segment(id, item.title, overwrite.unwrap_or(false), first_ms)
        .map_err(AppError::Msg)?;
    emit_convert_tasks(&app);
    Ok(task)
}

/// 列出某视频的分段转码产物（按段序号排序）
#[tauri::command]
pub async fn list_segments(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<Vec<MediaItem>> {
    let db = state.db.lock().unwrap();
    Ok(db.list_segments_of(&id)?)
}

/// 列出全部转码任务
#[tauri::command]
pub async fn list_convert_tasks(
    state: State<'_, AppState>,
) -> AppResult<Vec<crate::convert_queue::ConvertTask>> {
    Ok(state.convert_queue.list())
}

/// 开始任务：将排队中的任务移到队列最前（当前任务完成后立即执行）
#[tauri::command]
pub async fn convert_task_start(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<()> {
    state
        .convert_queue
        .start_now(&task_id)
        .map_err(AppError::Msg)?;
    emit_convert_tasks(&app);
    Ok(())
}

/// 取消任务：排队中直接标记取消；进行中的终止 ffmpeg / 下载；已暂停的下载取消时清理临时文件
#[tauri::command]
pub async fn convert_task_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<()> {
    let before = state
        .convert_queue
        .list()
        .into_iter()
        .find(|t| t.id == task_id);
    state
        .convert_queue
        .cancel(&task_id)
        .map_err(AppError::Msg)?;
    // 已暂停的下载没有执行器在跑，取消时由命令清理 .part 临时文件；
    // 运行中的下载由执行器在感知取消后自行清理
    if let Some(t) = before {
        if t.kind == crate::convert_queue::TaskKind::Download
            && t.state == crate::convert_queue::TaskState::Paused
        {
            if let Some(dest) = t.dest_path.as_deref() {
                let _ = tokio::fs::remove_file(part_path(std::path::Path::new(dest))).await;
            }
        }
    }
    emit_convert_tasks(&app);
    Ok(())
}

/// 移除一条已结束（完成/失败/取消）的任务记录
#[tauri::command]
pub async fn convert_task_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> AppResult<()> {
    state
        .convert_queue
        .remove(&task_id)
        .map_err(AppError::Msg)?;
    emit_convert_tasks(&app);
    Ok(())
}

/// 清除全部已结束的任务记录
#[tauri::command]
pub async fn convert_task_clear(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    state.convert_queue.clear_finished();
    emit_convert_tasks(&app);
    Ok(())
}

/// 执行单个排队转码任务；成功时返回转码产物入库后的新条目
async fn run_queued_transcode(
    app: &AppHandle,
    state: &AppState,
    task: &crate::convert_queue::ConvertTask,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> AppResult<Option<MediaItem>> {
    let item = {
        let db = state.db.lock().unwrap();
        db.get(&task.media_id)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    if !std::path::Path::new(&item.file_path).is_file() {
        return Err(AppError::Msg("文件已丢失或无法访问".into()));
    }
    let settings = state.current_settings();
    let (ffmpeg, _) = components::ffmpeg_binary(&settings, &state.paths);
    let ffmpeg = ffmpeg
        .ok_or_else(|| AppError::Msg("未检测到 ffmpeg，请先在设置中下载或指定 FFmpeg 组件".into()))?;

    // 转码产物统一存放在 data 目录
    let dir = state.paths.data_dir.join("converted");
    std::fs::create_dir_all(&dir)?;
    let stem = std::path::Path::new(&item.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".into());
    let out_name = format!("{} (转码).mp4", stem);
    // 用户确认覆盖时直接写同名文件（ffmpeg -y），否则追加序号避免覆盖
    let dest = if task.overwrite {
        dir.join(&out_name)
    } else {
        unique_dest(&dir, &out_name)
    };

    let args = transcode_args(&item.file_path, &dest, None, None);

    let progress_app = app.clone();
    let progress_media_id = task.media_id.clone();
    let progress_task_id = task.id.clone();
    let progress_queue = state.convert_queue.clone();
    let result = run_ffmpeg_with_progress(
        &ffmpeg,
        &args,
        item.duration_ms,
        Some(cancel),
        move |pct| {
            progress_queue.set_percent(&progress_task_id, pct);
            emit_convert(
                &progress_app,
                ConvertProgress {
                    id: progress_media_id.clone(),
                    kind: "transcode",
                    status: "running",
                    percent: Some(pct),
                    error: None,
                    item: None,
                },
            );
        },
    )
    .await;

    if let Err(e) = result {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }

    // 源条目视为已处理；保留在「不兼容」文件夹原位置，由转码产物的快捷方式卡片承接跳转
    let updated = {
        let db = state.db.lock().unwrap();
        let _ = db.set_compat_resolved(&task.media_id, true);
        db.get(&task.media_id).ok().flatten()
    };
    if let Some(u) = updated {
        let _ = app.emit("media-updated", &u);
    }

    // 转码产物入库（继承原名，含 (转码) 后缀），并记录与源视频的关联
    let registered = {
        let db = state.db.lock().unwrap();
        match db.register(&dest) {
            Ok((new_item, _)) => {
                let _ = db.set_source_media(&new_item.id, &task.media_id);
                db.get(&new_item.id).ok().flatten()
            }
            Err(e) => {
                tracing::error!("转码产物入库失败: {}", e);
                None
            }
        }
    };
    match registered {
        Some(new_item) => {
            spawn_probe_task(app.clone(), new_item.id.clone());
            Ok(Some(new_item))
        }
        None => Ok(None),
    }
}

/// 分段转码默认第一段时长：总时长的 10% 向上取整到 5 分钟（如 10、15、20 分钟）
fn default_first_segment_ms(total_ms: u64) -> u64 {
    const FIVE_MIN: u64 = 300_000;
    let raw = (total_ms as f64 * 0.1).ceil() as u64;
    ((raw + FIVE_MIN - 1) / FIVE_MIN) * FIVE_MIN
}

/// 转码单个分段并入库（登记源关联与段序号），base_pct/scale_pct 用于映射整体进度
#[allow(clippy::too_many_arguments)]
async fn transcode_segment(
    app: &AppHandle,
    state: &AppState,
    ffmpeg: &std::path::PathBuf,
    source: &MediaItem,
    dest: &std::path::Path,
    start_sec: Option<f64>,
    dur_sec: Option<f64>,
    pass_duration_ms: Option<u64>,
    base_pct: f64,
    scale_pct: f64,
    task: &crate::convert_queue::ConvertTask,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    segment_index: i64,
) -> AppResult<MediaItem> {
    let args = transcode_args(&source.file_path, dest, start_sec, dur_sec);
    let progress_app = app.clone();
    let progress_media_id = task.media_id.clone();
    let progress_task_id = task.id.clone();
    let progress_queue = state.convert_queue.clone();
    let result = run_ffmpeg_with_progress(
        ffmpeg,
        &args,
        pass_duration_ms,
        Some(cancel),
        move |pct| {
            let overall = base_pct + pct * scale_pct;
            progress_queue.set_percent(&progress_task_id, overall);
            emit_convert(
                &progress_app,
                ConvertProgress {
                    id: progress_media_id.clone(),
                    kind: "transcode",
                    status: "running",
                    percent: Some(overall),
                    error: None,
                    item: None,
                },
            );
        },
    )
    .await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(dest);
        return Err(e);
    }

    // 分段产物入库：登记源关联与段序号
    let registered = {
        let db = state.db.lock().unwrap();
        match db.register(dest) {
            Ok((new_item, _)) => {
                let _ = db.set_source_media(&new_item.id, &task.media_id);
                let _ = db.set_segment_index(&new_item.id, segment_index);
                db.get(&new_item.id).ok().flatten()
            }
            Err(e) => {
                tracing::error!("分段产物入库失败: {}", e);
                None
            }
        }
    };
    let seg = registered.ok_or_else(|| AppError::Msg("分段产物入库失败".into()))?;
    spawn_probe_task(app.clone(), seg.id.clone());
    let _ = app.emit("media-updated", &seg);
    Ok(seg)
}

/// 执行单个分段转码任务：第一段（先看，完成即弹窗提醒复制）+ 第二段（剩余部分）
async fn run_queued_segment_transcode(
    app: &AppHandle,
    state: &AppState,
    task: &crate::convert_queue::ConvertTask,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> AppResult<Option<MediaItem>> {
    let item = {
        let db = state.db.lock().unwrap();
        db.get(&task.media_id)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    if !std::path::Path::new(&item.file_path).is_file() {
        return Err(AppError::Msg("文件已丢失或无法访问".into()));
    }
    let settings = state.current_settings();
    let (ffmpeg, _) = components::ffmpeg_binary(&settings, &state.paths);
    let ffmpeg = ffmpeg
        .ok_or_else(|| AppError::Msg("未检测到 ffmpeg，请先在设置中下载或指定 FFmpeg 组件".into()))?;
    let Some(total) = item.duration_ms else {
        return Err(AppError::Msg("视频时长未知，请先「刷新媒体信息」".into()));
    };

    let first_ms = task
        .first_ms
        .unwrap_or_else(|| default_first_segment_ms(total))
        .min(total);
    if first_ms >= total {
        // 视频过短不足以分段，按整段转码处理
        return run_queued_transcode(app, state, task, cancel).await;
    }

    let dir = state.paths.data_dir.join("converted");
    std::fs::create_dir_all(&dir)?;
    let stem = std::path::Path::new(&item.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".into());
    let first_sec = first_ms as f64 / 1000.0;
    let overwrite = task.overwrite;

    // 第一段：0 ~ first_ms，完成即登记并弹出复制链接提醒
    let seg1_name = format!("{} (分段1).mp4", stem);
    let seg1_dest = if overwrite {
        dir.join(&seg1_name)
    } else {
        unique_dest(&dir, &seg1_name)
    };
    let seg1 = transcode_segment(
        app,
        state,
        &ffmpeg,
        &item,
        &seg1_dest,
        None,
        Some(first_sec),
        Some(first_ms),
        0.0,
        0.5,
        task,
        cancel.clone(),
        1,
    )
    .await?;
    // 通知前端弹出「复制链接」窗口（分段播放）
    let _ = app.emit("segment-ready", &seg1);

    // 段间取消检查：第二段尚未开始，保留已完成的第一段
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(AppError::Msg("任务已取消（第一段已完成并保留）".into()));
    }

    // 第二段：first_ms ~ 结尾
    let seg2_name = format!("{} (分段2).mp4", stem);
    let seg2_dest = if overwrite {
        dir.join(&seg2_name)
    } else {
        unique_dest(&dir, &seg2_name)
    };
    transcode_segment(
        app,
        state,
        &ffmpeg,
        &item,
        &seg2_dest,
        Some(first_sec),
        None,
        Some(total - first_ms),
        0.5,
        0.5,
        task,
        cancel,
        2,
    )
    .await?;

    // 源条目视为已处理；保留在原位置，由分段产物卡片承接复制链接
    let updated = {
        let db = state.db.lock().unwrap();
        let _ = db.set_compat_resolved(&task.media_id, true);
        db.get(&task.media_id).ok().flatten()
    };
    if let Some(u) = updated {
        let _ = app.emit("media-updated", &u);
    }
    Ok(None)
}

/// 转码队列执行器：单 worker 顺序执行排队中的任务
pub fn spawn_convert_worker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let Some(state) = app.try_state::<AppState>() else {
                continue;
            };
            let Some((task, cancel_flag)) = state.convert_queue.take_next() else {
                continue;
            };
            emit_convert_tasks(&app);

            let result = match task.kind {
                crate::convert_queue::TaskKind::Transcode => {
                    run_queued_transcode(&app, &state, &task, cancel_flag.clone()).await
                }
                crate::convert_queue::TaskKind::Segment => {
                    run_queued_segment_transcode(&app, &state, &task, cancel_flag.clone()).await
                }
                // HLS 已停用，不会再有排队任务
                crate::convert_queue::TaskKind::Hls => Ok(None),
                // 下载任务不占用转码 worker（直接以 Running 注册，由独立执行器处理）
                crate::convert_queue::TaskKind::Download => Ok(None),
            };
            let cancelled = cancel_flag.load(std::sync::atomic::Ordering::Relaxed);
            match result {
                Ok(new_item) => {
                    state
                        .convert_queue
                        .finish(&task.id, crate::convert_queue::TaskState::Done, None);
                    emit_convert(
                        &app,
                        ConvertProgress {
                            id: task.media_id.clone(),
                            kind: "transcode",
                            status: "done",
                            percent: Some(100.0),
                            error: None,
                            item: new_item,
                        },
                    );
                }
                Err(e) => {
                    let (task_state, msg) = if cancelled {
                        (
                            crate::convert_queue::TaskState::Cancelled,
                            "任务已取消".to_string(),
                        )
                    } else {
                        (crate::convert_queue::TaskState::Error, e.to_string())
                    };
                    state
                        .convert_queue
                        .finish(&task.id, task_state, Some(msg.clone()));
                    emit_convert(
                        &app,
                        ConvertProgress {
                            id: task.media_id.clone(),
                            kind: "transcode",
                            status: "error",
                            percent: None,
                            error: Some(msg),
                            item: None,
                        },
                    );
                }
            }
            emit_convert_tasks(&app);
        }
    });
}

/// 边转边播（HLS）：后台转码，首个分片写入播放列表后立即返回根路径短链接
#[tauri::command]
pub async fn make_hls(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    mode: UrlMode,
) -> AppResult<String> {
    {
        let mut jobs = state.hls_jobs.write().unwrap();
        if !jobs.insert(id.clone()) {
            return Err(AppError::Msg("该视频正在生成边转边播（HLS）".into()));
        }
    }
    // 与转码队列互斥：同一视频不允许同时排队转码和边转边播
    if state.convert_queue.has_active(&id) {
        state.hls_jobs.write().unwrap().remove(&id);
        return Err(AppError::Msg("该视频已有进行中的转码任务，请先在「转码任务」页取消".into()));
    }

    let result = (|| async {
        let item = get_item_checked(&state, &id)?;
        let settings = state.current_settings();
        let (ffmpeg, _) = components::ffmpeg_binary(&settings, &state.paths);
        let ffmpeg = ffmpeg
            .ok_or_else(|| AppError::Msg("未检测到 ffmpeg，请先在设置中下载或指定 FFmpeg 组件".into()))?;

        let hls_dir = hls_root_of(&state).join(&id);
        std::fs::create_dir_all(&hls_dir)?;
        // 清理旧的分片文件
        if let Ok(entries) = std::fs::read_dir(&hls_dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }

        let seg_pattern = hls_dir.join("seg%03d.ts");
        let playlist = hls_dir.join("index.m3u8");
        let input = item.file_path.clone();
        let duration_ms = item.duration_ms;
        let args: Vec<String> = vec![
            "-y".into(),
            "-i".into(),
            input,
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a:0".into(),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "20".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-f".into(),
            "hls".into(),
            "-hls_time".into(),
            "4".into(),
            // event 模式：每写完一个分片就重写 index.m3u8（VOD 模式只在全部完成后写出，
            // 会导致「分片已生成但播放列表迟迟没有内容」）
            "-hls_list_size".into(),
            "0".into(),
            "-hls_playlist_type".into(),
            "event".into(),
            "-hls_segment_filename".into(),
            seg_pattern.to_string_lossy().to_string(),
            "-progress".into(),
            "pipe:1".into(),
            "-nostats".into(),
            playlist.to_string_lossy().to_string(),
        ];

        // 注册到转码任务队列：在「转码任务」页展示进度并支持取消（不占用转码 worker）
        let (hls_task, cancel_flag) = state
            .convert_queue
            .start_hls_task(&id, item.title.clone())
            .map_err(AppError::Msg)?;
        emit_convert_tasks(&app);
        let hls_task_id = hls_task.id.clone();

        let progress_app = app.clone();
        let progress_id = id.clone();
        let ffmpeg_path = ffmpeg.clone();
        let bg_task_id = hls_task_id.clone();
        let bg_cancel = cancel_flag.clone();
        // 后台失败信息（供主流程轮询提前退出）
        let shared_err: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let err_slot = shared_err.clone();
        tauri::async_runtime::spawn(async move {
            emit_convert(
                &progress_app,
                ConvertProgress {
                    id: progress_id.clone(),
                    kind: "hls",
                    status: "running",
                    percent: None,
                    error: None,
                    item: None,
                },
            );

            let run_app = progress_app.clone();
            let run_id = progress_id.clone();
            let cb_app = run_app.clone();
            let pct_app = progress_app.clone();
            let pct_task_id = bg_task_id.clone();
            let result = run_ffmpeg_with_progress(
                &ffmpeg_path,
                &args,
                duration_ms,
                Some(bg_cancel.clone()),
                move |pct| {
                    // 同步进度到转码任务队列（「转码任务」页展示）
                    if let Some(st) = pct_app.try_state::<AppState>() {
                        st.convert_queue.set_percent(&pct_task_id, pct);
                    }
                    emit_convert(
                        &cb_app,
                        ConvertProgress {
                            id: run_id.clone(),
                            kind: "hls",
                            status: "running",
                            percent: Some(pct),
                            error: None,
                            item: None,
                        },
                    );
                },
            )
            .await;

            match result {
                Ok(()) => {
                    // HLS 全部转码完成后才视为已处理：标记并移出「不兼容」文件夹
                    {
                        let st = progress_app.state::<AppState>();
                        let updated = {
                            let d = st.db.lock().unwrap();
                            let _ = d.set_compat_resolved(&progress_id, true);
                            let _ = d.set_media_folder(&progress_id, None);
                            d.get(&progress_id).ok().flatten()
                        };
                        if let Some(u) = updated {
                            let _ = progress_app.emit("media-updated", &u);
                        }
                        st.convert_queue
                            .finish_if_running(&bg_task_id, crate::convert_queue::TaskState::Done, None);
                        emit_convert_tasks(&progress_app);
                    }
                    emit_convert(
                        &progress_app,
                        ConvertProgress {
                            id: progress_id.clone(),
                            kind: "hls",
                            status: "done",
                            percent: Some(100.0),
                            error: None,
                            item: None,
                        },
                    );
                }
                Err(e) => {
                    *err_slot.lock().unwrap() = Some(e.to_string());
                    // 清理半成品并回滚登记
                    let st = progress_app.state::<AppState>();
                    let dir = hls_root_of(&st).join(&progress_id);
                    let _ = std::fs::remove_dir_all(&dir);
                    {
                        let d = st.db.lock().unwrap();
                        let _ = d.clear_hls_playlist(&progress_id);
                        let updated = d.get(&progress_id).ok().flatten();
                        if let Some(u) = updated {
                            let _ = progress_app.emit("media-updated", &u);
                        }
                    }
                    // 区分用户取消与真实失败，同步转码任务队列状态
                    let (task_state, task_err) =
                        if bg_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            (
                                crate::convert_queue::TaskState::Cancelled,
                                "任务已取消".to_string(),
                            )
                        } else {
                            (crate::convert_queue::TaskState::Error, e.to_string())
                        };
                    st.convert_queue
                        .finish_if_running(&bg_task_id, task_state, Some(task_err));
                    emit_convert_tasks(&progress_app);
                    emit_convert(
                        &progress_app,
                        ConvertProgress {
                            id: progress_id.clone(),
                            kind: "hls",
                            status: "error",
                            percent: None,
                            error: Some(e.to_string()),
                            item: None,
                        },
                    );
                }
            }

            if let Some(st) = progress_app.try_state::<AppState>() {
                st.hls_jobs.write().unwrap().remove(&progress_id);
            }
        });

        // 轮询等待首个分片写入播放列表（最长 3 分钟）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Some(err) = shared_err.lock().unwrap().clone() {
                return Err(AppError::Msg(err));
            }
            if let Ok(content) = std::fs::read_to_string(&playlist) {
                let has_segment = content.lines().any(|l| {
                    let t = l.trim();
                    !t.is_empty() && !t.starts_with('#') && t.to_ascii_lowercase().ends_with(".ts")
                });
                if has_segment {
                    break;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::Msg(
                    "等待首个分片超时，请确认视频文件可正常解码".into(),
                ));
            }
        }

        // 就绪：登记根路径短链接 + 移出「不兼容」文件夹
        // （分片路径改写由 HTTP 层动态完成，静态改写会被 ffmpeg 逐段重写覆盖）
        let stem = {
            let db = state.db.lock().unwrap();
            db.get(&id)
                .ok()
                .flatten()
                .and_then(|i| {
                    std::path::Path::new(&i.file_name)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| id.clone())
        };
        let playlist_name = pick_playlist_name(&state, &id, &stem);
        {
            let d = state.db.lock().unwrap();
            let _ = d.set_hls_playlist(&id, &playlist_name);
            let updated = d.get(&id).ok().flatten();
            if let Some(u) = updated {
                let _ = app.emit("media-updated", &u);
            }
        }
        tracing::info!("边转边播（HLS）已可播放：/{}", playlist_name);

        // 链接已就绪，允许后续重新生成；后台任务完成时幂等移除
        state.hls_jobs.write().unwrap().remove(&id);

        let host = match mode {
            UrlMode::Localhost => "127.0.0.1".to_string(),
            UrlMode::Lan => resolve_lan_ip(&state).await.ok_or_else(|| {
                AppError::Msg("未检测到可用的局域网 IPv4 地址，请确认已连接局域网".into())
            })?,
            UrlMode::Easytier => {
                return Err(AppError::Msg("该地址模式已停用，请使用本地模式或局域网模式".into()))
            }
        };
        Ok(format!(
            "http://{}:{}/{}",
            host,
            settings.port,
            url_builder::encode_segment(&playlist_name)
        ))
    })()
    .await;

    if let Err(e) = result {
        state.hls_jobs.write().unwrap().remove(&id);
        return Err(e);
    }
    result
}

/// HLS 是否已生成
#[tauri::command]
pub async fn get_hls_status(state: State<'_, AppState>, id: String) -> AppResult<bool> {
    let generating = state.hls_jobs.read().unwrap().contains(&id);
    if generating {
        return Ok(false);
    }
    Ok(hls_root_of(&state)
        .join(&id)
        .join("index.m3u8")
        .is_file())
}

/// 构建 HLS 播放链接（根路径短链接，如 http://host:port/MyVideo.m3u8）
#[tauri::command]
pub async fn build_hls_url(
    state: State<'_, AppState>,
    id: String,
    mode: UrlMode,
) -> AppResult<String> {
    let playlist = {
        let db = state.db.lock().unwrap();
        let item = db
            .get(&id)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?;
        let exists = hls_root_of(&state)
            .join(&item.id)
            .join("index.m3u8")
            .is_file();
        if exists {
            item.hls_playlist
                .clone()
                .ok_or_else(|| AppError::Msg("尚未生成边转边播（HLS），请先生成".into()))
        } else {
            Err(AppError::Msg("尚未生成边转边播（HLS），请先生成".into()))
        }?
    };
    let settings = state.current_settings();
    let host = match mode {
        UrlMode::Localhost => "127.0.0.1".to_string(),
        UrlMode::Lan => resolve_lan_ip(&state).await.ok_or_else(|| {
            AppError::Msg("未检测到可用的局域网 IPv4 地址，请确认已连接局域网".into())
        })?,
        UrlMode::Easytier => {
            return Err(AppError::Msg("该地址模式已停用，请使用本地模式或局域网模式".into()))
        }
    };
    Ok(format!(
        "http://{}:{}/{}",
        host,
        settings.port,
        url_builder::encode_segment(&playlist)
    ))
}

// ---------- 媒体库文件夹管理（仅映射，不影响磁盘文件） ----------

#[tauri::command]
pub async fn list_folders(state: State<'_, AppState>) -> AppResult<Vec<crate::media_library::Folder>> {
    Ok(state.db.lock().unwrap().list_folders()?)
}

#[tauri::command]
pub async fn create_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> AppResult<crate::media_library::Folder> {
    Ok(state
        .db
        .lock()
        .unwrap()
        .create_folder(&name, parent_id.as_deref())?)
}

#[tauri::command]
pub async fn rename_folder(state: State<'_, AppState>, id: String, name: String) -> AppResult<()> {
    Ok(state.db.lock().unwrap().rename_folder(&id, &name)?)
}

#[tauri::command]
pub async fn delete_folder(state: State<'_, AppState>, id: String) -> AppResult<()> {
    Ok(state.db.lock().unwrap().delete_folder(&id)?)
}

#[tauri::command]
pub async fn rename_media(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> AppResult<MediaItem> {
    let item = state.db.lock().unwrap().rename_media(&id, &name)?;
    Ok(item)
}

#[tauri::command]
pub async fn move_media(
    state: State<'_, AppState>,
    id: String,
    folder_id: Option<String>,
) -> AppResult<MediaItem> {
    {
        let db = state.db.lock().unwrap();
        db.set_media_folder(&id, folder_id.as_deref())?;
        let item = db.get(&id)?.ok_or_else(|| AppError::Msg("媒体条目不存在".into()))?;
        Ok(item)
    }
}
