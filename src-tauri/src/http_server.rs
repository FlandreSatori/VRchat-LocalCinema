use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path as AxumPath, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, Method, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;
use tower::limit::concurrency::ConcurrencyLimitLayer;

use crate::error::AppResult;
use crate::media_library::Db;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub bind_addr: String,
    pub port: u16,
    pub error: Option<String>,
}

#[derive(Clone)]
struct MediaState {
    db: Arc<Mutex<Db>>,
    thumb_dir: PathBuf,
    hls_dir: PathBuf,
    /// 是否允许对等节点查看本机媒体库（关闭时 /api/media 返回空清单）
    allow_peer_view: Arc<std::sync::atomic::AtomicBool>,
    /// 在线视频（B 站）直链缓存：媒体 id → 官方 CDN 直链与其过期时间
    online_links: Arc<Mutex<HashMap<String, CachedLink>>>,
    /// 已校验路径缓存：文件名 → (规范化路径, 校验时间)，TTL 60s
    /// 用于避免每次 Range 请求重复执行 safe_registered_file + File::open
    validated_paths: Arc<dashmap::DashMap<String, (PathBuf, Instant)>>,
    /// 媒体流读缓冲（KB），与设置页「媒体流读缓冲」实时同步
    stream_buf_kb: Arc<AtomicUsize>,
}

/// 在线视频的官方 CDN 直链缓存条目
#[derive(Clone)]
struct CachedLink {
    url: String,
    /// Unix 秒，直链失效时间（来自 URL 的 `deadline` 参数）
    expires_at: i64,
}

#[derive(Clone)]
pub struct HttpServer {
    status: Arc<RwLock<ServerStatus>>,
    state: MediaState,
    tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<()>>>>,
    generation: Arc<AtomicU64>,
}

impl HttpServer {
    pub fn new(
        db: Arc<Mutex<Db>>,
        thumb_dir: &Path,
        hls_dir: &Path,
        allow_peer_view: Arc<std::sync::atomic::AtomicBool>,
        stream_buf_kb: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            status: Arc::new(RwLock::new(ServerStatus {
                running: false,
                bind_addr: String::new(),
                port: 0,
                error: None,
            })),
            state: MediaState {
                db,
                thumb_dir: thumb_dir.to_path_buf(),
                hls_dir: hls_dir.to_path_buf(),
                allow_peer_view,
                online_links: Arc::new(Mutex::new(HashMap::new())),
                validated_paths: Arc::new(dashmap::DashMap::new()),
                stream_buf_kb,
            },
            tx: Arc::new(RwLock::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn status(&self) -> ServerStatus {
        self.status.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 启动 HTTP 服务。会检查端口是否被占用。
    pub async fn start(&self, bind_ip: &str, port: u16) -> AppResult<()> {
        {
            let s = self.status.read().unwrap_or_else(|e| e.into_inner());
            if s.running {
                return Ok(());
            }
        }
        // 未开启「允许他人查看我的媒体库」时，即使处于局域网模式也只在环回地址监听，
        // 从而在监听端口上完全阻断来自局域网的 TCP 连接（主机本身仍可被 ping 通）。
        let bind_ip = if self
            .state
            .allow_peer_view
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            bind_ip
        } else {
            "127.0.0.1"
        };
        let addr_str = format!("{}:{}", bind_ip, port);
        let addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| crate::error::AppError::Msg(format!("解析绑定地址失败: {}", e)))?;
        // 预先检测端口是否可用
        let test_listener = TcpListener::bind(addr).await.map_err(|e| {
            crate::error::AppError::Msg(format!("端口 {} 已被占用或无法绑定: {}", port, e))
        })?;
        drop(test_listener);
        // 重建 listener
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| crate::error::AppError::Msg(format!("绑定地址失败: {}", e)))?;
        let actual_port = listener.local_addr()?.port();
        let actual_addr = listener.local_addr()?.to_string();

        let app = Router::new()
            .route("/media/{id}/{*file_name}", get(serve_media))
            .route("/media/{id}", get(serve_media_short))
            .route("/thumb/{*thumb_name}", get(serve_thumb))
            .route("/api/media", get(api_media))
            .route("/api/node", get(api_node))
            .route("/download/{id}", get(serve_download))
            .route("/hls/{id}/{*file}", get(serve_hls))
            .route("/online/{id}", get(serve_online))
            .route("/", get(|| async { "VRchat-LocalCinema" }))
            .route("/{file_name}", get(serve_media_basename))
            .layer(ConcurrencyLimitLayer::new(64))
            .layer(axum::middleware::from_fn(log_peer_request))
            .with_state(self.state.clone());

        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        {
            let mut lock = self.tx.write().unwrap_or_else(|e| e.into_inner());
            *lock = Some(tx);
        }
        let generation = {
            let mut s = self.status.write().unwrap_or_else(|e| e.into_inner());
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            *s = ServerStatus {
                running: true,
                bind_addr: actual_addr.clone(),
                port: actual_port,
                error: None,
            };
            generation
        };
        let scope = if bind_ip == "0.0.0.0" {
            "局域网(0.0.0.0)"
        } else {
            "仅本机(127.0.0.1)"
        };
        tracing::info!("HTTP 服务启动: {} 监听范围={}", actual_addr, scope);
        // 记录本机可用 IPv4 网卡，便于排查「对方无法访问本机媒体库」时确认监听地址与虚拟 IP
        let mut lan_addrs: Vec<String> = Vec::new();
        for iface in crate::network_interfaces::list_ipv4_interfaces() {
            for ip in &iface.ips {
                lan_addrs.push(format!("{}={}", iface.name, ip));
            }
        }
        tracing::info!(
            "本机可用 IPv4 网卡: {}",
            if lan_addrs.is_empty() {
                "无".to_string()
            } else {
                lan_addrs.join(", ")
            }
        );
        let srv = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );

