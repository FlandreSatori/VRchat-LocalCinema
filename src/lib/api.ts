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
  /** 是否允许其他人（对等节点）查看本机媒体库 */
  allowPeerView: boolean
  defaultPlayerPath: string
  defaultPlayerArgs: string
  thumbDir: string
  autostart: boolean
  closeBehavior: CloseBehavior
  ffmpegPath: string
  ffprobePath: string
  easytierCorePath: string
  easytierCliPath: string
  /** cloudflared 路径（Cloudflare 快速隧道），空表示自动检测 */
  cloudflaredPath: string
  downloadDir: string
  /** 媒体流读缓冲（KB），默认 256，不推荐修改 */
  streamBufferKb: number
  /** 禁用电源节流（Windows EcoQoS 豁免），默认开启 */
  disablePowerThrottling: boolean
  /** 一键压制 720p 峰值码率（Mbps，范围 1-100，保留两位小数） */
  streaming720pBitrateMbps: number
  /** 一键压制 1080p 峰值码率（Mbps，范围 1-100，保留两位小数） */
  streaming1080pBitrateMbps: number
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
  /** 在线媒体（B 站等）的原始页面链接；本机媒体为 null */
  sourceUrl: string | null
  /** 媒体来源：local（本机文件）/ online（在线流媒体） */
  sourceKind: 'local' | 'online'
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

