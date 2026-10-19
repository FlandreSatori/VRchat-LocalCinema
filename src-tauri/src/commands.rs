use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use crate::tunnel::TunnelStatus;
use crate::url_builder::{self, UrlInput};

pub struct AppState {
    pub paths: AppPaths,
    pub settings: RwLock<Settings>,
    pub db: Arc<Mutex<Db>>,
    pub server: HttpServer,
    /// HTTP 重配与隧道启停共用，避免端口切换时仍启动指向旧端口的隧道。
    pub network_operation: tokio::sync::Mutex<()>,
    pub recent_logs: RecentLogs,
    /// 是否允许对等节点查看本机媒体库（与设置同步，供 HTTP 服务读取）
    pub allow_peer_view: Arc<AtomicBool>,
    /// 媒体流读缓冲（KB），与设置同步，供 HTTP 服务读取
    pub stream_buf_kb: Arc<AtomicUsize>,
    /// 监听模式是否由「启动内置 EasyTier 组网」自动切换为局域网；
    /// 组网停止时据此自动恢复为本地模式，用户手动设置的局域网模式不受影响
    pub easytier_forced_lan: AtomicBool,
    pub easytier: easytier::EasytierManager,
    /// Cloudflare 快速隧道（无需组网即可公网分享）
    pub tunnel: crate::tunnel::TunnelManager,
    /// 转码任务队列（按任务序列执行）
    pub convert_queue: crate::convert_queue::ConvertQueue,
    /// 正在进行的 HLS 转码任务（媒体 id）
    pub hls_jobs: RwLock<HashSet<String>>,
}

