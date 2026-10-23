use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::Serialize;

const HISTORY_DURATION_MS: u64 = 3_600_000;
const BASELINE_WINDOW_MS: u64 = 60_000;
const DEFAULT_BASELINE_MBPS: f64 = 10.0;

#[derive(Clone)]
pub struct TrafficStats {
    inner: Arc<TrafficInner>,
}

struct TrafficInner {
    upload: AtomicU64,
    download: AtomicU64,
    local_upload: AtomicU64,
    local_download: AtomicU64,
    session_id: String,
    started_at: i64,
    db: Mutex<TrafficDb>,
}

struct TrafficDb {
    connection: Connection,
    last_flush: i64,
    finished: bool,
    baseline_mbps: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficPoint {
    pub timestamp: i64,
    pub duration_ms: u64,
    pub upload_bytes: u64,
    pub download_bytes: u64,
    pub local_upload_bytes: u64,
    pub local_download_bytes: u64,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficStatsResult {
    pub points: Vec<TrafficPoint>,
    pub total_upload_bytes: u64,
    pub total_download_bytes: u64,
    pub total_local_upload_bytes: u64,
    pub total_local_download_bytes: u64,
    pub active_duration_ms: u64,
    pub started_at: i64,
    pub upload_baseline_mbps: f64,
}

impl TrafficStats {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::from_connection_at(Connection::open(path)?, now_ms())
    }

    fn from_connection_at(mut connection: Connection, now: i64) -> rusqlite::Result<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS sessions(
                 id TEXT PRIMARY KEY,
                 started_at INTEGER NOT NULL,
                 ended_at INTEGER,
                 heartbeat_ms INTEGER
             );
             CREATE TABLE IF NOT EXISTS buckets(
                 session_id TEXT NOT NULL,
                 start_ms INTEGER NOT NULL,
                 end_ms INTEGER NOT NULL,
                 upload INTEGER NOT NULL DEFAULT 0,
                 download INTEGER NOT NULL DEFAULT 0,
                 local_upload INTEGER NOT NULL DEFAULT 0,
                 local_download INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(session_id,start_ms)
             );",
        )?;
        let has_heartbeat = {
            let mut statement = connection.prepare("PRAGMA table_info(sessions)")?;
            let names = statement.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for name in names {
                found |= name? == "heartbeat_ms";
            }
            found
        };
        let transaction = connection.transaction()?;
        if !has_heartbeat {
            transaction.execute_batch("ALTER TABLE sessions ADD COLUMN heartbeat_ms INTEGER;")?;
        }
        // 旧版本的 buckets 表没有本机/远程分流列，补齐后历史流量按远程计
        let missing_local_columns = {
            let mut statement = transaction.prepare("PRAGMA table_info(buckets)")?;
            let names = statement.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for name in names {
                found |= name? == "local_upload";
            }
            !found
        };
        if missing_local_columns {
            transaction.execute_batch(
                "ALTER TABLE buckets ADD COLUMN local_upload INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE buckets ADD COLUMN local_download INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        transaction.execute_batch(
            "UPDATE sessions SET heartbeat_ms = MAX(started_at, COALESCE(
                 heartbeat_ms,
                 (SELECT MAX(end_ms) FROM buckets WHERE session_id=sessions.id),
                 ended_at, started_at
             ));
             UPDATE sessions SET ended_at=heartbeat_ms WHERE ended_at IS NULL;",
        )?;
        transaction.commit()?;
        let baseline_mbps = baseline_from_connection(&connection)?;
        let started_at: i64 = connection.query_row(
            "SELECT MIN(started_at) FROM sessions",
            [],
            |row| Ok(row.get::<_, Option<i64>>(0)?.unwrap_or(now).min(now)),
        )?;
        let session_id = uuid::Uuid::new_v4().to_string();
        connection.execute(
            "INSERT INTO sessions(id,started_at,heartbeat_ms) VALUES(?1,?2,?2)",
            params![session_id, now],
        )?;
        Ok(Self {
            inner: Arc::new(TrafficInner {
                upload: AtomicU64::new(0),
                download: AtomicU64::new(0),
                local_upload: AtomicU64::new(0),
                local_download: AtomicU64::new(0),
                session_id,
                started_at,
                db: Mutex::new(TrafficDb {
                    connection,
                    last_flush: now,
                    finished: false,
                    baseline_mbps,
                }),
            }),
        })
    }

    pub fn record_upload(&self, bytes: u64, is_local: bool) {
        self.inner.upload.fetch_add(bytes, Ordering::Relaxed);
        if is_local {
            self.inner.local_upload.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    pub fn record_download(&self, bytes: u64, is_local: bool) {
        self.inner.download.fetch_add(bytes, Ordering::Relaxed);
        if is_local {
            self.inner.local_download.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    pub fn baseline_mbps(&self) -> f64 {
        self.inner.db.lock().unwrap_or_else(|e| e.into_inner()).baseline_mbps
    }

    pub fn flush(&self) -> rusqlite::Result<()> {
        self.flush_at(now_ms(), false)
    }

    pub fn finish(&self) -> rusqlite::Result<()> {
        self.flush_at(now_ms(), true)
    }

    fn flush_at(&self, now: i64, finish: bool) -> rusqlite::Result<()> {
        let mut db = self.inner.db.lock().unwrap_or_else(|e| e.into_inner());
        if db.finished {
            return Ok(());
        }
        let start = db.last_flush;
        let mut end = now.max(start);
        if end == start && !finish {
            return Ok(());
        }
        let upload = self.inner.upload.swap(0, Ordering::Relaxed);
        let download = self.inner.download.swap(0, Ordering::Relaxed);
        let local_upload = self.inner.local_upload.swap(0, Ordering::Relaxed);
        let local_download = self.inner.local_download.swap(0, Ordering::Relaxed);
        if end == start && (upload > 0 || download > 0) {
            end += 1;
        }
        let result = (|| {
            let transaction = db.connection.transaction()?;
            if end > start {
                transaction.execute(
                    "INSERT INTO buckets(session_id,start_ms,end_ms,upload,download,local_upload,local_download)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![self.inner.session_id, start, end, upload, download, local_upload, local_download],
                )?;
            }
            transaction.execute(
                "UPDATE sessions SET heartbeat_ms=?2,
                 ended_at=CASE WHEN ?3 THEN ?2 ELSE ended_at END WHERE id=?1",
                params![self.inner.session_id, end, finish],
            )?;
            transaction.commit()
        })();
        if let Err(error) = result {
            self.inner.upload.fetch_add(upload, Ordering::Relaxed);
            self.inner.download.fetch_add(download, Ordering::Relaxed);
            self.inner.local_upload.fetch_add(local_upload, Ordering::Relaxed);
            self.inner.local_download.fetch_add(local_download, Ordering::Relaxed);
            return Err(error);
        }
        db.last_flush = end;
        db.finished = finish;
        db.baseline_mbps = baseline_from_connection(&db.connection)?;
        Ok(())
    }

    pub fn query(&self, range: &str) -> rusqlite::Result<TrafficStatsResult> {
        self.query_at(range, now_ms())
    }

    fn query_at(&self, range: &str, now: i64) -> rusqlite::Result<TrafficStatsResult> {
        let since = match range {
            "1h" => now.saturating_sub(HISTORY_DURATION_MS as i64),
            "24h" => now.saturating_sub(86_400_000),
            "all" => i64::MIN,
            _ => return Err(rusqlite::Error::InvalidParameterName(
                "range 只能为 1h、24h 或 all".to_string(),
            )),
        };
        let db = self.inner.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = db.connection.prepare(
            "SELECT b.start_ms,b.end_ms,b.upload,b.download,
                    COALESCE(b.local_upload,0),COALESCE(b.local_download,0),
                    b.session_id,s.started_at,
                    COALESCE(s.ended_at,s.heartbeat_ms,s.started_at)
             FROM buckets b JOIN sessions s ON s.id=b.session_id
             WHERE b.end_ms>?1 AND b.start_ms<?2 ORDER BY b.start_ms,b.session_id",
        )?;
        let rows = statement.query_map(params![since, now], |row| {
            Ok((
                row.get::<_, i64>(0)?, 
                row.get::<_, i64>(1)?, 
                row.get::<_, u64>(2)?, 
                row.get::<_, u64>(3)?,
                row.get::<_, u64>(4)?,
                row.get::<_, u64>(5)?,
                row.get::<_, String>(6)?, 
                row.get::<_, i64>(7)?, 
                row.get::<_, i64>(8)?
            ))
        })?;
        let mut points = Vec::new();
        for row in rows {
            let (start, end, upload, download, local_upload, local_download, id, session_start, session_end) = row?;
            if let Some(point) = clipped_point(start, end, upload, download, local_upload, local_download, id, since.max(session_start), now.min(session_end)) {
                points.push(point);
            }
        }
        if !db.finished {
            if let Some(point) = clipped_point(
                db.last_flush, 
                now, 
                self.inner.upload.load(Ordering::Relaxed), 
                self.inner.download.load(Ordering::Relaxed),
                self.inner.local_upload.load(Ordering::Relaxed),
                self.inner.local_download.load(Ordering::Relaxed),
                self.inner.session_id.clone(), 
                since, 
                now
            ) {
                points.push(point);
            }
        }
        points.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.session_id.cmp(&b.session_id)));
        Ok(TrafficStatsResult {
            total_upload_bytes: points.iter().map(|point| point.upload_bytes).sum(),
            total_download_bytes: points.iter().map(|point| point.download_bytes).sum(),
            total_local_upload_bytes: points.iter().map(|point| point.local_upload_bytes).sum(),
            total_local_download_bytes: points.iter().map(|point| point.local_download_bytes).sum(),
            active_duration_ms: points.iter().filter(|point| point.upload_bytes > 0).map(|point| point.duration_ms).sum(),
            points,
            started_at: self.inner.started_at,
            upload_baseline_mbps: db.baseline_mbps,
        })
    }
}

fn clipped_point(start: i64, end: i64, upload: u64, download: u64, local_upload: u64, local_download: u64, session_id: String, since: i64, until: i64) -> Option<TrafficPoint> {
    let from = start.max(since);
    let to = end.min(until);
    if to <= from { return None; }
    let duration_ms = (to - from) as u64;
    let full_duration = (end - start) as u128;
    Some(TrafficPoint {
        timestamp: from,
        duration_ms,
        upload_bytes: (upload as u128 * duration_ms as u128 / full_duration) as u64,
        download_bytes: (download as u128 * duration_ms as u128 / full_duration) as u64,
        local_upload_bytes: (local_upload as u128 * duration_ms as u128 / full_duration) as u64,
        local_download_bytes: (local_download as u128 * duration_ms as u128 / full_duration) as u64,
        session_id,
    })
}

fn baseline_from_connection(db: &Connection) -> rusqlite::Result<f64> {
    let mut statement = db.prepare(
        "SELECT b.session_id,b.start_ms,b.end_ms,b.upload,s.started_at,
                COALESCE(s.ended_at,s.heartbeat_ms,s.started_at)
         FROM buckets b JOIN sessions s ON s.id=b.session_id
         WHERE b.end_ms>b.start_ms ORDER BY b.session_id,b.start_ms",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, u64>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?)))?;
    let mut maximum = 0.0f64;
    let mut samples: Vec<(i64, i64, u64)> = Vec::new();
    let mut previous_session = String::new();
    let mut previous_end = i64::MIN;
    for row in rows {
        let (session, start, end, upload, session_start, session_end) = row?;
        let start = start.max(session_start);
        let end = end.min(session_end);
        if end <= start { continue; }
        if session != previous_session || start != previous_end {
            samples.clear();
        }
        samples.push((start, end, upload));
        previous_session = session;
        previous_end = end;
        let window_start = end - BASELINE_WINDOW_MS as i64;
        let mut rates = Vec::new();
        let mut covered = 0u64;
        for (sample_start, sample_end, bytes) in samples.iter().rev() {
            if *sample_end <= window_start { break; }
            let overlap = (*sample_end).min(end) - (*sample_start).max(window_start);
            if overlap > 0 {
                let duration = (*sample_end - *sample_start) as f64;
                rates.push((*bytes as f64 * 8.0 / (duration * 1000.0), overlap as u64));
                covered += overlap as u64;
            }
        }
        if covered == BASELINE_WINDOW_MS {
            rates.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut accumulated = 0;
            let median = rates.into_iter().find_map(|(rate, duration)| {
                accumulated += duration;
                (accumulated * 2 >= BASELINE_WINDOW_MS).then_some(rate)
            }).unwrap_or(0.0);
            maximum = maximum.max(median);
        }
    }
    Ok(if maximum > 0.0 { maximum } else { DEFAULT_BASELINE_MBPS })
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats_at(now: i64) -> TrafficStats {
        TrafficStats::from_connection_at(Connection::open_in_memory().unwrap(), now).unwrap()
    }

    #[test]
    fn flush_preserves_real_intervals_across_minute_and_counts_idle_once() {
        let stats = stats_at(59_000);
        stats.record_upload(8_000, false);
        stats.record_download(4_000, false);
        stats.flush_at(61_000, false).unwrap();
        stats.flush_at(71_000, false).unwrap();
        let result = stats.query_at("all", 76_000).unwrap();
        assert_eq!(result.total_upload_bytes, 8_000);
        assert_eq!(result.total_download_bytes, 4_000);
        assert_eq!(result.active_duration_ms, 2_000);
        assert_eq!(result.points.len(), 3);
        assert_eq!(result.points[0].timestamp, 59_000);
        assert_eq!(result.points[0].duration_ms, 2_000);
        assert_eq!(result.points[1].duration_ms, 10_000);
        assert_eq!(result.points[1].upload_bytes, 0);
        let db = stats.inner.db.lock().unwrap();
        let interval: (i64, i64) = db.connection.query_row("SELECT start_ms,end_ms FROM buckets ORDER BY start_ms LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(interval, (59_000, 61_000));
    }

    #[test]
    fn live_bytes_move_to_database_without_double_counting() {
        let stats = stats_at(100);
        stats.record_upload(1_000, false);
        stats.record_download(500, false);
        for _ in 0..2 {
            let result = stats.query_at("all", 200).unwrap();
            assert_eq!(result.total_upload_bytes, 1_000);
            assert_eq!(result.total_download_bytes, 500);
            assert_eq!(result.active_duration_ms, 100);
        }
        stats.flush_at(200, false).unwrap();
        stats.record_upload(250, false);
        let result = stats.query_at("all", 300).unwrap();
        assert_eq!(result.total_upload_bytes, 1_250);
        assert_eq!(result.active_duration_ms, 200);
        stats.flush_at(300, false).unwrap();
        assert_eq!(stats.query_at("all", 300).unwrap().total_upload_bytes, 1_250);
    }

    #[test]
    fn wall_clock_windows_clip_bytes_and_duration_proportionally() {
        let stats = stats_at(0);
        stats.record_upload(4_000, false);
        stats.record_download(2_000, false);
        stats.flush_at(4_000, false).unwrap();
        let hour = stats.query_at("1h", 3_602_000).unwrap();
        assert_eq!(hour.points[0].timestamp, 2_000);
        assert_eq!(hour.points[0].duration_ms, 2_000);
        assert_eq!(hour.total_upload_bytes, 2_000);
        assert_eq!(hour.total_download_bytes, 1_000);
        assert_eq!(hour.active_duration_ms, 2_000);
        assert_eq!(stats.query_at("all", 3_602_000).unwrap().total_upload_bytes, 4_000);
        let day = stats.query_at("24h", 86_403_000).unwrap();
        assert_eq!(day.total_upload_bytes, 1_000);
        assert_eq!(day.active_duration_ms, 1_000);
        assert!(stats.query_at("week", 4_000).is_err());
        assert!(stats.query_at("", 4_000).is_err());
    }

    #[test]
    fn recovery_caps_crashed_sessions_at_heartbeat_and_preserves_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.sqlite");
        {
            let stats = TrafficStats::from_connection_at(Connection::open(&path).unwrap(), 1_000).unwrap();
            stats.record_upload(1_000, false);
            stats.flush_at(11_000, false).unwrap();
        }
        let stats = TrafficStats::from_connection_at(Connection::open(&path).unwrap(), 3_600_000).unwrap();
        let result = stats.query_at("all", 3_610_000).unwrap();
        assert_eq!(result.started_at, 1_000);
        assert_eq!(result.active_duration_ms, 10_000);
        assert_eq!(result.points.len(), 2);
        assert_ne!(result.points[0].session_id, result.points[1].session_id);
        assert_eq!(stats.query_at("1h", 7_200_000).unwrap().total_upload_bytes, 0);
    }

    #[test]
    fn migrates_old_sessions_without_counting_downtime() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,started_at INTEGER NOT NULL,ended_at INTEGER); CREATE TABLE buckets(session_id TEXT,start_ms INTEGER,end_ms INTEGER,upload INTEGER,download INTEGER,PRIMARY KEY(session_id,start_ms)); INSERT INTO sessions VALUES('old',1000,NULL),('empty',12000,NULL); INSERT INTO buckets VALUES('old',0,11000,11000,0);").unwrap();
        let stats = TrafficStats::from_connection_at(connection, 100_000).unwrap();
        let result = stats.query_at("all", 101_000).unwrap();
        assert_eq!(result.started_at, 1_000);
        assert_eq!(result.active_duration_ms, 10_000);
        assert_eq!(result.total_upload_bytes, 10_000);
    }

    #[test]
    fn baseline_requires_complete_contiguous_minute_and_uses_maximum_persistent_rate() {
        let stats = stats_at(0);
        stats.record_upload(7_500_000, false);
        stats.flush_at(60_000, false).unwrap();
        assert!((stats.baseline_mbps() - 1.0).abs() < 1e-9);
        stats.record_upload(75_000_000, false);
        stats.flush_at(120_000, false).unwrap();
        assert!((stats.baseline_mbps() - 10.0).abs() < 1e-9);
        stats.record_upload(1, false);
        stats.flush_at(180_000, true).unwrap();
        assert!((stats.baseline_mbps() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn baseline_is_restored_and_never_joins_sessions_across_downtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.sqlite");
        {
            let stats = TrafficStats::from_connection_at(Connection::open(&path).unwrap(), 0).unwrap();
            stats.record_upload(75_000_000, false);
            stats.flush_at(60_000, true).unwrap();
        }
        let stats = TrafficStats::from_connection_at(Connection::open(&path).unwrap(), 1_000_000).unwrap();
        assert!((stats.baseline_mbps() - 10.0).abs() < 1e-9);
        stats.record_upload(75_000_000, false);
        stats.flush_at(1_060_000, true).unwrap();
        assert!((stats.baseline_mbps() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn short_upload_sample_keeps_default_baseline_and_download_only_is_inactive() {
        let stats = stats_at(0);
        stats.record_download(1000, false);
        stats.flush_at(30_000, false).unwrap();
        assert_eq!(stats.baseline_mbps(), DEFAULT_BASELINE_MBPS);
        assert_eq!(stats.query_at("all", 30_000).unwrap().active_duration_ms, 0);
    }

    #[test]
    fn finish_is_idempotent_and_stops_runtime_at_exit() {
        let stats = stats_at(100);
        stats.record_upload(100, false);
        stats.flush_at(200, true).unwrap();
        stats.flush_at(300, true).unwrap();
        stats.flush_at(400, false).unwrap();
        let result = stats.query_at("all", 500).unwrap();
        assert_eq!(result.total_upload_bytes, 100);
        assert_eq!(result.active_duration_ms, 100);
        assert_eq!(result.points.len(), 1);
    }

    #[test]
    fn failed_flush_restores_counters_and_does_not_advance_heartbeat() {
        let stats = stats_at(100);
        stats.record_upload(1_000, false);
        stats.record_download(500, false);
        {
            let db = stats.inner.db.lock().unwrap();
            db.connection.execute_batch("CREATE TRIGGER fail_heartbeat BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        }
        assert!(stats.flush_at(200, false).is_err());
        let result = stats.query_at("all", 200).unwrap();
        assert_eq!(result.total_upload_bytes, 1_000);
        assert_eq!(result.total_download_bytes, 500);
        assert_eq!(result.points.len(), 1);
        {
            let db = stats.inner.db.lock().unwrap();
            assert_eq!(db.last_flush, 100);
            db.connection.execute_batch("DROP TRIGGER fail_heartbeat").unwrap();
        }
        stats.flush_at(300, false).unwrap();
        let result = stats.query_at("all", 300).unwrap();
        assert_eq!(result.total_upload_bytes, 1_000);
        assert_eq!(result.active_duration_ms, 200);
    }

    #[test]
    fn concurrent_recording_and_flush_preserve_every_byte() {
        let stats = stats_at(0);
        let workers: Vec<_> = (0..4).map(|_| {
            let stats = stats.clone();
            std::thread::spawn(move || {
                for _ in 0..10_000 {
                    stats.record_upload(3, false);
                    stats.record_download(2, false);
                }
            })
        }).collect();
        for now in 1..20 { stats.flush_at(now, false).unwrap(); }
        for worker in workers { worker.join().unwrap(); }
        stats.flush_at(20, true).unwrap();
        let result = stats.query_at("all", 30).unwrap();
        assert_eq!(result.total_upload_bytes, 120_000);
        assert_eq!(result.total_download_bytes, 80_000);
        assert!(result.active_duration_ms > 0);
        assert!(result.active_duration_ms <= 20);
    }
}
