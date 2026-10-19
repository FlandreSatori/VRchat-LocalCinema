<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { Play, Square, RotateCw, RefreshCw, AlertTriangle, Cloud, Copy, Check, Download, ExternalLink, Loader2 } from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import Card from '../components/ui/Card.vue'
import Button from '../components/ui/Button.vue'
import Badge from '../components/ui/Badge.vue'
import Input from '../components/ui/Input.vue'
import {
  startServer,
  stopServer,
  restartServer,
  getRecentLogs,
  getTunnelStatus,
  startTunnel,
  stopTunnel,
  getComponentsStatus,
  downloadComponent,
  openComponentPage,
  onComponentDownloadProgress,
  openUrl,
  type LogEntry,
  type Settings,
  type ComponentStatus,
  type ComponentDownloadProgress,
  type UnlistenFn,
} from '../lib/api'
import { serverStore, applyServerStatus, loadSettings, tunnelStore as tunnel, applyTunnelStatus } from '../lib/store'
import { formatTime } from '../lib/format'
import { toast, errMsg } from '../lib/toast'

const operating = ref<'' | 'start' | 'stop' | 'restart'>('')
const logs = ref<LogEntry[]>([])
const logsLoading = ref(false)
const settings = ref<Settings | null>(null)

// ===== 公网隧道（Cloudflare 快速隧道） =====
const tunnelOperating = ref<'' | 'start' | 'stop'>('')
const tunnelCopied = ref(false)
const cloudflared = ref<ComponentStatus | null>(null)
const cfInstalling = ref(false)
const cfProgress = ref<ComponentDownloadProgress | null>(null)
let unlistenCf: UnlistenFn | null = null

const statusBadge = computed(() => {
  if (serverStore.error && !serverStore.running) return { variant: 'destructive' as const, text: '错误' }
  return serverStore.running
    ? { variant: 'success' as const, text: '运行中' }
    : { variant: 'secondary' as const, text: '已停止' }
})

const listenModeText = computed(() => {
  const s = settings.value
  if (!s) return '-'
  switch (s.listenMode) {
    case 'loopback':
      return '仅本机 (127.0.0.1)'
    case 'lan':
      return '局域网 (0.0.0.0)'
  }
})

async function operate(kind: 'start' | 'stop' | 'restart') {
  if (operating.value) return
  operating.value = kind
  try {
    const status =
      kind === 'start' ? await startServer() : kind === 'stop' ? await stopServer() : await restartServer()
    applyServerStatus(status)
    toast.success(kind === 'start' ? '服务已启动' : kind === 'stop' ? '服务已停止' : '服务已重启')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    operating.value = ''
  }
}

async function loadLogs() {
  logsLoading.value = true
  try {
    logs.value = await getRecentLogs(100)
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    logsLoading.value = false
  }
}

function levelVariant(level: string) {
  switch (level.toUpperCase()) {
    case 'ERROR':
      return 'destructive' as const
    case 'WARN':
      return 'warning' as const
    case 'DEBUG':
    case 'TRACE':
      return 'outline' as const
    default:
      return 'secondary' as const
  }
}

const tunnelBadge = computed(() => {
  if (tunnel.running) {
    return { variant: tunnel.ready ? 'success' as const : 'warning' as const, text: tunnel.ready ? '已连接' : '连接中…' }
  }
  if (tunnel.error) return { variant: 'destructive' as const, text: '错误' }
  return { variant: 'secondary' as const, text: '已停止' }
})

