import { computed, reactive, ref } from 'vue'
import { message } from '@tauri-apps/plugin-dialog'
import {
  getServerStatus,
  getSettings,
  getTunnelStatus,
  onTunnelStatusChanged,
  getFrpStatus,
  onFrpStatusChanged,
  listMedia,
  importFiles,
  transcodeStreamingCopy,
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
  type FrpStatus,
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

export const incompatibleImportState = reactive({ items: [] as MediaItem[], batch: 0 })

/// 大文件阈值（≥2GB）
export const LARGE_FILE_BYTES = 2_147_483_648
export function isLargeMedia(item: MediaItem): boolean {
  return (item.fileSize ?? 0) >= LARGE_FILE_BYTES
}

/// ≥2GB 大文件列表（增量维护：仅在媒体增改时同步，避免整库反复过滤）
export const largeMediaItems = ref<MediaItem[]>([])

export const folderStore = reactive<{ folders: Folder[]; loaded: boolean }>({
  folders: [],
  loaded: false,
})

export const settingsCache = ref<Settings | null>(null)

/// 局域网地址是否真正可达：只有服务实际绑定到非环回地址时才成立。
/// 仅本机监听时生成的 192.168.x.x 链接其他设备必然连不上（VRChat 表现为 Loading failed）。
export const lanListenAvailable = computed(() => {
  const addr = serverStore.bindAddr
  return serverStore.running && !!addr && !addr.startsWith('127.0.0.1')
})

/// 局域网链接不可用时的原因说明（禁用项的悬停提示与说明文字共用）
export const lanUnavailableReason = computed(() => {
  const s = settingsCache.value
  if (s?.listenMode === 'lan') return '服务尚未绑定到局域网地址，请稍候重试'
  // 已开启开关但仍停留在本地监听：需要用户再切换监听模式，否则提示会与实际状态不符
  if (s?.allowPeerView) return '已开启「允许其他人查看我的媒体库」，但当前仍是本地监听；请在「设置 · 媒体库」切换到局域网模式'
  return '未开启「允许其他人查看我的媒体库」，开启后才能使用局域网链接'
})

/// 设置页的折叠分组标识（与 SettingsPage 的 openGroup 取值一致）
export type SettingsGroup = 'library' | 'cache' | 'components' | 'system' | 'transcode'

/// 跨页面跳转到设置页时的目标分组：点击后切页并展开、滚动定位该分组。
/// tick 每次请求自增，设置页已在当前页时也能重复触发；pending 用于页面挂载时消费一次。
export const settingsFocus = reactive<{ group: SettingsGroup | ''; tick: number; pending: boolean }>({
  group: '',
  tick: 0,
  pending: false,
})

export function focusSettingsGroup(group: SettingsGroup) {
  settingsFocus.group = group
  settingsFocus.pending = true
  settingsFocus.tick++
}

/// 消费一次待处理的目标分组（页面挂载或收到 tick 变化时调用），避免留下持久状态
export function consumeSettingsFocus(): SettingsGroup | '' {
  if (!settingsFocus.pending) return ''
  settingsFocus.pending = false
  return settingsFocus.group
}

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

/// frp 公网隧道状态（与 Cloudflare 隧道并列展示在「公网隧道」分组）
export const frpStore = reactive<FrpStatus>({
  running: false,
  ready: false,
  tunnelCount: 0,
  port: null,
  error: null,
  params: '',
  revision: 0,
})

export function applyFrpStatus(status: FrpStatus) {
  if (status.revision >= frpStore.revision) Object.assign(frpStore, status)
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

/// 增量同步大文件列表（命中则加入/更新，否则移除）
function syncLargeItem(item: MediaItem) {
  const arr = largeMediaItems.value
  const idx = arr.findIndex((m) => m.id === item.id)
  if (isLargeMedia(item)) {
    if (idx >= 0) arr[idx] = item
    else arr.push(item)
  } else if (idx >= 0) {
    arr.splice(idx, 1)
  }
}

function dropLargeItem(id: string) {
  const arr = largeMediaItems.value
  const idx = arr.findIndex((m) => m.id === id)
  if (idx >= 0) arr.splice(idx, 1)
}

export function upsertMedia(item: MediaItem) {
  const idx = mediaStore.items.findIndex((m) => m.id === item.id)
  if (idx >= 0) mediaStore.items[idx] = item
  else mediaStore.items.unshift(item)
  syncLargeItem(item)
}

export function removeMediaItem(id: string) {
  const idx = mediaStore.items.findIndex((m) => m.id === id)
  if (idx >= 0) mediaStore.items.splice(idx, 1)
  dropLargeItem(id)
}

/// 导入成功后的压制提示状态
export const faststartHintState = reactive({ batch: 0, largeItems: [] as MediaItem[], dialogOpen: false })

/// 一键压制为流媒体 MP4（默认 SVT-AV1）：不主动提升分辨率与码率——
/// 仅在源高于 1080p 时降采样到 1080p，否则保持原始分辨率；
/// 码率以设置为上限（按源分辨率选 720/1080 档），实际由 CRF 决定。
export async function transcodeStreamingItem(item: MediaItem): Promise<ConvertTask> {
  const settings = await loadSettings()
  const h = item.height ?? 0
  const resolution: 'original' | '1080' = h > 1080 ? '1080' : 'original'
  const mbps = h > 0 && h <= 720 ? settings.streaming720pBitrateMbps : settings.streaming1080pBitrateMbps
  return transcodeStreamingCopy(item.id, resolution, mbps)
}

export async function importMediaPaths(paths: string[]): Promise<MediaItem[]> {
  const result = await importFiles(paths)
  result.items.forEach(upsertMedia)
  // 拖入文件夹时后端会按目录结构生成媒体库文件夹，需刷新
  await loadFolders(true)
  if (result.skipped.length) {
    await message(`已导入 ${result.items.length} 个视频，跳过 ${result.skipped.length} 个文件：\n${result.skipped.map(issue => `${issue.path}：${issue.reason}`).join('\n')}`, { title: '导入结果', kind: 'warning' })
  }
  // 导入成功后触发压制提示（App.vue 渲染；一批只提示一次）
  if (result.items.length > 0) {
    faststartHintState.largeItems = result.items.filter(isLargeMedia)
    faststartHintState.batch++
    incompatibleImportState.items = result.items.filter(item => item.compatStatus === 'unsupported')
    incompatibleImportState.batch++
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
    largeMediaItems.value = media.filter(isLargeMedia)
    settingsCache.value = settings
    folderStore.folders = await listFolders()
    folderStore.loaded = true
  } finally {
    mediaStore.loaded = true
  }
  onMediaUpdated((item) => {
    upsertMedia(item)
    if (item.compatStatus === 'unsupported') {
      incompatibleImportState.items = [...incompatibleImportState.items.filter(existing => existing.id !== item.id), item]
      incompatibleImportState.batch++
    }
  })
  onServerStatusChanged(applyServerStatus)
  // 先订阅再拉取快照；revision 保证迟到的快照不会覆盖较新的事件。
  await onTunnelStatusChanged(applyTunnelStatus)
  getTunnelStatus().then(applyTunnelStatus).catch(() => {})
  await onFrpStatusChanged(applyFrpStatus)
  getFrpStatus().then(applyFrpStatus).catch(() => {})
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