        let status = self.status.clone();
        let current_generation = self.generation.clone();
        tokio::spawn(async move {
            let serve = srv.with_graceful_shutdown(async move {
                rx.recv().await;
            });
            if let Err(e) = serve.await {
                tracing::error!("HTTP 服务错误: {}", e);
                let mut s = status.write().unwrap_or_else(|e| e.into_inner());
                if current_generation.load(Ordering::SeqCst) == generation {
                    s.running = false;
                    s.error = Some(e.to_string());
                }
            } else {
                tracing::info!("HTTP 服务已停止");
                let mut s = status.write().unwrap_or_else(|e| e.into_inner());
                if current_generation.load(Ordering::SeqCst) == generation {
                    s.running = false;
                }
            }
        });
        Ok(())
    }

    pub fn stop(&self) {
        // 停止接受新连接不应等待旧视频流排空，也不能让旧任务覆盖新实例状态。
        {
            let mut status = self.status.write().unwrap_or_else(|e| e.into_inner());
            self.generation.fetch_add(1, Ordering::SeqCst);
            status.running = false;
        }
        let tx = self.tx.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(t) = tx {
            let _ = t.try_send(());
        }
    }

    pub fn restart(&self, bind_ip: &str, port: u16) {
        self.stop();
        let this = self.clone();
        let ip = bind_ip.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Err(e) = this.start(&ip, port).await {
                tracing::error!("HTTP 服务重启失败: {}", e);
                let mut s = this.status.write().unwrap_or_else(|e| e.into_inner());
                s.running = false;
                s.error = Some(e.to_string());
            }
        });
    }
}

/// 需要完整记录的视频类请求路径：这些请求来自播放器（VRChat/AVPro、浏览器等），
/// 记录后可与 VRChat 客户端日志（[AVProVideo] Opening / Using playback path）对照。
/// 排除 /thumb/（封面）与 /（根路径探活），避免 UI 加载时刷屏。
fn is_media_path(path: &str) -> bool {
    path.starts_with("/media/")
        || path.starts_with("/download/")
        || path.starts_with("/hls/")
        || (!path.starts_with("/api/") && !path.starts_with("/thumb/") && path.len() > 1)
}

