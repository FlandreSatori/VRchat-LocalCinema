<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { Film, Server, Network, Settings, Info, FilePlus, Minus, Square, Copy, X, ListVideo } from 'lucide-vue-next'
import { isTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import type { UnlistenFn } from '@tauri-apps/api/event'
import brandIcon from '../watching_video.png'
import Toaster from './components/Toaster.vue'
import ThemeToggle from './components/ThemeToggle.vue'
import { APP_BRAND, APP_VERSION } from './lib/appinfo'
import { initStore, serverStore, importMediaPaths } from './lib/store'
import { toast, errMsg } from './lib/toast'

const router = useRouter()
const appWindow = isTauri() ? getCurrentWindow() : null
const maximized = ref(false)

const navItems = [
  { to: '/library', label: '媒体库', icon: Film },
  { to: '/convert', label: '转码任务', icon: ListVideo },
  { to: '/server', label: 'HTTP 服务', icon: Server },
  { to: '/easytier', label: 'EasyTier 组网', icon: Network },
  { to: '/settings', label: '设置', icon: Settings },
  { to: '/about', label: '关于', icon: Info },
]

const dragActive = ref(false)
const importing = ref(false)

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

let unlistenDrag: UnlistenFn | null = null
let unlistenResize: UnlistenFn | null = null
let disposed = false

onMounted(async () => {
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
