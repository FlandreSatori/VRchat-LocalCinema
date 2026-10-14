import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type { UnlistenFn }

export type ListenMode = 'loopback' | 'lan'
export type UrlMode = 'localhost' | 'lan'
export type CloseBehavior = 'exit' | 'tray'
export type MediaStatus = 'ok' | 'processing' | 'missing'
export type CompatStatus = 'supported' | 'unsupported'

export interface EasytierConfig {
  protocols: string[]
  peerUrl: string
  roomName: string
  roomPassword: string
  memberName: string
  ipv4: string
  ipv6: string
  /** 自动分配虚拟 IP（DHCP），开启时忽略手动 ipv4 */
  dhcp: boolean
  kcpProxy: boolean
  kcpInput: boolean
  quicProxy: boolean
  quicInput: boolean
  latencyFirst: boolean
  compression: string
  upnp: boolean
  multiThread: boolean
  listenPort: number
}

export interface Settings {
  listenMode: ListenMode
  port: number
  defaultUrlMode: UrlMode
  lanAdapterIp: string
  defaultPlayerPath: string
  defaultPlayerArgs: string
  thumbDir: string
  autostart: boolean
  closeBehavior: CloseBehavior
  ffmpegPath: string
  ffprobePath: string
  easytierCorePath: string
  easytierCliPath: string
  downloadDir: string
  easytier: EasytierConfig
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
  hash: string | null
  container: string | null
  videoCodec: string | null
  pixFmt: string | null
  bitDepth: number | null
  width: number | null
  height: number | null
  audioCodec: string | null
  audioSampleRate: number | null
  audioChannels: number | null
  compatStatus: CompatStatus | null
  compatReason: string | null
  compatResolved: boolean
  hlsPlaylist: string | null
  folderId: string | null
  /** 对应的源视频 id（转码产物的「快捷方式」卡片用） */
  sourceMediaId: string | null
  /** 分段转码的段序号（1/2；非分段产物为 null） */
  segmentIndex: number | null
  status: MediaStatus
  addedAt: number
  updatedAt: number
}

export interface ConvertProgress {
  id: string
  kind: 'transcode' | 'hls' | 'segment'
  status: 'running' | 'done' | 'error'
  percent: number | null
  error: string | null
  item: MediaItem | null
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
  bundledDir: string
  componentsDir: string
  downloadDir: string
  portable: boolean
}

export interface ComponentStatus {
  kind: 'ffmpeg' | 'easytier'
  label: string
  installed: boolean
  source: 'custom' | 'bundled' | 'downloaded' | 'system' | 'none'
  path: string | null
  version: string | null
  manualUrl: string
  missing: string[]
  builtin: boolean
}

export interface ComponentDownloadProgress {
  kind: string
  downloaded: number
  total: number | null
}

export interface EasytierStatus {
  running: boolean
  pid: number | null
  rpcPortal: string
  error: string | null
  args: string[]
}

export interface PeerInfo {
  hostname: string
  ipv4: string
  latencyMs: number | null
  loss: number | null
  cost: string
  nat: string
  version: string
}

export interface RoomInfo {
  peers: PeerInfo[]
  raw: string
  error: string | null
}