/// 诊断用中间件：
/// - 对端媒体库接口（/api/*）记录来源地址；
/// - 视频类请求（/media/、/{文件名}、/download/、/hls/）完整记录
///   「谁用什么播放器访问了什么视频」：客户端 IP、User-Agent、Range 原文，
///   以及响应状态码 / Content-Range / Content-Length（实际发送字节数）。
///   User-Agent 是判别播放路径的关键：Media Foundation 网络源是 NSPlayer/WMFSDK。
async fn log_peer_request(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if path.starts_with("/api/") {
        tracing::info!("收到对端媒体库请求: {} {} 来自 {}", method, path, addr);
    }
    let media = is_media_path(&path);
    if media {
        let header_str = |name: header::HeaderName| {
            req.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string()
        };
        tracing::info!(
            "媒体请求: {} {} 来自 {} | UA={} | Range={}",
            method,
            path,
            addr,
            header_str(header::USER_AGENT),
            header_str(header::RANGE)
        );
    }

    let res = next.run(req).await;

    if media {
        let h = res.headers();
        let get = |name: header::HeaderName| {
            h.get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string()
        };
        tracing::info!(
            "媒体响应: {} {} -> {} | Content-Range={} | Content-Length={}",
            method,
            path,
            res.status().as_u16(),
            get(header::CONTENT_RANGE),
            get(header::CONTENT_LENGTH)
        );
    }
    res
}

