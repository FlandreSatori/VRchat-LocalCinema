use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub items: Vec<MediaItem>,
    pub skipped: Vec<ImportIssue>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportIssue {
    pub path: String,
    pub reason: String,
}

pub fn safe_registered_file(path: &Path) -> bool {
    if !path.is_file() { return false; }
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() { continue; }
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                if meta.file_type().is_symlink() { return false; }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 { return false; }
                }
            }
            Err(_) => return false,
        }
    }
    true
}

pub const VIDEO_EXTENSIONS: [&str; 9] = [
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaStatus {
    /// 文件可用
    Ok,
    /// 正在后台提取元数据/封面
    Processing,
    /// 文件丢失/无法访问
    Missing,
}

impl MediaStatus {
    fn as_str(&self) -> &'static str {
        match self {
            MediaStatus::Ok => "ok",
            MediaStatus::Processing => "processing",
            MediaStatus::Missing => "missing",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "processing" => MediaStatus::Processing,
            "missing" => MediaStatus::Missing,
            _ => MediaStatus::Ok,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub id: String,
    pub title: String,
    pub file_name: String,
    pub file_path: String,
    pub file_size: u64,
    pub duration_ms: Option<u64>,
    pub format: String,
    /// 封面文件名（相对封面缓存目录），None 表示无封面
    pub thumbnail: Option<String>,
    pub status: MediaStatus,
    pub added_at: i64,
    pub updated_at: i64,
}

pub struct Db {
    conn: Connection,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS media (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                file_name TEXT NOT NULL,
                file_path TEXT NOT NULL UNIQUE,
                file_size INTEGER NOT NULL,
                duration_ms INTEGER,
                format TEXT,
                thumbnail TEXT,
                status TEXT NOT NULL DEFAULT 'processing',
                added_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn is_supported_video(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
    }

    /// 登记一个文件。如果路径已存在则返回已有条目。
    pub fn register(&self, path: &Path) -> AppResult<(MediaItem, bool)> {
        if !safe_registered_file(path) {
            return Err(AppError::Msg("文件不存在或路径含符号链接/重解析点，不能登记".into()));
        }
        let canonical = std::fs::canonicalize(path)?;
        let path_str = canonical.to_string_lossy().to_string();
        for item in self.list()? {
            if item.file_path.eq_ignore_ascii_case(&path_str)
                || std::fs::canonicalize(&item.file_path).ok().as_ref() == Some(&canonical) {
                return Ok((item, false));
            }
        }
        let meta = std::fs::metadata(path)?;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path_str.clone());
        if !crate::url_builder::valid_basename(&file_name) {
            return Err(AppError::Msg("文件名不能映射为播放 URL".into()));
        }
        if let Some(existing) = self.list()?.into_iter().find(|m| m.file_name.to_lowercase() == file_name.to_lowercase()) {
            return Err(AppError::Msg(format!("同名文件冲突（不区分大小写）：{} 已登记为 {}；请移除冲突条目或自行重命名后再导入", file_name, existing.file_path)));
        }
        let title = path
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file_name.clone());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO media (id, title, file_name, file_path, file_size, duration_ms, format, thumbnail, status, added_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL, 'processing', ?7, ?7)",
            params![id, title, file_name, path_str, meta.len() as i64, ext, now],
        )?;
        let item = self.get(&id)?.expect("just inserted");
        Ok((item, true))
    }

    fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<MediaItem> {
        let status: String = row.get("status")?;
        Ok(MediaItem {
            id: row.get("id")?,
            title: row.get("title")?,
            file_name: row.get("file_name")?,
            file_path: row.get("file_path")?,
            file_size: row.get::<_, i64>("file_size")? as u64,
            duration_ms: row.get::<_, Option<i64>>("duration_ms")?.map(|v| v as u64),
            format: row.get::<_, Option<String>>("format")?.unwrap_or_default(),
            thumbnail: row.get("thumbnail")?,
            status: MediaStatus::from_str(&status),
            added_at: row.get("added_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    const SELECT: &'static str =
        "SELECT id, title, file_name, file_path, file_size, duration_ms, format, thumbnail, status, added_at, updated_at FROM media";

    pub fn get(&self, id: &str) -> AppResult<Option<MediaItem>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{} WHERE id = ?1", Self::SELECT))?;
        let mut rows = stmt.query_map(params![id], Self::row_to_item)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_by_path(&self, path: &str) -> AppResult<Option<MediaItem>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{} WHERE file_path = ?1", Self::SELECT))?;
        let mut rows = stmt.query_map(params![path], Self::row_to_item)?;
        Ok(rows.next().transpose()?)
    }

    pub fn list(&self) -> AppResult<Vec<MediaItem>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{} ORDER BY added_at DESC", Self::SELECT))?;
        let rows = stmt.query_map([], Self::row_to_item)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn resolve_basename(&self, name: &str) -> AppResult<Option<MediaItem>> {
        let matches: Vec<_> = self.list()?.into_iter()
            .filter(|m| m.file_name.to_lowercase() == name.to_lowercase()).collect();
        if matches.len() > 1 {
            return Err(AppError::Msg(format!("同名文件冲突：{} 对应多个已登记文件，请先移除冲突条目", name)));
        }
        Ok(matches.into_iter().find(|m| m.file_name == name))
    }

    pub fn import_paths(&self, paths: &[String]) -> ImportResult {
        let mut result = ImportResult { items: Vec::new(), skipped: Vec::new() };
        for p in paths {
            let path = Path::new(p);
            let registered = if !Self::is_supported_video(path) {
                Err(AppError::Msg("不支持的格式".into()))
            } else { self.register(path) };
            match registered {
                Ok((item, _)) => result.items.push(item),
                Err(e) => result.skipped.push(ImportIssue { path: p.clone(), reason: e.to_string() }),
            }
        }
        result
    }

    pub fn remove(&self, id: &str) -> AppResult<Option<MediaItem>> {
        let item = self.get(id)?;
        if item.is_some() {
            self.conn.execute("DELETE FROM media WHERE id = ?1", params![id])?;
        }
        Ok(item)
    }

    pub fn update_after_probe(
        &self,
        id: &str,
        duration_ms: Option<u64>,
        thumbnail: Option<String>,
        status: MediaStatus,
        file_size: Option<u64>,
    ) -> AppResult<Option<MediaItem>> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE media SET duration_ms = COALESCE(?2, duration_ms),
                thumbnail = COALESCE(?3, thumbnail),
                status = ?4,
                file_size = COALESCE(?5, file_size),
                updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                duration_ms.map(|v| v as i64),
                thumbnail,
                status.as_str(),
                file_size.map(|v| v as i64),
                now
            ],
        )?;
        self.get(id)
    }

    pub fn set_status(&self, id: &str, status: MediaStatus) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now_ms()],
        )?;
        Ok(())
    }
}
