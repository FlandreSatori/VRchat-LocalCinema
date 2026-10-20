<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import { FolderOpen, RotateCcw, Trash2, Download, ExternalLink, AlertTriangle, RefreshCw } from 'lucide-vue-next'
import { open } from '@tauri-apps/plugin-dialog'
import Card from '../components/ui/Card.vue'
import Button from '../components/ui/Button.vue'
import Input from '../components/ui/Input.vue'
import Label from '../components/ui/Label.vue'
import Badge from '../components/ui/Badge.vue'
import Select from '../components/ui/Select.vue'
import Switch from '../components/ui/Switch.vue'
import Separator from '../components/ui/Separator.vue'
import {
  getSettings,
  saveSettings,
  resetSettings,
  detectPlayers,
  getAppPaths,
  getNetworkInterfaces,
  openDataDir,
  clearThumbnailCache,
  getConvertCacheInfo,
  openConvertDir,
  clearConvertCache,
  listMedia,
  getComponentsStatus,
  downloadComponent,
  openComponentPage,
  onComponentDownloadProgress,
  type AppPaths,
  type CloseBehavior,
  type ComponentKindId,
  type ComponentStatus,
  type ConvertCacheInfo,
  type ListenMode,
  type NetInterface,
  type PlayerInfo,
  type Settings,
  type UrlMode,
} from '../lib/api'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { settingsCache, mediaStore, largeMediaItems } from '../lib/store'
import { toast, errMsg } from '../lib/toast'
import { confirmDialog } from '../lib/confirm'
import { formatSize } from '../lib/format'

const form = reactive<Settings>({
  listenMode: 'loopback',
  port: 17001,
  defaultUrlMode: 'localhost',
  lanAdapterIp: '',
  allowPeerView: false,
  defaultPlayerPath: '',
  defaultPlayerArgs: '',
  thumbDir: '',
  autostart: false,
  closeBehavior: 'ask',
  // 占位初始值，实际以 getSettings() 返回的版本号为准
  settingsRevision: 1,
  ffmpegPath: '',
  ffprobePath: '',
  easytierCorePath: '',
  easytierCliPath: '',
  cloudflaredPath: '',
  downloadDir: '',
  streamBufferKb: 256,
  disablePowerThrottling: true,
  streaming720pBitrateMbps: 3,
  streaming1080pBitrateMbps: 6,
  easytier: {
    protocols: ['tcp', 'ws'],
    peerUrl: '',
    roomName: 'VRchat',
    roomPassword: '123',
    memberName: '',
    ipv4: '',
    ipv6: '',
    dhcp: true,
    kcpProxy: true,
    kcpInput: false,
    quicProxy: false,
    quicInput: false,
    latencyFirst: false,
    compression: 'none',
    upnp: true,
    multiThread: true,
    listenPort: 11010,
  },
})

const components = ref<ComponentStatus[]>([])
const installing = ref<string | null>(null)
const progressMap = reactive<Record<string, { downloaded: number; total: number | null }>>({})

function componentOf(kind: ComponentKindId): ComponentStatus | undefined {
  return components.value.find((c) => c.kind === kind)
}

function sourceText(c: ComponentStatus): string {
  switch (c.source) {
    case 'custom': return '手动路径'
    case 'bundled': return '随包内置'
    case 'downloaded': return '自动下载'
    case 'system': return '系统 PATH'
    default: return '未安装'
  }
}
function sourceVariant(c: ComponentStatus): 'success' | 'secondary' | 'warning' {
  if (!c.installed) return 'warning'
  return c.builtin || c.source === 'custom' ? 'success' : 'secondary'
}

function progressText(kind: string): string {
  if (installing.value !== kind) return ''
  const p = progressMap[kind]
  if (!p) return '正在获取最新版本信息…'
  if (!p.total) return `已下载 ${(p.downloaded / 1048576).toFixed(1)} MB`
  const pct = Math.min(100, Math.round((p.downloaded / p.total) * 100))
  return `已下载 ${(p.downloaded / 1048576).toFixed(1)} / ${(p.total / 1048576).toFixed(1)} MB（${pct}%）`
}

