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
    if !path.is_file() {
        return false;
    }
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return false;
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return false;
                    }
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

/// 兼容性状态：supported / unsupported；None 表示尚未探测
pub const COMPAT_SUPPORTED: &str = "supported";
pub const COMPAT_UNSUPPORTED: &str = "unsupported";

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
    /// 文件内容 SHA-256（小写十六进制），None 表示尚未计算
    pub hash: Option<String>,
    // ===== ffprobe 深度探测结果 =====
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub audio_codec: Option<String>,
    pub audio_sample_rate: Option<u64>,
    pub audio_channels: Option<u32>,
    /// supported / unsupported；None 表示尚未探测
    pub compat_status: Option<String>,
    pub compat_reason: Option<String>,
    /// 不兼容条目是否已通过转码/HLS 得到处理（处理完成后回到主视图）
    pub compat_resolved: bool,
    /// 已生成的 HLS 播放列表文件名（根路径短链接，如 MyVideo.m3u8）
    pub hls_playlist: Option<String>,
    /// 所属文件夹 id；None 表示根目录（仅映射，不影响磁盘文件）
    pub folder_id: Option<String>,
    /// 对应的源视频 id（转码产物的「快捷方式」卡片用；非转码产物为 None）
    pub source_media_id: Option<String>,
    /// 分段转码的段序号（1 = 第一段，2 = 第二段；非分段产物为 None）
    pub segment_index: Option<i64>,
    pub status: MediaStatus,
    pub added_at: i64,
    pub updated_at: i64,
}

/// 对外暴露的媒体摘要，用于对等节点之间比对媒体（不含本机路径）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSummary {
    pub id: String,
    pub title: String,
    pub file_name: String,
    pub file_size: u64,
    pub duration_ms: Option<u64>,
    pub hash: Option<String>,
}

/// 内置「不兼容」文件夹 id（不可删除/重命名）
pub const BUILTIN_INCOMPATIBLE_ID: &str = "builtin-incompatible";