/// 返回对等节点的媒体清单（用于房间内互相浏览媒体库）
async fn api_media(State(state): State<MediaState>) -> Response<Body> {
    // 未开启「允许其他人查看我的媒体库」时返回空清单（对方看不到任何视频）
    if !state
        .allow_peer_view
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        let body = serde_json::json!({ "items": [] });
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            )
            .body(Body::from(body.to_string()))
            .unwrap();
    }
    let summaries = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        db.list_summaries()
    };
    match summaries {
        Ok(items) => {
            let body = serde_json::json!({ "items": items });
            Response::builder()
                .status(StatusCode::OK)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json; charset=utf-8"),
                )
                .body(Body::from(body.to_string()))
                .unwrap()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

/// 返回本节点的基本信息（未开启「允许他人查看我的媒体库」时不对外暴露）
async fn api_node(State(state): State<MediaState>) -> Response<Body> {
    if !state
        .allow_peer_view
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return not_found();
    }
    let body = serde_json::json!({
        "app": "VRchat-LocalCinema",
        "hostname": crate::easytier::default_member_name(),
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        )
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 以附件形式下载媒体文件
async fn serve_download(
    State(state): State<MediaState>,
    AxumPath(id): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    let item = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.get(&id) {
            Ok(Some(v)) => v,
            _ => return not_found(),
        }
    };
    let path = PathBuf::from(&item.file_path);
    if !crate::media_library::safe_registered_file(&path) {
        return not_found();
    }
    let mut resp = serve_file(&path, &method, &headers, Some("下载"), stream_buf(&state)).await;
    if resp.status().is_success() {
        let encoded: String = percent_encoding::utf8_percent_encode(
            &item.file_name,
            percent_encoding::NON_ALPHANUMERIC,
        )
        .to_string();
        let value = format!("attachment; filename*=UTF-8''{}", encoded);
        if let Ok(v) = HeaderValue::from_str(&value) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    resp
}

/// 阶段 2A：已校验路径缓存的有效期
const VALIDATED_PATH_TTL: Duration = Duration::from_secs(60);
/// 缓存条目上限：超出时顺手清理过期项，避免长期运行无限增长
const VALIDATED_PATH_MAX: usize = 512;

/// 读取「已校验路径缓存」：命中且未过期时返回路径，过期条目立即清除。
///
/// 先克隆出路径再释放 DashMap 分片读锁，避免把读锁带过 `serve_file` 的 await。
fn cached_valid_path(state: &MediaState, key: &str) -> Option<PathBuf> {
    let entry = state.validated_paths.get(key)?;
    let path = entry.value().0.clone();
    let validated_at = entry.value().1;
    drop(entry);
    if validated_at.elapsed() < VALIDATED_PATH_TTL {
        Some(path)
    } else {
        state.validated_paths.remove(key);
        None
    }
}

/// 写入「已校验路径缓存」；条目数超上限时清理过期项。
fn cache_valid_path(state: &MediaState, key: &str, path: &Path) {
    state
        .validated_paths
        .insert(key.to_string(), (path.to_path_buf(), Instant::now()));
    if state.validated_paths.len() > VALIDATED_PATH_MAX {
        state
            .validated_paths
            .retain(|_, (_, at)| at.elapsed() < VALIDATED_PATH_TTL);
    }
}

async fn serve_media_basename(
    State(state): State<MediaState>,
    AxumPath(file_name): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    if !crate::url_builder::valid_basename(&file_name) {
        return not_found();
    }

    // 阶段 2A：已校验路径缓存优先，命中即跳过全部校验与库查询
    if let Some(cached_path) = cached_valid_path(&state, &file_name) {
        return serve_file(&cached_path, &method, &headers, Some("媒体"), stream_buf(&state)).await;
    }

    enum Target {
        Media(PathBuf),
        Hls { id: String, path: PathBuf },
    }
    // 在锁内先完成路径解析，避免跨 await 持有互斥锁
    let target = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.resolve_basename(&file_name) {
            Ok(Some(item)) => Target::Media(PathBuf::from(item.file_path)),
            Ok(None) => {
                // 未匹配到媒体文件时回退查找 HLS 播放列表（根路径短链接）
                if file_name.to_ascii_lowercase().ends_with(".m3u8") {
                    match db.resolve_hls_playlist(&file_name) {
                        Ok(Some(hls_item)) => Target::Hls {
                            id: hls_item.id.clone(),
                            path: state.hls_dir.join(&hls_item.id).join("index.m3u8"),
                        },
                        Ok(None) => {
                            tracing::warn!("404: HLS 播放列表库查询未命中 file_name={}", file_name);
                            return not_found();
                        }
                        Err(e) => {
                            tracing::error!("HLS 播放列表查询出错（返回 409）file_name={} err={}", file_name, e);
                            return Response::builder()
                                .status(StatusCode::CONFLICT)
                                .body(Body::from(e.to_string()))
                                .unwrap()
                        }
                    }
                } else {
                    tracing::warn!("404: 库查询未命中 file_name={}", file_name);
                    return not_found();
                }
            }
            Err(e) => {
                tracing::error!("库查询出错（返回 409）file_name={} err={}", file_name, e);
                return Response::builder()
                    .status(StatusCode::CONFLICT)
                    .body(Body::from(e.to_string()))
                    .unwrap()
            }
        }
    };
    match target {
        Target::Hls { id, path } => {
            if !path.is_file() {
                return not_found();
            }
            // ffmpeg 转码期间会逐段重写 index.m3u8，静态改写会被覆盖，
            // 因此在 HTTP 层读取时动态把相对分片 URI 补全为 /hls/{id}/ 前缀
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => return not_found(),
            };
            let mut out = String::with_capacity(content.len() + 64);
            for line in content.lines() {
                let trimmed = line.trim_end();
                if !trimmed.is_empty()
                    && !trimmed.starts_with('#')
                    && !trimmed.contains("://")
                    && !trimmed.starts_with('/')
                {
                    out.push_str(&format!("/hls/{}/{}", id, trimmed));
                } else {
                    out.push_str(trimmed);
                }
                out.push('\n');
            }
            let mut resp = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
                .body(Body::from(out))
                .unwrap();
            if method == Method::HEAD {
                *resp.body_mut() = Body::empty();
            }
            resp
        }
        Target::Media(path) => {
            // 走完整校验路径，成功后写入缓存
            if !crate::media_library::safe_registered_file(&path) {
                tracing::warn!("404: safe_registered_file 返回 false, path={:?}", path);
                return not_found();
            }
            // 校验通过，写入缓存（后续 Range 请求将跳过校验）
            cache_valid_path(&state, &file_name, &path);
            serve_file(&path, &method, &headers, Some("媒体"), stream_buf(&state)).await
        }
    }
}

