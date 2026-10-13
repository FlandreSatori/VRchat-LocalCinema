<script setup lang="ts">
import { computed, ref } from 'vue'
import { Film, Plus, Search, AlertTriangle, Loader2 } from 'lucide-vue-next'
import { open } from '@tauri-apps/plugin-dialog'
import Button from '../components/ui/Button.vue'
import Input from '../components/ui/Input.vue'
import Select from '../components/ui/Select.vue'
import MediaCard from '../components/MediaCard.vue'
import UrlDialog from '../components/UrlDialog.vue'
import type { MediaItem } from '../lib/api'
import { mediaStore, serverStore, importMediaPaths, removeMediaItem } from '../lib/store'
import { toast, errMsg } from '../lib/toast'

const search = ref('')
const sortBy = ref<'addedAt' | 'title' | 'fileSize' | 'durationMs'>('addedAt')
const sortOrder = ref<'asc' | 'desc'>('desc')
const statusFilter = ref<'all' | 'ok' | 'missing'>('all')
const importing = ref(false)

const urlDialogOpen = ref(false)
const urlDialogItem = ref<MediaItem | null>(null)

const sortOptions = [
  { value: 'addedAt', label: '添加时间' },
  { value: 'title', label: '名称' },
  { value: 'fileSize', label: '大小' },
  { value: 'durationMs', label: '时长' },
]
const orderOptions = [
  { value: 'desc', label: '降序' },
  { value: 'asc', label: '升序' },
]
const statusOptions = [
  { value: 'all', label: '全部状态' },
  { value: 'ok', label: '正常' },
  { value: 'missing', label: '失效' },
]

const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  let list = mediaStore.items
  if (statusFilter.value !== 'all') {
    list = list.filter((m) => m.status === statusFilter.value)
  }
  if (q) {
    list = list.filter((m) => m.title.toLowerCase().includes(q) || m.fileName.toLowerCase().includes(q))
  }
  const dir = sortOrder.value === 'asc' ? 1 : -1
  return [...list].sort((a, b) => {
    let cmp = 0
    switch (sortBy.value) {
      case 'title':
        cmp = a.title.localeCompare(b.title, 'zh-CN')
        break
      case 'fileSize':
        cmp = a.fileSize - b.fileSize
        break
      case 'durationMs':
        cmp = (a.durationMs ?? -1) - (b.durationMs ?? -1)
        break
      default:
        cmp = a.addedAt - b.addedAt
    }
    return cmp * dir
  })
})

function setSortBy(v: string) {
  sortBy.value = v as typeof sortBy.value
}
function setSortOrder(v: string) {
  sortOrder.value = v as typeof sortOrder.value
}
function setStatusFilter(v: string) {
  statusFilter.value = v as typeof statusFilter.value
}

async function doImport(paths: string[]) {
  if (!paths.length || importing.value) return
  importing.value = true
  try {
    const items = await importMediaPaths(paths)
    toast.success(`已导入 ${items.length} 个视频`)
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    importing.value = false
  }
}

async function importViaDialog() {
  try {
    const selected = await open({
      multiple: true,
      filters: [{ name: '视频文件', extensions: ['mp4', 'mkv', 'webm', 'mov', 'avi', 'm4v', 'ts', 'flv', 'wmv'] }],
    })
    if (Array.isArray(selected) && selected.length) {
      await doImport(selected)
    } else if (typeof selected === 'string') {
      await doImport([selected])
    }
  } catch (e) {
    toast.error(errMsg(e))
  }
}

function openUrlDialog(item: MediaItem) {
  urlDialogItem.value = item
  urlDialogOpen.value = true
}

function onRemoved(id: string) {
  removeMediaItem(id)
}
</script>

<template>
  <div class="mx-auto max-w-6xl p-6">
    <div class="flex items-center justify-between gap-4">
      <div>
        <h1 class="text-xl font-semibold">媒体库</h1>
        <p class="mt-0.5 text-sm text-muted-foreground">共 {{ mediaStore.items.length }} 个视频</p>
      </div>
      <Button :disabled="importing" @click="importViaDialog">
        <Loader2 v-if="importing" class="h-4 w-4 animate-spin" />
        <Plus v-else class="h-4 w-4" />
        导入视频
      </Button>
    </div>

    <div v-if="!serverStore.running" class="mt-4 flex items-center gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-600 dark:text-amber-400">
      <AlertTriangle class="h-4 w-4 shrink-0" />
      <span>HTTP 服务未运行，播放链接不可用。</span>
      <RouterLink to="/server" class="font-medium underline underline-offset-4 hover:text-amber-700 dark:hover:text-amber-300">
        前往启动
      </RouterLink>
    </div>

    <div class="mt-4 flex flex-wrap items-center gap-2">
      <div class="relative w-64">
        <Search class="absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        <Input v-model="search" placeholder="搜索标题或文件名" class="pl-8" />
      </div>
      <Select :model-value="sortBy" :options="sortOptions" class="w-32" @update:model-value="setSortBy" />
      <Select :model-value="sortOrder" :options="orderOptions" class="w-24" @update:model-value="setSortOrder" />
      <Select :model-value="statusFilter" :options="statusOptions" class="w-28" @update:model-value="setStatusFilter" />
    </div>

    <div v-if="mediaStore.loaded && filtered.length === 0" class="mt-16 flex flex-col items-center justify-center text-center">
      <template v-if="mediaStore.items.length === 0">
        <Film class="h-16 w-16 text-muted-foreground/40" />
        <h2 class="mt-4 text-lg font-medium">媒体库为空</h2>
        <p class="mt-1 text-sm text-muted-foreground">拖入视频文件或点击导入</p>
        <Button class="mt-4" :disabled="importing" @click="importViaDialog">
          <Plus class="h-4 w-4" />
          导入视频
        </Button>
      </template>
      <template v-else>
        <Search class="h-12 w-12 text-muted-foreground/40" />
        <p class="mt-4 text-sm text-muted-foreground">没有匹配的视频</p>
      </template>
    </div>

    <div v-else class="mt-4 grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-4">
      <MediaCard v-for="item in filtered" :key="item.id" :item="item" @copy-link="openUrlDialog" @removed="onRemoved" />
    </div>

    <UrlDialog v-model:open="urlDialogOpen" :item="urlDialogItem" />
  </div>
</template>
