<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Download, RefreshCw, CheckCircle2, HardDriveDownload, Loader2, XCircle } from 'lucide-vue-next'
import Dialog from '../components/ui/Dialog.vue'
import Badge from '../components/ui/Badge.vue'
import Button from '../components/ui/Button.vue'
import { fetchPeerMedia, downloadPeerMediaTask, testPeerConnection, type MediaSummary, type PeerInfo } from '../lib/api'
import { mediaStore, convertTaskStore } from '../lib/store'
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

/** 本地已有的内容哈希集合：优先按内容比对 */
const localByHash = computed(() => {
  const set = new Set<string>()
  for (const item of mediaStore.items) {
    if (item.hash) set.add(item.hash)
  }
  return set
})

/** 本地已有的「文件名|大小」集合：哈希尚未算出（探测未完成）时的兜底比对 */
const localByNameSize = computed(() => {
  const set = new Set<string>()
  for (const item of mediaStore.items) {
    set.add(`${item.fileName.toLowerCase()}|${item.fileSize}`)
  }
  return set
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
  // 按内容哈希比对；哈希缺失时按 文件名 + 文件大小 兜底
  if (item.hash && localByHash.value.has(item.hash)) return true
  return localByNameSize.value.has(`${item.fileName.toLowerCase()}|${item.fileSize}`)
}

/** 该条目是否有进行中/已暂停的下载任务（任务页可见） */
function activeDownloadState(item: MediaSummary): string | null {
  const t = convertTaskStore.tasks.find(
    (t) =>
      t.kind === 'download' &&
      t.peerMediaId === item.id &&
      (t.state === 'queued' || t.state === 'running' || t.state === 'paused')
  )
  return t?.state ?? null
}

/** 请求失败时做 ping/TCP 连通性检测，区分「网络不可达」与「对方未开放查看」 */
async function describeConnectError(ip: string, port: number): Promise<string> {
  try {
    const r = await testPeerConnection(ip, port)
    if (!r.tcp) {
      return r.ping
        ? '对方不允许你查看媒体库（对方未开启「允许其他人查看我的媒体库」，或双方端口不一致）'
        : '无法连接（对方不在线或网络不可达）'
    }
  } catch {
    // 非 Tauri 环境或检测失败，回退到通用提示
  }
  return '对方媒体库不存在（对方可能未运行本软件或未启动 HTTP 服务）'
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
  } catch {
    // 请求失败：进一步做 ping/TCP 检测，给出精确原因
    items.value = []
    error.value = await describeConnectError(peer.ipv4, props.port)
  } finally {
    loading.value = false
  }
}

async function download(item: MediaSummary) {
  const peer = props.peer
  if (!peer || !peer.ipv4) return
  try {
    await downloadPeerMediaTask({
      ip: peer.ipv4,
      port: props.port,
      id: item.id,
      fileName: item.fileName,
      fileSize: item.fileSize,
      title: item.title,
      hash: item.hash ?? undefined,
    })
    toast.success('已加入下载队列')
  } catch (e: any) {
    toast.error(errMsg(e))
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
        <Badge v-else-if="activeDownloadState(item)" variant="secondary" class="shrink-0">
          <Loader2 v-if="activeDownloadState(item) === 'running'" class="mr-1 h-3 w-3 animate-spin" />
          {{ activeDownloadState(item) === 'paused' ? '已暂停' : '下载中' }}
        </Badge>
        <Button v-else variant="outline" size="sm" class="shrink-0" @click="download(item)">
          <HardDriveDownload class="h-4 w-4" />
          下载
        </Button>
      </div>
    </div>
    <p class="mt-4 flex items-center gap-1.5 text-xs text-muted-foreground">
      <Download class="h-3.5 w-3.5" />
      下载在后台进行，可在「任务」页查看进度、暂停或取消；完成后自动导入媒体库
    </p>
  </Dialog>
</template>