async function loadTunnel() {
  try {
    applyTunnelStatus(await getTunnelStatus())
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function loadCloudflared() {
  try {
    const list = await getComponentsStatus()
    cloudflared.value = list.find((c) => c.kind === 'cloudflared') ?? null
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function operateTunnel(kind: 'start' | 'stop') {
  if (tunnelOperating.value) return
  tunnelOperating.value = kind
  try {
    applyTunnelStatus(kind === 'start' ? await startTunnel() : await stopTunnel())
    toast.success(kind === 'start' ? '隧道启动中，稍候生成公网地址' : '隧道已停止')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    tunnelOperating.value = ''
  }
}

async function copyTunnelUrl() {
  try {
    applyTunnelStatus(await getTunnelStatus())
    if (!tunnel.ready || !tunnel.url) throw new Error('隧道尚未连接，无法复制公网地址')
    await writeText(tunnel.url)
    tunnelCopied.value = true
    toast.success('已复制公网地址')
    setTimeout(() => {
      tunnelCopied.value = false
    }, 1500)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openTunnelInBrowser() {
  if (!tunnel.ready || !tunnel.url) return
  try {
    await openUrl(tunnel.url)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function installCloudflared() {
  if (cfInstalling.value) return
  cfInstalling.value = true
  cfProgress.value = null
  try {
    cloudflared.value = await downloadComponent('cloudflared')
    toast.success('cloudflared 已安装')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    cfInstalling.value = false
    cfProgress.value = null
  }
}

async function openCloudflaredPage() {
  try {
    await openComponentPage('cloudflared')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

const cfProgressText = computed(() => {
  const p = cfProgress.value
  if (!p) return ''
  if (!p.total) return `已下载 ${(p.downloaded / 1048576).toFixed(1)} MB`
  const pct = Math.min(100, Math.round((p.downloaded / p.total) * 100))
  return `已下载 ${(p.downloaded / 1048576).toFixed(1)} / ${(p.total / 1048576).toFixed(1)} MB（${pct}%）`
})

onMounted(async () => {
  try {
    settings.value = await loadSettings()
  } catch {
    settings.value = null
  }
  loadLogs()
  loadTunnel()
  loadCloudflared()
  try {
    const unlisten = await onComponentDownloadProgress((p) => {
      if (p.kind === 'cloudflared') cfProgress.value = p
    })
    if (disposed) unlisten()
    else unlistenCf = unlisten
  } catch (e) {
    if (!disposed) toast.error(errMsg(e))
  }
})

let disposed = false
onUnmounted(() => {
  disposed = true
  unlistenCf?.()
})
</script>

<template>
  <div class="mx-auto max-w-4xl space-y-4 p-6">
    <h1 class="text-xl font-semibold">HTTP 服务</h1>

    <Card class="p-5">
      <div class="flex items-center justify-between">
        <div class="space-y-1.5">
          <div class="flex items-center gap-2">
            <Badge :variant="statusBadge.variant" class="px-3 py-1 text-sm">{{ statusBadge.text }}</Badge>
          </div>
          <p v-if="serverStore.running" class="text-sm text-muted-foreground">
            监听地址 <span class="font-mono text-foreground">{{ serverStore.bindAddr }}</span>
          </p>
          <p v-else class="text-sm text-muted-foreground">端口 {{ serverStore.port }}</p>
          <p v-if="serverStore.error" class="text-sm text-destructive">{{ serverStore.error }}</p>
        </div>
        <div class="flex gap-2">
          <Button :disabled="serverStore.running || operating !== ''" @click="operate('start')">
            <Play class="h-4 w-4" />
            {{ operating === 'start' ? '启动中…' : '启动' }}
          </Button>
          <Button variant="outline" :disabled="!serverStore.running || operating !== ''" @click="operate('restart')">
            <RotateCw class="h-4 w-4" />
            {{ operating === 'restart' ? '重启中…' : '重启' }}
          </Button>
          <Button variant="destructive" :disabled="!serverStore.running || operating !== ''" @click="operate('stop')">
            <Square class="h-4 w-4" />
            {{ operating === 'stop' ? '停止中…' : '停止' }}
          </Button>
        </div>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <div class="flex items-center justify-between gap-4">
        <div class="flex items-center gap-2">
          <Cloud class="h-4 w-4" />
          <h2 class="font-medium">公网隧道（Cloudflare）</h2>
          <Badge :variant="tunnelBadge.variant">{{ tunnelBadge.text }}</Badge>
        </div>
        <div class="flex gap-2">
          <Button
            v-if="!tunnel.running"
            :disabled="tunnelOperating !== '' || (cloudflared !== null && !cloudflared.installed)"
            @click="operateTunnel('start')"
          >
            <Play class="h-4 w-4" />
            {{ tunnelOperating === 'start' ? '启动中…' : '启动隧道' }}
          </Button>
          <Button v-else variant="destructive" :disabled="tunnelOperating !== ''" @click="operateTunnel('stop')">
            <Square class="h-4 w-4" />
            {{ tunnelOperating === 'stop' ? '停止中…' : '停止隧道' }}
          </Button>
        </div>
      </div>

      <p class="text-xs text-muted-foreground">
        无需 Cloudflare 账号与自有域名，即可把本机媒体服务暴露为一个临时公网 HTTPS 地址。
        观看方<strong>无需安装本程序、也无需组网</strong>，直接在 VRChat / 浏览器打开链接即可。
        注意：快速隧道地址每次启动都会变化，且带宽受本机上传限制。
        修改 HTTP 端口或停止 HTTP 服务会自动关闭隧道；需要时请重新开启并分享新链接。
      </p>

      <!-- 缺少 cloudflared：引导下载 -->
      <div
        v-if="cloudflared && !cloudflared.installed"
        class="flex flex-wrap items-center gap-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400"
      >
        <AlertTriangle class="h-4 w-4 shrink-0" />
        <span class="flex-1">
          未检测到 cloudflared（缺少 {{ cloudflared.missing.join(', ') }}），需先下载后才能启动隧道。
        </span>
        <Button size="sm" :disabled="cfInstalling" @click="installCloudflared">
          <Download class="h-4 w-4" />
          {{ cfInstalling ? '下载中…' : '自动下载' }}
        </Button>
        <Button variant="outline" size="sm" @click="openCloudflaredPage">
          <ExternalLink class="h-4 w-4" />
          手动下载
        </Button>
      </div>
      <p v-if="cfProgressText" class="text-xs text-muted-foreground">{{ cfProgressText }}</p>

      <!-- 运行中：展示公网地址 -->
      <div v-if="tunnel.running">
        <template v-if="tunnel.ready && tunnel.url">
          <div class="flex gap-2">
            <Input :model-value="tunnel.url" readonly class="flex-1 font-mono text-xs" />
            <Button variant="outline" size="sm" class="h-9 shrink-0 px-3" @click="openTunnelInBrowser">
              <ExternalLink class="h-4 w-4" />
            </Button>
            <Button size="sm" class="h-9 shrink-0 px-3" @click="copyTunnelUrl">
              <Check v-if="tunnelCopied" class="h-4 w-4" />
              <Copy v-else class="h-4 w-4" />
              {{ tunnelCopied ? '已复制' : '复制' }}
            </Button>
          </div>
          <p class="mt-2 text-xs text-muted-foreground">
            在媒体库对某个视频「复制链接」时选择「公网隧道」，即可得到可直接分享的完整播放链接。
          </p>
        </template>
        <p v-else class="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 class="h-4 w-4 animate-spin" />
          正在申请地址或连接 Cloudflare（最长等待 60 秒）…
        </p>
      </div>
      <p v-if="tunnel.error" class="max-h-48 overflow-y-auto whitespace-pre-wrap break-all text-xs text-destructive">{{ tunnel.error }}</p>
    </Card>

    <Card class="p-5">
      <div class="flex items-start justify-between gap-4">
        <div>
          <h2 class="font-medium">监听模式</h2>
          <p class="mt-1 text-sm text-muted-foreground">当前模式：{{ listenModeText }}</p>
          <p
            v-if="settings?.listenMode === 'lan'"
            class="mt-2 flex items-start gap-1.5 text-sm text-amber-600 dark:text-amber-400"
          >
            <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0" />
            局域网模式允许其他设备访问你媒体库中的视频，请确认网络环境可信
          </p>
        </div>
        <RouterLink to="/settings">
          <Button variant="outline" size="sm">修改监听设置</Button>
        </RouterLink>
      </div>
    </Card>

    <Card class="p-5">
      <div class="flex items-center justify-between">
        <h2 class="font-medium">最近日志</h2>
        <Button variant="ghost" size="sm" :disabled="logsLoading" @click="loadLogs">
          <RefreshCw class="h-4 w-4" :class="logsLoading ? 'animate-spin' : ''" />
          刷新
        </Button>
      </div>
      <div v-if="logs.length === 0" class="mt-4 text-center text-sm text-muted-foreground">暂无日志</div>
      <div v-else class="mt-3 max-h-96 space-y-1 overflow-y-auto font-mono text-xs">
        <div
          v-for="log in logs"
          :key="`${log.ts}·${log.level}·${log.target}·${log.message}`"
          class="flex items-start gap-2 rounded px-1.5 py-1 hover:bg-accent/50"
        >
          <span class="shrink-0 text-muted-foreground">{{ formatTime(log.ts) }}</span>
          <Badge :variant="levelVariant(log.level)" class="shrink-0 px-1.5 py-0 text-[10px]">{{ log.level }}</Badge>
          <span class="break-all leading-snug">{{ log.message }}</span>
        </div>
      </div>
    </Card>
  </div>
</template>