async fn serve_media(
    State(state): State<MediaState>,
    AxumPath((id, _file_name)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    // 阶段 2A：与 /{文件名} 路由共用同一份「已校验路径缓存」。
    // 键以 "/media/" 前缀区分（文件名不可能含 '/'，故不会与 basename 键冲突）。
    let cache_key = format!("/media/{}", id);
    if let Some(cached_path) = cached_valid_path(&state, &cache_key) {
        return serve_file(&cached_path, &method, &headers, Some("媒体"), stream_buf(&state))
            .await
            .into_response();
    }
    let item = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.get(&id) {
            Ok(Some(v)) => v,
            _ => return not_found().into_response(),
        }
    };
    let path = std::path::PathBuf::from(&item.file_path);
    if !crate::media_library::safe_registered_file(&path) {
        return not_found().into_response();
    }
    cache_valid_path(&state, &cache_key, &path);
    serve_file(&path, &method, &headers, Some("媒体"), stream_buf(&state))
        .await
        .into_response()
}

async fn serve_media_short(
    State(state): State<MediaState>,
    AxumPath(id): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    serve_media(State(state), AxumPath((id, String::new())), method, headers).await
}

async fn serve_thumb(
    State(state): State<MediaState>,
    AxumPath(thumb_name): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    let thumb_path = sanitize_thumb_path(&state.thumb_dir, &thumb_name);
    let allowed = thumb_path
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
        .unwrap_or(false);
    if !allowed {
        return not_found().into_response();
    }
    let path = thumb_path.unwrap();
    if !path.is_file() {
        return not_found().into_response();
    }
    serve_file(&path, &method, &headers, None, stream_buf(&state))
        .await
        .into_response()
}

/// 提供 HLS 播放列表与分片：/hls/{id}/index.m3u8、/hls/{id}/segNNN.ts
///
/// **已弃用**：HLS 相关命令已在 `lib.rs` 中注释停用，此路由仅为将来可能重新启用
/// 而保留（当前期间并不会有新的 HLS 产物生成，因此实际不会被访问）。
async fn serve_hls(
    State(state): State<MediaState>,
    AxumPath((id, file)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    // id 必须是 UUID（36 位，含连字符），防止路径遍历
    if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return not_found().into_response();
    }
    // 文件名仅允许字母数字/点/下划线/连字符，且不允许隐藏文件
    if file.is_empty()
        || file.starts_with('.')
        || !file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return not_found().into_response();
    }
    let dir = state.hls_dir.join(&id);
    let path = dir.join(&file);
    if !path.starts_with(&state.hls_dir) || !path.is_file() {
        return not_found().into_response();
    }
    let mut resp = serve_file(&path, &method, &headers, None, stream_buf(&state)).await;
    // m3u8 必须使用标准 MIME，部分播放器据此识别
    if file.to_ascii_lowercase().ends_with(".m3u8") {
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.apple.mpegurl"),
        );
    } else if file.to_ascii_lowercase().ends_with(".ts") {
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("video/mp2t"),
        );
    }
    resp.into_response()
}

