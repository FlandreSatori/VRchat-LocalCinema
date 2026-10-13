import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type ListenMode = 'loopback' | 'lan'
export type UrlMode = 'localhost' | 'lan'
export type CloseBehavior = 'exit' | 'tray'
export type MediaStatus = 'ok' | 'processing' | 'missing'

export interface Settings {
  listenMode: ListenMode
  port: number
  defaultUrlMode: UrlMode
  defaultPlayerPath: string
  defaultPlayerArgs: string
  thumbDir: string
  autostart: boolean
  closeBehavior: CloseBehavior
  ffmpegPath: string
  ffprobePath: string
}

export interface MediaItem {
  id: string
  title: string
  fileName: string
  filePath: string
  fileSize: number
  durationMs: number | null
  format: string
  thumbnail: string | null
  status: MediaStatus
  addedAt: number
  updatedAt: number
}

export interface ServerStatus {
  running: boolean
  bindAddr: string
  port: number
  error: string | null
}

export interface NetInterface {
  name: string
  ips: string[]
}

export interface PlayerInfo {
  name: string
  path: string
}

export interface LogEntry {
  ts: number
  level: string
  target: string
  message: string
}

export interface AppPaths {
  dataDir: string
  thumbDir: string
  logDir: string
  settingsFile: string
  dbFile: string
  portable: boolean
}

// Settings
export function getSettings(): Promise<Settings> {
  return invoke('get_settings')
}
export function saveSettings(settings: Settings): Promise<void> {
  return invoke('save_settings', { settings })
}
export function resetSettings(): Promise<Settings> {
  return invoke('reset_settings')
}

// Server
export function getServerStatus(): Promise<ServerStatus> {
  return invoke('get_server_status')
}
export function startServer(): Promise<ServerStatus> {
  return invoke('start_server')
}
export function stopServer(): Promise<ServerStatus> {
  return invoke('stop_server')
}
export function restartServer(): Promise<ServerStatus> {
  return invoke('restart_server')
}

// Media
export function listMedia(): Promise<MediaItem[]> {
  return invoke('list_media')
}
export interface ImportResult {
  items: MediaItem[]
  skipped: { path: string; reason: string }[]
}

export function importFiles(paths: string[]): Promise<ImportResult> {
  return invoke('import_files', { paths })
}
export function refreshMedia(id: string): Promise<MediaItem> {
  return invoke('refresh_media', { id })
}
export function removeMedia(id: string): Promise<void> {
  return invoke('remove_media', { id })
}
export function setCustomThumbnail(id: string, imagePath: string): Promise<MediaItem> {
  return invoke('set_custom_thumbnail', { id, imagePath })
}

// Network / URL
export function getNetworkInterfaces(): Promise<NetInterface[]> {
  return invoke('get_network_interfaces')
}
export function buildMediaUrl(
  id: string,
  mode: UrlMode
): Promise<string> {
  return invoke('build_media_url', { id, mode })
}

// Player / system
export function detectPlayers(): Promise<PlayerInfo[]> {
  return invoke('detect_players')
}
export function openInPlayer(id: string, playerPath?: string): Promise<void> {
  return invoke('open_in_player', { id, playerPath })
}
export function openWithSystemDefault(id: string): Promise<void> {
  return invoke('open_with_system_default', { id })
}
export function openInFolder(id: string): Promise<void> {
  return invoke('open_in_folder', { id })
}
export function openUrl(url: string): Promise<void> {
  return invoke('open_url', { url })
}

// Logs / paths
export function getRecentLogs(limit: number): Promise<LogEntry[]> {
  return invoke('get_recent_logs', { limit })
}
export function getAppPaths(): Promise<AppPaths> {
  return invoke('get_app_paths')
}
export function openDataDir(): Promise<void> {
  return invoke('open_data_dir')
}
export function clearThumbnailCache(): Promise<number> {
  return invoke('clear_thumbnail_cache')
}

// Events
export function onMediaUpdated(callback: (item: MediaItem) => void): Promise<UnlistenFn> {
  return listen<MediaItem>('media-updated', (e) => callback(e.payload))
}
export function onServerStatusChanged(callback: (status: ServerStatus) => void): Promise<UnlistenFn> {
  return listen<ServerStatus>('server-status-changed', (e) => callback(e.payload))
}
