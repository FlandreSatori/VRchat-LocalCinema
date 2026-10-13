use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
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
}

#[derive(Clone)]
pub struct HttpServer {
    status: Arc<RwLock<ServerStatus>>,
    state: MediaState,
    tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<()>>>>,
}

impl HttpServer {
    pub fn new(db: Arc<Mutex<Db>>, thumb_dir: &Path) -> Self {
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
            },
            tx: Arc::new(RwLock::new(None)),
        }
    }

    pub fn status(&self) -> ServerStatus {
        self.status.read().unwrap().clone()
    }

    /// 启动 HTTP 服务。会检查端口是否被占用。
    pub async fn start(&self, bind_ip: &str, port: u16) -> AppResult<()> {
        {
            let s = self.status.read().unwrap();
            if s.running {
                return Ok(());
            }
        }
        let addr_str = format!("{}:{}", bind_ip, port);
        let addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| crate::error::AppError::Msg(format!("解析绑定地址失败: {}", e)))?;
        // 预先检测端口是否可用
        let test_listener = TcpListener::bind(addr)
            .await
            .map_err(|e| crate::error::AppError::Msg(format!("端口 {} 已被占用或无法绑定: {}", port, e)))?;
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
            .route("/", get(|| async { "VRchat-LocalCinema" }))
            .route("/{file_name}", get(serve_media_basename))
            .layer(ConcurrencyLimitLayer::new(64))
            .with_state(self.state.clone());

        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        {
            let mut lock = self.tx.write().unwrap();
            *lock = Some(tx);
        }
        {
            let mut s = self.status.write().unwrap();
            *s = ServerStatus {
                running: true,
                bind_addr: actual_addr.clone(),
                port: actual_port,
                error: None,
            };
        }
        tracing::info!("HTTP 服务启动: {}", actual_addr);
        let srv = axum::serve(listener, app);

        let status = self.status.clone();
        tokio::spawn(async move {
            let serve = srv.with_graceful_shutdown(async move {
                rx.recv().await;
            });
            if let Err(e) = serve.await {
                tracing::error!("HTTP 服务错误: {}", e);
                let mut s = status.write().unwrap();
                s.running = false;
                s.error = Some(e.to_string());
            } else {
                tracing::info!("HTTP 服务已停止");
                let mut s = status.write().unwrap();
                s.running = false;
            }
        });
        Ok(())
    }

    pub fn stop(&self) {
        let tx = self.tx.read().unwrap().clone();
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
                let mut s = this.status.write().unwrap();
                s.running = false;
                s.error = Some(e.to_string());
            }
        });
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
    let item = {
        let db = state.db.lock().unwrap();
        match db.resolve_basename(&file_name) {
            Ok(Some(item)) => item,
            Ok(None) => return not_found(),
            Err(e) => return Response::builder()
                .status(StatusCode::CONFLICT)
                .body(Body::from(e.to_string())).unwrap(),
        }
    };
    let path = PathBuf::from(item.file_path);
    if !crate::media_library::safe_registered_file(&path) {
        return not_found();
    }
    serve_file(&path, &method, &headers).await
}

async fn serve_media(
    State(state): State<MediaState>,
    AxumPath((id, _file_name)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> impl IntoResponse {
    let item = {
        let db = state.db.lock().unwrap();
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
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp"))
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

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
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
        let mut hdr = HeaderMap::new();
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        hdr.insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime.to_string()).unwrap_or(HeaderValue::from_static("application/octet-stream")));
        return Response::builder()
            .status(StatusCode::OK)
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
                    return Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_TYPE, HeaderValue::from_str(&mime.to_string()).unwrap_or(HeaderValue::from_static("application/octet-stream")))
                        .header(header::CONTENT_LENGTH, HeaderValue::from(length))
                        .header(
                            header::CONTENT_RANGE,
                            HeaderValue::from_str(&content_range)
                                .unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
                        )
                        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
                        .body(body_stream)
                        .unwrap();
                }
            }
            // 无法解析或越界
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", total_len)).unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
                )
                .body(Body::empty())
                .unwrap();
        }
    }

    // HEAD: 只返回头，不读 body
    if *method == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, HeaderValue::from_str(&mime.to_string()).unwrap_or(HeaderValue::from_static("application/octet-stream")))
            .header(header::CONTENT_LENGTH, HeaderValue::from(total_len))
            .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
            .body(Body::empty())
            .unwrap();
    }

    let stream = ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_str(&mime.to_string()).unwrap_or(HeaderValue::from_static("application/octet-stream")))
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