/// 在线视频（B 站）反向代理：/online/{id}
///
/// 把本软件媒体库中的「在线条目」映射为可被 VRChat 直接播放的本机链接。
/// 实际字节来自 B 站官方 CDN（`upos-*.bilivideo.com`），本路由只做：
/// 1. 从库中取该条目的 bvid/cid，解析（或复用缓存）官方单文件 MP4 直链；
/// 2. 原样转发请求（含 Range）到 CDN 并流式回传，因此可 seek；
/// 3. 直链过期（含 `deadline` 签名，约 2 小时）时自动重新解析；CDN 返回 403/410 时重试一次。
///
/// 仅在环回地址（127.0.0.1）使用，绝不生成局域网链接。
async fn serve_online(
    State(state): State<MediaState>,
    AxumPath(id): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    if !is_safe_id(&id) {
        return not_found();
    }
    // 1. 取在线条目的 bvid / cid
    let key = {
        let db = state.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.get(&id) {
            Ok(Some(item)) if item.source_kind == "online" => item.file_path,
            _ => return not_found(),
        }
    };
    let Some((bvid, cid)) = crate::bilibili::parse_online_key(&key) else {
        return not_found();
    };

    // 2. 取直链（缓存命中且未临近过期时直接复用）
    let now = crate::bilibili::now_secs();
    let cached = {
        let cache = state.online_links.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(&id)
            .filter(|e| e.expires_at - 60 > now)
            .map(|e| e.url.clone())
    };
    let url = match cached {
        Some(u) => u,
        None => match resolve_and_cache(&state, &id, &bvid, cid).await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!("在线视频 {} 解析直链失败: {}", id, e);
                return bad_gateway(&e);
            }
        },
    };

    // 3. 转发到 CDN；直链失效（403/410）时重解析并重试一次
    let resp = forward_to_cdn(&url, &method, &headers).await;
    if matches!(
        resp.status(),
        StatusCode::FORBIDDEN | StatusCode::GONE | StatusCode::UNAUTHORIZED
    ) {
        tracing::warn!(
            "在线视频 {} 直链返回 {}，重新解析后重试",
            id,
            resp.status()
        );
        {
            let mut cache = state.online_links.lock().unwrap_or_else(|e| e.into_inner());
            cache.remove(&id);
        }
        if let Ok(fresh) = resolve_and_cache(&state, &id, &bvid, cid).await {
            return forward_to_cdn(&fresh, &method, &headers).await;
        }
    }
    resp
}

/// 解析官方直链并写入缓存
async fn resolve_and_cache(
    state: &MediaState,
    id: &str,
    bvid: &str,
    cid: i64,
) -> Result<String, String> {
    let link = crate::bilibili::resolve_play_url(bvid, cid)
        .await
        .map_err(|e| e.to_string())?;
    let mut cache = state.online_links.lock().unwrap_or_else(|e| e.into_inner());
    cache.insert(
        id.to_string(),
        CachedLink {
            url: link.url.clone(),
            expires_at: link.expires_at,
        },
    );
    Ok(link.url)
}

/// 原样转发请求到 B 站官方 CDN（注入浏览器 UA 与 Referer，透传 Range），流式回传正文。
async fn forward_to_cdn(url: &str, method: &Method, req_headers: &HeaderMap) -> Response<Body> {
    let mut rb = crate::bilibili::stream_client()
        .request(reqwest::Method::GET, url)
        .header(reqwest::header::REFERER, "https://www.bilibili.com/");
    if let Some(range) = req_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
    {
        rb = rb.header(reqwest::header::RANGE, range.to_string());
    }
    let upstream = match rb.send().await {
        Ok(r) => r,
        Err(e) => return bad_gateway(&format!("访问 B 站 CDN 失败: {}", e)),
    };
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    {
        let out_headers = builder.headers_mut().unwrap();
        // 只透传与播放/寻址相关的响应头，避免把 CDN 的连接类头直接带回去
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
            header::ACCEPT_RANGES,
            header::LAST_MODIFIED,
            header::ETAG,
        ] {
            if let Some(v) = upstream.headers().get(&name) {
                out_headers.insert(name, v.clone());
            }
        }
    }
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(upstream.bytes_stream())
    };
    builder
        .body(body)
        .unwrap_or_else(|_| internal_error("构建在线播放响应失败"))
}

