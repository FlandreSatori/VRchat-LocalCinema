//! 转码任务队列：按任务序列顺序执行（单 worker），
//! 支持查看进度、开始（插队到队列最前）、取消（排队中直接取消，进行中终止 ffmpeg）与移除记录。
//! 边转边播（HLS）任务也在队列中展示进度（直接以 Running 状态注册，不占用转码 worker）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

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
    /// 分段转码：第一段时长（毫秒）；其余任务为 None
    pub first_ms: Option<u64>,
    pub state: TaskState,
    pub percent: Option<f64>,
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Default)]
struct Inner {
    tasks: Vec<ConvertTask>,
    /// 运行中任务的取消标志（转码 worker 与 HLS 后台任务可并行，故为列表）
    running_cancel: Vec<(String, Arc<AtomicBool>)>,
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
    ) -> Result<ConvertTask, String> {
        let mut inner = self.inner.lock().unwrap();
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
            first_ms: None,
            state: TaskState::Queued,
            percent: None,
            error: None,
            created_at: now_ms(),
        };
        inner.tasks.push(task.clone());
        Ok(task)
    }

    /// 加入分段转码队列：第一段转 first_ms 毫秒（先看），第二段转剩余部分
    pub fn enqueue_segment(
        &self,
        media_id: String,
        title: String,
        overwrite: bool,
        first_ms: u64,
    ) -> Result<ConvertTask, String> {
        let mut inner = self.inner.lock().unwrap();
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
            first_ms: Some(first_ms),
            state: TaskState::Queued,
            percent: None,
            error: None,
            created_at: now_ms(),
        };
        inner.tasks.push(task.clone());
        Ok(task)
    }

    /// 注册一个边转边播（HLS）任务：直接以 Running 状态进入队列并返回取消标志。
    /// HLS 不占用转码 worker，由后台线程独立执行，仅借用队列展示进度。
    pub fn start_hls_task(
        &self,
        media_id: &str,
        title: String,
    ) -> Result<(ConvertTask, Arc<AtomicBool>), String> {
        let mut inner = self.inner.lock().unwrap();
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
            first_ms: None,
            state: TaskState::Running,
            percent: None,
            error: None,
            created_at: now_ms(),
        };
        let flag = Arc::new(AtomicBool::new(false));
        inner.running_cancel.push((task.id.clone(), flag.clone()));
        inner.tasks.push(task.clone());
        Ok((task, flag))
    }

    /// 开始任务：将排队中的任务移到队列最前（当前任务完成后立即执行）
    pub fn start_now(&self, task_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
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
        let mut inner = self.inner.lock().unwrap();
        let Some(task) = inner.tasks.iter_mut().find(|t| t.id == task_id) else {
            return Err("任务不存在".into());
        };
        match task.state {
            TaskState::Queued => {
                task.state = TaskState::Cancelled;
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
        let mut inner = self.inner.lock().unwrap();
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
        let mut inner = self.inner.lock().unwrap();
        inner
            .tasks
            .retain(|t| matches!(t.state, TaskState::Queued | TaskState::Running));
    }

    /// 是否存在该媒体的排队/进行中任务
    pub fn has_active(&self, media_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.tasks.iter().any(|t| {
            t.media_id == media_id && matches!(t.state, TaskState::Queued | TaskState::Running)
        })
    }

    pub fn list(&self) -> Vec<ConvertTask> {
        self.inner.lock().unwrap().tasks.clone()
    }

    /// 取出下一个排队任务并标记为运行中，返回 (任务, 取消标志)
    pub fn take_next(&self) -> Option<(ConvertTask, Arc<AtomicBool>)> {
        let mut inner = self.inner.lock().unwrap();
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
        let mut inner = self.inner.lock().unwrap();
        inner.running_cancel.retain(|(id, _)| id != task_id);
    }

    pub fn set_percent(&self, task_id: &str, percent: f64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            t.percent = Some(percent);
        }
    }

    /// 标记任务结束（完成/失败/取消）并清除运行标记
    pub fn finish(&self, task_id: &str, state: TaskState, error: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            t.state = state;
            if state == TaskState::Done {
                t.percent = Some(100.0);
            }
            t.error = error;
        }
        inner.running_cancel.retain(|(id, _)| id != task_id);
    }

    /// 仅当任务仍处于 Running 时标记结束（供 HLS 后台任务使用，
    /// 避免与其它结束路径重复转移状态）
    pub fn finish_if_running(&self, task_id: &str, state: TaskState, error: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) {
            if t.state != TaskState::Running {
                return;
            }
            t.state = state;
            if state == TaskState::Done {
                t.percent = Some(100.0);
            }
            t.error = error;
        }
        inner.running_cancel.retain(|(id, _)| id != task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enqueue_dedup_and_start_now() {
        let q = ConvertQueue::default();
        let a = q.enqueue("m1".into(), "A".into(), false).unwrap();
        let _b = q.enqueue("m2".into(), "B".into(), false).unwrap();
        assert!(q.enqueue("m1".into(), "A".into(), false).is_err());

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

        let a = q.enqueue("m1".into(), "A".into(), false).unwrap();
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
        let _ = q.enqueue("m2".into(), "B".into(), false).unwrap();
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
        let a = q.enqueue("m1".into(), "A".into(), false).unwrap();
        assert!(q.start_hls_task("m1", "A".into()).is_err());
        // 取消并移除排队中的转码任务
        assert!(q.cancel(&a.id).is_ok());
        q.remove(&a.id).unwrap();

        // HLS 任务直接 Running，并占用取消标志
        let (hls, flag) = q.start_hls_task("m1", "A".into()).unwrap();
        assert_eq!(hls.kind, TaskKind::Hls);
        assert_eq!(hls.state, TaskState::Running);
        assert!(q.start_hls_task("m1", "A".into()).is_err());
        assert!(q.enqueue("m1".into(), "A".into(), false).is_err());

        // HLS 任务不占用转码 worker（take_next 只取 Queued）
        let _b = q.enqueue("m2".into(), "B".into(), false).unwrap();
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
}