async function refreshComponents() {
  try {
    components.value = await getComponentsStatus()
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function installComponent(kind: ComponentKindId) {
  if (installing.value) return
  installing.value = kind
  delete progressMap[kind]
  try {
    const status = await downloadComponent(kind)
    const idx = components.value.findIndex((c) => c.kind === kind)
    if (idx >= 0) components.value[idx] = status
    else components.value.push(status)
    toast.success(`${status.label} 已安装到内置组件目录`)
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    installing.value = null
    delete progressMap[kind]
  }
}

async function openManual(kind: ComponentKindId) {
  try {
    await openComponentPage(kind)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

const loading = ref(true)
const saving = ref(false)
/// 遮罩仅在保存耗时较长（如重启 HTTP 服务）时出现，避免每次修改都闪遮罩
const applyingOpen = ref(false)
/// 首次加载完成前不自动保存，避免把占位初始值写进配置
const ready = ref(false)
/// 外部同步（settingsCache → form）期间暂停自动保存，防止回声
const syncing = ref(false)
let saveTimer: number | null = null
const appPaths = ref<AppPaths | null>(null)
const players = ref<PlayerInfo[]>([])

// 监听模式为局域网时服务需要绑定到 0.0.0.0，而这依赖「允许其他人查看我的媒体库」；
// 未开启该开关时禁用局域网选项，避免出现「局域网模式却只监听 127.0.0.1」的矛盾状态。
const listenModeOptions = computed(() => [
  { value: 'loopback', label: '本地模式' },
  { value: 'lan', label: '局域网模式', disabled: !form.allowPeerView },
])
const urlModeOptions = computed(() => [
  { value: 'localhost', label: '本地模式' },
  { value: 'lan', label: '局域网模式', disabled: !form.allowPeerView },
])
const closeBehaviorOptions = [
  { value: 'ask', label: '每次都询问' },
  { value: 'exit', label: '完全关闭' },
  { value: 'tray', label: '最小化到托盘' },
]
const streamBufferOptions = [
  { value: '8', label: '8 KB' },
  { value: '64', label: '64 KB' },
  { value: '256', label: '256 KB（默认）' },
]

const playerOptions = computed(() => {
  const opts = [
    { value: '', label: '跟随系统默认' },
    ...players.value.map((p) => ({ value: p.path, label: `${p.name}（${p.path}）` })),
  ]
  if (form.defaultPlayerPath && !opts.some((o) => o.value === form.defaultPlayerPath)) {
    opts.push({ value: form.defaultPlayerPath, label: form.defaultPlayerPath })
  }
  return opts
})

/// 端口无效时暂停自动保存，由行内提示纠错
const portInvalid = computed(() => form.port < 1 || form.port > 65535)

/// 其他页面改动设置后（左下角快捷开关、关闭弹窗「不再提示」、组网页保存等）
/// 表单实时跟随刷新，避免显示滞后；同步期间暂停自动保存，防止回声。
/// 注意：需定义在 listenMode watcher 之前，保证同批触发时 syncing 先行置位
watch(settingsCache, (s) => {
  if (!s) return
  syncing.value = true
  Object.assign(form, s)
  void nextTick(() => {
    syncing.value = false
  })
})

watch(() => form.listenMode, (mode, prev) => {
  form.defaultUrlMode = mode === 'lan' ? 'lan' : 'localhost'
  if (mode === 'lan' && prev !== 'lan' && !syncing.value) {
    toast.info('已切换到局域网模式')
    if (largeMediaItems.value.length) toast.warning(`媒体库有 ${largeMediaItems.value.length} 个 2 GB 以上视频；开放局域网分享前建议压制为流媒体 MP4`)
  }
})

// 关闭「允许其他人查看我的媒体库」后必须回到本地模式：此时服务只会绑定环回地址，
// 若仍保持局域网模式就会出现「局域网模式却只监听 127.0.0.1」的矛盾状态。
watch(() => form.allowPeerView, (on) => {
  if (on || form.listenMode !== 'lan') return
  form.listenMode = 'loopback'
  if (!syncing.value) toast.info('已关闭「允许其他人查看我的媒体库」，监听模式已切回本地')
})

/// 局域网模式使用的网卡（IP）选择
const lanInterfaces = ref<NetInterface[]>([])
const adapterOptions = computed(() => [
  { value: '', label: '自动（EasyTier 运行时优先虚拟 IP）' },
  ...lanInterfaces.value.flatMap((n) =>
    n.ips.map((ip) => ({ value: ip, label: `${n.name}（${ip}）` }))
  ),
])

onMounted(async () => {
  try {
    lanInterfaces.value = await getNetworkInterfaces()
  } catch {
    // 网卡枚举失败时仅提供自动选项
  }
})

function setLanAdapter(v: string) {
  form.lanAdapterIp = v
}

function setListenMode(v: string) {
  if (v === 'lan' && !form.allowPeerView) return
  form.listenMode = v as ListenMode
}
function setUrlMode(v: string) {
  if (v === 'lan' && !form.allowPeerView) return
  form.defaultUrlMode = v as UrlMode
  form.listenMode = v === 'lan' ? 'lan' : 'loopback'
}
function setCloseBehavior(v: string) {
  form.closeBehavior = v as CloseBehavior
}
function setStreamBuffer(v: string) {
  form.streamBufferKb = Number(v)
}

/// 一键压制码率：限制为 1-100 Mbps，保留两位小数
function clampBitrate(v: number): number {
  if (!Number.isFinite(v)) return 1
  return Math.min(100, Math.max(1, Math.round(v * 100) / 100))
}
function setBitrate(key: 'streaming720pBitrateMbps' | 'streaming1080pBitrateMbps', raw: string) {
  if (raw.trim() === '') return
  form[key] = clampBitrate(Number(raw))
}

async function pickExe(title: string): Promise<string | null> {
  const selected = await open({
    multiple: false,
    title,
    filters: [{ name: '可执行文件', extensions: ['exe'] }],
  })
  return typeof selected === 'string' ? selected : null
}

async function browsePlayer() {
  try {
    const p = await pickExe('选择默认播放器')
    if (p) form.defaultPlayerPath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseFfmpeg() {
  try {
    const p = await pickExe('选择 ffmpeg 可执行文件')
    if (p) form.ffmpegPath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseFfprobe() {
  try {
    const p = await pickExe('选择 ffprobe 可执行文件')
    if (p) form.ffprobePath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseEasytierCore() {
  try {
    const p = await pickExe('选择 easytier-core 可执行文件')
    if (p) form.easytierCorePath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseEasytierCli() {
  try {
    const p = await pickExe('选择 easytier-cli 可执行文件')
    if (p) form.easytierCliPath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseCloudflared() {
  try {
    const p = await pickExe('选择 cloudflared 可执行文件')
    if (p) form.cloudflaredPath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseDownloadDir() {
  try {
    const selected = await open({ directory: true, multiple: false, title: '选择下载保存目录' })
    if (typeof selected === 'string') form.downloadDir = selected
  } catch (e) {
    toast.error(errMsg(e))
  }
}

/// 所有设置项修改即应用：表单任意变化防抖后整体保存（端口等网络项变化会触发服务自动重启）
function scheduleSave() {
  if (saveTimer !== null) window.clearTimeout(saveTimer)
  saveTimer = window.setTimeout(() => {
    saveTimer = null
    void persist()
  }, 600)
}

async function persist() {
  if (!ready.value || syncing.value || saving.value) return
  if (portInvalid.value) return
  form.streaming720pBitrateMbps = clampBitrate(form.streaming720pBitrateMbps)
  form.streaming1080pBitrateMbps = clampBitrate(form.streaming1080pBitrateMbps)
  saving.value = true
  // 保存很快（仅写配置）时不打扰；只有重启 HTTP 服务等耗时操作才显示遮罩
  const slow = window.setTimeout(() => {
    applyingOpen.value = true
  }, 250)
  try {
    await saveSettings({ ...form })
    settingsCache.value = { ...form }
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    window.clearTimeout(slow)
    applyingOpen.value = false
    saving.value = false
  }
}

watch(
  form,
  () => {
    if (ready.value && !syncing.value) scheduleSave()
  },
  { deep: true }
)

async function reset() {
  const ok = await confirmDialog({
    title: '恢复默认设置？',
    description: '确定恢复默认设置吗？所有自定义配置将被重置。',
    confirmText: '恢复默认',
    danger: true,
  })
  if (!ok) return
  try {
    const s = await resetSettings()
    Object.assign(form, s)
    form.streaming720pBitrateMbps = clampBitrate(form.streaming720pBitrateMbps)
    form.streaming1080pBitrateMbps = clampBitrate(form.streaming1080pBitrateMbps)
    settingsCache.value = { ...s }
    toast.success('已恢复默认设置')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function clearCache() {
  try {
    const n = await clearThumbnailCache()
    toast.success(`已清理 ${n} 个未使用的封面缓存文件`)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openDir() {
  try {
    await openDataDir()
  } catch (e) {
    toast.error(errMsg(e))
  }
}

// ---------- 转码缓存 ----------

const convertCache = ref<ConvertCacheInfo | null>(null)
const clearingConvertCache = ref(false)

async function refreshConvertCache() {
  try {
    convertCache.value = await getConvertCacheInfo()
  } catch {
    convertCache.value = null
  }
}

async function openTranscodeDir() {
  try {
    await openConvertDir()
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function doClearConvertCache() {
  const info = convertCache.value
  if (!info || clearingConvertCache.value) return
  const ok = await confirmDialog({
    title: '清除转码缓存？',
    description:
      `将删除转码目录下的 ${info.fileCount} 个文件（约 ${formatSize(info.totalBytes)}），` +
      '并从媒体库移除对应的转码产物记录。源视频与磁盘上的其他文件不受影响。',
    confirmText: '清除',
    danger: true,
  })
  if (!ok) return
  clearingConvertCache.value = true
  try {
    const r = await clearConvertCache()
    // 转码产物记录已删除，重新拉取媒体库列表
    mediaStore.items = await listMedia()
    toast.success(
      `已清除 ${r.removed_files} 个文件（释放 ${formatSize(r.freed_bytes)}），移除 ${r.removed_records} 条转码产物记录`
    )
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    clearingConvertCache.value = false
    await refreshConvertCache()
  }
}

let unlistenProgress: UnlistenFn | null = null

onMounted(async () => {
  try {
    const [s, paths, detected] = await Promise.all([
      getSettings(),
      getAppPaths().catch(() => null),
      detectPlayers().catch(() => [] as PlayerInfo[]),
    ])
    Object.assign(form, s)
    // 兼容历史配置：KCP 与 QUIC 代理互斥，同时开启时保留 KCP
    if (form.easytier.kcpProxy && form.easytier.quicProxy) form.easytier.quicProxy = false
    form.streaming720pBitrateMbps = clampBitrate(form.streaming720pBitrateMbps)
    form.streaming1080pBitrateMbps = clampBitrate(form.streaming1080pBitrateMbps)
    appPaths.value = paths
    players.value = detected
    await refreshComponents()
    await refreshConvertCache()
    unlistenProgress = await onComponentDownloadProgress((p) => {
      progressMap[p.kind] = { downloaded: p.downloaded, total: p.total }
    })
    // 等首批 watch 回调跑完再启用自动保存，避免把加载时的赋值当成用户修改
    await nextTick()
    ready.value = true
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    loading.value = false
  }
})

onUnmounted(() => {
  unlistenProgress?.()
})
</script>

<template>
  <div class="mx-auto max-w-3xl space-y-4 p-6 pb-12">
    <!-- 保存耗时较长（如重启 HTTP 服务）时提示，短暂保存不显示 -->
    <div
      v-if="applyingOpen"
      class="fixed inset-0 z-[60] flex items-center justify-center bg-background/70 backdrop-blur-sm"
    >
      <div class="flex items-center gap-3 rounded-lg border border-border bg-card px-4 py-3 shadow-lg">
        <RefreshCw class="h-4 w-4 animate-spin" />
        <span class="text-sm">正在应用设置，请稍候…</span>
      </div>
    </div>

    <div class="flex items-center justify-between">
      <h1 class="text-xl font-semibold">设置</h1>
      <p class="text-xs text-muted-foreground">所有修改即时生效</p>
    </div>

    <Card class="space-y-4 p-5">
      <div>
        <h2 class="font-medium">HTTP 服务</h2>
        <p class="mt-0.5 text-xs text-muted-foreground">修改监听或端口后服务将自动重启</p>
      </div>
      <div class="grid grid-cols-2 gap-4">
        <div class="space-y-1.5">
          <Label>监听模式</Label>
          <Select :model-value="form.listenMode" :options="listenModeOptions" class="w-full" @update:model-value="setListenMode" />
        </div>
        <div class="space-y-1.5">
          <Label>端口</Label>
          <Input
            type="number"
            :min="1"
            :max="65535"
            :model-value="String(form.port)"
            @update:model-value="form.port = Math.round(Number($event)) || 0"
          />
          <p v-if="portInvalid" class="text-xs text-destructive">
            端口必须在 1-65535 之间；修正前其他修改不会保存
          </p>
        </div>
      </div>
      <p class="text-xs text-muted-foreground">
        本地模式仅允许本机访问；局域网模式会自动选择本机可用的物理网卡地址，其他同网络设备可访问。
      </p>
      <p v-if="!form.allowPeerView" class="text-xs text-amber-600 dark:text-amber-500">
        局域网模式需先开启下方「允许其他人查看我的媒体库」；未开启时服务只监听本机地址。
      </p>
      <Separator />
      <div class="flex items-center justify-between gap-4">
        <div>
          <Label>允许其他人查看我的媒体库</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">
            开启后才能切换到局域网监听模式；开启时自动添加入站防火墙放行规则（首次需管理员授权），请确保所有成员的监听端口一致
          </p>
        </div>
        <Switch v-model:checked="form.allowPeerView" />
      </div>
      <p v-if="form.allowPeerView" class="text-xs text-amber-600 dark:text-amber-500">
        已开启：他人可通过 {{ form.port }} 端口查看你的媒体库。请保持所有人的监听端口一致；关闭后对方将看不到任何视频。
      </p>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">播放链接</h2>
      <div class="space-y-1.5">
        <Label>默认地址模式</Label>
        <Select :model-value="form.defaultUrlMode" :options="urlModeOptions" class="w-full" @update:model-value="setUrlMode" />
      </div>
      <div class="space-y-1.5">
        <Label>局域网地址（网卡）</Label>
        <Select :model-value="form.lanAdapterIp" :options="adapterOptions" class="w-full" @update:model-value="setLanAdapter" />
      </div>
      <p class="text-xs text-muted-foreground">
        局域网模式默认自动选地址：EasyTier 组网运行时优先使用虚拟 IP，未运行时使用物理网卡地址；也可在此手动指定网卡。
      </p>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">外部播放器</h2>
      <div class="space-y-1.5">
        <Label>默认播放器</Label>
        <div class="flex gap-2">
          <Select v-model="form.defaultPlayerPath" :options="playerOptions" class="flex-1" />
          <Button variant="outline" @click="browsePlayer">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
      <div class="space-y-1.5">
        <Label>启动参数</Label>
        <Input v-model="form.defaultPlayerArgs" placeholder="{path} 将被替换为文件路径" />
        <p class="text-xs text-muted-foreground">留空则使用播放器默认参数；{path} 占位符会被替换为视频文件路径。</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">封面与缓存</h2>
      <div class="space-y-1.5">
        <Label>封面缓存目录</Label>
        <Input v-model="form.thumbDir" :placeholder="appPaths?.thumbDir ?? '默认目录'" />
        <p class="text-xs text-muted-foreground">留空使用默认目录{{ appPaths ? `：${appPaths.thumbDir}` : '' }}</p>
      </div>
      <Button variant="outline" size="sm" @click="clearCache">
        <Trash2 class="h-4 w-4" />
        清理未使用封面缓存
      </Button>
      <Separator />
      <div class="space-y-1.5">
        <Label>转码缓存目录</Label>
        <p class="break-all font-mono text-xs text-muted-foreground">
          {{ convertCache?.dir ?? '—' }}
        </p>
        <p class="text-xs text-muted-foreground">
          转码产物（MP4/分段）与 HLS 分片共
          {{ convertCache ? `${convertCache.fileCount} 个文件，占用 ${formatSize(convertCache.totalBytes)}` : '…' }}
        </p>
      </div>
      <div class="flex flex-wrap gap-2">
        <Button variant="outline" size="sm" @click="openTranscodeDir">
          <FolderOpen class="h-4 w-4" />
          打开目录
        </Button>
        <Button
          variant="destructive"
          size="sm"
          :disabled="!convertCache || convertCache.fileCount === 0 || clearingConvertCache"
          @click="doClearConvertCache"
        >
          <Trash2 class="h-4 w-4" />
          {{ clearingConvertCache ? '清理中…' : '一键清除转码缓存' }}
        </Button>
      </div>
      <p class="text-xs text-muted-foreground">
        清除会同时从媒体库移除对应的转码产物卡片，源视频保留在原位置；转码队列中有任务进行时无法清理。
      </p>
    </Card>

    <Card class="space-y-4 p-5">
      <div class="flex items-center justify-between">
        <div>
          <h2 class="font-medium">内置组件</h2>
          <p class="mt-0.5 text-xs text-muted-foreground">优先使用内置目录中的组件，其次回退到系统 PATH</p>
        </div>
        <Button variant="ghost" size="sm" :disabled="loading" @click="refreshComponents">
          <RefreshCw class="h-4 w-4" />
          刷新
        </Button>
      </div>

      <div v-for="kind in (['ffmpeg', 'easytier', 'cloudflared'] as const)" :key="kind" class="space-y-2 rounded-md border border-border p-3">
        <div class="flex items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            <span class="text-sm font-medium">{{ componentOf(kind)?.label ?? (kind === 'ffmpeg' ? 'FFmpeg' : kind === 'easytier' ? 'EasyTier' : 'cloudflared') }}</span>
            <Badge v-if="componentOf(kind)" :variant="sourceVariant(componentOf(kind)!)">
              {{ sourceText(componentOf(kind)!) }}
            </Badge>
          </div>
          <div class="flex gap-2">
            <Button size="sm" :disabled="installing !== null" @click="installComponent(kind)">
              <Download class="h-4 w-4" />
              {{ installing === kind ? '下载中…' : '自动下载最新版' }}
            </Button>
            <Button variant="outline" size="sm" @click="openManual(kind)">
              <ExternalLink class="h-4 w-4" />
              手动下载
            </Button>
          </div>
        </div>
        <p v-if="componentOf(kind)?.version" class="font-mono text-xs text-muted-foreground">
          {{ componentOf(kind)!.version }}
        </p>
        <p v-if="componentOf(kind)?.path" class="break-all font-mono text-xs text-muted-foreground">
          {{ componentOf(kind)!.path }}
        </p>
        <div v-if="componentOf(kind) && !componentOf(kind)!.installed" class="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-500">
          <AlertTriangle class="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>
            似乎未安装{{ componentOf(kind)!.label }}（缺少 {{ componentOf(kind)!.missing.join(', ') }}），需要手动填入路径或下载吗？
          </span>
        </div>
        <p v-if="progressText(kind)" class="text-xs text-muted-foreground">{{ progressText(kind) }}</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">FFmpeg 手动路径</h2>
      <div class="space-y-1.5">
        <Label>ffmpeg 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.ffmpegPath" placeholder="留空则依次检测内置目录和 PATH" class="flex-1" />
          <Button variant="outline" @click="browseFfmpeg">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
      <div class="space-y-1.5">
        <Label>ffprobe 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.ffprobePath" placeholder="留空则依次检测内置目录和 PATH" class="flex-1" />
          <Button variant="outline" @click="browseFfprobe">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">EasyTier 手动路径</h2>
      <div class="space-y-1.5">
        <Label>easytier-core 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.easytierCorePath" placeholder="留空则依次检测内置目录和 PATH" class="flex-1" />
          <Button variant="outline" @click="browseEasytierCore">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
      <div class="space-y-1.5">
        <Label>easytier-cli 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.easytierCliPath" placeholder="留空则依次检测内置目录和 PATH" class="flex-1" />
          <Button variant="outline" @click="browseEasytierCli">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
      <p class="text-xs text-muted-foreground">组网配置请前往「EasyTier 组网」页面。</p>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">Cloudflare 隧道手动路径</h2>
      <div class="space-y-1.5">
        <Label>cloudflared 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.cloudflaredPath" placeholder="留空则依次检测内置目录和 PATH" class="flex-1" />
          <Button variant="outline" @click="browseCloudflared">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
        <p class="text-xs text-muted-foreground">用于「公网隧道」快速分享；隧道开关在「HTTP 服务」页面。</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">下载</h2>
      <div class="space-y-1.5">
        <Label>下载保存目录</Label>
        <div class="flex gap-2">
          <Input v-model="form.downloadDir" :placeholder="appPaths?.downloadDir ?? '默认目录'" class="flex-1" />
          <Button variant="outline" @click="browseDownloadDir">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
        <p class="text-xs text-muted-foreground">从房间成员处下载的媒体保存到此目录，留空使用默认目录{{ appPaths ? `：${appPaths.downloadDir}` : '' }}</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">系统</h2>
      <div class="flex items-center justify-between">
        <div>
          <Label>开机启动</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">登录 Windows 后自动启动本应用</p>
        </div>
        <Switch v-model:checked="form.autostart" />
      </div>
      <Separator />
      <div class="flex items-center justify-between gap-4">
        <div>
          <Label>关闭窗口时</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">点击窗口关闭按钮后的行为</p>
        </div>
        <Select :model-value="form.closeBehavior" :options="closeBehaviorOptions" class="w-40" @update:model-value="setCloseBehavior" />
      </div>
      <Separator />
      <div class="flex items-center justify-between gap-4">
        <div>
          <Label>播放时禁用电源节流</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">申请 Windows 电源节流（EcoQoS）豁免，避免窗口收到托盘后媒体供数被系统降速；修改后需重启应用生效</p>
        </div>
        <Switch v-model:checked="form.disablePowerThrottling" />
      </div>
      <Separator />
      <div class="flex items-center justify-between gap-4">
        <div>
          <Label>媒体流读缓冲</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">影响播放供数的高级参数，不推荐修改；仅在排查播放卡顿时按开发者建议调整</p>
        </div>
        <Select :model-value="String(form.streamBufferKb)" :options="streamBufferOptions" class="w-40" @update:model-value="setStreamBuffer" />
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">一键压制</h2>
      <div class="space-y-1.5">
        <Label>720P 码率（Mbps）</Label>
        <Input
          type="number"
          :min="1"
          :max="100"
          :step="0.01"
          :model-value="String(form.streaming720pBitrateMbps)"
          class="w-32"
          @update:model-value="setBitrate('streaming720pBitrateMbps', $event)"
        />
        <p class="text-xs text-muted-foreground">720p 及以下源文件的瞬时码率上限（峰值上限，非平均码率）。实际码率由画质（CRF，默认 28）决定，不会超过此值；范围 1-100，保留两位小数（默认 3）</p>
      </div>
      <Separator />
      <div class="space-y-1.5">
        <Label>1080P+ 码率（Mbps）</Label>
        <Input
          type="number"
          :min="1"
          :max="100"
          :step="0.01"
          :model-value="String(form.streaming1080pBitrateMbps)"
          class="w-32"
          @update:model-value="setBitrate('streaming1080pBitrateMbps', $event)"
        />
        <p class="text-xs text-muted-foreground">高于 720p 源文件（含降采样到 1080p）的瞬时码率上限（峰值上限，非平均码率）。实际码率由画质（CRF，默认 28）决定，不会超过此值；范围 1-100，保留两位小数（默认 6）</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">其他</h2>
      <div class="flex flex-wrap gap-2">
        <Button variant="outline" @click="openDir">
          <FolderOpen class="h-4 w-4" />
          打开配置目录
        </Button>
        <Button variant="destructive" @click="reset">
          <RotateCcw class="h-4 w-4" />
          恢复默认设置
        </Button>
      </div>
    </Card>
  </div>
</template>