/// 媒体 id 形如 UUID（36 位十六进制 + 连字符）
fn is_safe_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn bad_gateway(msg: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Body::from(msg.to_string()))
        .unwrap()
}

fn sanitize_thumb_path(base: &Path, input: &str) -> Option<PathBuf> {
    let decoded = percent_decode(input);
    let cleaned = decoded.replace('\\', "/");
    let mut safe = PathBuf::new();
    let mut depth: i32 = 0;
    for seg in cleaned.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            if depth <= 0 {
                return None; // 路径遍历
            }
            safe.pop();
            depth -= 1;
            continue;
        }
        safe.push(seg);
        depth += 1;
    }
    let joined = base.join(&safe);
    // 确保仍在 base 下
    if joined.starts_with(base) {
        Some(joined)
    } else {
        None
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 在字节层面做百分号解码，避免对多字节 UTF-8 字符串做字节切片导致 panic。
/// 注意：axum 已对路径参数做过一次百分号解码，这里的 % 多是字面量。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 当前生效的媒体流读缓冲（字节）；设置范围 8KB-4MB，越界收敛
fn stream_buf(state: &MediaState) -> usize {
    state
        .stream_buf_kb
        .load(std::sync::atomic::Ordering::Relaxed)
        .clamp(8, 4096)
        * 1024
}

/// 媒体传输遥测：统计实际交给 HTTP 层的字节数，在正常结束（EOF）或被对端
/// 提前中止（连接断开导致流被丢弃）时各记录一条日志。
/// 响应头日志只能看到「允诺发多少」，看不到「实际发了多少、谁停的」；
/// 这条日志用于区分卡顿是「客户端主动掐断」还是「服务器供数中断」。
struct CountingStream<S> {
    inner: S,
    label: String,
    sent: u64,
    started: Instant,
    done: bool,
}

impl<S> CountingStream<S> {
    fn new(inner: S, label: String) -> Self {
        Self {
            inner,
            label,
            sent: 0,
            started: Instant::now(),
            done: false,
        }
    }

    fn write_log(&self, aborted: bool) {
        let secs = self.started.elapsed().as_secs_f64();
        let kbps = if secs > 0.001 {
            self.sent as f64 / secs / 1024.0
        } else {
            0.0
        };
        tracing::info!(
            "媒体传输{}: {} | 已发={} | 耗时={:.1}s | 均速={:.0}KB/s",
            if aborted { "中止" } else { "完成" },
            self.label,
            self.sent,
            secs,
            kbps
        );
    }

    fn write_error_log(&self, error: &str) {
        let secs = self.started.elapsed().as_secs_f64();
        tracing::error!(
            "媒体传输错误: {} | 已发={} | 耗时={:.1}s | 错误={}",
            self.label,
            self.sent,
            secs,
            error
        );
    }
}

impl<S, E> Stream for CountingStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    type Item = Result<Bytes, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = Pin::new(&mut self.inner).poll_next(cx);
        match &poll {
            Poll::Ready(Some(Ok(chunk))) => self.sent += chunk.len() as u64,
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                self.write_error_log(&format!("{}", e));
            }
            Poll::Ready(None) => {
                self.done = true;
                self.write_log(false);
            }
            _ => {}
        }
        poll
    }
}

impl<S> Drop for CountingStream<S> {
    fn drop(&mut self) {
        if !self.done {
            self.write_log(true);
        }
    }
}