/// 媒体库文件夹（仅存储映射关系，不影响磁盘文件）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub created_at: i64,
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
                hash TEXT,
                status TEXT NOT NULL DEFAULT 'processing',
                added_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )?;
        // 旧版本数据库升级：补充列（列已存在时忽略错误）
        for col in [
            "ALTER TABLE media ADD COLUMN hash TEXT",
            "ALTER TABLE media ADD COLUMN container TEXT",
            "ALTER TABLE media ADD COLUMN video_codec TEXT",
            "ALTER TABLE media ADD COLUMN pix_fmt TEXT",
            "ALTER TABLE media ADD COLUMN bit_depth INTEGER",
            "ALTER TABLE media ADD COLUMN width INTEGER",
            "ALTER TABLE media ADD COLUMN height INTEGER",
            "ALTER TABLE media ADD COLUMN audio_codec TEXT",
            "ALTER TABLE media ADD COLUMN audio_sample_rate INTEGER",
            "ALTER TABLE media ADD COLUMN audio_channels INTEGER",
            "ALTER TABLE media ADD COLUMN compat_status TEXT",
            "ALTER TABLE media ADD COLUMN compat_reason TEXT",
            "ALTER TABLE media ADD COLUMN compat_resolved INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE media ADD COLUMN hls_playlist TEXT",
            "ALTER TABLE media ADD COLUMN folder_id TEXT",
            "ALTER TABLE media ADD COLUMN source_media_id TEXT",
            "ALTER TABLE media ADD COLUMN segment_index INTEGER",
        ] {
            let _ = conn.execute(col, []);
        }
        // 媒体库文件夹表 + 内置「不兼容」文件夹
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS folders (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                parent_id TEXT,
                created_at INTEGER NOT NULL
            );",
        )?;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO folders (id, name, parent_id, created_at) VALUES (?1, ?2, NULL, ?3)",
            params![BUILTIN_INCOMPATIBLE_ID, "不兼容", now_ms()],
        );
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
            return Err(AppError::Msg(
                "文件不存在或路径含符号链接/重解析点，不能登记".into(),
            ));
        }
        let canonical = std::fs::canonicalize(path)?;
        let path_str = canonical.to_string_lossy().to_string();
        for item in self.list()? {
            if item.file_path.eq_ignore_ascii_case(&path_str)
                || std::fs::canonicalize(&item.file_path).ok().as_ref() == Some(&canonical)
            {
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
        if let Some(existing) = self
            .list()?
            .into_iter()
            .find(|m| m.file_name.to_lowercase() == file_name.to_lowercase())
        {
            return Err(AppError::Msg(format!(
                "同名文件冲突（不区分大小写）：{} 已登记为 {}；请移除冲突条目或自行重命名后再导入",
                file_name, existing.file_path
            )));
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
            hash: row.get("hash")?,
            container: row.get("container")?,
            video_codec: row.get("video_codec")?,
            pix_fmt: row.get("pix_fmt")?,
            bit_depth: row.get::<_, Option<i64>>("bit_depth")?.map(|v| v as u32),
            width: row.get::<_, Option<i64>>("width")?.map(|v| v as u32),
            height: row.get::<_, Option<i64>>("height")?.map(|v| v as u32),
            audio_codec: row.get("audio_codec")?,
            audio_sample_rate: row
                .get::<_, Option<i64>>("audio_sample_rate")?
                .map(|v| v as u64),
            audio_channels: row
                .get::<_, Option<i64>>("audio_channels")?
                .map(|v| v as u32),
            compat_status: row.get("compat_status")?,
            compat_reason: row.get("compat_reason")?,
            compat_resolved: row.get::<_, Option<i64>>("compat_resolved")?.unwrap_or(0) != 0,
            hls_playlist: row.get("hls_playlist")?,
            folder_id: row.get("folder_id")?,
            source_media_id: row.get("source_media_id")?,
            segment_index: row.get::<_, Option<i64>>("segment_index")?,
            status: MediaStatus::from_str(&status),
            added_at: row.get("added_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    const SELECT: &'static str = "SELECT id, title, file_name, file_path, file_size, duration_ms, format, thumbnail, hash, container, video_codec, pix_fmt, bit_depth, width, height, audio_codec, audio_sample_rate, audio_channels, compat_status, compat_reason, compat_resolved, hls_playlist, folder_id, source_media_id, segment_index, status, added_at, updated_at FROM media";

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
        let matches: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|m| m.file_name.to_lowercase() == name.to_lowercase())
            .collect();
        if matches.len() > 1 {
            return Err(AppError::Msg(format!(
                "同名文件冲突：{} 对应多个已登记文件，请先移除冲突条目",
                name
            )));
        }
        Ok(matches.into_iter().find(|m| m.file_name == name))
    }

    pub fn import_paths(&self, paths: &[String]) -> ImportResult {
        let mut result = ImportResult {
            items: Vec::new(),
            skipped: Vec::new(),
        };
        for p in paths {
            let path = Path::new(p);
            let registered = if !Self::is_supported_video(path) {
                Err(AppError::Msg("不支持的格式".into()))
            } else {
                self.register(path)
            };
            match registered {
                Ok((item, _)) => result.items.push(item),
                Err(e) => result.skipped.push(ImportIssue {
                    path: p.clone(),
                    reason: e.to_string(),
                }),
            }
        }
        result
    }

    pub fn remove(&self, id: &str) -> AppResult<Option<MediaItem>> {
        let item = self.get(id)?;
        if item.is_some() {
            self.conn
                .execute("DELETE FROM media WHERE id = ?1", params![id])?;
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
        probe: Option<(&crate::compat::ProbeInfo, &crate::compat::CompatResult)>,
    ) -> AppResult<Option<MediaItem>> {
        let now = now_ms();
        if let Some((info, compat)) = probe {
            self.conn.execute(
                "UPDATE media SET duration_ms = COALESCE(?2, duration_ms),
                    thumbnail = COALESCE(?3, thumbnail),
                    status = ?4,
                    file_size = COALESCE(?5, file_size),
                    container = ?6,
                    video_codec = ?7,
                    pix_fmt = ?8,
                    bit_depth = ?9,
                    width = ?10,
                    height = ?11,
                    audio_codec = ?12,
                    audio_sample_rate = ?13,
                    audio_channels = ?14,
                    compat_status = ?15,
                    compat_reason = ?16,
                    updated_at = ?17
                 WHERE id = ?1",
                params![
                    id,
                    duration_ms.map(|v| v as i64),
                    thumbnail,
                    status.as_str(),
                    file_size.map(|v| v as i64),
                    info.container,
                    info.video_codec,
                    info.pix_fmt,
                    info.bit_depth.map(|v| v as i64),
                    info.width.map(|v| v as i64),
                    info.height.map(|v| v as i64),
                    info.audio_codec,
                    info.audio_sample_rate.map(|v| v as i64),
                    info.audio_channels.map(|v| v as i64),
                    if compat.supported {
                        COMPAT_SUPPORTED
                    } else {
                        COMPAT_UNSUPPORTED
                    },
                    compat.reason,
                    now
                ],
            )?;
        } else {
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
        }
        self.get(id)
    }

    pub fn set_status(&self, id: &str, status: MediaStatus) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now_ms()],
        )?;
        Ok(())
    }

    pub fn set_hash(&self, id: &str, hash: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET hash = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, hash, now_ms()],
        )?;
        Ok(())
    }

    /// 标记不兼容条目已通过转码/HLS 处理
    pub fn set_compat_resolved(&self, id: &str, resolved: bool) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET compat_resolved = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, resolved as i64, now_ms()],
        )?;
        Ok(())
    }

    /// 记录转码产物对应的源视频 id（前端据此渲染「快捷方式」卡片并支持跳转源位置）
    pub fn set_source_media(&self, id: &str, source_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET source_media_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, source_id, now_ms()],
        )?;
        Ok(())
    }

    /// 记录分段转码的段序号（1/2）
    pub fn set_segment_index(&self, id: &str, index: i64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET segment_index = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, index, now_ms()],
        )?;
        Ok(())
    }

    /// 列出某源视频的全部分段产物（按段序号排序）
    pub fn list_segments_of(&self, source_id: &str) -> AppResult<Vec<MediaItem>> {
        let mut items: Vec<MediaItem> = self
            .list()?
            .into_iter()
            .filter(|m| m.source_media_id.as_deref() == Some(source_id) && m.segment_index.is_some())
            .collect();
        items.sort_by_key(|m| m.segment_index.unwrap_or(0));
        Ok(items)
    }

    /// 记录 HLS 播放列表文件名（根路径短链接）
    pub fn set_hls_playlist(&self, id: &str, playlist: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET hls_playlist = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, playlist, now_ms()],
        )?;
        Ok(())
    }

    /// 按播放列表文件名查找媒体条目
    pub fn resolve_hls_playlist(&self, name: &str) -> AppResult<Option<MediaItem>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{} WHERE hls_playlist = ?1", Self::SELECT))?;
        let mut rows = stmt.query_map(params![name], Self::row_to_item)?;
        Ok(rows.next().transpose()?)
    }

    /// 清除 HLS 登记（边转边播失败回滚）
    pub fn clear_hls_playlist(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE media SET hls_playlist = NULL, compat_resolved = 0, updated_at = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }

    // ---------- 文件夹（仅映射，不影响磁盘文件） ----------

    fn get_folder(&self, id: &str) -> AppResult<Option<Folder>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, parent_id, created_at FROM folders WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(Folder {
                id: row.get("id")?,
                name: row.get("name")?,
                parent_id: row.get("parent_id")?,
                created_at: row.get("created_at")?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn list_folders(&self) -> AppResult<Vec<Folder>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, parent_id, created_at FROM folders ORDER BY created_at ASC, name ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Folder {
                id: row.get("id")?,
                name: row.get("name")?,
                parent_id: row.get("parent_id")?,
                created_at: row.get("created_at")?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按名称+父级查找文件夹，不存在则创建（导入目录结构时使用）
    pub fn get_or_create_folder(&self, name: &str, parent_id: Option<&str>) -> AppResult<Folder> {
        for f in self.list_folders()? {
            if f.parent_id.as_deref() == parent_id && f.name == name {
                return Ok(f);
            }
        }
        self.create_folder(name, parent_id)
    }

    pub fn create_folder(&self, name: &str, parent_id: Option<&str>) -> AppResult<Folder> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Msg("文件夹名不能为空".into()));
        }
        if name.len() > 100 {
            return Err(AppError::Msg("文件夹名过长（最多 100 字符）".into()));
        }
        if let Some(pid) = parent_id {
            if self.get_folder(pid)?.is_none() {
                return Err(AppError::Msg("目标文件夹不存在".into()));
            }
        }
        for f in self.list_folders()? {
            if f.parent_id.as_deref() == parent_id && f.name == name {
                return Err(AppError::Msg(format!("同级已存在同名文件夹「{}」", name)));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO folders (id, name, parent_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, parent_id, now],
        )?;
        Ok(Folder {
            id,
            name: name.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            created_at: now,
        })
    }

    pub fn rename_folder(&self, id: &str, name: &str) -> AppResult<()> {
        if id == BUILTIN_INCOMPATIBLE_ID {
            return Err(AppError::Msg("内置文件夹不能重命名".into()));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Msg("文件夹名不能为空".into()));
        }
        let folder = self
            .get_folder(id)?
            .ok_or_else(|| AppError::Msg("文件夹不存在".into()))?;
        for f in self.list_folders()? {
            if f.id != id && f.parent_id == folder.parent_id && f.name == name {
                return Err(AppError::Msg(format!("同级已存在同名文件夹「{}」", name)));
            }
        }
        self.conn.execute(
            "UPDATE folders SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// 删除文件夹：其内媒体移回根目录，子文件夹递归删除
    pub fn delete_folder(&self, id: &str) -> AppResult<()> {
        if id == BUILTIN_INCOMPATIBLE_ID {
            return Err(AppError::Msg("内置文件夹不能删除".into()));
        }
        self.get_folder(id)?
            .ok_or_else(|| AppError::Msg("文件夹不存在".into()))?;
        let all = self.list_folders()?;
        let mut to_delete = vec![id.to_string()];
        loop {
            let mut grew = false;
            for f in &all {
                if let Some(p) = &f.parent_id {
                    if to_delete.contains(p) && !to_delete.contains(&f.id) {
                        to_delete.push(f.id.clone());
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        for fid in &to_delete {
            self.conn.execute(
                "UPDATE media SET folder_id = NULL, updated_at = ?2 WHERE folder_id = ?1",
                params![fid, now_ms()],
            )?;
            self.conn.execute("DELETE FROM folders WHERE id = ?1", params![fid])?;
        }
        Ok(())
    }

    /// 设置媒体所属文件夹（None = 根目录）
    pub fn set_media_folder(&self, id: &str, folder_id: Option<&str>) -> AppResult<()> {
        if let Some(fid) = folder_id {
            if self.get_folder(fid)?.is_none() {
                return Err(AppError::Msg("目标文件夹不存在".into()));
            }
        }
        self.conn.execute(
            "UPDATE media SET folder_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, folder_id, now_ms()],
        )?;
        Ok(())
    }

    /// 重命名媒体条目（仅映射关系：file_name/title，不影响磁盘文件；播放链接随 file_name 变化）
    pub fn rename_media(&self, id: &str, new_name: &str) -> AppResult<MediaItem> {
        let name = new_name.trim();
        if !crate::url_builder::valid_basename(name) {
            return Err(AppError::Msg("文件名含非法字符，无法映射为播放 URL".into()));
        }
        let ext = std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            return Err(AppError::Msg(format!(
                "扩展名 .{} 不是支持的视频类型，请保留原扩展名",
                ext
            )));
        }
        let item = self
            .get(id)?
            .ok_or_else(|| AppError::Msg("媒体条目不存在".into()))?;
        for m in self.list()? {
            if m.id != id && m.file_name.to_lowercase() == name.to_lowercase() {
                return Err(AppError::Msg(format!(
                    "同名文件已存在：{}（不区分大小写）",
                    m.file_name
                )));
            }
        }
        let title = std::path::Path::new(name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| name.to_string());
        self.conn.execute(
            "UPDATE media SET file_name = ?2, title = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, name, title, now_ms()],
        )?;
        let _ = item;
        Ok(self.get(id)?.expect("just updated"))
    }

    /// 返回可供对等节点比对的媒体摘要（文件仍存在的条目）
    pub fn list_summaries(&self) -> AppResult<Vec<MediaSummary>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|m| m.status != MediaStatus::Missing)
            .map(|m| MediaSummary {
                id: m.id,
                title: m.title,
                file_name: m.file_name,
                file_size: m.file_size,
                duration_ms: m.duration_ms,
                hash: m.hash,
            })
            .collect())
    }
}
