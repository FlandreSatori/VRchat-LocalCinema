//! 转码任务队列：按任务序列顺序执行（单 worker），
//! 支持查看进度、开始（插队到队列最前）、取消（排队中直接取消，进行中终止 ffmpeg）与移除记录。
//! 边转边播（HLS）任务也在队列中展示进度（直接以 Running 状态注册，不占用转码 worker）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::transcode::TranscodeOptions;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// 排队中（计划任务）
    Queued,
    /// 进行中
    Running,
    /// 已完成
    Done,
    /// 失败
    Error,
    /// 已取消
    Cancelled,
    /// 已暂停（仅下载任务，可继续）
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// 转码为 MP4（占用转码 worker）
    Transcode,
    /// 分段转码：第一段按设定时长先转（先看），第二段转剩余部分
    Segment,
    /// 边转边播（HLS，后台独立执行）——已停用，保留类型以兼容旧记录
    Hls,
    /// 下载对端媒体文件（后台独立执行，不占用转码 worker，可暂停/继续/取消）
    Download,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertTask {
    pub id: String,
    pub media_id: String,
    pub title: String,
    pub kind: TaskKind,
    /// 转码 MP4 时：是否覆盖已存在的产物
    pub overwrite: bool,
    /// 转码设置（流选择 / 编码方式 / 字幕处理等）；下载任务为空配置
    pub options: TranscodeOptions,
    /// 分段转码：第一段时长（毫秒）；其余任务为 None
    pub first_ms: Option<u64>,
    pub state: TaskState,
    pub percent: Option<f64>,
    pub error: Option<String>,
    pub created_at: i64,
    // ===== 下载任务专用字段（其余任务为 None）=====
    /// 对端文件名（下载落盘名）
    pub file_name: Option<String>,
    /// 对端地址与端口
    pub peer_ip: Option<String>,
    pub peer_port: Option<u16>,
    /// 对端媒体 id（media_id 在登记本地条目前暂存它）
    pub peer_media_id: Option<String>,
    /// 下载任务：对端提供的文件哈希（下载完成后校验），其余任务为 None
    pub expected_hash: Option<String>,
    /// 最终落盘路径（用于断点续传与取消时清理临时文件）
    pub dest_path: Option<String>,
    /// 已接收字节 / 总字节
    pub received: Option<u64>,
    pub total: Option<u64>,
    /// 转码为倍速、下载为 bytes/s
    pub speed: Option<f64>,
    pub fps: Option<f64>,
    pub remaining_seconds: Option<f64>,
}

#[derive(Default)]
struct Inner {
    tasks: Vec<ConvertTask>,
    /// 运行中任务的取消标志（转码 worker 与 HLS/下载后台任务可并行，故为列表）
    running_cancel: Vec<(String, Arc<AtomicBool>)>,
    /// 运行中下载任务的暂停标志
    running_pause: Vec<(String, Arc<AtomicBool>)>,
}

/// 任务列表上限：超过后优先裁剪最旧的已结束任务，避免列表随下载/转码持续增长
const MAX_TASKS: usize = 256;

impl Inner {
    /// 裁剪任务列表：超出上限时从最旧的已结束任务（Done/Error/Cancelled 且无运行标志）开始移除，
    /// 直到回到上限；Queued/Running/Paused 不参与裁剪。
    fn trim_tasks(&mut self) {
        if self.tasks.len() <= MAX_TASKS {
            return;
        }
        let mut excess = self.tasks.len() - MAX_TASKS;
        let mut i = 0;
        while excess > 0 && i < self.tasks.len() {
            let id = self.tasks[i].id.clone();
            let finished = matches!(
                self.tasks[i].state,
                TaskState::Done | TaskState::Error | TaskState::Cancelled
            );
            let running = self.running_cancel.iter().any(|(rid, _)| rid == &id)
                || self.running_pause.iter().any(|(rid, _)| rid == &id);
            if finished && !running {
                self.tasks.remove(i);
                excess -= 1;
            } else {
                i += 1;
            }
        }
    }
}

/// 线程安全的转码任务队列
#[derive(Default, Clone)]
pub struct ConvertQueue {
    inner: Arc<Mutex<Inner>>,
    seq: Arc<AtomicU64>,
}

