import { reactive, ref } from 'vue'
import { message } from '@tauri-apps/plugin-dialog'
import {
  getServerStatus,
  getSettings,
  getTunnelStatus,
  onTunnelStatusChanged,
  listMedia,
  importFiles,
  onMediaUpdated,
  onServerStatusChanged,
  onConvertProgress,
  onConvertTasksUpdated,
  listConvertTasks,
  listFolders,
  type MediaItem,
  type Folder,
  type ServerStatus,
  type Settings,
  type TunnelStatus,
  type ConvertTask,
} from './api'

export const serverStore = reactive<ServerStatus>({
  running: false,
  bindAddr: '',
  port: 17001,
  error: null,
})

export const mediaStore = reactive<{ items: MediaItem[]; loaded: boolean }>({
  items: [],
  loaded: false,
})

export const folderStore = reactive<{ folders: Folder[]; loaded: boolean }>({
  folders: [],
  loaded: false,
})

export const settingsCache = ref<Settings | null>(null)

export const tunnelStore = reactive<TunnelStatus>({
  running: false,
  ready: false,
  url: null,
  port: null,
  error: null,
  revision: 0,
})

export function applyTunnelStatus(status: TunnelStatus) {
  if (status.revision >= tunnelStore.revision) Object.assign(tunnelStore, status)
}

/// 转码 / 分段 / HLS 任务状态：id -> { kind, percent, status }
export const convertStore = reactive<{
  jobs: Record<string, { kind: 'transcode' | 'segment' | 'hls'; percent: number | null; status: 'queued' | 'running' | 'done' | 'error' }>
}>({ jobs: {} })

/// 转码任务队列（媒体卡片浮层与「转码任务」页共用）
export const convertTaskStore = reactive<{ tasks: ConvertTask[]; loaded: boolean }>({
  tasks: [],
  loaded: false,
})

export function applyConvertTasks(tasks: ConvertTask[]) {
  convertTaskStore.tasks = tasks
  convertTaskStore.loaded = true
  // 同步媒体卡片上的转码进度浮层（转码 MP4 与分段转码；HLS/下载任务由任务页单独展示）
  for (const t of tasks) {
    if (t.kind === 'hls' || t.kind === 'download') continue
    if (t.state === 'queued') {
      convertStore.jobs[t.mediaId] = { kind: t.kind, percent: null, status: 'queued' }
    } else if (t.state === 'running') {
      convertStore.jobs[t.mediaId] = { kind: t.kind, percent: t.percent, status: 'running' }
    } else {
      const cur = convertStore.jobs[t.mediaId]
      if (cur && cur.kind !== 'hls') delete convertStore.jobs[t.mediaId]
    }
  }
}

export function applyServerStatus(s: ServerStatus) {
  serverStore.running = s.running
  serverStore.bindAddr = s.bindAddr
  serverStore.port = s.port
  serverStore.error = s.error
}

export function upsertMedia(item: MediaItem) {
  const idx = mediaStore.items.findIndex((m) => m.id === item.id)
  if (idx >= 0) mediaStore.items[idx] = item
  else mediaStore.items.unshift(item)
}

export function removeMediaItem(id: string) {
  const idx = mediaStore.items.findIndex((m) => m.id === id)
  if (idx >= 0) mediaStore.items.splice(idx, 1)
}

/// 导入成功后的 FastStart 提示状态：batch 每批导入自增（App.vue 据此触发提示，
/// 避免连续两批同类型导入时因标志位没变化而漏提示）；含 ≥2GB 大文件的批次用弹窗，
/// 其余批次用 toast
export const faststartHintState = reactive({ batch: 0, hasLargeFile: false, dialogOpen: false })

export async function importMediaPaths(paths: string[]): Promise<MediaItem[]> {
  const result = await importFiles(paths)
  result.items.forEach(upsertMedia)
  // 拖入文件夹时后端会按目录结构生成媒体库文件夹，需刷新
  await loadFolders(true)
  if (result.skipped.length) {
    await message(`已导入 ${result.items.length} 个视频，跳过 ${result.skipped.length} 个文件：\n${result.skipped.map(issue => `${issue.path}：${issue.reason}`).join('\n')}`, { title: '导入结果', kind: 'warning' })
  }
  // 导入成功后触发 FastStart 提示（App.vue 渲染；一批只提示一次）
  if (result.items.length > 0) {
    faststartHintState.hasLargeFile = result.items.some(it => (it.fileSize ?? 0) >= 2 * 1024 * 1024 * 1024)
    faststartHintState.dialogOpen = faststartHintState.hasLargeFile
    faststartHintState.batch++
  }
  return result.items
}

export async function loadSettings(force = false): Promise<Settings> {
  if (!force && settingsCache.value) return settingsCache.value
  const s = await getSettings()
  settingsCache.value = s
  return s
}

export async function loadFolders(force = false): Promise<Folder[]> {
  if (force || !folderStore.loaded) {
    folderStore.folders = await listFolders()
    folderStore.loaded = true
  }
  return folderStore.folders
}

let initialized = false

export async function initStore() {
  if (initialized) return
  initialized = true
  try {
    const [status, media, settings] = await Promise.all([getServerStatus(), listMedia(), getSettings()])
    applyServerStatus(status)
    mediaStore.items = media
    settingsCache.value = settings
    folderStore.folders = await listFolders()
    folderStore.loaded = true
  } finally {
    mediaStore.loaded = true
  }
  onMediaUpdated(upsertMedia)
  onServerStatusChanged(applyServerStatus)
  // 先订阅再拉取快照；revision 保证迟到的快照不会覆盖较新的事件。
  await onTunnelStatusChanged(applyTunnelStatus)
  getTunnelStatus().then(applyTunnelStatus).catch(() => {})
  listConvertTasks()
    .then(applyConvertTasks)
    .catch(() => {})
  onConvertTasksUpdated(applyConvertTasks)
  onConvertProgress((p) => {
    if (p.status === 'done') {
      const cur = convertStore.jobs[p.id]
      // 分段转码任务分两段执行，进行中事件仍会到达；仅在结束事件时清理
      if (cur && (cur.kind === 'transcode' || cur.kind === 'segment')) delete convertStore.jobs[p.id]
      if (p.kind === 'transcode' && p.item) upsertMedia(p.item)
    } else if (p.status === 'error') {
      delete convertStore.jobs[p.id]
    } else {
      convertStore.jobs[p.id] = { kind: p.kind, percent: p.percent, status: p.status }
    }
  })
}