async fn serve_file(
    path: &Path,
    method: &Method,
    headers: &HeaderMap,
    telemetry: Option<&'static str>,
    buf_cap: usize,
) -> Response<Body> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("404: File::open 失败, path={:?} err={}", path, e);
            return not_found();
        }
    };
    let meta = match file.metadata().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("404: metadata 失败, path={:?} err={}", path, e);
            return not_found();
        }
    };
    let total_len = meta.len();
    if total_len == 0 {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime.to_string())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            )
            .body(Body::empty())
            .unwrap();
    }

    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let range_header = headers.get(header::RANGE).cloned();
    // 遥测标签：媒体/下载路由传入，封面与 HLS 分片不记录（量小且刷屏）
    let telemetry_label = telemetry.map(|kind| {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        (kind, name)
    });

    if let Some(range_val) = range_header {
        if let Ok(range_str) = range_val.to_str() {
            if let Ok(ranges) = http_range_header::parse_range_header(range_str) {
                let first = match ranges.validate(total_len) {
                    Ok(v) => v.into_iter().next(),
                    Err(_) => None,
                };
                if let Some(range) = first {
                    // http-range-header 返回的 Range 是闭区间 [start, end]
                    let start = *range.start();
                    let end = (*range.end()).min(total_len - 1);
                    let length = end - start + 1;
                    let body_stream = match file.seek(std::io::SeekFrom::Start(start)).await {
                        Ok(_) => {
                            let limited = file.take(length);
                            let stream = ReaderStream::with_capacity(limited, buf_cap);
                            match &telemetry_label {
                                Some((kind, name)) => Body::from_stream(CountingStream::new(
                                    stream,
                                    format!("{} {} [range {}..+{}]", kind, name, start, length),
                                )),
                                None => Body::from_stream(stream),
                            }
                        }
                        Err(_) => {
                            return internal_error("文件定位失败");
                        }
                    };
                    let content_range = format!("bytes {}-{}/{}", start, end, total_len);
                    // HEAD 只返回头，不发送正文
                    let body = if *method == Method::HEAD {
                        Body::empty()
                    } else {
                        body_stream
                    };
                    return Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(
                            header::CONTENT_TYPE,
                            HeaderValue::from_str(&mime.to_string())
                                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
                        )
                        .header(header::CONTENT_LENGTH, HeaderValue::from(length))
                        .header(
                            header::CONTENT_RANGE,
                            HeaderValue::from_str(&content_range)
                                .unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
                        )
                        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
                        .body(body)
                        .unwrap();
                }
            }
            // 无法解析或越界
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", total_len))
                        .unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
                )
                .body(Body::empty())
                .unwrap();
        }
    }

    // HEAD: 只返回头，不读 body
    if *method == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&mime.to_string())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            )
            .header(header::CONTENT_LENGTH, HeaderValue::from(total_len))
            .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
            .body(Body::empty())
            .unwrap();
    }

    // 无 Range 的完整 GET
    let stream = ReaderStream::with_capacity(file, buf_cap);
    let body = match &telemetry_label {
        Some((kind, name)) => Body::from_stream(CountingStream::new(
            stream,
            format!("{} {} [full {}]", kind, name, total_len),
        )),
        None => Body::from_stream(stream),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&mime.to_string())
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        )
        .header(header::CONTENT_LENGTH, HeaderValue::from(total_len))
        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .body(body)
        .unwrap()
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap()
}

fn internal_error(msg: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from(msg.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_paths_are_logged_but_ui_noise_is_not() {
        // 播放器直连的文件名（VRChat 播放器用的就是这种形式）
        assert!(is_media_path("/Clerks.1994.%E7%96%AF%E7%8B%82.mkv"));
        assert!(is_media_path("/media/abc/c1.mkv"));
        assert!(is_media_path("/download/abc"));
        assert!(is_media_path("/hls/abc/index.m3u8"));
        // 封面、根路径探活、对端媒体库接口不记录（避免刷屏）
        assert!(!is_media_path("/thumb/abc.jpg"));
        assert!(!is_media_path("/"));
        assert!(!is_media_path("/api/media"));
        assert!(!is_media_path("/api/node"));
    }
}