impl AppState {
    pub fn current_settings(&self) -> Settings {
        self.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
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
    let _network = state.network_operation.lock().await;
    if settings.port == 0 {
        return Err(AppError::Msg("端口必须在 1-65535 之间".to_string()));
    }
    settings.normalize_network_mode();
    // 内置组网运行中且局域网监听是自动切换来的 → 不允许手动切回本地模式（否则组网成员无法访问）
    if settings.listen_mode == ListenMode::Loopback
        && state.easytier_forced_lan.load(Ordering::SeqCst)
    {
        return Err(AppError::Msg(
            "内置组网（EasyTier）运行中，媒体共享需保持局域网监听模式；请先在「组网」页停止组网，再切换为本地模式"
                .to_string(),
        ));
    }
    let old = state.current_settings();
    // 「允许他人查看」切换会改变实际绑定的监听地址（关闭时强制只监听环回地址），因此需重建服务
    let net_changed = old.bind_ip() != settings.bind_ip()
        || old.port != settings.port
        || old.allow_peer_view != settings.allow_peer_view;
    let autostart_changed = old.autostart != settings.autostart;

    // 同步「允许他人查看媒体库」的入站防火墙规则：开启（或端口变化）需要提权放行；
    // 关闭时尽力移除（应用层已不再对外返回清单，移除失败不影响功能）。
    let need_allow =
        settings.allow_peer_view && (!old.allow_peer_view || old.port != settings.port);
    if need_allow {
        apply_firewall_rule(&state.paths.data_dir, settings.port, true).await?;
    } else if old.allow_peer_view && !settings.allow_peer_view {
        if let Err(e) = apply_firewall_rule(&state.paths.data_dir, old.port, false).await {
            tracing::warn!("移除媒体库入站防火墙规则失败: {}", e);
        }
    }

    // 端口改变会令现有隧道回源地址失效；关闭旧隧道，由用户手动重新开启。
    if old.port != settings.port && state.tunnel.status().running {
        state.tunnel.stop(&app).await?;
        tracing::info!("HTTP 端口变化，已关闭公网隧道，请重新启动以生成新链接");
    }

    {
        let mut guard = state.settings.write().unwrap_or_else(|e| e.into_inner());
        *guard = settings.clone();
    }
    state
        .allow_peer_view
        .store(settings.allow_peer_view, Ordering::Relaxed);
    state
        .stream_buf_kb
        .store(settings.stream_buffer_kb as usize, Ordering::Relaxed);
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
        if result.is_err() {
            state.tunnel.stop(&app).await?;
        }
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
    let _network = state.network_operation.lock().await;
    let settings = state.current_settings();
    let server = state.server.clone();
    let r = server.start(&settings.bind_ip(), settings.port).await;
    AppState::emit_status(&app, &server);
    r.map(|_| server.status())
}

#[tauri::command]
pub async fn stop_server(app: AppHandle, state: State<'_, AppState>) -> AppResult<ServerStatus> {
    let _network = state.network_operation.lock().await;
    state.tunnel.stop(&app).await?;
    state.server.stop();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    AppState::emit_status(&app, &state.server);
    Ok(state.server.status())
}

#[tauri::command]
pub async fn restart_server(app: AppHandle, state: State<'_, AppState>) -> AppResult<ServerStatus> {
    let _network = state.network_operation.lock().await;
    let settings = state.current_settings();
    let server = state.server.clone();
    server.stop();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let result = server.start(&settings.bind_ip(), settings.port).await;
    AppState::emit_status(&app, &server);
    if result.is_err() {
        state.tunnel.stop(&app).await?;
    }
    result?;
    Ok(server.status())
}

#[tauri::command]
pub async fn list_media(state: State<'_, AppState>) -> AppResult<Vec<MediaItem>> {
    let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
    let mut items = db.list()?;
    // 轻量级状态检查：仅 stat 文件存在性（在线流媒体条目不落盘，跳过）
    for item in items.iter_mut() {
        if item.source_kind == "online" {
            continue;
        }
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
            let db = db.lock().unwrap_or_else(|e| e.into_inner());
            match db.get(&id) {
                Ok(Some(v)) => v,
                _ => return,
            }
        };
        let file_path = PathBuf::from(&item.file_path);

        if !file_path.is_file() {
            let db = db.lock().unwrap_or_else(|e| e.into_inner());
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
            let db = db.lock().unwrap_or_else(|e| e.into_inner());
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
                        let db = db.lock().unwrap_or_else(|e| e.into_inner());
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
                        .unwrap_or_else(|e| e.into_inner())
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
    let result = state.db.lock().unwrap_or_else(|e| e.into_inner()).import_paths(&file_paths);
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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

/// 把在线视频标题整理为合法的媒体库文件名主体（去掉 Windows 非法字符、限长）
fn sanitize_online_title(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut s = cleaned.trim().trim_matches(|c| c == '.' || c == ' ').to_string();
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }
    if s.is_empty() {
        s = "在线视频".to_string();
    }
    s.chars().take(80).collect()
}

/// 通过 B 站链接导入在线视频：解析官方单文件 MP4 直链并生成与本地视频一致的媒体卡片。
///
/// - 免登录上限 720P；更高清晰度需用户自行用 JJdown 下载后导入成本地视频。
/// - 在线条目播放走 HTTP 服务的 `/online/{id}` 代理，只生成 127.0.0.1 链接。
#[tauri::command]
pub async fn import_online_video(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> AppResult<MediaItem> {
    let bvid = crate::bilibili::parse_bvid(&url).ok_or_else(|| {
        AppError::Msg("未能识别 BV 号，请粘贴 B 站视频链接（形如 https://www.bilibili.com/video/BV...）".to_string())
    })?;
    let info = crate::bilibili::fetch_video_info(&bvid).await?;
    // 先解析一次直链以校验可播放，并取得文件大小与时长
    let link = crate::bilibili::resolve_play_url(&info.bvid, info.cid).await?;
    let key = crate::bilibili::online_key(&info.bvid, info.cid);
    // 同一视频（同一分 P）重复导入时直接复用既有条目
    {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = db.get_by_path(&key)? {
            return Ok(existing);
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    // 封面下载失败不影响导入（卡片回退为占位图）
    let mut thumb_name: Option<String> = None;
    if !info.cover_url.trim().is_empty() {
        let name = format!("{}.jpg", id);
        let out = thumb_dir_of(&state).join(&name);
        match crate::bilibili::fetch_cover_to(&info.cover_url, &out).await {
            Ok(()) => thumb_name = Some(name),
            Err(e) => tracing::warn!("下载在线视频封面失败: {}", e),
        }
    }

    let stem = sanitize_online_title(&info.title);
    let file_name = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut candidate = format!("{}.mp4", stem);
        let mut n = 1;
        while !db.find_by_name(&candidate)?.is_empty() {
            n += 1;
            candidate = format!("{}-{}.mp4", stem, n);
        }
        candidate
    };

    let item = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        let (item, created) = db.register_online(
            &id,
            &info.title,
            &file_name,
            &key,
            link.size,
            Some(info.duration_ms),
            thumb_name,
            &url,
        )?;
        if !created {
            // 并发下已被其它请求登记：删除本次下载、未被引用的封面文件
            let _ = std::fs::remove_file(thumb_dir_of(&state).join(format!("{}.jpg", id)));
            return Ok(item);
        }
        item
    };
    let _ = app.emit("media-updated", &item);
    tracing::info!(
        "已导入在线视频: {} ({}) 质量={} 大小={}",
        item.title,
        key,
        link.quality,
        link.size
    );
    Ok(item)
}

async fn refresh_media_internal(
    app: &AppHandle,
    state: &State<'_, AppState>,
    id: &str,
) -> AppResult<MediaItem> {
    let item = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db.get(id)?.ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    if item.source_kind == "online" {
        // 在线条目无本机文件，无需重新探测（直链在播放时按需刷新）
        return Ok(item);
    }
    let path = PathBuf::from(&item.file_path);
    if !path.is_file() {
        let updated = {
            let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
            db.set_status(id, MediaStatus::Missing)?;
            db.get(id)?
                .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
    // id 会被拼进封面文件名，先校验其形态，避免路径逃逸
    if !is_safe_media_id(&id) {
        return Err(AppError::Msg("媒体 id 非法".to_string()));
    }
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
    // 先确认条目存在，再写入封面文件
    {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        if db.get(&id)?.is_none() {
            return Err(AppError::Msg("媒体条目不存在".to_string()));
        }
    }
    let thumb_name = format!("{}.{}", id, ext);
    let out = thumb_dir_of(&state).join(&thumb_name);
    thumbnail::import_custom_thumbnail(&src, &out)?;
    let updated = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db.update_after_probe(&id, None, Some(thumb_name), MediaStatus::Ok, None, None)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?
    };
    let _ = app.emit("media-updated", &updated);
    Ok(updated)
}

/// 媒体 id 形如 UUID（36 位十六进制 + 连字符），用于拼接文件名前的安全校验
fn is_safe_media_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
    let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
    let item = db
        .get(id)?
        .ok_or_else(|| AppError::Msg("媒体条目不存在".to_string()))?;
    if item.source_kind == "online" {
        return Err(AppError::Msg(
            "在线视频没有本机文件；请复制播放链接供 VRChat 播放，或用 JJdown 下载后导入成本地视频".to_string(),
        ));
    }
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
pub async fn is_first_launch(state: State<'_, AppState>) -> AppResult<bool> {
    // 检查免责声明标记文件是否存在
    let marker_file = state.paths.data_dir.join(".disclaimer_accepted");
    Ok(!marker_file.exists())
}

#[tauri::command]
pub async fn mark_disclaimer_accepted(state: State<'_, AppState>) -> AppResult<()> {
    // 创建免责声明标记文件
    let marker_file = state.paths.data_dir.join(".disclaimer_accepted");
    std::fs::write(marker_file, "")?;
    Ok(())
}

/// 判断可执行文件是否位于系统临时目录（常见于直接在压缩包预览里双击运行、
/// 或解压工具「在临时目录中打开」）。此时解压往往不完整，且数据目录会随临时目录被清理。
fn running_in_temp_dir(exe: &Path) -> bool {
    let (Ok(exe), Ok(temp)) = (exe.canonicalize(), std::env::temp_dir().canonicalize()) else {
        return false;
    };
    exe.starts_with(temp)
}

#[tauri::command]
pub async fn is_running_in_temp_dir() -> AppResult<bool> {
    Ok(running_in_temp_dir(&std::env::current_exe()?))
}

/// 退出应用（与托盘菜单「退出」一致：先停掉后台服务再退出）
#[tauri::command]
pub async fn quit_app(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    state.server.stop();
    state.easytier.stop_detached();
    state.tunnel.stop_detached();
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn open_data_dir(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let dir = state.paths.data_dir.clone();
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| AppError::Msg(format!("打开配置目录失败: {}", e)))
}

/// 打开任意数据目录（不存在时先创建），供「关于」界面的各数据路径一键打开
#[tauri::command]
pub async fn open_dir(app: AppHandle, path: String) -> AppResult<()> {
    let dir = PathBuf::from(&path);
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    app.opener()
        .open_path(dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| AppError::Msg(format!("打开目录失败: {}", e)))
}

#[tauri::command]
pub async fn clear_thumbnail_cache(state: State<'_, AppState>) -> AppResult<u64> {
    let dir = thumb_dir_of(&state);
    let mut removed: u64 = 0;
    // 仅清理当前媒体库未引用的缓存
    let used: std::collections::HashSet<String> = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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

/// 计算文件 BLAKE3（流式分块读取，避免占用大量内存；比 SHA-256 更快）
pub(crate) async fn hash_file(path: &std::path::Path) -> Option<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path).ok()?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = std::io::Read::read(&mut file, &mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Some(hasher.finalize().to_hex().to_string())
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
        components::status(ComponentKind::Cloudflared, &settings, &state.paths),
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

// ===== Cloudflare 快速隧道（无需组网即可公网分享） =====

#[tauri::command]
pub async fn get_tunnel_status(state: State<'_, AppState>) -> AppResult<TunnelStatus> {
    Ok(state.tunnel.status())
}

#[tauri::command]
pub async fn start_tunnel(app: AppHandle, state: State<'_, AppState>) -> AppResult<TunnelStatus> {
    let _network = state.network_operation.lock().await;
    let settings = state.current_settings();
    let (bin, _) = components::cloudflared_binary(&settings, &state.paths);
    let bin = bin.ok_or_else(|| {
        AppError::Msg("未检测到 cloudflared，请先在设置中下载或指定组件".to_string())
    })?;
    // 隧道转发的是本机媒体服务，确保 HTTP 服务在运行
    if !state.server.status().running {
        state.server.start(&settings.bind_ip(), settings.port).await?;
        AppState::emit_status(&app, &state.server);
    }
    state.tunnel.start(&app, &bin, state.server.status().port).await
}

#[tauri::command]
pub async fn stop_tunnel(app: AppHandle, state: State<'_, AppState>) -> AppResult<TunnelStatus> {
    let _network = state.network_operation.lock().await;
    state.tunnel.stop(&app).await
}

/// 生成某条媒体的公网隧道播放链接（需隧道已启动）
#[tauri::command]
pub async fn build_tunnel_url(state: State<'_, AppState>, id: String) -> AppResult<String> {
    let _network = state.network_operation.lock().await;
    let server = state.server.status();
    if !server.running {
        return Err(AppError::Msg("HTTP 服务未运行，请先启动公网隧道".to_string()));
    }
    let base = state.tunnel.status().playback_base(server.port)?;
    let item = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db.get(&id)?
    }
    .ok_or_else(|| AppError::Msg("媒体不存在".to_string()))?;
    if item.source_kind == "online" {
        return Err(AppError::Msg("在线视频不支持公网隧道链接".to_string()));
    }
    if !url_builder::valid_basename(&item.file_name) {
        return Err(AppError::Msg("文件名不能映射为播放 URL".to_string()));
    }
    Ok(format!(
        "{}/{}",
        base.trim_end_matches('/'),
        url_builder::encode_segment(&item.file_name)
    ))
}

// ===== EasyTier 组网 =====

#[tauri::command]
pub async fn get_easytier_status(state: State<'_, AppState>) -> AppResult<EasytierStatus> {
    state.easytier.refresh();
    Ok(state.easytier.status())
}

#[tauri::command]
pub async fn start_easytier(app: AppHandle, state: State<'_, AppState>) -> AppResult<EasytierStatus> {
    let _network = state.network_operation.lock().await;
    let mut settings = state.current_settings();
    // 组网媒体共享通过虚拟网段访问 HTTP 服务，仅本机（127.0.0.1）监听会导致
    // 成员之间（包括查看自己的媒体库）无法连接，启动组网时自动切换为局域网监听
    if settings.listen_mode == ListenMode::Loopback {
        settings.listen_mode = ListenMode::Lan;
        {
            let mut guard = state.settings.write().unwrap_or_else(|e| e.into_inner());
            *guard = settings.clone();
        }
        settings.save(&state.paths.settings_file)?;
        // 记录「由启动组网自动切换」，组网停止时据此自动恢复本地模式
        state.easytier_forced_lan.store(true, Ordering::SeqCst);
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
    let _network = state.network_operation.lock().await;
    state.easytier.stop().await;
    // 局域网监听是启动组网时自动切换的 → 停止组网后自动恢复为本地模式；
    // 若局域网模式是用户手动设置（forced_lan 未置位）则保持不变。
    if state.easytier_forced_lan.swap(false, Ordering::SeqCst) {
        let mut settings = state.current_settings();
        if settings.listen_mode == ListenMode::Lan {
            settings.listen_mode = ListenMode::Loopback;
            {
                let mut guard = state.settings.write().unwrap_or_else(|e| e.into_inner());
                *guard = settings.clone();
            }
            settings.save(&state.paths.settings_file)?;
            let server = state.server.clone();
            server.stop();
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            server.start(&settings.bind_ip(), settings.port).await?;
            AppState::emit_status(&app, &server);
            tracing::info!("组网已停止，监听模式自动恢复为本地模式");
        }
    }
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
    let resp = match peer_client()?.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("读取对端媒体库失败 {}: {}", url, e);
            return Err(AppError::Msg(format!("无法连接 {}：{}", ip, e)));
        }
    };
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        tracing::warn!("读取对端媒体库失败 {}: HTTP {}", url, code);
        return Err(AppError::Msg(format!("对方媒体库不可用（HTTP {}）", code)));
    }
    let parsed: ApiMediaResponse = resp
        .json()
        .await
        .map_err(|e| AppError::Msg(format!("解析对方媒体库失败: {}", e)))?;
    tracing::info!("成功读取对端媒体库 {}，共 {} 条", url, parsed.items.len());
    Ok(parsed.items)
}

// ---------- 网络连通性 & 防火墙 ----------

/// 入站防火墙规则显示名（纯 ASCII，便于复用与移除）
const FIREWALL_RULE_NAME: &str = "VRchat-LocalCinema Media Library";

/// 增删媒体库入站防火墙规则。需要管理员权限，通过 Start-Process -Verb RunAs 提权执行
/// （首次会弹出一次 UAC 授权）。allow=true 时放行该端口的入站 TCP；allow=false 时移除规则。
async fn apply_firewall_rule(data_dir: &Path, port: u16, allow: bool) -> AppResult<()> {
    let name = crate::easytier::ps_quote(FIREWALL_RULE_NAME);
    let script = if allow {
        format!(
            "$ErrorActionPreference = 'Stop'\nGet-NetFirewallRule -DisplayName {} -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue\nNew-NetFirewallRule -DisplayName {} -Direction Inbound -Action Allow -Protocol TCP -LocalPort {} -Profile Any | Out-Null\nWrite-Output 'OK'\n",
            name, name, port
        )
    } else {
        format!(
            "$ErrorActionPreference = 'Stop'\nGet-NetFirewallRule -DisplayName {} -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue\nWrite-Output 'OK'\n",
            name
        )
    };
    let path = data_dir.join("firewall-rule.ps1");
    crate::easytier::write_utf8_bom(&path, &script)?;
    let wrapper = format!(
        "try {{ $p = Start-Process -FilePath 'powershell' -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',{}) -Verb RunAs -WindowStyle Hidden -Wait -PassThru; if ($p.ExitCode -ne 0) {{ Write-Output ('ERR|防火墙规则执行失败（退出码 ' + $p.ExitCode + '）') }} else {{ Write-Output 'OK' }} }} catch {{ Write-Output ('ERR|' + $_.Exception.Message); exit 1 }}",
        crate::easytier::ps_quote(&path.to_string_lossy())
    );
    let result = crate::easytier::run_powershell_script(&wrapper).await;
    let _ = std::fs::remove_file(&path);
    result
}

/// 对端连通性检测结果
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerConnCheck {
    /// ICMP 是否可达
    pub ping: bool,
    /// 目标端口 TCP 是否可连接
    pub tcp: bool,
}

/// 检测对端 IP 的 ICMP 与目标端口 TCP 连通性（用于给出精确的失败提示）
#[tauri::command]
pub async fn test_peer_connection(ip: String, port: u16) -> AppResult<PeerConnCheck> {
    let ip = ip.trim().to_string();
    if ip.is_empty() {
        return Err(AppError::Msg("请输入对方的虚拟 IP".to_string()));
    }
    let tcp = {
        let ip = ip.clone();
        tokio::task::spawn_blocking(move || {
            use std::net::{TcpStream, ToSocketAddrs};
            match (ip.as_str(), port).to_socket_addrs() {
                Ok(addrs) => addrs.into_iter().any(|a| {
                    TcpStream::connect_timeout(&a, std::time::Duration::from_secs(2)).is_ok()
                }),
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false)
    };
    let ping = ping_host(&ip).await;
    Ok(PeerConnCheck { ping, tcp })
}

/// 单次 ICMP ping（ping.exe 退出码为 0 视为可达），无窗口执行
async fn ping_host(ip: &str) -> bool {
    let mut cmd = tokio::process::Command::new("ping");
    cmd.args(["-n", "1", "-w", "1500", ip])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    matches!(cmd.status().await, Ok(s) if s.success())
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
    hash: Option<String>,
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        let dup = db.list()?.into_iter().find(|m| {
            m.status != MediaStatus::Missing && m.file_name == safe_name
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
    // 下载收尾会依据 dest.exists() 决定是否顺延序号，且长时间只写 .part，
    // 故此处仅探测不占位，避免预先创建空文件影响收尾流程。
    let dest = unique_dest_peek(&dir, &safe_name);

    let (task, cancel_flag, pause_flag) = state
        .convert_queue
        .register_download_task(
            id.clone(),
            title,
            safe_name.clone(),
            ip.clone(),
            port,
            dest.to_string_lossy().to_string(),
            hash.map(|h| h.to_lowercase()),
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
        unique_dest_peek(dir, &name)
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
    let mut speed_sample: Option<(std::time::Instant, u64)> = None;
    queue.set_telemetry(&task_id, None, None, None);
    let end = run_download_session(
        &ip,
        port,
        &peer_media_id,
        &dest,
        &part,
        &cancel_flag,
        &pause_flag,
        move |received, total, percent| {
            let now = std::time::Instant::now();
            let (sample_time, sample_bytes) = *speed_sample.get_or_insert((now, received));
            if now.duration_since(sample_time).as_secs_f64() >= 0.3 {
                let speed = received.saturating_sub(sample_bytes) as f64 / now.duration_since(sample_time).as_secs_f64();
                let remaining = total.filter(|_| speed > 0.0)
                    .map(|t| t.saturating_sub(received) as f64 / speed);
                progress_queue.set_telemetry(&progress_task_id, Some(speed), None, remaining);
                speed_sample = Some((now, received));
            }
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
            // 完整性校验：对比文件大小与对端哈希，避免半截或损坏文件入库
            let size = tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0);
            let mut verify_err: Option<String> = None;
            // 重新从队列读取最新任务（获取会话中写入的 total）
            let task_final = queue.list().into_iter().find(|t| t.id == task_id);
            if let Some(ref t) = task_final {
                if let Some(total) = t.total {
                    if size != total {
                        verify_err = Some(format!(
                            "文件大小校验失败（期望 {} 字节，实际 {} 字节）",
                            total, size
                        ));
                    }
                }
                if verify_err.is_none() {
                    if let Some(expected) = t.expected_hash.as_deref() {
                        match hash_file(&dest).await {
                            Some(actual) if actual.eq_ignore_ascii_case(expected) => {}
                            Some(_) => verify_err = Some("文件哈希校验失败".into()),
                            None => tracing::warn!("下载文件哈希计算失败，跳过哈希校验"),
                        }
                    }
                }
            }
            if let Some(msg) = verify_err {
                let _ = tokio::fs::remove_file(&dest).await;
                queue.finish(
                    &task_id,
                    crate::convert_queue::TaskState::Error,
                    Some(format!("{}，已删除下载文件", msg)),
                );
                emit_convert_tasks(&app);
                return;
            }
            let reg = {
                let db = st.db.lock().unwrap_or_else(|e| e.into_inner());
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

/// 尝试以原子方式独占路径：用 create_new 打开成功即为空文件占位，失败（已存在等）返回 false
fn try_claim(path: &std::path::Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .is_ok()
}

/// 目标文件重名时追加序号，避免覆盖。
/// 采用 create_new 原子占位：创建成功即返回该路径（已被空文件占据），
/// 已存在则顺延到下一个序号，避免并发写入者选到同一名字而互相覆盖。
/// 调用方需保证后续会写入/改名到该路径（如 ffmpeg 输出到 .part 再 promote）。
fn unique_dest(dir: &std::path::Path, file_name: &str) -> PathBuf {
    let mut candidate = dir.join(file_name);
    if try_claim(&candidate) {
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
        if try_claim(&candidate) {
            break;
        }
    }
    candidate
}

/// 只探测不占位的旧实现：返回一个「看起来没人用」的路径（存在 TOCTOU）。
/// 供后续自行判断 exists()/改名（如下载收尾）的调用方使用，避免预先创建空文件打乱其逻辑。
fn unique_dest_peek(dir: &std::path::Path, file_name: &str) -> PathBuf {
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
        .unwrap_or_else(|e| e.into_inner())
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

#[derive(Debug, Default, Clone, Copy)]
struct FfmpegProgress {
    percent: Option<f64>,
    fps: Option<f64>,
    speed: Option<f64>,
    remaining_seconds: Option<f64>,
}

fn progress_number(value: &str) -> Option<f64> {
    value.trim().trim_end_matches('x').parse::<f64>().ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// 把 ffmpeg 的退出码渲染为可读文本：负值即 libavutil 错误码，附上符号名便于直接定位。
fn describe_exit_code(code: Option<i32>) -> String {
    match code {
        Some(c) if c >= 0 => c.to_string(),
        Some(-1163346256) => {
            "-1163346256 (AVERROR_PATCHWELCOME：FFmpeg 内部该路径尚未实现)".to_string()
        }
        Some(-22) => "-22 (AVERROR(EINVAL)：参数或像素格式不被支持)".to_string(),
        Some(c) => c.to_string(),
        None => "未知（进程被异常终止）".to_string(),
    }
}

/// 运行 ffmpeg 并按 progress 块解析遥测；ETA 由剩余媒体时长 / speed 计算。
/// 传入 cancel 标志后可随时终止进程（转码队列取消任务用）。
/// `cwd` 用于烧录字幕时把工作目录切到字幕所在目录（见 `transcode::subtitle_filter_cwd`）。
async fn run_ffmpeg_with_progress(
    ffmpeg: &PathBuf,
    args: &[String],
    duration_ms: Option<u64>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    cwd: Option<&std::path::Path>,
    mut on_progress: impl FnMut(FfmpegProgress),
) -> AppResult<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut cmd = tokio::process::Command::new(ffmpeg);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.kill_on_drop(true);
    if cancel.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
        return Err(AppError::Msg("任务已取消".into()));
    }
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
        // 只保留**尾部**（最近的报错），避免被开头的大段输入文件元数据/编码器 banner 占满后
        // 把真正的报错行截掉（此前保留前 8000 字符，长素材下报错信息几乎必然丢失）。
        const KEEP: usize = 32_000;
        let mut collected = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            collected.push_str(&line);
            collected.push('\n');
            if collected.len() > KEEP {
                let cut = collected.len() - KEEP;
                let cut = (cut..collected.len())
                    .find(|i| collected.is_char_boundary(*i))
                    .unwrap_or(collected.len());
                collected.drain(..cut);
            }
        }
        collected
    });

    let mut progress = FfmpegProgress::default();
    let mut out_seconds = None;
    let mut lines = BufReader::new(stdout).lines();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        let line = tokio::select! {
            _ = tick.tick() => {
                if cancel.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
                    let _ = child.kill().await;
                    let _ = err_task.await;
                    return Err(AppError::Msg("任务已取消".into()));
                }
                continue;
            }
            line = lines.next_line() => line,
        };
        match line {
            Ok(Some(line)) => {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "fps" => progress.fps = progress_number(value),
                        "speed" => progress.speed = progress_number(value).filter(|s| *s > 0.0),
                        "out_time_us" | "out_time_ms" => {
                            out_seconds = progress_number(value).map(|us| us / 1_000_000.0);
                        }
                        "progress" => {
                            let duration = duration_ms.filter(|d| *d > 0).map(|d| d as f64 / 1000.0);
                            progress.percent = duration.zip(out_seconds)
                                .map(|(d, t)| (t / d * 100.0).clamp(0.0, 100.0));
                            progress.remaining_seconds = duration.zip(out_seconds).zip(progress.speed)
                                .map(|((d, t), s)| (d - t).max(0.0) / s);
                            on_progress(progress);
                            progress = FfmpegProgress::default();
                            out_seconds = None;
                        }
                        _ => {}
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                let _ = child.kill().await;
                let _ = err_task.await;
                return Err(AppError::Msg(format!("读取 ffmpeg 进度失败: {}", e)));
            }
        }
    }

    let status = child.wait().await?;
    let err_text = err_task
        .await
        .unwrap_or_else(|_| "无法获取错误详情".to_string());
    if !status.success() {
        // 完整 stderr（尾部）写入应用日志（供远程排查导出），任务卡片携带末尾若干行关键信息
        tracing::warn!("ffmpeg 失败（退出码 {:?}）args={:?} stderr:\n{}", status.code(), args, err_text);
        let non_empty: Vec<&str> = err_text.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail: Vec<&str> = non_empty.iter().rev().take(8).rev().copied().collect();
        let brief: String = tail.join("\n").chars().take(2000).collect();
        return Err(AppError::Msg(format!(
            "ffmpeg 退出码 {}:\n{}",
            describe_exit_code(status.code()),
            if brief.is_empty() { "未知错误".into() } else { brief }
        )));
    }
    on_progress(FfmpegProgress {
        percent: Some(100.0),
        remaining_seconds: Some(0.0),
        ..Default::default()
    });
    Ok(())
}

// ===== 转码任务队列 =====

fn emit_convert_tasks(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        let tasks = state.convert_queue.list();
        let _ = app.emit("convert-tasks-updated", &tasks);
    }
}

/// 按实际流信息校正用户配置：不可直拷进 MP4 的编码强制重编码，
/// 烧录时若未指定字幕轨则自动选第一条文本字幕。返回 (校正后的配置, 选中的字幕编码)。
fn sanitize_options(
    o: &crate::transcode::TranscodeOptions,
    streams: &[crate::transcode::StreamInfo],
) -> (
    crate::transcode::TranscodeOptions,
    Option<crate::transcode::StreamInfo>,
) {
    let mut out = o.clone();
    // 显式指定的流索引必须与实际流类型匹配，否则视为未指定（避免把音频/字幕索引当视频用）
    if let Some(i) = out.video_stream {
        if !streams.iter().any(|s| s.index == i && s.kind == "video") {
            out.video_stream = None;
        }
    }
    if let Some(i) = out.audio_stream {
        if !streams.iter().any(|s| s.index == i && s.kind == "audio") {
            out.audio_stream = None;
        }
    }
    // 视频：直拷目标编码不在 MP4 允许列表时改为重编码
    if out.video_copy() {
        let ok = out
            .video_stream
            .and_then(|i| streams.iter().find(|s| s.index == i))
            .or_else(|| streams.iter().find(|s| s.kind == "video"))
            .map(|s| s.copyable)
            .unwrap_or(false);
        if !ok {
            out.video_mode = Some("h264".into());
        }
    }
    // 音频：DTS / TrueHD 等无法直拷进 MP4
    if out.include_audio() && out.audio_copy() {
        let ok = out
            .audio_stream
            .and_then(|i| streams.iter().find(|s| s.index == i))
            .or_else(|| streams.iter().find(|s| s.kind == "audio"))
            .map(|s| s.copyable)
            .unwrap_or(false);
        if !ok {
            out.audio_mode = Some("aac".into());
        }
    }
    // 字幕：确定要用的字幕轨（显式指定优先，否则自动选第一条文本字幕；烧录模式可退而选图形字幕）
    let sub = if out.wants_subtitle() {
        let chosen = out
            .subtitle_stream
            .and_then(|i| streams.iter().find(|s| s.index == i && s.kind == "subtitle"))
            .or_else(|| {
                streams
                    .iter()
                    .find(|s| s.kind == "subtitle" && s.text_subtitle)
            })
            .or_else(|| {
                if out.burn_subtitles() {
                    streams
                        .iter()
                        .find(|s| s.kind == "subtitle" && s.graphic_subtitle)
                } else {
                    None
                }
            });
        // 内嵌 / 外挂只能处理文本字幕；烧录可处理文本或图形字幕
        let usable = chosen.filter(|s| {
            if out.embed_subtitles() || out.external_subtitles() {
                s.text_subtitle
            } else {
                s.text_subtitle || s.graphic_subtitle
            }
        });
        match usable {
            Some(s) => {
                out.subtitle_stream = Some(s.index);
                out.subtitle_graphic = Some(s.graphic_subtitle);
                Some(s.clone())
            }
            // 找不到可用的字幕轨时放弃字幕，避免整个任务失败
            _ => {
                out.subtitle_mode = None;
                out.subtitle_stream = None;
                out.subtitle_graphic = None;
                None
            }
        }
    } else {
        None
    };
    (out, sub)
}

/// 把外挂字幕复制到转码临时目录并改成安全文件名。
///
/// 外挂字幕由用户命名（可能含空格、逗号、括号等），直接传给 `subtitles` 滤镜会被
/// ffmpeg 的选项切分逻辑误解析（见 `transcode::subtitle_filter_value`），因此统一
/// 复制成程序生成的 `sub-<uuid>-side.<ext>` 再使用；该临时文件由 [`SubtitleFile`] 负责清理。
fn stage_sidecar_subtitle(path: &Path, codec: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("vrchat-localcinema");
    std::fs::create_dir_all(&dir).ok()?;
    let dest = dir.join(format!(
        "sub-{}-side.{}",
        uuid::Uuid::new_v4(),
        crate::transcode::subtitle_ext(codec)
    ));
    match std::fs::copy(path, &dest) {
        Ok(_) => Some(dest),
        Err(e) => {
            tracing::warn!("复制外挂字幕失败: {}", e);
            None
        }
    }
}

/// 把指定字幕流抽取为独立临时文件（烧录用）。返回临时文件路径；
/// 失败时返回 None（调用方退化为不烧录字幕）。
async fn extract_subtitle_file(
    ffmpeg: &std::path::PathBuf,
    input: &str,
    index: usize,
    codec: &str,
    cancel: Arc<AtomicBool>,
) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("vrchat-localcinema");
    std::fs::create_dir_all(&dir).ok()?;
    let dest = dir.join(format!("sub-{}-{}.{}", uuid::Uuid::new_v4(), index, crate::transcode::subtitle_ext(codec)));
    let args: Vec<String> = vec![
        "-y".into(),
        "-i".into(),
        input.to_string(),
        "-map".into(),
        format!("0:{}", index),
        "-c:s".into(),
        if matches!(codec, "ass" | "ssa") { "ass".into() } else { "srt".into() },
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        dest.to_string_lossy().to_string(),
    ];
    match run_ffmpeg_with_progress(ffmpeg, &args, None, Some(cancel), None, |_| {}).await {
        Ok(()) => Some(dest),
        Err(_) => {
            let _ = std::fs::remove_file(dest);
            None
        }
    }
}

/// 转码临时产物路径（`<原名>.part.mp4`）：ffmpeg 先写临时文件，成功后改名为正式产物，
/// 中途退出/崩溃不会在产物目录留下被误认为已完成转码的半成品。
/// 注意：下载任务使用 part_path（`.part` 后缀，支持断点续传）；此处必须保留 .mp4 扩展名供 ffmpeg 识别格式
fn transcode_part_path(dest: &std::path::Path) -> std::path::PathBuf {
    let name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output.mp4".into());
    dest.with_file_name(format!("{}.part.mp4", name))
}

/// 转码成功后把临时产物改名到最终路径。
/// Windows 上 rename 不能覆盖已存在文件，因此目标已存在时先把它挪到 .bak 再改名，
/// 任一步失败都回滚，避免先删后改名期间崩溃导致旧产物丢失。
fn promote_part(part: &std::path::Path, dest: &std::path::Path) -> AppResult<()> {
    if std::fs::rename(part, dest).is_ok() {
        return Ok(());
    }
    let file_name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let bak = dest.with_file_name(format!("{}.bak", file_name));
    let had_dest = dest.exists();
    if had_dest {
        std::fs::rename(dest, &bak)
            .map_err(|e| AppError::Msg(format!("保存转码产物失败: {}", e)))?;
    }
    match std::fs::rename(part, dest) {
        Ok(()) => {
            if had_dest {
                let _ = std::fs::remove_file(&bak);
            }
            Ok(())
        }
        Err(e) => {
            if had_dest {
                let _ = std::fs::rename(&bak, dest);
            }
            Err(AppError::Msg(format!("保存转码产物失败: {}", e)))
        }
    }
}

/// 查询该视频的全部媒体流（视频 / 音频 / 字幕），供「转码设置」对话框展示与选择。
/// 除 ffprobe 报告的内嵌流外，还包含同目录下的外挂字幕文件（见 `transcode::parse_sidecar_subtitles`）。
#[tauri::command]
pub async fn get_media_streams(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<Vec<crate::transcode::StreamInfo>> {
    let item = get_item_checked(&state, &id)?;
    let settings = state.current_settings();
    let (ffprobe, _) = components::ffprobe_binary(&settings, &state.paths);
    let ffprobe =
        ffprobe.ok_or_else(|| AppError::Msg("未检测到 ffprobe，请先在设置中指定 FFmpeg 组件".into()))?;
    let json = thumbnail::probe_media_json(&ffprobe, std::path::Path::new(&item.file_path)).await?;
    let mut streams = crate::transcode::parse_streams(&json);
    streams.extend(crate::transcode::parse_sidecar_subtitles(std::path::Path::new(
        &item.file_path,
    )));
    Ok(streams)
}

/// 加入转码队列（按任务序列执行），可在「转码任务」页查看进度、开始或取消；
/// overwrite 为 true 时直接覆盖已存在的转码产物（前端已弹出覆盖确认）；
/// options 为「转码设置」对话框产出的配置，缺省时按完整转码（H.264 + AAC）处理
#[tauri::command]
pub async fn transcode_media(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    overwrite: Option<bool>,
    options: Option<crate::transcode::TranscodeOptions>,
) -> AppResult<crate::convert_queue::ConvertTask> {
    let item = get_item_checked(&state, &id)?;
    if state.hls_jobs.read().unwrap_or_else(|e| e.into_inner()).contains(&id) {
        return Err(AppError::Msg("该视频正在边转边播，请等待完成或先取消".into()));
    }
    let options = options.unwrap_or_default();
    let task = state
        .convert_queue
        .enqueue(id, item.title, overwrite.unwrap_or(false), options)
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
    
    if kind.as_deref() == Some("segment") {
        let names = vec![
            format!("{} (分段1).mp4", stem),
            format!("{} (分段2).mp4", stem),
        ];
        return Ok(names.iter().any(|n| dir.join(n).exists()));
    }
    
    // 常规转码：检查主产物及 Windows 重复文件命名（" (转码) (1).mp4"、" (转码) (2).mp4" …）
    let base_name = format!("{} (转码).mp4", stem);
    if dir.join(&base_name).exists() {
        return Ok(true);
    }
    for i in 1..=99 {
        let numbered = format!("{} (转码) ({}).mp4", stem, i);
        if dir.join(&numbered).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 样本生成完成事件（前端据此提示并打开所在目录）
#[derive(Clone, serde::Serialize)]
struct SampleDoneEvent {
    media_id: String,
    ok: bool,
    path: Option<String>,
    error: Option<String>,
}

/// 生成视频样本：调用 ffmpeg 截取视频正中间的 20 秒转码保存到源视频所在目录，
/// 完成后发送 sample-done 事件，由前端打开资源管理器定位文件
#[tauri::command]
pub async fn generate_sample(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<()> {
    let item = get_item_checked(&state, &id)?;
    if !std::path::Path::new(&item.file_path).is_file() {
        return Err(AppError::Msg("文件已丢失或无法访问".into()));
    }
    let settings = state.current_settings();
    let (ffmpeg, _) = components::ffmpeg_binary(&settings, &state.paths);
    let ffmpeg = ffmpeg
        .ok_or_else(|| AppError::Msg("未检测到 ffmpeg，请先在设置中下载或指定 FFmpeg 组件".into()))?;

    let media_id = id;
    let file_path = item.file_path.clone();
    let duration_ms = item.duration_ms;
    tokio::spawn(async move {
        let event = match run_sample_ffmpeg(&ffmpeg, &file_path, duration_ms).await {
            Ok(dest) => SampleDoneEvent {
                media_id,
                ok: true,
                path: Some(dest.to_string_lossy().to_string()),
                error: None,
            },
            Err(e) => SampleDoneEvent {
                media_id,
                ok: false,
                path: None,
                error: Some(e.to_string()),
            },
        };
        let _ = app.emit("sample-done", &event);
    });
    Ok(())
}

/// 构建样本截取参数：流复制（-c copy）保持原编码与原封装格式，仅重封装视频正中间的 20 秒，
/// 不重新转码。`start_sec` 为起点（不传则从 0 开始）。
/// 样本保留所有视频/音频/字幕轨，以便代表原文件的完整信息；
/// 通过保留原封装格式（原扩展名）确保多轨兼容性（mkv 支持多轨，不会像强制 MP4 那样报 -22）。
fn sample_args(input: &str, dest: &std::path::Path, start_sec: Option<f64>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-y".into()];
    // -ss 放在 -i 之前：对 -c copy 而言是输入侧快速定位，输出从关键帧开始，不会产生损坏文件
    if let Some(s) = start_sec {
        args.extend(["-ss".into(), format!("{:.3}", s)]);
    }
    args.extend([
        "-i".into(),
        input.to_string(),
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "0:a?".into(),
        "-map".into(),
        "0:s?".into(),
        "-c".into(),
        "copy".into(),
        "-t".into(),
        "20".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        dest.to_string_lossy().to_string(),
    ]);
    args
}

/// 样本临时文件：与最终产物同名但保留原扩展名（`xxx (样本).mkv.part.mkv`）。
/// ffmpeg 按扩展名选择封装器，不能复用转码的 `.part.mp4` —— 否则 mkv/webm 等
/// 非 MP4 样本会被错误封装为 MP4，流复制不兼容的轨（DTS/字幕/多视频轨）直接报
/// Invalid argument（退出码 -22）。
fn sample_part_path(dest: &std::path::Path) -> std::path::PathBuf {
    let name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "sample".into());
    let ext = dest
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "mkv".into());
    dest.with_file_name(format!("{}.part.{}", name, ext))
}

/// 截取视频正中间的 20 秒生成样本文件：-c copy 流复制（保持原格式和原封装，速度极快），
/// 命名为 `<原名> (样本).<原扩展名>`，保存到源视频所在目录；
/// 起点取 `(总时长 - 20) / 2`，时长未知或不足 20 秒时从 0 开始
async fn run_sample_ffmpeg(
    ffmpeg: &PathBuf,
    file_path: &str,
    duration_ms: Option<u64>,
) -> AppResult<PathBuf> {
    let src = std::path::Path::new(file_path);
    let dir = src.parent().unwrap_or(std::path::Path::new("."));
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".into());
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp4".into());
    let dest = unique_dest(dir, &format!("{} (样本).{}", stem, ext));
    let part = sample_part_path(&dest);
    let start_sec = duration_ms
        .map(|d| (d as f64 / 1000.0 - 20.0) / 2.0)
        .filter(|s| *s > 0.0);
    let args = sample_args(file_path, &part, start_sec);
    let result = run_ffmpeg_with_progress(ffmpeg, &args, Some(20_000), None, None, |_| {}).await;
    if let Err(e) = result {
        // 失败时清理临时产物与原子占位的空目标文件
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    // 清理可能残留的 .part.<原扩展> 样本临时产物（如 .part.mkv）
    if let Some(parent) = dest.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let dest_stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(&format!("{}.part.", dest_stem)) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    promote_part(&part, &dest)?;
    Ok(dest)
}

/// 检查本机是否缺少指定音频编码的系统解码器（codec: "ac3" | "eac3"）。
/// VRChat/AVPro 用系统 Media Foundation 解码，Windows 11 24H2 起系统不带 Dolby
/// 解码器；检测失败一律按「不缺」处理，避免误报引导用户安装。
#[tauri::command]
pub async fn system_audio_decoder_missing(codec: String) -> AppResult<bool> {
    Ok(!crate::system_codecs::audio_decoder_available(&codec))
}

/// 在资源管理器中定位文件（选中该文件）
#[tauri::command]
pub async fn reveal_path(path: String) -> AppResult<()> {
    external_player::reveal_in_folder(&PathBuf::from(path))
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
    options: Option<crate::transcode::TranscodeOptions>,
) -> AppResult<crate::convert_queue::ConvertTask> {
    let item = get_item_checked(&state, &id)?;
    if state.hls_jobs.read().unwrap_or_else(|e| e.into_inner()).contains(&id) {
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
        .enqueue_segment(id, item.title, overwrite.unwrap_or(false), first_ms, options.unwrap_or_default())
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
    let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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

    let part = transcode_part_path(&dest);

    let (options, subtitle_file) = prepare_transcode_options(state, &ffmpeg, &item, &task.options, cancel.clone()).await;
    let args = crate::transcode::build_args(
        &item.file_path,
        &part,
        &options,
        subtitle_file.as_deref(),
        None,
        None,
    );

    let progress_app = app.clone();
    let progress_media_id = task.media_id.clone();
    let progress_task_id = task.id.clone();
    let progress_queue = state.convert_queue.clone();
    let sub_cwd = subtitle_file
        .as_deref()
        .and_then(crate::transcode::subtitle_filter_cwd);
    let result = run_ffmpeg_with_progress(
        &ffmpeg,
        &args,
        item.duration_ms,
        Some(cancel),
        sub_cwd.as_deref(),
        move |progress| {
            if let Some(pct) = progress.percent {
                progress_queue.set_percent(&progress_task_id, pct);
            }
            progress_queue.set_telemetry(&progress_task_id, progress.speed, progress.fps, progress.remaining_seconds);
            emit_convert_tasks(&progress_app);
            emit_convert(
                &progress_app,
                ConvertProgress {
                    id: progress_media_id.clone(),
                    kind: "transcode",
                    status: "running",
                    percent: progress.percent,
                    error: None,
                    item: None,
                },
            );
        },
    )
    .await;

    if let Err(e) = result {
        // 失败时清理临时产物；非覆盖模式下 dest 为原子占位空文件，一并移除（覆盖模式需保留原文件）
        let _ = std::fs::remove_file(&part);
        if !task.overwrite {
            let _ = std::fs::remove_file(&dest);
        }
        return Err(e);
    }
    promote_part(&part, &dest)?;
    if options.external_subtitles() {
        if let Some(f) = subtitle_file.as_deref() {
            if let Err(e) = std::fs::copy(f, dest.with_extension(f.extension().unwrap_or_default())) {
                tracing::warn!("写出外挂字幕失败: {}", e);
            }
        }
    }

    // 源条目视为已处理；保留在「不兼容」文件夹原位置，由转码产物的快捷方式卡片承接跳转
    let updated = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        let _ = db.set_compat_resolved(&task.media_id, true);
        db.get(&task.media_id).ok().flatten()
    };
    if let Some(u) = updated {
        let _ = app.emit("media-updated", &u);
    }

    let registered = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
    if let Some(new_item) = &registered {
        spawn_probe_task(app.clone(), new_item.id.clone());
    }
    Ok(registered)
}

struct SubtitleFile(Option<PathBuf>);

impl SubtitleFile {
    fn as_deref(&self) -> Option<&Path> {
        self.0.as_deref()
    }
}

impl Drop for SubtitleFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn prepare_transcode_options(
    state: &AppState,
    ffmpeg: &PathBuf,
    item: &MediaItem,
    requested: &crate::transcode::TranscodeOptions,
    cancel: Arc<AtomicBool>,
) -> (crate::transcode::TranscodeOptions, SubtitleFile) {
    let settings = state.current_settings();
    let streams = if requested.video_copy()
        || requested.audio_copy()
        || requested.wants_subtitle()
    {
        let (ffprobe, _) = components::ffprobe_binary(&settings, &state.paths);
        match ffprobe {
            Some(p) => {
                let mut streams = thumbnail::probe_media_json(&p, std::path::Path::new(&item.file_path))
                    .await
                    .map(|j| crate::transcode::parse_streams(&j))
                    .unwrap_or_default();
                // 外挂字幕与内嵌字幕一样参与选择与烧录，必须一起交给 sanitize_options 解析索引
                streams.extend(crate::transcode::parse_sidecar_subtitles(
                    std::path::Path::new(&item.file_path),
                ));
                streams
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let (options, subtitle_stream) = sanitize_options(requested, &streams);

    // 烧录 / 外挂模式：先把字幕备成独立临时文件（文本烧录交给 subtitles 滤镜，外挂则复制到产物旁）。
    // 图形字幕烧录走 overlay 滤镜，不备为文本文件；外挂字幕文件直接复制（无需从容器抽取）。
    let mut subtitle_file: Option<std::path::PathBuf> = None;
    let needs_text_extract =
        options.external_subtitles() || (options.burn_subtitles() && !options.subtitle_graphic());
    if needs_text_extract {
        if let Some(s) = &subtitle_stream {
            subtitle_file = match s.sidecar_path.as_deref() {
                Some(p) => stage_sidecar_subtitle(std::path::Path::new(p), &s.codec),
                None => extract_subtitle_file(ffmpeg, &item.file_path, s.index, &s.codec, cancel).await,
            };
        }
    }
    (options, SubtitleFile(subtitle_file))
}

/// 分段转码默认第一段时长：总时长的 10% 向上取整到 5 分钟（如 10、15、20 分钟）
fn default_first_segment_ms(total_ms: u64) -> u64 {
    const FIVE_MIN: u64 = 300_000;
    let raw = (total_ms as f64 * 0.1).ceil() as u64;
    ((raw + FIVE_MIN - 1) / FIVE_MIN) * FIVE_MIN
}

fn align_segment_subtitles(args: &mut [String], start_sec: f64) {
    if start_sec > 0.0 {
        if let Some(i) = args.iter().position(|a| a == "-vf") {
            // 输入侧 -ss 将帧时间归零；libass 需要原片时间，编码前再恢复段内时间。
            args[i + 1] = format!("setpts=PTS+{start_sec:.3}/TB,{},setpts=PTS-{start_sec:.3}/TB", args[i + 1]);
        }
    }
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
    let part = transcode_part_path(dest);
    let (options, subtitle_file) = prepare_transcode_options(state, ffmpeg, source, &task.options, cancel.clone()).await;
    let mut args = crate::transcode::build_args(
        &source.file_path,
        &part,
        &options,
        subtitle_file.as_deref(),
        start_sec,
        dur_sec,
    );
    if options.burn_subtitles() && subtitle_file.as_deref().is_some() {
        align_segment_subtitles(&mut args, start_sec.unwrap_or(0.0));
    }
    let sub_cwd = subtitle_file.as_deref().and_then(crate::transcode::subtitle_filter_cwd);
    let progress_app = app.clone();
    let progress_media_id = task.media_id.clone();
    let progress_task_id = task.id.clone();
    let progress_queue = state.convert_queue.clone();
    let result = run_ffmpeg_with_progress(
        ffmpeg,
        &args,
        pass_duration_ms,
        Some(cancel.clone()),
        sub_cwd.as_deref(),
        move |progress| {
            let overall = progress.percent.map(|pct| base_pct + pct * scale_pct);
            if let Some(pct) = overall {
                progress_queue.set_percent(&progress_task_id, pct);
            }
            // 第一段的 ETA 也包含尚未开始的第二段，使用当前倍速估计。
            let future_ms = if segment_index == 1 {
                source.duration_ms.unwrap_or(0).saturating_sub(pass_duration_ms.unwrap_or(0))
            } else { 0 };
            let remaining = progress.remaining_seconds.zip(progress.speed)
                .map(|(eta, speed)| eta + future_ms as f64 / 1000.0 / speed);
            progress_queue.set_telemetry(&progress_task_id, progress.speed, progress.fps, remaining);
            emit_convert_tasks(&progress_app);
            emit_convert(
                &progress_app,
                ConvertProgress {
                    id: progress_media_id.clone(),
                    kind: "transcode",
                    status: "running",
                    percent: overall,
                    error: None,
                    item: None,
                },
            );
        },
    )
    .await;
    if let Err(e) = result {
        // 失败时清理临时产物；非覆盖模式下 dest 为原子占位空文件，一并移除（覆盖模式需保留原文件）
        let _ = std::fs::remove_file(&part);
        if !task.overwrite {
            let _ = std::fs::remove_file(dest);
        }
        return Err(e);
    }
    promote_part(&part, dest)?;
    if options.external_subtitles() {
        if let Some(sub) = subtitle_file.as_deref() {
            let sidecar = dest.with_extension(sub.extension().unwrap_or_default());
            let sidecar_part = sidecar.with_file_name(format!("sub-{}.{}", uuid::Uuid::new_v4(), sub.extension().unwrap_or_default().to_string_lossy()));
            let cleanup = SubtitleFile(Some(sidecar_part.clone()));
            let mut sub_args = vec!["-y".into(), "-i".into(), sub.to_string_lossy().to_string(),
                "-ss".into(), format!("{:.3}", start_sec.unwrap_or(0.0))];
            if let Some(d) = dur_sec {
                sub_args.extend(["-t".into(), format!("{d:.3}")]);
            }
            sub_args.extend(["-progress".into(), "pipe:1".into(), "-nostats".into(), sidecar_part.to_string_lossy().to_string()]);
            run_ffmpeg_with_progress(ffmpeg, &sub_args, pass_duration_ms, Some(cancel), None, |_| {}).await?;
            promote_part(&sidecar_part, &sidecar)?;
            drop(cleanup);
        }
    }

    // 分段产物入库：登记源关联与段序号
    let registered = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
        // 视频过短不足以分段，视为只有一个分段（不回落整段转码，保持分段命名契约）
        tracing::info!("视频时长 {}ms 小于首段设定 {}ms，仅转单段", total, first_ms);
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut jobs = state.hls_jobs.write().unwrap_or_else(|e| e.into_inner());
        if !jobs.insert(id.clone()) {
            return Err(AppError::Msg("该视频正在生成边转边播（HLS）".into()));
        }
    }
    // 与转码队列互斥：同一视频不允许同时排队转码和边转边播
    if state.convert_queue.has_active(&id) {
        state.hls_jobs.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
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
                None,
                move |progress| {
                    if let Some(st) = pct_app.try_state::<AppState>() {
                        if let Some(pct) = progress.percent {
                            st.convert_queue.set_percent(&pct_task_id, pct);
                        }
                        st.convert_queue.set_telemetry(&pct_task_id, progress.speed, progress.fps, progress.remaining_seconds);
                        emit_convert_tasks(&pct_app);
                    }
                    emit_convert(
                        &cb_app,
                        ConvertProgress {
                            id: run_id.clone(),
                            kind: "hls",
                            status: "running",
                            percent: progress.percent,
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
                            let d = st.db.lock().unwrap_or_else(|e| e.into_inner());
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
                    *err_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
                    // 清理半成品并回滚登记
                    let st = progress_app.state::<AppState>();
                    let dir = hls_root_of(&st).join(&progress_id);
                    let _ = std::fs::remove_dir_all(&dir);
                    {
                        let d = st.db.lock().unwrap_or_else(|e| e.into_inner());
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
                st.hls_jobs.write().unwrap_or_else(|e| e.into_inner()).remove(&progress_id);
            }
        });

        // 轮询等待首个分片写入播放列表（最长 3 分钟）
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Some(err) = shared_err.lock().unwrap_or_else(|e| e.into_inner()).clone() {
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
            let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
            let d = state.db.lock().unwrap_or_else(|e| e.into_inner());
            let _ = d.set_hls_playlist(&id, &playlist_name);
            let updated = d.get(&id).ok().flatten();
            if let Some(u) = updated {
                let _ = app.emit("media-updated", &u);
            }
        }
        tracing::info!("边转边播（HLS）已可播放：/{}", playlist_name);

        // 链接已就绪，允许后续重新生成；后台任务完成时幂等移除
        state.hls_jobs.write().unwrap_or_else(|e| e.into_inner()).remove(&id);

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
        state.hls_jobs.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
        return Err(e);
    }
    result
}

/// HLS 是否已生成
#[tauri::command]
pub async fn get_hls_status(state: State<'_, AppState>, id: String) -> AppResult<bool> {
    let generating = state.hls_jobs.read().unwrap_or_else(|e| e.into_inner()).contains(&id);
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
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
    Ok(state.db.lock().unwrap_or_else(|e| e.into_inner()).list_folders()?)
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
        .unwrap_or_else(|e| e.into_inner())
        .create_folder(&name, parent_id.as_deref())?)
}

#[tauri::command]
pub async fn rename_folder(state: State<'_, AppState>, id: String, name: String) -> AppResult<()> {
    Ok(state.db.lock().unwrap_or_else(|e| e.into_inner()).rename_folder(&id, &name)?)
}

#[tauri::command]
pub async fn delete_folder(state: State<'_, AppState>, id: String) -> AppResult<()> {
    Ok(state.db.lock().unwrap_or_else(|e| e.into_inner()).delete_folder(&id)?)
}

#[tauri::command]
pub async fn rename_media(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> AppResult<MediaItem> {
    let item = state.db.lock().unwrap_or_else(|e| e.into_inner()).rename_media(&id, &name)?;
    Ok(item)
}

#[tauri::command]
pub async fn move_media(
    state: State<'_, AppState>,
    id: String,
    folder_id: Option<String>,
) -> AppResult<MediaItem> {
    {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db.set_media_folder(&id, folder_id.as_deref())?;
        let item = db.get(&id)?.ok_or_else(|| AppError::Msg("媒体条目不存在".into()))?;
        Ok(item)
    }
}