export interface MediaSummary {
  id: string
  title: string
  fileName: string
  fileSize: number
  durationMs: number | null
  hash: string | null
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

// 转码 / 分段转码 / 下载任务
export type ConvertTaskState = 'queued' | 'running' | 'paused' | 'done' | 'error' | 'cancelled'
export type ConvertTaskKind = 'transcode' | 'segment' | 'hls' | 'download'
export interface ConvertTask {
  id: string
  mediaId: string
  title: string
  kind: ConvertTaskKind
  /** 转码 MP4 时：是否覆盖已存在的转码产物 */
  overwrite: boolean
  /** 分段转码：第一段时长（毫秒）；其余任务为 null */
  firstMs: number | null
  state: ConvertTaskState
  percent: number | null
  error: string | null
  createdAt: number
  /** 下载任务：对端文件名（下载落盘名）；其余任务为 null */
  fileName?: string | null
  /** 下载任务：对端地址与端口；其余任务为 null */
  peerIp?: string | null
  peerPort?: number | null
  /** 下载任务：对端媒体 id；其余任务为 null */
  peerMediaId?: string | null
  /** 下载任务：最终落盘路径；其余任务为 null */
  destPath?: string | null
  /** 下载任务：已接收字节 / 总字节 */
  received?: number | null
  total?: number | null
}
export function transcodeMedia(id: string, overwrite?: boolean): Promise<ConvertTask> {
  return invoke('transcode_media', { id, overwrite })
}
export function transcodeOutputExists(id: string, kind?: 'transcode' | 'segment'): Promise<boolean> {
  return invoke('transcode_output_exists', { id, kind })
}
export function segmentTranscode(id: string, firstMs: number, overwrite?: boolean): Promise<ConvertTask> {
  return invoke('segment_transcode', { id, firstMs, overwrite })
}
export function listSegments(id: string): Promise<MediaItem[]> {
  return invoke('list_segments', { id })
}
/** 第一段分段转码完成时触发（payload 为第一段产物条目） */
export function onSegmentReady(
  callback: (item: MediaItem) => void
): Promise<UnlistenFn> {
  return listen<MediaItem>('segment-ready', (e) => callback(e.payload))
}
export function listConvertTasks(): Promise<ConvertTask[]> {
  return invoke('list_convert_tasks')
}
export function convertTaskStart(taskId: string): Promise<void> {
  return invoke('convert_task_start', { taskId })
}
export function convertTaskCancel(taskId: string): Promise<void> {
  return invoke('convert_task_cancel', { taskId })
}
export function convertTaskRemove(taskId: string): Promise<void> {
  return invoke('convert_task_remove', { taskId })
}
export function convertTaskClear(): Promise<void> {
  return invoke('convert_task_clear')
}
// HLS 边转边播已停用（改为分段转码），后端命令不再注册
// export function makeHls(id: string, mode: UrlMode): Promise<string> {
//   return invoke('make_hls', { id, mode })
// }
// export function getHlsStatus(id: string): Promise<boolean> {
//   return invoke('get_hls_status', { id })
// }
// export function buildHlsUrl(id: string, mode: UrlMode): Promise<string> {
//   return invoke('build_hls_url', { id, mode })
// }
export function onConvertProgress(
  callback: (progress: ConvertProgress) => void
): Promise<UnlistenFn> {
  return listen<ConvertProgress>('convert-progress', (e) => callback(e.payload))
}
export function onConvertTasksUpdated(
  callback: (tasks: ConvertTask[]) => void
): Promise<UnlistenFn> {
  return listen<ConvertTask[]>('convert-tasks-updated', (e) => callback(e.payload))
}

// ---------- 媒体库文件夹（仅映射，不影响磁盘文件） ----------

export interface Folder {
  id: string
  name: string
  parentId: string | null
  createdAt: number
}

/// 内置「不兼容」文件夹 id
export const BUILTIN_INCOMPATIBLE_ID = 'builtin-incompatible'

export function listFolders(): Promise<Folder[]> {
  return invoke('list_folders')
}
export function createFolder(name: string, parentId: string | null): Promise<Folder> {
  return invoke('create_folder', { name, parentId })
}
export function renameFolder(id: string, name: string): Promise<void> {
  return invoke('rename_folder', { id, name })
}
export function deleteFolder(id: string): Promise<void> {
  return invoke('delete_folder', { id })
}
export function renameMedia(id: string, name: string): Promise<MediaItem> {
  return invoke('rename_media', { id, name })
}
export function moveMedia(id: string, folderId: string | null): Promise<MediaItem> {
  return invoke('move_media', { id, folderId })
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

export interface ConvertCacheInfo {
  dir: string
  fileCount: number
  totalBytes: number
}

export interface ClearConvertCacheResult {
  removedFiles: number
  freedBytes: number
  removedRecords: number
}

export function getConvertCacheInfo(): Promise<ConvertCacheInfo> {
  return invoke('get_convert_cache_info')
}
export function openConvertDir(): Promise<void> {
  return invoke('open_convert_dir')
}
export function clearConvertCache(): Promise<ClearConvertCacheResult> {
  return invoke('clear_convert_cache')
}

// Events
export function onMediaUpdated(callback: (item: MediaItem) => void): Promise<UnlistenFn> {
  return listen<MediaItem>('media-updated', (e) => callback(e.payload))
}
export function onServerStatusChanged(callback: (status: ServerStatus) => void): Promise<UnlistenFn> {
  return listen<ServerStatus>('server-status-changed', (e) => callback(e.payload))
}

// 内置组件（FFmpeg / EasyTier）
export function getComponentsStatus(): Promise<ComponentStatus[]> {
  return invoke('get_components_status')
}
export function downloadComponent(kind: 'ffmpeg' | 'easytier'): Promise<ComponentStatus> {
  return invoke('download_component', { kind })
}
export function openComponentPage(kind: 'ffmpeg' | 'easytier'): Promise<void> {
  return invoke('open_component_page', { kind })
}
export function onComponentDownloadProgress(
  callback: (progress: ComponentDownloadProgress) => void
): Promise<UnlistenFn> {
  return listen<ComponentDownloadProgress>('component-download-progress', (e) => callback(e.payload))
}

// EasyTier 组网
export function getEasytierStatus(): Promise<EasytierStatus> {
  return invoke('get_easytier_status')
}
export function startEasytier(): Promise<EasytierStatus> {
  return invoke('start_easytier')
}
export function stopEasytier(): Promise<EasytierStatus> {
  return invoke('stop_easytier')
}
export function getRoomInfo(): Promise<RoomInfo> {
  return invoke('get_room_info')
}
export function readEasytierLog(): Promise<string | null> {
  return invoke('read_easytier_log')
}
export function resetNetworkAdapter(): Promise<EasytierStatus> {
  return invoke('reset_network_adapter')
}
export function onEasytierStatusChanged(callback: (status: EasytierStatus) => void): Promise<UnlistenFn> {
  return listen<EasytierStatus>('easytier-status-changed', (e) => callback(e.payload))
}

// 房间内媒体共享
export function fetchPeerMedia(ip: string, port: number): Promise<MediaSummary[]> {
  return invoke('fetch_peer_media', { ip, port })
}
/** 加入对端媒体下载任务（后台执行，可在「任务」页暂停/继续/取消，完成后自动入库） */
export function downloadPeerMediaTask(params: {
  ip: string
  port: number
  id: string
  fileName: string
  fileSize: number
  title: string
}): Promise<ConvertTask> {
  return invoke('download_peer_media_task', params)
}
export function downloadTaskPause(taskId: string): Promise<void> {
  return invoke('download_task_pause', { taskId })
}
export function downloadTaskResume(taskId: string): Promise<void> {
  return invoke('download_task_resume', { taskId })
}