/** Cloudflare 快速隧道状态 */
export interface TunnelStatus {
  running: boolean
  ready: boolean
  /** 公网 HTTPS 地址（形如 https://xxx.trycloudflare.com）；启动初期为 null */
  url: string | null
  port: number | null
  error: string | null
  revision: number
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

export type ComponentKindId = 'ffmpeg' | 'easytier' | 'cloudflared'

export interface ComponentStatus {
  kind: ComponentKindId
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
/** 通过 B 站链接导入在线视频（免登录上限 720P），生成本机代理播放链接 */
export function importOnlineVideo(url: string): Promise<MediaItem> {
  return invoke('import_online_video', { url })
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
  /** FFmpeg 实测倍速、帧率与剩余秒数；未知时为 null */
  speed?: number | null
  fps?: number | null
  remainingSeconds?: number | null
}
/** 一条媒体流（供「转码设置」对话框展示音轨 / 字幕轨选择） */
export interface MediaStreamInfo {
  /** ffprobe 报告流的绝对索引 */
  index: number
  kind: 'video' | 'audio' | 'subtitle'
  codec: string
  profile?: string | null
  language?: string | null
  title?: string | null
  isDefault: boolean
  width?: number | null
  height?: number | null
  pixFmt?: string | null
  channels?: number | null
  channelLayout?: string | null
  sampleRate?: number | null
  /** 是否可直接直拷进 MP4（视频 / 音频轨用；字幕轨恒为 false） */
  copyable: boolean
  /** 是否为文本字幕（可烧录 / 内嵌 / 外挂）；图形字幕为 false */
  textSubtitle: boolean
  /** 是否为图形（位图）字幕：只能用 overlay 方式烧录，不可内嵌或外挂 */
  graphicSubtitle: boolean
  /** 外挂（同目录同名）字幕文件的绝对路径；仅外挂字幕条目有值，内嵌字幕轨为 null */
  sidecarPath?: string | null
}
/** 获取媒体的完整流清单（需先下载/指定 ffprobe 组件） */
export function getMediaStreams(id: string): Promise<MediaStreamInfo[]> {
  return invoke('get_media_streams', { id })
}

/** 转码配置：字段缺省时按「自动」处理（与后端 TranscodeOptions 一一对应） */
export interface TranscodeOptions {
  /** 视频流绝对索引；null = 第一路视频 */
  videoStream?: number | null
  /** 音频流绝对索引；null = 第一路音频 */
  audioStream?: number | null
  /** 是否保留音频（默认保留） */
  includeAudio?: boolean | null
  /** 字幕流绝对索引；烧录 / 内嵌时生效，null = 自动选第一条文本字幕 */
  subtitleStream?: number | null
  /** 字幕处理：burn（烧录）/ embed（内嵌）/ external（外挂文件）/ drop（丢弃） */
  subtitleMode?: 'burn' | 'embed' | 'external' | 'drop' | null
  /** 选中的字幕流是否为图形（位图）字幕：图形字幕只能用 overlay 方式烧录 */
  subtitleGraphic?: boolean | null
  /** 视频处理：copy / h264 / h265 / av1 */
  videoMode?: 'copy' | 'h264' | 'h265' | 'av1' | null
  /** 目标分辨率高度：original / 2160 / 1440 / 1080 / 720 / 480 */
  resolution?: string | null
  /** 质量 CRF（0-51，越小越清晰） */
  crf?: number | null
  /** 编码预设：ultrafast … veryslow */
  preset?: string | null
  /** 视频像素格式：auto / yuv420p / yuv420p10le */
  pixFmt?: string | null
  /** 音频处理：copy / aac */
  audioMode?: 'copy' | 'aac' | null
  /** AAC 码率 */
  audioBitrate?: string | null
  /** 音频声道数（降混）；null 或 0 = 保持原样 */
  audioChannels?: number | null
  /** 音频采样率 */
  audioSampleRate?: number | null
  /** MP4 faststart（默认开启） */
  faststart?: boolean | null
  /** 视频峰值码率（如 6000k） */
  maxVideoBitrate?: string | null
  /** 视频码率缓冲区（如 12000k） */
  videoBufferSize?: string | null
  /** 关键帧间隔（帧数） */
  keyframeInterval?: number | null
}
export interface TranscodeSourceProfile {
  videoStream: number
  duration: number | null
  width: number | null
  height: number | null
  fps: number | null
  totalFrames: number | null
  videoCodec: string
  bitrate: number | null
  pixFmt: string | null
  fileSize: number
}
export interface EstimateSource {
  profile: TranscodeSourceProfile
  streams: MediaStreamInfo[]
  effectiveOptions: TranscodeOptions
}
export interface ProbeEstimate {
  fps: number | null
  samples: number[]
  method: 'frame-difference'
  reason: string | null
  effectiveOptions: TranscodeOptions
}
export function loadEstimateSource(id: string, options?: TranscodeOptions, requestId?: string): Promise<EstimateSource> {
  return invoke('load_estimate_source', { id, options, requestId })
}
export function probeTranscodeEstimate(id: string, requestId: string, options: TranscodeOptions): Promise<ProbeEstimate> {
  return invoke('probe_transcode_estimate', { id, requestId, options })
}
export function cancelTranscodeEstimate(requestId: string): Promise<void> {
  return invoke('cancel_transcode_estimate', { requestId })
}

export function transcodeMedia(
  id: string,
  overwrite?: boolean,
  options?: TranscodeOptions
): Promise<ConvertTask> {
  return invoke('transcode_media', { id, overwrite, options })
}
/** 一键压制为流媒体 MP4：默认 AV1（体积更小且可播放）。
 *  不提升分辨率（仅在源高于 1080p 时降采样）、不强制音频采样率；
 *  码率由 CRF 决定，设置为「不高于」的上限。 */
export function transcodeStreamingCopy(
  id: string,
  resolution: 'original' | '1080',
  maxrateMbps: number
): Promise<ConvertTask> {
  const kbps = Math.max(1, Math.round(maxrateMbps * 1000))
  return invoke('transcode_media', {
    id, overwrite: false, options: {
      videoMode: 'av1', audioMode: 'aac', includeAudio: true, subtitleMode: 'drop',
      resolution, crf: 28, preset: '10', pixFmt: 'yuv420p',
      audioBitrate: '160k', audioChannels: 2, faststart: true,
      maxVideoBitrate: `${kbps}k`, videoBufferSize: `${kbps * 2}k`,
    } satisfies TranscodeOptions,
  })
}
export function transcodeOutputExists(id: string, kind?: 'transcode' | 'segment'): Promise<boolean> {
  return invoke('transcode_output_exists', { id, kind })
}
/** 微软商店搜不到官方 `Microsoft.DolbyAudioExtensions`，只有 OEM 版（实测无效），
 *  因此只能给用户第三方下载页面。链接来自第三方站点、安全性未知，请用户自行判断。 */
export const DOLBY_DECODER_URL = 'https://www.free-codecs.com/download/dolby-audio-extensions.htm'
/** 备选页面：ElevenForum 关于「Win11 24H2 不再自带 AC-3 编解码器」的讨论帖（同样为第三方） */
export const DOLBY_DECODER_FORUM_URL =
  'https://www.elevenforum.com/t/ac-3-dolby-digital-codec-no-longer-included-with-windows-11-version-24h2.25597/'
/** 检查本机是否缺少指定音频编码的系统解码器（codec: 'ac3' | 'eac3'）；检测失败按不缺处理 */
export function systemAudioDecoderMissing(codec: 'ac3' | 'eac3'): Promise<boolean> {
  return invoke('system_audio_decoder_missing', { codec })
}
/** 带进程级缓存的解码器缺失检测（安装解码器后约 30 秒内自动刷新） */
const decoderMissingCache = new Map<string, boolean>()
export async function audioDecoderMissingCached(codec: 'ac3' | 'eac3'): Promise<boolean> {
  const cached = decoderMissingCache.get(codec)
  if (cached !== undefined) return cached
  const missing = await systemAudioDecoderMissing(codec)
  decoderMissingCache.set(codec, missing)
  if (missing) {
    // 缺失时短缓存（15s），安装后尽快刷新为可用
    setTimeout(() => decoderMissingCache.delete(codec), 15_000)
  }
  return missing
}
/** 样本生成完成事件：ok 为 true 时 path 为样本文件路径 */
export interface SampleDoneEvent {
  mediaId: string
  ok: boolean
  path: string | null
  error: string | null
}
/** 生成视频样本（截取正中间 20 秒，保存到源视频所在目录），完成后触发 sample-done 事件 */
export function generateSample(id: string): Promise<void> {
  return invoke('generate_sample', { id })
}
/** 监听样本生成完成事件 */
export function onSampleDone(
  callback: (e: SampleDoneEvent) => void
): Promise<UnlistenFn> {
  return listen<SampleDoneEvent>('sample-done', (ev) => callback(ev.payload))
}
/** 在资源管理器中定位文件（选中） */
export function revealPath(path: string): Promise<void> {
  return invoke('reveal_path', { path })
}
export function segmentTranscode(id: string, firstMs: number, overwrite?: boolean, options?: TranscodeOptions): Promise<ConvertTask> {
  return invoke('segment_transcode', { id, firstMs, overwrite, options })
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
export function isFirstLaunch(): Promise<boolean> {
  return invoke('is_first_launch')
}
export function markDisclaimerAccepted(): Promise<void> {
  return invoke('mark_disclaimer_accepted')
}
/** 程序是否运行在系统临时目录（如直接在压缩包内运行）：启动时需警告并退出 */
export function isRunningInTempDir(): Promise<boolean> {
  return invoke('is_running_in_temp_dir')
}
/** 退出应用（与托盘菜单「退出」一致） */
export function quitApp(): Promise<void> {
  return invoke('quit_app')
}
export function openDataDir(): Promise<void> {
  return invoke('open_data_dir')
}
/** 打开任意数据目录（不存在时后端会先创建） */
export function openDir(path: string): Promise<void> {
  return invoke('open_dir', { path })
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
export function downloadComponent(kind: ComponentKindId): Promise<ComponentStatus> {
  return invoke('download_component', { kind })
}
export function openComponentPage(kind: ComponentKindId): Promise<void> {
  return invoke('open_component_page', { kind })
}
export function onComponentDownloadProgress(
  callback: (progress: ComponentDownloadProgress) => void
): Promise<UnlistenFn> {
  return listen<ComponentDownloadProgress>('component-download-progress', (e) => callback(e.payload))
}

// Cloudflare 快速隧道（无需组网即可公网分享）
export function getTunnelStatus(): Promise<TunnelStatus> {
  return invoke('get_tunnel_status')
}
export function startTunnel(): Promise<TunnelStatus> {
  return invoke('start_tunnel')
}
export function stopTunnel(): Promise<TunnelStatus> {
  return invoke('stop_tunnel')
}
/** 生成某条媒体的公网隧道播放链接（需隧道已启动） */
export function buildTunnelUrl(id: string): Promise<string> {
  return invoke('build_tunnel_url', { id })
}
export function onTunnelStatusChanged(callback: (status: TunnelStatus) => void): Promise<UnlistenFn> {
  return listen<TunnelStatus>('tunnel-status-changed', (e) => callback(e.payload))
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
/** 对端连通性检测结果：ICMP 可达 / 目标端口 TCP 可连接 */
export interface PeerConnCheck {
  ping: boolean
  tcp: boolean
}
/** 检测对端 IP 的 ping 与目标端口 TCP 连通性 */
export function testPeerConnection(ip: string, port: number): Promise<PeerConnCheck> {
  return invoke('test_peer_connection', { ip, port })
}
/** 加入对端媒体下载任务（后台执行，可在「任务」页暂停/继续/取消，完成后自动入库） */
export function downloadPeerMediaTask(params: {
  ip: string
  port: number
  id: string
  fileName: string
  fileSize: number
  title: string
  hash?: string
}): Promise<ConvertTask> {
  return invoke('download_peer_media_task', params)
}
export function downloadTaskPause(taskId: string): Promise<void> {
  return invoke('download_task_pause', { taskId })
}
export function downloadTaskResume(taskId: string): Promise<void> {
  return invoke('download_task_resume', { taskId })
}
