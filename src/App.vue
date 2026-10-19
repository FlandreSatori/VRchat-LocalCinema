<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { Film, Server, Network, Settings, Info, FilePlus, Minus, Square, Copy, X, ListVideo, MessageSquare, TriangleAlert } from 'lucide-vue-next'
import { isTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type { UnlistenFn } from '@tauri-apps/api/event'
import brandIcon from '../watching_video.png'
import Toaster from './components/Toaster.vue'
import ConfirmHost from './components/ConfirmHost.vue'
import DisclaimerDialog from './components/DisclaimerDialog.vue'
import Dialog from './components/ui/Dialog.vue'
import Button from './components/ui/Button.vue'
import ThemeToggle from './components/ThemeToggle.vue'
import { APP_BRAND, APP_VERSION } from './lib/appinfo'
import { initStore, serverStore, mediaStore, importMediaPaths, faststartHintState, transcodeStreamingItem } from './lib/store'
import { formatSize } from './lib/format'
import { toast, errMsg } from './lib/toast'
import { isFirstLaunch, markDisclaimerAccepted, isRunningInTempDir, quitApp, type MediaItem } from './lib/api'

const router = useRouter()
const appWindow = isTauri() ? getCurrentWindow() : null
const maximized = ref(false)

const navItems = [
  { to: '/library', label: '媒体库', icon: Film },
  { to: '/convert', label: '任务', icon: ListVideo },
  { to: '/server', label: 'HTTP 服务', icon: Server },
  { to: '/easytier', label: 'EasyTier 组网', icon: Network },
  { to: '/settings', label: '设置', icon: Settings },
  { to: '/feedback', label: '反馈', icon: MessageSquare },
  { to: '/about', label: '关于', icon: Info },
]

const dragActive = ref(false)
const importing = ref(false)
const showDisclaimer = ref(false)
const isFirstLaunchFlag = ref(false)
/// 运行在系统临时目录（如直接在压缩包内运行）时的强制警告
const tempDirWarning = ref(false)

const statusDotClass = computed(() => {
  if (serverStore.error && !serverStore.running) return 'bg-red-500'
  return serverStore.running ? 'bg-emerald-500' : 'bg-zinc-400'
})

const statusText = computed(() => {
  if (serverStore.error && !serverStore.running) return '服务错误'
  if (serverStore.running) return `运行中 · ${serverStore.bindAddr}`
  return '服务已停止'
})

async function windowAction(action: 'minimize' | 'toggleMaximize' | 'close' | 'startDragging') {
  if (!appWindow) return
  try {
    await appWindow[action]()
    if (action === 'toggleMaximize') await syncMaximized()
  } catch (e) {
    toast.error(errMsg(e))
  }
}

function onTitlebarMouseDown(event: MouseEvent) {
  if (event.button !== 0 || (event.target as HTMLElement).closest('button')) return
  void windowAction(event.detail === 2 ? 'toggleMaximize' : 'startDragging')
}

async function syncMaximized() {
  if (appWindow) maximized.value = await appWindow.isMaximized()
}

async function handleDrop(paths: string[]) {
  if (!paths.length || importing.value) return
  importing.value = true
  try {
    const items = await importMediaPaths(paths)
    toast.success(`已导入 ${items.length} 个视频`)
    router.push('/library')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    importing.value = false
  }
}

async function handleDisclaimerAgree() {
  try {
    await markDisclaimerAccepted()
    isFirstLaunchFlag.value = false
  } catch (e) {
    console.error('标记免责声明失败:', e)
    toast.error(errMsg(e))
  }
}

/// 临时目录警告对话框：强制模式，只能确认退出
function onTempDirWarningUpdate(open: boolean) {
  if (!open) return
  tempDirWarning.value = open
}

async function quitFromTempWarning() {
  try {
    await quitApp()
  } catch (e) {
    toast.error(errMsg(e))
  }
}

// 导入含 ≥2GB 大文件的批次时提示可一键压制
const compressingId = ref('')
async function compressLargeItem(item: MediaItem) {
  if (compressingId.value) return
  compressingId.value = item.id
  try {
    await transcodeStreamingItem(item)
    toast.info('已加入压制队列，完成后请使用新媒体卡片')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    compressingId.value = ''
  }
}

watch(() => faststartHintState.batch, () => {
  if (faststartHintState.batch === 0 || !faststartHintState.largeItems.length) return
  faststartHintState.dialogOpen = true
})

function onImportDialogUpdate(open: boolean) {
  faststartHintState.dialogOpen = open
}

let unlistenDrag: UnlistenFn | null = null
let unlistenResize: UnlistenFn | null = null
let disposed = false

// 禁用 WebView 原生右键菜单（返回/前进等），右键菜单位置由应用自定义菜单接管；
// 文本输入框内保留原生菜单，方便粘贴
function onContextMenu(e: MouseEvent) {
  const el = e.target as HTMLElement | null
  if (el?.closest('input, textarea, [contenteditable="true"]')) return
  e.preventDefault()
}

onMounted(async () => {
  document.addEventListener('contextmenu', onContextMenu)

  // 运行在临时目录（如直接在压缩包里双击运行）时警告并退出：随包组件可能缺失、临时路径会被系统清理
  try {
    if (await isRunningInTempDir()) {
      tempDirWarning.value = true
      return
    }
  } catch {
    // 非 Tauri 环境（如纯浏览器预览）下忽略
  }

  if (appWindow) {
    try {
      await syncMaximized()
      const unlisten = await appWindow.onResized(() => {
        void syncMaximized().catch((e) => toast.error(errMsg(e)))
      })
      if (disposed) unlisten()
      else unlistenResize = unlisten
    } catch (e) {
      toast.error(errMsg(e))
    }
  }
  
  // 检查是否首次启动
  try {
    const firstLaunch = await isFirstLaunch()
    if (firstLaunch) {
      isFirstLaunchFlag.value = true
      showDisclaimer.value = true
    }
  } catch (e) {
    // 检查失败不阻止应用启动
    console.error('检查首次启动状态失败:', e)
  }
  
  try {
    await initStore()
  } catch (e) {
    toast.error(errMsg(e))
  }
  try {
    const unlisten = await getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload
      if (payload.type === 'enter') {
        // 仅外部文件拖拽显示导入遮罩；窗口内部拖动（如媒体卡片）paths 为空
        dragActive.value = payload.paths.length > 0
      } else if (payload.type === 'over') {
        // 保持 enter 时的状态（over 事件不携带 paths）
      } else if (payload.type === 'leave') {
        dragActive.value = false
      } else if (payload.type === 'drop') {
        dragActive.value = false
        if (payload.paths.length) handleDrop(payload.paths)
      }
    })
    if (disposed) unlisten()
    else unlistenDrag = unlisten
  } catch {
    // 非 Tauri 环境（如纯浏览器预览）下忽略
  }
})

