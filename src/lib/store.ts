import { reactive, ref } from 'vue'
import { message } from '@tauri-apps/plugin-dialog'
import {
  getServerStatus,
  getSettings,
  listMedia,
  importFiles,
  onMediaUpdated,
  onServerStatusChanged,
  type MediaItem,
  type ServerStatus,
  type Settings,
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

export const settingsCache = ref<Settings | null>(null)

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

export async function importMediaPaths(paths: string[]): Promise<MediaItem[]> {
  const result = await importFiles(paths)
  result.items.forEach(upsertMedia)
  if (result.skipped.length) {
    await message(`已导入 ${result.items.length} 个视频，跳过 ${result.skipped.length} 个文件：\n${result.skipped.map(issue => `${issue.path}：${issue.reason}`).join('\n')}`, { title: '导入结果', kind: 'warning' })
  }
  return result.items
}

export async function loadSettings(force = false): Promise<Settings> {
  if (!force && settingsCache.value) return settingsCache.value
  const s = await getSettings()
  settingsCache.value = s
  return s
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
  } finally {
    mediaStore.loaded = true
  }
  onMediaUpdated(upsertMedia)
  onServerStatusChanged(applyServerStatus)
}