impl ConvertQueue {
    /// 加入转码队列；同一视频在排队/进行中时不允许重复加入
    pub fn enqueue(
        &self,
        media_id: String,
        title: String,
        overwrite: bool,
        options: TranscodeOptions,
    ) -> Result<ConvertTask, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.tasks.iter().any(|t| {
            t.media_id == media_id && matches!(t.state, TaskState::Queued | TaskState::Running)
        }) {
            return Err("该视频已在转码队列中".into());
        }
        let task = ConvertTask {
            id: format!("task-{}-{}", now_ms(), self.seq.fetch_add(1, Ordering::Relaxed)),
            media_id,
            title,
            kind: TaskKind::Transcode,
            overwrite,
            options,
            first_ms: None,
            state: TaskState::Queued,
            percent: None,
            error: None,
            created_at: now_ms(),
            file_name: None,
            peer_ip: None,
            peer_port: None,
            peer_media_id: None,
            expected_hash: None,
            dest_path: None,
            received: None,
            total: None,
            speed: None,
            fps: None,
            remaining_seconds: None,
        };
        inner.tasks.push(task.clone());
        inner.trim_tasks();
        Ok(task)
    }

    /// 加入分段转码队列：第一段转 first_ms 毫秒（先看），第二段转剩余部分
    pub fn enqueue_segment(
        &self,
        media_id: String,
        title: String,
        overwrite: bool,
        first_ms: u64,
        options: TranscodeOptions,
    ) -> Result<ConvertTask, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.tasks.iter().any(|t| {
            t.media_id == media_id && matches!(t.state, TaskState::Queued | TaskState::Running)
        }) {
            return Err("该视频已在转码队列中".into());
        }
        let task = ConvertTask {
            id: format!("task-{}-{}", now_ms(), self.seq.fetch_add(1, Ordering::Relaxed)),
            media_id,
            title,
            kind: TaskKind::Segment,
            overwrite,
            options,
            first_ms: Some(first_ms),
            state: TaskState::Queued,
            percent: None,
            error: None,
            created_at: now_ms(),
            file_name: None,
            peer_ip: None,
            peer_port: None,
            peer_media_id: None,
            expected_hash: None,
            dest_path: None,
            received: None,
            total: None,
            speed: None,
            fps: None,
            remaining_seconds: None,
        };
        inner.tasks.push(task.clone());
        inner.trim_tasks();
        Ok(task)
    }

    /// 注册一个边转边播（HLS）任务：直接以 Running 状态进入队列并返回取消标志。
    /// HLS 不占用转码 worker，由后台线程独立执行，仅借用队列展示进度。
    pub fn start_hls_task(
        &self,
        media_id: &str,
        title: String,
    ) -> Result<(ConvertTask, Arc<AtomicBool>), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.tasks.iter().any(|t| {
            t.media_id == media_id && matches!(t.state, TaskState::Queued | TaskState::Running)
        }) {
            return Err("该视频已有进行中的转码任务，请先在「转码任务」页取消".into());
        }
        let task = ConvertTask {
            id: format!("task-{}-{}", now_ms(), self.seq.fetch_add(1, Ordering::Relaxed)),
            media_id: media_id.to_string(),
            title,
            kind: TaskKind::Hls,
            overwrite: false,
            options: TranscodeOptions::default(),
            first_ms: None,
            state: TaskState::Running,
            percent: None,
            error: None,
            created_at: now_ms(),
            file_name: None,
            peer_ip: None,
            peer_port: None,
            peer_media_id: None,
            expected_hash: None,
            dest_path: None,
            received: None,
            total: None,
            speed: None,
            fps: None,
            remaining_seconds: None,
        };
        let flag = Arc::new(AtomicBool::new(false));
        inner.running_cancel.push((task.id.clone(), flag.clone()));
        inner.tasks.push(task.clone());
        inner.trim_tasks();
        Ok((task, flag))
    }

    /// 注册一个下载任务：直接以 Running 状态进入队列，返回 (任务, 取消标志, 暂停标志)。
    /// 下载不占用转码 worker，由后台独立执行，仅借用队列展示进度。
    pub fn register_download_task(
        &self,
        peer_media_id: String,
        title: String,
        file_name: String,
        peer_ip: String,
        peer_port: u16,
        dest_path: String,
        expected_hash: Option<String>,
    ) -> Result<(ConvertTask, Arc<AtomicBool>, Arc<AtomicBool>), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let active = |t: &ConvertTask| {
            t.kind == TaskKind::Download
                && matches!(
                    t.state,
                    TaskState::Queued | TaskState::Running | TaskState::Paused
                )
        };
        if inner.tasks.iter().any(|t| {
            active(t)
                && t.peer_media_id.as_deref() == Some(peer_media_id.as_str())
                && t.peer_ip.as_deref() == Some(peer_ip.as_str())
        }) {
            return Err("该文件已在下载队列中".into());
        }
        if inner
            .tasks
            .iter()
            .any(|t| active(t) && t.file_name.as_deref() == Some(file_name.as_str()))
        {
            return Err("已有同名文件正在下载".into());
        }
        let task = ConvertTask {
            id: format!("task-{}-{}", now_ms(), self.seq.fetch_add(1, Ordering::Relaxed)),
            media_id: peer_media_id.clone(),
            title,
            kind: TaskKind::Download,
            overwrite: false,
            options: TranscodeOptions::default(),
            first_ms: None,
            state: TaskState::Running,
            percent: Some(0.0),
            error: None,
            created_at: now_ms(),
            file_name: Some(file_name),
            peer_ip: Some(peer_ip),
            peer_port: Some(peer_port),
            peer_media_id: Some(peer_media_id),
            expected_hash,
            dest_path: Some(dest_path),
            received: Some(0),
            total: None,
            speed: None,
            fps: None,
            remaining_seconds: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        inner.running_cancel.push((task.id.clone(), cancel.clone()));
        inner.running_pause.push((task.id.clone(), pause.clone()));
        inner.tasks.push(task.clone());
        inner.trim_tasks();
        Ok((task, cancel, pause))
    }

    /// 暂停下载任务（仅运行中）：置位暂停标志，由下载执行器保存进度并转入 Paused
    pub fn pause_download(&self, task_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(task) = inner.tasks.iter_mut().find(|t| t.id == task_id) else {
            return Err("任务不存在".into());
        };
        if task.kind != TaskKind::Download || task.state != TaskState::Running {
            return Err("仅进行中的下载任务可以暂停".into());
        }
        if let Some((_, flag)) = inner.running_pause.iter().find(|(id, _)| id == task_id) {
            flag.store(true, Ordering::Relaxed);
            Ok(())
        } else {
            Err("任务不在运行中".into())
        }
    }

    /// 继续下载任务（已暂停或失败）：重新标记为运行中并返回新的取消/暂停标志
    pub fn resume_download(
        &self,
        task_id: &str,
    ) -> Result<(ConvertTask, Arc<AtomicBool>, Arc<AtomicBool>), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let task = {
            let Some(task) = inner.tasks.iter_mut().find(|t| t.id == task_id) else {
                return Err("任务不存在".into());
            };
            if task.kind != TaskKind::Download {
                return Err("仅下载任务支持继续".into());
            }
            if !matches!(task.state, TaskState::Paused | TaskState::Error) {
                return Err("仅已暂停或失败的任务可以继续".into());
            }
            task.state = TaskState::Running;
            task.error = None;
            task.clone()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        inner.running_cancel.retain(|(id, _)| id != task_id);
        inner.running_pause.retain(|(id, _)| id != task_id);
        inner.running_cancel.push((task_id.to_string(), cancel.clone()));
        inner.running_pause.push((task_id.to_string(), pause.clone()));
        Ok((task, cancel, pause))
    }

    /// 下载执行器：更新进度
    pub fn set_download_progress(
        &self,
        task_id: &str,
        received: u64,
        total: Option<u64>,
        percent: Option<f64>,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            t.received = Some(received);
            if total.is_some() {
                t.total = total;
            }
            t.percent = percent;
        }
    }

    /// 下载执行器：转入已暂停（保存已接收进度并清理运行标志）
    pub fn mark_download_paused(&self, task_id: &str, received: u64, total: Option<u64>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            if t.state == TaskState::Running {
                t.state = TaskState::Paused;
                t.received = Some(received);
                if total.is_some() {
                    t.total = total;
                }
            }
        }
        inner.running_pause.retain(|(id, _)| id != task_id);
        inner.running_cancel.retain(|(id, _)| id != task_id);
    }

    /// 开始任务：将排队中的任务移到队列最前（当前任务完成后立即执行）
    pub fn start_now(&self, task_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pos) = inner.tasks.iter().position(|t| t.id == task_id) else {
            return Err("任务不存在".into());
        };
        if inner.tasks[pos].state != TaskState::Queued {
            return Err("仅排队中的任务可以开始".into());
        }
        let task = inner.tasks.remove(pos);
        let insert_at = inner
            .tasks
            .iter()
            .position(|t| t.state == TaskState::Queued)
            .unwrap_or(inner.tasks.len());
        inner.tasks.insert(insert_at, task);
        Ok(())
    }

    /// 取消任务：排队中直接标记取消；进行中设置取消标志（由执行器终止 ffmpeg）
    pub fn cancel(&self, task_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(task) = inner.tasks.iter_mut().find(|t| t.id == task_id) else {
            return Err("任务不存在".into());
        };
        match task.state {
            TaskState::Queued | TaskState::Paused => {
                // 排队中直接标记取消；已暂停的下载任务由调用方清理临时文件
                task.state = TaskState::Cancelled;
                task.speed = None;
                task.fps = None;
                task.remaining_seconds = None;
                Ok(())
            }
            TaskState::Running => {
                if let Some((_, flag)) = inner.running_cancel.iter().find(|(id, _)| id == task_id) {
                    flag.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                Err("任务不在运行中".into())
            }
            _ => Err("任务已结束".into()),
        }
    }

    /// 移除一条已结束任务的记录
    pub fn remove(&self, task_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pos) = inner.tasks.iter().position(|t| t.id == task_id) else {
            return Err("任务不存在".into());
        };
        if matches!(
            inner.tasks[pos].state,
            TaskState::Queued | TaskState::Running
        ) {
            return Err("任务尚未结束，请先取消".into());
        }
        inner.tasks.remove(pos);
        Ok(())
    }

    /// 清除全部已结束任务的记录
    pub fn clear_finished(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .tasks
            .retain(|t| matches!(t.state, TaskState::Queued | TaskState::Running));
    }

    /// 是否存在该媒体的排队/进行中任务
    pub fn has_active(&self, media_id: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.tasks.iter().any(|t| {
            t.media_id == media_id && matches!(t.state, TaskState::Queued | TaskState::Running)
        })
    }

    pub fn list(&self) -> Vec<ConvertTask> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).tasks.clone()
    }

    /// 取出下一个排队任务并标记为运行中，返回 (任务, 取消标志)
    pub fn take_next(&self) -> Option<(ConvertTask, Arc<AtomicBool>)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pos = inner
            .tasks
            .iter()
            .position(|t| t.state == TaskState::Queued)?;
        inner.tasks[pos].state = TaskState::Running;
        let task = inner.tasks[pos].clone();
        let flag = Arc::new(AtomicBool::new(false));
        inner.running_cancel.push((task.id.clone(), flag.clone()));
        Some((task, flag))
    }

    /// 清除运行标记（任务结束或提前返回时调用，避免取消标志悬空）
    pub fn clear_running(&self, task_id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.running_cancel.retain(|(id, _)| id != task_id);
    }

    pub fn set_percent(&self, task_id: &str, percent: f64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            t.percent = Some(percent);
        }
    }

    pub fn set_telemetry(&self, task_id: &str, speed: Option<f64>, fps: Option<f64>, remaining_seconds: Option<f64>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            t.speed = speed;
            t.fps = fps;
            t.remaining_seconds = remaining_seconds;
        }
    }

    /// 标记任务结束（完成/失败/取消）并清除运行标记
    pub fn finish(&self, task_id: &str, state: TaskState, error: Option<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            t.state = state;
            if state == TaskState::Done {
                t.percent = Some(100.0);
            }
            t.error = error;
            t.speed = None;
            t.fps = None;
            t.remaining_seconds = None;
        }
        inner.running_cancel.retain(|(id, _)| id != task_id);
        inner.running_pause.retain(|(id, _)| id != task_id);
    }

    /// 仅当任务仍处于 Running 时标记结束（供 HLS 后台任务使用，
    /// 避免与其它结束路径重复转移状态）
    pub fn finish_if_running(&self, task_id: &str, state: TaskState, error: Option<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            if t.state != TaskState::Running {
                return;
            }
            t.state = state;
            if state == TaskState::Done {
                t.percent = Some(100.0);
            }
            t.error = error;
            t.speed = None;
            t.fps = None;
            t.remaining_seconds = None;
        }
        inner.running_cancel.retain(|(id, _)| id != task_id);
        inner.running_pause.retain(|(id, _)| id != task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_dedup_and_start_now() {
        let q = ConvertQueue::default();
        let a = q.enqueue("m1".into(), "A".into(), false, TranscodeOptions::default()).unwrap();
        let _b = q.enqueue("m2".into(), "B".into(), false, TranscodeOptions::default()).unwrap();
        assert!(q.enqueue("m1".into(), "A".into(), false, TranscodeOptions::default()).is_err());

        // B 开始后应排在 A 之前
        assert!(q.start_now(&_b.id).is_ok());
        let list = q.list();
        assert_eq!(list[0].id, _b.id);
        assert_eq!(list[1].id, a.id);
    }

    #[test]
    fn test_take_next_and_cancel() {
        let q = ConvertQueue::default();
        assert!(q.take_next().is_none());

        let a = q.enqueue("m1".into(), "A".into(), false, TranscodeOptions::default()).unwrap();
        let (task, flag) = q.take_next().unwrap();
        assert_eq!(task.media_id, "m1");
        assert_eq!(task.state, TaskState::Running);
        assert!(!flag.load(Ordering::Relaxed));

        // 运行中取消 → 设置标志
        assert!(q.cancel(&task.id).is_ok());
        assert!(flag.load(Ordering::Relaxed));

        // 结束后清理
        q.finish(&task.id, TaskState::Done, None);
        let done = q.list().into_iter().find(|t| t.id == task.id).unwrap();
        assert_eq!(done.state, TaskState::Done);
        assert_eq!(done.percent, Some(100.0));

        // 排队中的任务直接取消
        let _ = q.enqueue("m2".into(), "B".into(), false, TranscodeOptions::default()).unwrap();
        let b = q.list().into_iter().find(|t| t.media_id == "m2").unwrap();
        assert!(q.cancel(&b.id).is_ok());
        let b = q.list().into_iter().find(|t| t.id == b.id).unwrap();
        assert_eq!(b.state, TaskState::Cancelled);

        // 移除已结束记录
        assert!(q.remove(&a.id).is_ok());
        assert!(q.remove(&b.id).is_ok());
        assert!(q.list().is_empty());
    }

    #[test]
    fn test_hls_task_and_finish_if_running() {
        let q = ConvertQueue::default();
        // 排队中的转码任务会阻止 HLS 注册
        let a = q.enqueue("m1".into(), "A".into(), false, TranscodeOptions::default()).unwrap();
        assert!(q.start_hls_task("m1", "A".into()).is_err());
        // 取消并移除排队中的转码任务
        assert!(q.cancel(&a.id).is_ok());
        q.remove(&a.id).unwrap();

        // HLS 任务直接 Running，并占用取消标志
        let (hls, flag) = q.start_hls_task("m1", "A".into()).unwrap();
        assert_eq!(hls.kind, TaskKind::Hls);
        assert_eq!(hls.state, TaskState::Running);
        assert!(q.start_hls_task("m1", "A".into()).is_err());
        assert!(q.enqueue("m1".into(), "A".into(), false, TranscodeOptions::default()).is_err());

        // HLS 任务不占用转码 worker（take_next 只取 Queued）
        let _b = q.enqueue("m2".into(), "B".into(), false, TranscodeOptions::default()).unwrap();
        let (next, _) = q.take_next().unwrap();
        assert_eq!(next.media_id, "m2");

        // 取消 HLS → 标志置位；finish_if_running 只在 Running 时生效
        assert!(q.cancel(&hls.id).is_ok());
        assert!(flag.load(Ordering::Relaxed));
        q.finish_if_running(&hls.id, TaskState::Cancelled, Some("任务已取消".into()));
        let done = q.list().into_iter().find(|t| t.id == hls.id).unwrap();
        assert_eq!(done.state, TaskState::Cancelled);

        // 非 Running 状态的 finish_if_running 不改变状态
        q.finish_if_running(&hls.id, TaskState::Done, None);
        let done = q.list().into_iter().find(|t| t.id == hls.id).unwrap();
        assert_eq!(done.state, TaskState::Cancelled);
    }

    #[test]
    fn test_download_task_lifecycle() {
        let q = ConvertQueue::default();
        let (task, cancel, pause) = q
            .register_download_task(
                "pm1".into(),
                "视频 A".into(),
                "video.mp4".into(),
                "10.0.0.2".into(),
                17001,
                "C:\\dl\\video.mp4".into(),
                Some("deadbeef".into()),
            )
            .unwrap();
        assert_eq!(task.kind, TaskKind::Download);
        assert_eq!(task.state, TaskState::Running);
        assert_eq!(task.received, Some(0));

        // 同一对端同一文件重复下载被拒绝；不同对端同名文件也被拒绝
        assert!(q
            .register_download_task(
                "pm1".into(),
                "视频 A".into(),
                "video.mp4".into(),
                "10.0.0.2".into(),
                17001,
                "x".into(),
                None
            )
            .is_err());
        assert!(q
            .register_download_task(
                "pm2".into(),
                "视频 B".into(),
                "video.mp4".into(),
                "10.0.0.3".into(),
                17001,
                "x".into(),
                None
            )
            .is_err());

        // 下载任务不占用转码 worker
        let _t = q.enqueue("m1".into(), "A".into(), false, TranscodeOptions::default()).unwrap();
        let (next, _) = q.take_next().unwrap();
        assert_eq!(next.kind, TaskKind::Transcode);

        // 进度更新 → 暂停保存进度 → 继续恢复
        q.set_download_progress(&task.id, 40, Some(100), Some(40.0));
        assert!(q.pause_download(&task.id).is_ok());
        q.mark_download_paused(&task.id, 40, Some(100));
        let t = q.list().into_iter().find(|t| t.id == task.id).unwrap();
        assert_eq!(t.state, TaskState::Paused);
        assert_eq!(t.received, Some(40));
        assert_eq!(t.total, Some(100));
        // 已暂停后不可再次暂停
        assert!(q.pause_download(&task.id).is_err());

        let (t2, cancel2, _pause2) = q.resume_download(&task.id).unwrap();
        assert_eq!(t2.state, TaskState::Running);
        assert!(t2.error.is_none());

        // 运行中取消 → 置位取消标志（由执行器清理临时文件并收尾）；
        // 继续后旧标志作废，只有新标志生效
        assert!(q.cancel(&task.id).is_ok());
        assert!(cancel2.load(Ordering::Relaxed));
        assert!(!cancel.load(Ordering::Relaxed));
        q.finish(&task.id, TaskState::Cancelled, None);
        let t = q.list().into_iter().find(|t| t.id == task.id).unwrap();
        assert_eq!(t.state, TaskState::Cancelled);
        // 已取消不可继续
        assert!(q.resume_download(&task.id).is_err());

        // 非下载任务不支持暂停/继续
        let (tr, _) = {
            let _ = cancel;
            let _ = pause;
            q.enqueue("m2".into(), "B".into(), false, TranscodeOptions::default()).unwrap();
            let (tr, flag) = q.take_next().unwrap();
            (tr, flag)
        };
        assert!(q.pause_download(&tr.id).is_err());
        assert!(q.resume_download(&tr.id).is_err());
        assert!(q.cancel(&tr.id).is_ok());
    }

    #[test]
    fn test_task_trim_caps_and_drops_oldest_finished() {
        // 连续登记并结束任务，列表长度应被裁剪到上限
        let q = ConvertQueue::default();
        for i in 0..(MAX_TASKS + 10) {
            let t = q
                .enqueue(format!("m{}", i), "T".into(), false, TranscodeOptions::default())
                .unwrap();
            q.finish(&t.id, TaskState::Done, None);
        }
        assert_eq!(q.list().len(), MAX_TASKS);
        // 最旧的已结束任务被移除，最新的仍保留
        assert!(!q.list().iter().any(|t| t.media_id == "m0"));
        assert!(q.list().iter().any(|t| t.media_id == format!("m{}", MAX_TASKS + 9)));

        // 进行中的任务不参与裁剪
        let q2 = ConvertQueue::default();
        let run = q2.enqueue("keep".into(), "K".into(), false, TranscodeOptions::default()).unwrap();
        q2.take_next();
        for i in 0..(MAX_TASKS + 5) {
            let t = q2
                .enqueue(format!("x{}", i), "X".into(), false, TranscodeOptions::default())
                .unwrap();
            q2.finish(&t.id, TaskState::Done, None);
        }
        assert!(q2.list().iter().any(|t| t.id == run.id));
    }
}
