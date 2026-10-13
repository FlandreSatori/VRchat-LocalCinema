<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Download, RefreshCw, CheckCircle2, HardDriveDownload, XCircle } from 'lucide-vue-next'
import Dialog from '../components/ui/Dialog.vue'
import Badge from '../components/ui/Badge.vue'
import Button from '../components/ui/Button.vue'
import { fetchPeerMedia, downloadPeerMedia, type MediaSummary, type PeerInfo } from '../lib/api'
import { mediaStore, upsertMedia } from '../lib/store'
import { toast, errMsg } from '../lib/toast'

const props = defineProps<{
  open: boolean
  peer: PeerInfo | null
  port: number
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

const items = ref<MediaSummary[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const downloadingId = ref<string | null>(null)

/** 本地已有的哈希集合，用于判断对方媒体是否已在本地 */
const localHashes = computed(() => {
  const map = new Map<string, string>()
  for (const item of mediaStore.items) {
    if (item.hash) map.set(item.hash, item.title)
  }
  return map
})

function formatSize(bytes: number): string {
  if (!bytes) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let i = 0
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024
    i += 1
  }
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function isLocal(item: MediaSummary): boolean {
  return !!item.hash && localHashes.value.has(item.hash)
}

async function load() {
  const peer = props.peer
  if (!peer || !peer.ipv4) {
    items.value = []
    error.value = '该成员暂无可用的虚拟 IP'
    return
  }
  loading.value = true
  error.value = null
  try {
    items.value = await fetchPeerMedia(peer.ipv4, props.port)
  } catch (e) {
    error.value = errMsg(e)
    items.value = []
  } finally {
    loading.value = false
  }
}

async function download(item: MediaSummary) {
  const peer = props.peer
  if (!peer || downloadingId.value) return
  downloadingId.value = item.id
  try {
    const media = await downloadPeerMedia(peer.ipv4, props.port, item.id, item.fileName)
    upsertMedia(media)
    toast.success(`已下载并导入：${media.title}`)
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    downloadingId.value = null
  }
}

watch(
  () => [props.open, props.peer?.ipv4],
  ([open]) => {
    if (open) load()
  }
)
</script>

<template>
  <Dialog
    :open="open"
    :title="`${peer?.hostname || '成员'} 的媒体库`"
    :description="peer?.ipv4 ? `${peer.ipv4} · 共 ${items.length} 个媒体` : undefined"
    class="max-w-2xl"
    @update:open="emit('update:open', $event)"
  >
    <div class="flex items-center justify-end">
      <Button variant="ghost" size="sm" :disabled="loading" @click="load">
        <RefreshCw class="h-4 w-4" :class="loading ? 'animate-spin' : ''" />
        刷新
      </Button>
    </div>

    <div v-if="loading" class="py-8 text-center text-sm text-muted-foreground">正在读取对方媒体库…</div>
    <div v-else-if="error" class="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm text-destructive">
      <XCircle class="mt-0.5 h-4 w-4 shrink-0" />
      <span>{{ error }}</span>
    </div>
    <div v-else-if="items.length === 0" class="py-8 text-center text-sm text-muted-foreground">对方媒体库为空</div>
    <div v-else class="max-h-[52vh] space-y-2 overflow-y-auto pr-1">
      <div
        v-for="item in items"
        :key="item.id"
        class="flex items-center justify-between gap-3 rounded-md border border-border p-3"
      >
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-medium">{{ item.title }}</p>
          <p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">
            {{ item.fileName }} · {{ formatSize(item.fileSize) }}
          </p>
        </div>
        <Badge v-if="isLocal(item)" variant="success" class="shrink-0">
          <CheckCircle2 class="mr-1 h-3 w-3" />
          本地已有
        </Badge>
        <Button
          v-else
          variant="outline"
          size="sm"
          class="shrink-0"
          :disabled="downloadingId !== null"
          @click="download(item)"
        >
          <HardDriveDownload class="h-4 w-4" />
          {{ downloadingId === item.id ? '下载中…' : '下载' }}
        </Button>
      </div>
    </div>
    <p class="mt-4 flex items-center gap-1.5 text-xs text-muted-foreground">
      <Download class="h-3.5 w-3.5" />
      下载后会保存到本地下载目录并自动导入媒体库
    </p>
  </Dialog>
</template>