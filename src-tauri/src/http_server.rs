use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path as AxumPath, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, Method, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
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
}

#[derive(Clone)]
pub struct HttpServer {
    status: Arc<RwLock<ServerStatus>>,
    state: MediaState,
    tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<()>>>>,
}

impl HttpServer {
    pub fn new(
        db: Arc<Mutex<Db>>,
        thumb_dir: &Path,
        hls_dir: &Path,
        allow_peer_view: Arc<std::sync::atomic::AtomicBool>,
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
            },
            tx: Arc::new(RwLock::new(None)),
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
        {
            let mut s = self.status.write().unwrap_or_else(|e| e.into_inner());
            *s = ServerStatus {
                running: true,
                bind_addr: actual_addr.clone(),
                port: actual_port,
                error: None,
            };
        }
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
        tokio::spawn(async move {
            let serve = srv.with_graceful_shutdown(async move {
                rx.recv().await;
            });
            if let Err(e) = serve.await {
                tracing::error!("HTTP 服务错误: {}", e);
                let mut s = status.write().unwrap_or_else(|e| e.into_inner());
                s.running = false;
                s.error = Some(e.to_string());
            } else {
                tracing::info!("HTTP 服务已停止");
                let mut s = status.write().unwrap_or_else(|e| e.into_inner());
                s.running = false;
            }
        });
        Ok(())
    }

    pub fn stop(&self) {
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
    let mut resp = serve_file(&path, &method, &headers).await;
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

async fn serve_media_basename(
    State(state): State<MediaState>,
    AxumPath(file_name): AxumPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    if !crate::url_builder::valid_basename(&file_name) {
        return not_found();
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
                        Ok(None) => return not_found(),
                        Err(e) => {
                            return Response::builder()
                                .status(StatusCode::CONFLICT)
                                .body(Body::from(e.to_string()))
                                .unwrap()
                        }
                    }
                } else {
                    return not_found();
                }
            }
            Err(e) => {
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
            if !crate::media_library::safe_registered_file(&path) {
                return not_found();
            }
            serve_file(&path, &method, &headers).await
        }
    }
}

async fn serve_media(
    State(state): State<MediaState>,
    AxumPath((id, _file_name)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> impl IntoResponse {
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
    serve_file(&path, &method, &headers).await.into_response()
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
    serve_file(&path, &method, &headers).await.into_response()
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
    let mut resp = serve_file(&path, &method, &headers).await;
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

async fn serve_file(path: &Path, method: &Method, headers: &HeaderMap) -> Response<Body> {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return not_found(),
    };
    let meta = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return not_found(),
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
                            let stream = ReaderStream::new(limited);
                            Body::from_stream(stream)
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

    let stream = ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(&mime.to_string())
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        )
        .header(header::CONTENT_LENGTH, HeaderValue::from(total_len))
        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .body(Body::from_stream(stream))
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
