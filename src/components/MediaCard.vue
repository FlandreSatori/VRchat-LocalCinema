<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  Film,
  Link,
  ChevronDown,
  Copy,
  Play,
  MonitorPlay,
  FolderOpen,
  RefreshCw,
  Image as ImageIcon,
  Trash2,
  Clock,
} from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { open } from '@tauri-apps/plugin-dialog'
import ContextMenu from './ui/ContextMenu.vue'
import DropdownMenu from './ui/DropdownMenu.vue'
import Button from './ui/Button.vue'
import Badge from './ui/Badge.vue'
import type { MenuEntry } from '../lib/menu'
import {
  buildMediaUrl,
  openInPlayer,
  openWithSystemDefault,
  openInFolder,
  refreshMedia,
  setCustomThumbnail,
  removeMedia,
  type MediaItem,
  type UrlMode,
} from '../lib/api'
import { toast, errMsg } from '../lib/toast'
import { formatSize, formatDuration } from '../lib/format'
import { serverStore } from '../lib/store'

const props = defineProps<{
  item: MediaItem
}>()

const emit = defineEmits<{
  'copy-link': [item: MediaItem]
  removed: [id: string]
}>()

const imgError = ref(false)

watch(
  () => [props.item.id, props.item.thumbnail],
  () => {
    imgError.value = false
  }
)

const thumbUrl = computed(() =>
  props.item.thumbnail ? `http://127.0.0.1:${serverStore.port}/thumb/${props.item.thumbnail}` : null
)

const showImage = computed(() => thumbUrl.value !== null && !imgError.value)

async function copyUrl(mode: UrlMode, label: string) {
  try {
    const url = await buildMediaUrl(props.item.id, mode)
    await writeText(url)
    toast.success(`已复制${label}`)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openDefault() {
  try {
    await openWithSystemDefault(props.item.id)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openPlayer() {
  try {
    await openInPlayer(props.item.id)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openFolder() {
  try {
    await openInFolder(props.item.id)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function doRefresh() {
  try {
    await refreshMedia(props.item.id)
    toast.info('正在刷新媒体信息')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function changeThumbnail() {
  try {
    const selected = await open({
      multiple: false,
      filters: [{ name: '图片文件', extensions: ['jpg', 'jpeg', 'png', 'webp'] }],
    })
    if (typeof selected === 'string') {
      await setCustomThumbnail(props.item.id, selected)
      toast.success('封面已更新')
    }
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function doRemove() {
  if (!confirm(`确定从媒体库移除「${props.item.title}」吗？\n仅移除记录，不会删除磁盘文件。`)) return
  try {
    await removeMedia(props.item.id)
    emit('removed', props.item.id)
    toast.success('已从媒体库移除')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

const menuItems = computed<MenuEntry[]>(() => [
  { type: 'item', label: '复制本机 URL', icon: Copy, onSelect: () => copyUrl('localhost', '本机 URL') },
  { type: 'item', label: '复制局域网 URL', icon: Copy, onSelect: () => copyUrl('lan', '局域网 URL') },
  { type: 'separator' },
  { type: 'item', label: '系统默认播放器打开', icon: Play, onSelect: openDefault },
  { type: 'item', label: '使用指定播放器打开', icon: MonitorPlay, onSelect: openPlayer },
  { type: 'item', label: '打开文件所在目录', icon: FolderOpen, onSelect: openFolder },
  { type: 'separator' },
  { type: 'item', label: '刷新媒体信息', icon: RefreshCw, onSelect: doRefresh },
  { type: 'item', label: '更换封面', icon: ImageIcon, onSelect: changeThumbnail },
  { type: 'separator' },
  { type: 'item', label: '从媒体库移除', icon: Trash2, danger: true, onSelect: doRemove },
])

const quickCopyItems = computed<MenuEntry[]>(() => [
  { type: 'item', label: '本地模式 URL', onSelect: () => copyUrl('localhost', '本机 URL') },
  { type: 'item', label: '局域网模式 URL', onSelect: () => copyUrl('lan', '局域网 URL') },
])
</script>

<template>
  <ContextMenu :items="menuItems">
    <div class="group overflow-hidden rounded-lg border border-border bg-card shadow-sm transition-colors hover:border-primary/40">
      <div class="relative aspect-video w-full overflow-hidden bg-zinc-900">
        <img
          v-if="showImage"
          :src="thumbUrl!"
          :alt="item.title"
          class="h-full w-full object-cover"
          loading="lazy"
          @error="imgError = true"
        />
        <div v-else class="flex h-full w-full items-center justify-center text-zinc-600">
          <Film class="h-10 w-10" />
        </div>
        <Badge v-if="item.status === 'processing'" variant="warning" class="absolute right-2 top-2">处理中</Badge>
        <Badge v-else-if="item.status === 'missing'" variant="destructive" class="absolute right-2 top-2">文件丢失</Badge>
        <span
          v-if="item.durationMs !== null"
          class="absolute bottom-2 right-2 rounded bg-black/70 px-1.5 py-0.5 text-xs text-white"
        >
          {{ formatDuration(item.durationMs) }}
        </span>
      </div>
      <div class="p-3">
        <div class="truncate text-sm font-medium" :title="item.title">{{ item.title }}</div>
        <div class="mt-1 flex items-center gap-2 text-xs text-muted-foreground">
          <span class="flex items-center gap-1">
            <Clock class="h-3 w-3" />
            {{ formatDuration(item.durationMs) }}
          </span>
          <span>·</span>
          <span>{{ formatSize(item.fileSize) }}</span>
          <span>·</span>
          <span class="uppercase">{{ item.format }}</span>
        </div>
        <div class="mt-3 flex gap-1.5">
          <Button size="sm" class="flex-1" @click="emit('copy-link', item)">
            <Link class="h-3.5 w-3.5" />
            复制播放链接
          </Button>
          <DropdownMenu :items="quickCopyItems" align="end">
            <Button size="sm" variant="outline" class="px-2" title="快速复制">
              <ChevronDown class="h-3.5 w-3.5" />
            </Button>
          </DropdownMenu>
        </div>
      </div>
    </div>
  </ContextMenu>
</template>
