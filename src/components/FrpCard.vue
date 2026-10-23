<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import { Network, Play, Square, AlertTriangle, ExternalLink, RefreshCw, Loader2, Terminal, FolderOpen, Download } from 'lucide-vue-next'
import Button from './ui/Button.vue'
import Badge from './ui/Badge.vue'
import Input from './ui/Input.vue'
import Label from './ui/Label.vue'
import Switch from './ui/Switch.vue'
import {
  downloadComponent,
  getAppPaths,
  getComponentsStatus,
  onComponentDownloadProgress,
  openComponentPage,
  openDir,
  openUrl,
  saveSettings,
  startFrp,
  stopFrp,
  readFrpOutput,
  type AppPaths,
  type ComponentDownloadProgress,
  type ComponentStatus,
  type FrpConfig,
  type Settings,
  type UnlistenFn,
} from '../lib/api'
import { frpStore, applyFrpStatus, settingsCache, tunnelStore } from '../lib/store'
import { toast, errMsg } from '../lib/toast'

const config = reactive<FrpConfig>({ launchParams: '', autoStart: false, autoRestart: true })
const frpc = ref<ComponentStatus | null>(null)
const installDir = ref('')
const operating = ref<'' | 'start' | 'stop'>('')
const installing = ref(false)
const refreshing = ref(false)
const downloadProgress = ref<ComponentDownloadProgress | null>(null)
const logsOpen = ref(false)
const logText = ref('')

let synced = false
let logTimer: number | null = null
let unlisten: UnlistenFn | null = null

/// 仅首次拿到设置时灌入初值：之后以本页编辑为准，避免其它页保存设置时覆盖用户正在输入的内容
watch(
  settingsCache,
  (s) => {
    if (!s || synced) return
    synced = true
    Object.assign(config, {
      launchParams: s.frp.launchParams,
      autoStart: s.frp.autoStart,
      autoRestart: s.frp.autoRestart,
    })
  },
  { immediate: true }
)

const badge = computed(() => {
  if (frpStore.running) {
    return {
      variant: frpStore.ready ? ('success' as const) : ('warning' as const),
      text: frpStore.ready ? '已连接' : '连接中…',
    }
  }
  if (frpStore.error) return { variant: 'destructive' as const, text: '错误' }
  return { variant: 'secondary' as const, text: '已停止' }
})

const missing = computed(() => frpc.value !== null && !frpc.value.installed)

const installProgressText = computed(() => {
  const p = downloadProgress.value
  if (!p) return ''
  if (!p.total) return `已下载 ${(p.downloaded / 1048576).toFixed(1)} MB`
  const pct = Math.min(100, Math.round((p.downloaded / p.total) * 100))
  return `已下载 ${(p.downloaded / 1048576).toFixed(1)} / ${(p.total / 1048576).toFixed(1)} MB（${pct}%）`
})

/// 保存 frp 配置；内容未变化时不发起 IPC，避免无谓的设置落盘与服务重配
async function applyConfig(quiet = true) {
  const base = settingsCache.value
  if (!base) return
  const updated: Settings = { ...base, frp: { ...config } }
  if (JSON.stringify(base.frp) === JSON.stringify(updated.frp)) return
  try {
    await saveSettings(updated)
    settingsCache.value = updated
  } catch (e) {
    if (!quiet) toast.error(errMsg(e))
  }
}

/// 重新探测 frpc。手动点击时给出结果反馈，否则按钮看起来像「没反应」
async function loadComponent(notify = false) {
  if (refreshing.value) return
  refreshing.value = true
  try {
    const list = await getComponentsStatus()
    const next = list.find((c) => c.kind === 'frp') ?? null
    frpc.value = next
    if (notify) {
      if (next?.installed) {
        toast.success(`已检测到 ${next.label}${next.version ? `（${next.version}）` : ''}`)
      } else {
        toast.info('仍未检测到 frpc，请确认文件已放入下方目录或在设置中指定路径')
      }
    }
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    refreshing.value = false
  }
}

