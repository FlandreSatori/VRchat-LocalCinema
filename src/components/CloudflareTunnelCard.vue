<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { Play, Square, AlertTriangle, Cloud, Copy, Check, Download, ExternalLink, Loader2 } from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import Button from './ui/Button.vue'
import Badge from './ui/Badge.vue'
import Input from './ui/Input.vue'
import {
  getTunnelStatus,
  startTunnel,
  stopTunnel,
  getComponentsStatus,
  downloadComponent,
  openComponentPage,
  onComponentDownloadProgress,
  openUrl,
  type ComponentStatus,
  type ComponentDownloadProgress,
  type UnlistenFn,
} from '../lib/api'
import { largeMediaItems, tunnelStore as tunnel, applyTunnelStatus, frpStore } from '../lib/store'
import { confirmDialog } from '../lib/confirm'
import { toast, errMsg } from '../lib/toast'

const tunnelOperating = ref<'' | 'start' | 'stop'>('')
const tunnelCopied = ref(false)
const cloudflared = ref<ComponentStatus | null>(null)
const cfInstalling = ref(false)
const cfProgress = ref<ComponentDownloadProgress | null>(null)
let unlistenCf: UnlistenFn | null = null

const badge = computed(() => {
  if (tunnel.running) {
    return {
      variant: tunnel.ready ? ('success' as const) : ('warning' as const),
      text: tunnel.ready ? '已连接' : '连接中…',
    }
  }
  if (tunnel.error) return { variant: 'destructive' as const, text: '错误' }
  return { variant: 'secondary' as const, text: '已停止' }
})

const missing = computed(() => cloudflared.value !== null && !cloudflared.value.installed)

const progressText = computed(() => {
  const p = cfProgress.value
  if (!p) return ''
  if (!p.total) return `已下载 ${(p.downloaded / 1048576).toFixed(1)} MB`
  const pct = Math.min(100, Math.round((p.downloaded / p.total) * 100))
  return `已下载 ${(p.downloaded / 1048576).toFixed(1)} / ${(p.total / 1048576).toFixed(1)} MB（${pct}%）`
})

async function loadCloudflared() {
  try {
    const list = await getComponentsStatus()
    cloudflared.value = list.find((c) => c.kind === 'cloudflared') ?? null
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function operate(kind: 'start' | 'stop') {
  if (tunnelOperating.value) return
  if (kind === 'start') {
    const count = largeMediaItems.value.length
    if (
      count &&
      !(await confirmDialog({
        title: '启动公网隧道',
        description: `媒体库中有 ${count} 个 2 GB 以上视频。公网播放受本机上传带宽影响，建议分享前压制为流媒体 MP4。仍要启动隧道吗？`,
        confirmText: '仍然启动',
      }))
    )
      return
  }
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

async function copyUrl() {
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

async function openInBrowser() {
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

onMounted(async () => {
  loadCloudflared()
  getTunnelStatus().then(applyTunnelStatus).catch(() => {})
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
  <section class="space-y-4">
    <div class="flex items-center justify-between gap-4">
      <div class="flex items-center gap-2">
        <Cloud class="h-4 w-4" />
        <h3 class="font-medium">Cloudflare 快速隧道</h3>
        <Badge :variant="badge.variant">{{ badge.text }}</Badge>
      </div>
      <div class="flex gap-2">
        <Button v-if="!tunnel.running" :disabled="tunnelOperating !== '' || missing || frpStore.running" @click="operate('start')">
          <Play class="h-4 w-4" />
          {{ tunnelOperating === 'start' ? '启动中…' : '启动隧道' }}
        </Button>
        <Button v-else variant="destructive" :disabled="tunnelOperating !== ''" @click="operate('stop')">
          <Square class="h-4 w-4" />
          {{ tunnelOperating === 'stop' ? '停止中…' : '停止隧道' }}
        </Button>
      </div>
    </div>

    <p class="text-xs text-muted-foreground">请搭配魔法使用。</p>

    <!-- 公网隧道二选一 -->
    <p v-if="frpStore.running" class="text-xs text-amber-600 dark:text-amber-400">
      frp 隧道正在运行
    </p>

    <!-- 缺少 cloudflared：引导下载 -->
    <div
      v-if="missing"
      class="flex flex-wrap items-center gap-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400"
    >
      <AlertTriangle class="h-4 w-4 shrink-0" />
      <span class="flex-1">
        未检测到 cloudflared（缺少 {{ cloudflared?.missing.join(', ') }}），需先下载后才能启动隧道。
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
    <p v-if="progressText" class="text-xs text-muted-foreground">{{ progressText }}</p>

    <!-- 运行中：展示公网地址 -->
    <div v-if="tunnel.running">
      <template v-if="tunnel.ready && tunnel.url">
        <div class="flex gap-2">
          <Input :model-value="tunnel.url" readonly class="flex-1 font-mono text-xs" />
          <Button variant="outline" size="sm" class="h-9 shrink-0 px-3" @click="openInBrowser">
            <ExternalLink class="h-4 w-4" />
          </Button>
          <Button size="sm" class="h-9 shrink-0 px-3" @click="copyUrl">
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
    <p
      v-if="tunnel.error"
      class="max-h-48 overflow-y-auto whitespace-pre-wrap break-all text-xs text-destructive"
    >{{ tunnel.error }}</p>
  </section>
</template>