onUnmounted(() => {
  disposed = true
  document.removeEventListener('contextmenu', onContextMenu)
  unlistenDrag?.()
  unlistenResize?.()
})
</script>

<template>
  <div class="flex h-screen flex-col overflow-hidden bg-background text-foreground">
    <header
      class="flex h-10 shrink-0 select-none items-center border-b border-border"
      @mousedown="onTitlebarMouseDown"
    >
      <div class="flex h-full w-[220px] shrink-0 items-center gap-2 border-r border-border bg-card px-4">
        <img :src="brandIcon" alt="" draggable="false" class="pointer-events-none h-6 w-6 object-contain" />
        <span class="pointer-events-none truncate text-xs font-semibold tracking-tight">{{ APP_BRAND }} <span class="font-normal text-muted-foreground">v{{ APP_VERSION }}</span></span>
      </div>
      <div class="h-full flex-1" />
      <div v-if="appWindow" class="flex h-full shrink-0">
        <button
          type="button"
          aria-label="最小化"
          title="最小化"
          class="flex h-full w-12 items-center justify-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          @click="windowAction('minimize')"
        >
          <Minus class="h-4 w-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          :aria-label="maximized ? '还原' : '最大化'"
          :title="maximized ? '还原' : '最大化'"
          class="flex h-full w-12 items-center justify-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          @click="windowAction('toggleMaximize')"
        >
          <component :is="maximized ? Copy : Square" class="h-3.5 w-3.5" aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label="关闭"
          title="关闭"
          class="flex h-full w-12 items-center justify-center text-muted-foreground transition-colors hover:bg-destructive hover:text-destructive-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          @click="windowAction('close')"
        >
          <X class="h-4 w-4" aria-hidden="true" />
        </button>
      </div>
    </header>

    <div class="flex min-h-0 flex-1">
      <aside class="flex w-[220px] shrink-0 flex-col border-r border-border bg-card">
        <nav class="flex-1 space-y-1 px-2 pt-3">
          <RouterLink
            v-for="item in navItems"
            :key="item.to"
            :to="item.to"
            class="flex items-center gap-3 rounded-md px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            active-class="bg-accent font-medium text-foreground"
          >
            <component :is="item.icon" class="h-4 w-4" />
            {{ item.label }}
          </RouterLink>
        </nav>
        <div class="space-y-2 border-t border-border p-3">
          <div class="flex items-center gap-2 px-1 text-xs text-muted-foreground" :title="serverStore.error ?? ''">
            <span class="h-2 w-2 shrink-0 rounded-full" :class="statusDotClass" />
            <span class="truncate">{{ statusText }}</span>
          </div>
          <ThemeToggle />
        </div>
      </aside>

      <main class="min-w-0 flex-1 overflow-y-auto">
        <RouterView />
      </main>
    </div>

    <Toaster />
    <ConfirmHost />

    <!-- 运行在系统临时目录（如直接在压缩包内运行）时的强制警告：确认后退出 -->
    <Dialog
      :open="tempDirWarning"
      title="检测到程序运行在临时目录"
      description="请先把压缩包完整解压到磁盘上的固定目录，再运行本程序"
      @update:open="onTempDirWarningUpdate"
    >
      <div class="space-y-4">
        <div class="flex items-start gap-2 rounded-md border border-amber-600/40 bg-amber-600/10 px-3 py-2 text-xs leading-relaxed text-amber-600">
          <TriangleAlert class="mt-0.5 h-4 w-4 shrink-0" />
          <span>临时目录中的文件会被系统随时清理。在这里运行时，随包的组件（FFmpeg、EasyTier 等）可能没有被一起解压出来，转码、组网与公网隧道将无法使用；若启用了开机启动，也会指向这个会被清理的临时路径。</span>
        </div>
        <div class="rounded-md border border-border bg-muted/40 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
          请将压缩包完整解压到磁盘上的固定目录（例如 D:\VRchat-LocalCinema），确认目录中含有 components 文件夹后再运行本程序。
        </div>
        <div class="flex justify-end gap-2">
          <Button @click="quitFromTempWarning">退出软件</Button>
        </div>
      </div>
    </Dialog>

    <DisclaimerDialog
      v-model:open="showDisclaimer"
      :enforced="isFirstLaunchFlag"
      @agree="handleDisclaimerAgree"
    />

    <!-- 导入含 ≥2GB 大文件时的压制提示 -->
    <Dialog
      :open="faststartHintState.dialogOpen"
      title="发现大文件"
      :description="`检测到 ${faststartHintState.largeItems.length} 个 ≥2GB 的视频，可压制为适合网络播放的流媒体 MP4`"
      @update:open="onImportDialogUpdate"
    >
      <div class="space-y-4">
        <div class="rounded-md border border-border bg-muted/40 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
          压制为 AV1 + AAC 的 MP4：保持原始分辨率（仅高于 1080P 时降到 1080P），原文件保留，压制结果作为新卡片加入媒体库。码率上限与画质可在「设置 · 一键压制」中调整。
        </div>
        <div class="max-h-56 space-y-2 overflow-y-auto rounded-md border border-border p-3">
          <div v-for="item in faststartHintState.largeItems" :key="item.id" class="flex items-center justify-between gap-3">
            <span class="min-w-0 truncate text-sm">{{ item.title }}<span class="text-muted-foreground"> · {{ formatSize(item.fileSize) }}</span></span>
            <Button size="sm" :disabled="compressingId !== ''" @click="compressLargeItem(item)">一键压制</Button>
          </div>
        </div>
        <div class="flex justify-end gap-2">
          <Button variant="outline" @click="onImportDialogUpdate(false)">稍后处理</Button>
        </div>
      </div>
    </Dialog>

    <div
      v-if="dragActive"
      class="pointer-events-none fixed inset-0 z-[90] flex items-center justify-center bg-background/70 backdrop-blur-sm"
    >
      <div class="flex items-center gap-3 rounded-lg border-2 border-dashed border-primary bg-card px-8 py-6 text-lg text-primary">
        <FilePlus class="h-6 w-6" />
        松开以导入视频 / 视频文件夹
      </div>
    </div>
  </div>
</template>