async function operate(kind: 'start' | 'stop') {
  if (operating.value) return
  operating.value = kind
  try {
    if (kind === 'start') {
      await applyConfig()
      applyFrpStatus(await startFrp())
      toast.success('frp 隧道启动中，稍候确认隧道状态')
      if (logsOpen.value) startPolling()
    } else {
      applyFrpStatus(await stopFrp())
      toast.success('frp 隧道已停止')
      if (logsOpen.value) await loadLog()
    }
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    operating.value = ''
  }
}

async function loadLog() {
  try {
    logText.value = await readFrpOutput()
  } catch {
    // 日志读取失败不影响启停操作，静默处理
  }
}

function startPolling() {
  if (logTimer !== null) return
  logTimer = window.setInterval(() => {
    if (frpStore.running) void loadLog()
  }, 2000)
}

function stopPolling() {
  if (logTimer !== null) {
    window.clearInterval(logTimer)
    logTimer = null
  }
}

function toggleLog() {
  logsOpen.value = !logsOpen.value
  if (logsOpen.value) void loadLog()
}

/// 打开 SakuraFrp 软件下载页（手动下载 frpc）
async function openFrpPage() {
  try {
    await openComponentPage('frp')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

/// 自动从 SakuraFrp 官方镜像下载最新 frpc
async function installFrpc() {
  if (installing.value) return
  installing.value = true
  downloadProgress.value = null
  try {
    frpc.value = await downloadComponent('frp')
    toast.success('frpc 已安装')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    installing.value = false
    downloadProgress.value = null
  }
}

async function openInstallDir() {
  try {
    await openDir(installDir.value)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

/// 打开教程类外链（失败时提示，避免点击无反馈）
async function openLink(url: string) {
  try {
    await openUrl(url)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

// 仅在日志展开且隧道运行时轮询，避免后台无谓的 IPC 开销
watch([logsOpen, () => frpStore.running], () => {
  if (logsOpen.value && frpStore.running) startPolling()
  else stopPolling()
})

onMounted(async () => {
  loadComponent()
  try {
    const paths: AppPaths = await getAppPaths()
    installDir.value = `${paths.componentsDir}\\frp`
  } catch {
    installDir.value = ''
  }
  try {
    const off = await onComponentDownloadProgress((p) => {
      if (p.kind === 'frp') downloadProgress.value = p
    })
    if (disposed) off()
    else unlisten = off
  } catch {
    // 进度订阅失败不影响安装流程本身
  }
})

let disposed = false
onUnmounted(() => {
  disposed = true
  unlisten?.()
  stopPolling()
})
</script>

<template>
  <section class="space-y-4">
    <div class="flex items-center justify-between gap-4">
      <div class="flex items-center gap-2">
        <Network class="h-4 w-4" />
        <h3 class="font-medium">frp 隧道</h3>
        <Badge :variant="badge.variant">{{ badge.text }}</Badge>
        <Badge v-if="frpStore.tunnelCount > 0" variant="outline">{{ frpStore.tunnelCount }} 条隧道</Badge>
      </div>
      <div class="flex gap-2">
        <Button v-if="!frpStore.running" :disabled="operating !== '' || missing || tunnelStore.running" @click="operate('start')">
          <Play class="h-4 w-4" />
          {{ operating === 'start' ? '启动中…' : '启动隧道' }}
        </Button>
        <Button v-else variant="destructive" :disabled="operating !== ''" @click="operate('stop')">
          <Square class="h-4 w-4" />
          {{ operating === 'stop' ? '停止中…' : '停止隧道' }}
        </Button>
      </div>
    </div>

    <!-- 公网隧道二选一 -->
    <p v-if="tunnelStore.running" class="text-xs text-amber-600 dark:text-amber-400">
      Cloudflare 隧道正在运行
    </p>

    <ol class="list-decimal space-y-1 pl-5 text-xs text-muted-foreground">
      <li>
        请<a
          href="https://www.natfrp.com/user/"
          class="text-primary hover:underline"
          @click.prevent="openLink('https://www.natfrp.com/user/')"
        >注册</a>SakuraFrp 账号，签到并复制访问密钥
      </li>
      <li>
        请在<a
          href="https://www.natfrp.com/tunnel/"
          class="text-primary hover:underline"
          @click.prevent="openLink('https://www.natfrp.com/tunnel/')"
        >隧道列表</a>里申请一条隧道
      </li>
      <li>请在下方填写「访问密钥:隧道ID」</li>
      <li>
        你也可以不使用内置的 frp，而是使用<a
          href="https://doc.natfrp.com/launcher/usage.html"
          class="text-primary hover:underline"
          @click.prevent="openLink('https://doc.natfrp.com/launcher/usage.html')"
        >frp 启动器</a>
      </li>
    </ol>

    <!-- 缺少 frpc：引导自动下载或手动放置 -->
    <div
      v-if="missing"
      class="flex flex-wrap items-center gap-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400"
    >
      <AlertTriangle class="h-4 w-4 shrink-0" />
      <span class="flex-1">
        未检测到 frpc（缺少 {{ frpc?.missing.join(', ') }}），需先安装后才能启动隧道。
      </span>
      <Button size="sm" :disabled="installing" @click="installFrpc">
        <Download class="h-4 w-4" />
        {{ installing ? '下载中…' : '自动下载' }}
      </Button>
      <Button variant="outline" size="sm" @click="openFrpPage">
        <ExternalLink class="h-4 w-4" />
        手动下载
      </Button>
      <Button v-if="installDir" variant="outline" size="sm" @click="openInstallDir">
        <FolderOpen class="h-4 w-4" />
        打开目录
      </Button>
      <Button variant="outline" size="sm" :disabled="refreshing" @click="loadComponent(true)">
        <RefreshCw class="h-4 w-4" :class="refreshing ? 'animate-spin' : ''" />
        {{ refreshing ? '检测中…' : '重新检测' }}
      </Button>
    </div>
    <p v-if="installProgressText" class="text-xs text-muted-foreground">{{ installProgressText }}</p>
    <p v-if="missing && installDir" class="text-xs text-muted-foreground">
      手动下载的 frpc 请放到 <span class="font-mono">{{ installDir }}</span>
      目录（或在「设置 · 内置组件」中指定路径），完成后点「重新检测」。
    </p>

    <div class="space-y-2">
      <Label>启动参数</Label>
      <Input
        v-model="config.launchParams"
        placeholder="wdnmdtoken666666:114514"
        :disabled="frpStore.running"
        @blur="applyConfig()"
      />
    </div>

    <div class="flex flex-wrap items-center gap-x-8 gap-y-2">
      <div class="flex items-center gap-2">
        <Switch v-model:checked="config.autoStart" @update:checked="applyConfig()" />
        <Label>随应用启动</Label>
      </div>
      <div class="flex items-center gap-2">
        <Switch v-model:checked="config.autoRestart" @update:checked="applyConfig()" />
        <Label>断线自动重连</Label>
      </div>
    </div>

    <!-- 运行中：展示隧道概况 -->
    <p v-if="frpStore.running" class="flex items-center gap-2 text-sm text-muted-foreground">
      <Loader2 v-if="!frpStore.ready" class="h-4 w-4 animate-spin" />
      <template v-if="frpStore.ready">
        {{ frpStore.tunnelCount }} 条隧道已建立，公网地址请在服务面板查看
      </template>
      <template v-else>正在登录 frp 服务器并建立隧道（最长等待 60 秒）…</template>
    </p>
    <p v-if="frpStore.params" class="text-xs text-muted-foreground">
      当前参数 <span class="font-mono text-foreground">{{ frpStore.params }}</span>
    </p>

    <p
      v-if="frpStore.error"
      class="max-h-48 overflow-y-auto whitespace-pre-wrap break-all text-xs text-destructive"
    >{{ frpStore.error }}</p>

    <div>
      <Button variant="ghost" size="sm" @click="toggleLog">
        <Terminal class="h-4 w-4" />
        {{ logsOpen ? '收起运行日志' : '查看运行日志' }}
      </Button>
      <pre
        v-if="logsOpen"
        class="mt-2 max-h-72 overflow-auto rounded-md bg-muted/50 px-3 py-2 font-mono text-xs leading-relaxed whitespace-pre-wrap break-all"
      >{{ logText || '暂无输出' }}</pre>
    </div>
  </section>
</template>
