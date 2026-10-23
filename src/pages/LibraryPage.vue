<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import {
  Film,
  Plus,
  Search,
  AlertTriangle,
  Loader2,
  Folder as FolderIcon,
  Pencil,
  Trash2,
  ChevronRight,
  FolderPlus,
  CornerUpLeft,
  Users,
  ListChecks,
  FolderInput,
  FileVideo,
  Check,
  Link,
} from 'lucide-vue-next'
import { open } from '@tauri-apps/plugin-dialog'
import Button from '../components/ui/Button.vue'
import Input from '../components/ui/Input.vue'
import Select from '../components/ui/Select.vue'
import Dialog from '../components/ui/Dialog.vue'
import ContextMenu from '../components/ui/ContextMenu.vue'
import Badge from '../components/ui/Badge.vue'
import MediaCard from '../components/MediaCard.vue'
import UrlDialog from '../components/UrlDialog.vue'
import PeerMediaDialog from '../components/PeerMediaDialog.vue'
import BatchTranscodeDialog from '../components/BatchTranscodeDialog.vue'
import type { MenuEntry } from '../lib/menu'
import type { MediaItem, Folder, PeerInfo, UnlistenFn, TranscodeOptions } from '../lib/api'
import { BUILTIN_INCOMPATIBLE_ID } from '../lib/api'
import {
  mediaStore,
  folderStore,
  serverStore,
  settingsCache,
  convertTaskStore,
  importMediaPaths,
  removeMediaItem,
  loadFolders,
  upsertMedia,
} from '../lib/store'
import {
  createFolder,
  renameFolder,
  deleteFolder,
  renameMedia,
  moveMedia,
  removeMedia,
  transcodeMedia,
  onSegmentReady,
  onSampleDone,
  revealPath,
  getEasytierStatus,
  getRoomInfo,
  importOnlineVideo,
} from '../lib/api'
import { toast, errMsg } from '../lib/toast'
import { confirmDialog } from '../lib/confirm'
import { formatDuration } from '../lib/format'

const search = ref('')
const sortBy = ref<'addedAt' | 'title' | 'fileSize' | 'durationMs'>('addedAt')
const sortOrder = ref<'asc' | 'desc'>('desc')
const statusFilter = ref<'all' | 'ok' | 'missing'>('all')
const importing = ref(false)

/// 当前所在文件夹；null = 根目录
const currentFolderId = ref<string | null>(null)

const urlDialogOpen = ref(false)
const urlDialogItem = ref<MediaItem | null>(null)
const urlDialogFormat = ref<'whole' | 'segment'>('whole')

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

// ---------- 资源管理器导航 ----------

const folderMap = computed(() => {
  const map: Record<string, Folder> = {}
  folderStore.folders.forEach((f) => (map[f.id] = f))
  return map
})

const breadcrumbs = computed(() => {
  const crumbs: Folder[] = []
  let cur = currentFolderId.value ? folderMap.value[currentFolderId.value] : null
  while (cur) {
    crumbs.unshift(cur)
    cur = cur.parentId ? folderMap.value[cur.parentId] : null
  }
  return crumbs
})

const childFolders = computed(() =>
  folderStore.folders
    .filter((f) => f.parentId === currentFolderId.value)
    .sort((a, b) =>
      a.id === BUILTIN_INCOMPATIBLE_ID
        ? -1
        : b.id === BUILTIN_INCOMPATIBLE_ID
          ? 1
          : a.createdAt - b.createdAt
    )
)

function folderMediaCount(fid: string): number {
  return mediaStore.items.filter((m) => m.folderId === fid).length
}

function enterFolder(f: Folder) {
  currentFolderId.value = f.id
}

function goUp() {
  const cur = currentFolderId.value ? folderMap.value[currentFolderId.value] : null
  currentFolderId.value = cur?.parentId ?? null
}

const searching = computed(() => search.value.trim().length > 0)

/// 搜索时隐藏文件夹，只显示全库匹配的视频
const visibleFolders = computed(() => (searching.value ? [] : childFolders.value))

const baseItems = computed(() =>
  searching.value
    ? mediaStore.items
    : mediaStore.items.filter((m) => m.folderId === currentFolderId.value)
)

const filtered = computed(() => {
  const q = search.value.trim().toLowerCase()
  let list = baseItems.value
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

// ---------- 导入 ----------

async function doImport(paths: string[]) {
  if (!paths.length || importing.value) return
  importing.value = true
  try {
    const items = await importMediaPaths(paths)
    toast.success(`已导入 ${items.length} 个视频`)
    // FastStart 提示由 store 的共享状态触发（App.vue 渲染），不在此处处理
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

// ---------- 在线视频导入（B 站） ----------

const onlineDialog = reactive({ open: false, url: '' })
const onlineImporting = ref(false)

function openOnlineDialog() {
  onlineDialog.url = ''
  onlineDialog.open = true
}

async function submitOnlineImport() {
  const url = onlineDialog.url.trim()
  if (!url) {
    toast.error('请粘贴 B 站视频链接')
    return
  }
  if (onlineImporting.value) return
  onlineImporting.value = true
  try {
    const item = await importOnlineVideo(url)
    upsertMedia(item)
    toast.success(`已导入在线视频「${item.title}」`)
    onlineDialog.open = false
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    onlineImporting.value = false
  }
}

// ---------- 文件夹管理对话框 ----------

const folderDialog = reactive<{
  open: boolean
  mode: 'create' | 'rename'
  folder: Folder | null
  name: string
}>({ open: false, mode: 'create', folder: null, name: '' })

function openCreateFolder() {
  folderDialog.mode = 'create'
  folderDialog.folder = null
  folderDialog.name = ''
  folderDialog.open = true
}

function openRenameFolder(f: Folder) {
  folderDialog.mode = 'rename'
  folderDialog.folder = f
  folderDialog.name = f.name
  folderDialog.open = true
}

async function submitFolderDialog() {
  const name = folderDialog.name.trim()
  if (!name) {
    toast.error('文件夹名不能为空')
    return
  }
  try {
    if (folderDialog.mode === 'create') {
      await createFolder(name, currentFolderId.value)
      await loadFolders(true)
      toast.success(`已创建文件夹「${name}」`)
    } else if (folderDialog.folder) {
      await renameFolder(folderDialog.folder.id, name)
      await loadFolders(true)
      toast.success('文件夹已重命名')
    }
    folderDialog.open = false
  } catch (e) {
    toast.error(errMsg(e))
  }
}

function folderMenu(f: Folder): MenuEntry[] {
  if (f.id === BUILTIN_INCOMPATIBLE_ID) return []
  return [
    { type: 'item', label: '重命名', icon: Pencil, onSelect: () => openRenameFolder(f) },
    { type: 'item', label: '删除文件夹', icon: Trash2, danger: true, onSelect: () => doDeleteFolder(f) },
  ]
}

async function doDeleteFolder(f: Folder) {
  const ok = await confirmDialog({
    title: '删除文件夹？',
    description: `确定删除文件夹「${f.name}」吗？其中的视频将移回上一级（不会删除磁盘文件）。`,
    confirmText: '删除',
    danger: true,
  })
  if (!ok) return
  try {
    await deleteFolder(f.id)
    await loadFolders(true)
    // 若当前位于被删子树内则退回根目录
    let cur = currentFolderId.value
    while (cur) {
      if (cur === f.id) {
        currentFolderId.value = null
        break
      }
      cur = folderMap.value[cur]?.parentId ?? null
    }
    toast.success('文件夹已删除')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

// ---------- 文件重命名 / 移动对话框 ----------

const fileDialog = reactive<{
  open: boolean
  mode: 'rename' | 'move'
  item: MediaItem | null
  name: string
  target: string
}>({ open: false, mode: 'rename', item: null, name: '', target: '__root__' })

const moveTargetOptions = computed(() => [
  { value: '__root__', label: '根目录' },
  ...folderStore.folders.map((f) => ({ value: f.id, label: f.name })),
])

function openRenameFile(item: MediaItem) {
  fileDialog.mode = 'rename'
  fileDialog.item = item
  fileDialog.name = item.fileName
  fileDialog.open = true
}

function openMoveFile(item: MediaItem) {
  fileDialog.mode = 'move'
  fileDialog.item = item
  fileDialog.target = item.folderId ?? '__root__'
  fileDialog.open = true
}

async function submitFileDialog() {
  if (!fileDialog.item) return
  try {
    if (fileDialog.mode === 'rename') {
      const name = fileDialog.name.trim()
      if (!name || name === fileDialog.item.fileName) {
        fileDialog.open = false
        return
      }
      const updated = await renameMedia(fileDialog.item.id, name)
      mediaItemUpdated(updated)
      toast.success('已重命名（播放链接已同步更新）')
    } else {
      const target = fileDialog.target === '__root__' ? null : fileDialog.target
      const updated = await moveMedia(fileDialog.item.id, target)
      mediaItemUpdated(updated)
      toast.success('已移动')
    }
    fileDialog.open = false
  } catch (e) {
    toast.error(errMsg(e))
  }
}

function mediaItemUpdated(item: MediaItem) {
  const idx = mediaStore.items.findIndex((m) => m.id === item.id)
  if (idx >= 0) mediaStore.items[idx] = item
  else mediaStore.items.unshift(item)
}

// ---------- 其它 ----------

function openUrlDialog(item: MediaItem, format: 'whole' | 'segment' = 'whole') {
  urlDialogItem.value = item
  urlDialogFormat.value = format
  urlDialogOpen.value = true
}

function onRemoved(id: string) {
  removeMediaItem(id)
}

/// 从转码产物「快捷方式」卡片跳转到源视频所在位置
function gotoSource(sourceId: string) {
  const src = mediaStore.items.find((m) => m.id === sourceId)
  if (!src) {
    toast.error('源视频已被移除')
    return
  }
  currentFolderId.value = src.folderId
  toast.info(`已跳转到源视频「${src.title}」所在位置`)
}

// ---------- 批量管理（多选：移动 / 删除 / 转码） ----------

const selectMode = ref(false)
const selectedIds = ref<Set<string>>(new Set())

function toggleSelectMode() {
  selectMode.value = !selectMode.value
  if (!selectMode.value) selectedIds.value = new Set()
}

function toggleSelect(id: string) {
  const next = new Set(selectedIds.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  selectedIds.value = next
}

const selectedCount = computed(() => selectedIds.value.size)
const allFilteredSelected = computed(
  () => filtered.value.length > 0 && filtered.value.every((m) => selectedIds.value.has(m.id))
)

function selectAllFiltered() {
  const next = new Set(selectedIds.value)
  if (allFilteredSelected.value) {
    filtered.value.forEach((m) => next.delete(m.id))
  } else {
    filtered.value.forEach((m) => next.add(m.id))
  }
  selectedIds.value = next
}

// 切换文件夹或修改搜索时清空选择，避免误操作不可见条目
watch([currentFolderId, search], () => {
  selectedIds.value = new Set()
})

/// 批量移动对话框
const batchMoveOpen = ref(false)
const batchMoveTarget = ref('__root__')

function openBatchMove() {
  batchMoveTarget.value = currentFolderId.value ?? '__root__'
  batchMoveOpen.value = true
}

async function submitBatchMove() {
  const target = batchMoveTarget.value === '__root__' ? null : batchMoveTarget.value
  const ids = [...selectedIds.value]
  let ok = 0
  for (const id of ids) {
    try {
      const updated = await moveMedia(id, target)
      mediaItemUpdated(updated)
      ok++
    } catch {
      // 单个失败不中断，最后统一汇报
    }
  }
  batchMoveOpen.value = false
  toast.success(`已移动 ${ok}/${ids.length} 个视频`)
}

/// 批量转码对话框：同一套设置应用到所有可转码条目
const batchTranscodeOpen = ref(false)
const batchTranscodeCount = ref(0)

/// 可转码的选中条目：跳过转码产物卡片、在线视频与已在队列中的条目
function transcodeCandidates(): string[] {
  return [...selectedIds.value].filter((id) => {
    const item = mediaStore.items.find((m) => m.id === id)
    const busy = convertTaskStore.tasks.some(
      (t) => t.mediaId === id && (t.state === 'queued' || t.state === 'running')
    )
    return !!item && item.sourceMediaId === null && item.sourceKind !== 'online' && !busy
  })
}

function openBatchTranscode() {
  batchTranscodeCount.value = transcodeCandidates().length
  batchTranscodeOpen.value = true
}

/// 批量转码：已存在产物时使用新文件名（不覆盖）
async function submitBatchTranscode(options: TranscodeOptions) {
  const candidates = transcodeCandidates()
  let queued = 0
  let skipped = selectedIds.value.size - candidates.length
  for (const id of candidates) {
    try {
      await transcodeMedia(id, false, options)
      queued++
    } catch {
      skipped++
    }
  }
  if (queued > 0) {
    toast.info(`已加入转码队列 ${queued} 个${skipped ? `，跳过 ${skipped} 个` : ''}`)
  } else {
    toast.error('没有可转码的条目（转码产物与已在队列中的会被跳过）')
  }
}

async function batchRemove() {
  const n = selectedIds.value.size
  const ok = await confirmDialog({
    title: '从媒体库移除？',
    description: `确定移除选中的 ${n} 个视频吗？仅移除记录，不会删除磁盘文件。`,
    confirmText: '移除',
    danger: true,
  })
  if (!ok) return
  let done = 0
  for (const id of [...selectedIds.value]) {
    try {
      await removeMedia(id)
      removeMediaItem(id)
      done++
    } catch {
      // 单个失败不中断
    }
  }
  selectedIds.value = new Set()
  toast.success(`已移除 ${done}/${n} 个`)
}

// ---------- 查看他人媒体库 ----------

const peerMediaOpen = ref(false)
const selectedPeer = ref<PeerInfo | null>(null)
/// 对方端口输入：留空时回退到本机监听端口
const peerPortInput = ref('')
const defaultPeerPort = computed(() => settingsCache.value?.port ?? 17001)
const peerPort = computed(() => {
  const n = Math.round(Number(peerPortInput.value.trim()))
  return Number.isFinite(n) && n >= 1 && n <= 65535 ? n : defaultPeerPort.value
})

/// 查看他人媒体库对话框：手动输入虚拟 IP/端口始终可用，EasyTier 运行时额外提供成员一键选择
const peerPickerOpen = ref(false)
const pickerLoading = ref(false)
const pickerPeers = ref<PeerInfo[]>([])
const manualIp = ref('')
/// 合法 IPv4：四段 0-255
const IPV4_RE = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/
/// 手工输入的虚拟 IP 是否为合法 IPv4（空字符串表示未填）
const manualIpValid = computed(() => IPV4_RE.test(manualIp.value.trim()))

async function openPeerMedia() {
  peerPickerOpen.value = true
  pickerLoading.value = true
  pickerPeers.value = []
  manualIp.value = ''
  peerPortInput.value = ''
  try {
    const st = await getEasytierStatus()
    if (st.running) {
      const info = await getRoomInfo()
      if (info.error) {
        toast.error(info.error)
      } else {
        pickerPeers.value = info.peers
      }
    }
    // 未启用 EasyTier 时不报错：仅提供手动输入（成员列表为空）
  } catch {
    // 非 Tauri 环境或获取失败：仅提供手动输入
  } finally {
    pickerLoading.value = false
  }
}

function choosePeer(peer: PeerInfo) {
  if (!peer.ipv4) {
    toast.error('该成员暂无可用的虚拟 IP')
    return
  }
  peerPickerOpen.value = false
  selectedPeer.value = peer
  peerMediaOpen.value = true
}

function submitManualIp() {
  const ip = manualIp.value.trim()
  if (!ip) {
    toast.error('请输入对方的虚拟 IP')
    return
  }
  if (!IPV4_RE.test(ip)) {
    toast.error('请输入有效的 IPv4 地址，例如 10.126.126.2')
    return
  }
  peerPickerOpen.value = false
  selectedPeer.value = { hostname: '', ipv4: ip, latencyMs: null, loss: null, cost: '', nat: '', version: '' }
  peerMediaOpen.value = true
}

function costText(cost: string): string {
  if (cost.startsWith('p2p')) return '直连'
  if (cost.startsWith('relay')) return '中转'
  return cost || '-'
}

// 第一段转码完成：弹出「复制链接」窗口（分段播放）；样本生成完成：提示并打开所在目录
let unlistenSegmentReady: UnlistenFn | null = null
let unlistenSampleDone: UnlistenFn | null = null
/// 组件是否已卸载：卸载发生在监听器注册完成前时，仍要保证返回的 unlisten 被释放
let unmounted = false
onMounted(async () => {
  try {
    const fn = await onSegmentReady((seg) => {
      mediaItemUpdated(seg)
      const source = seg.sourceMediaId
        ? mediaStore.items.find((m) => m.id === seg.sourceMediaId) ?? seg
        : seg
      toast.success(`「${seg.title}」第一段转码完成，请复制链接`)
      openUrlDialog(source, 'segment')
    })
    if (unmounted) fn()
    else unlistenSegmentReady = fn
  } catch {
    // 非 Tauri 环境忽略
  }
  try {
    const fn = await onSampleDone((e) => {
      if (e.ok && e.path) {
        toast.success('样本已生成，正在打开所在目录')
        revealPath(e.path).catch(() => {})
      } else {
        toast.error(`样本生成失败：${e.error ?? '未知错误'}`)
      }
    })
    if (unmounted) fn()
    else unlistenSampleDone = fn
  } catch {
    // 非 Tauri 环境忽略
  }
})
onUnmounted(() => {
  unmounted = true
  unlistenSegmentReady?.()
  unlistenSampleDone?.()
})
</script>

<template>
  <div class="mx-auto max-w-6xl p-6">
    <div class="flex items-center justify-between gap-4">
      <div>
        <h1 class="text-xl font-semibold">媒体库</h1>
        <p class="mt-0.5 text-sm text-muted-foreground">共 {{ mediaStore.items.length }} 个视频</p>
      </div>
      <div class="flex gap-2">
        <Button variant="outline" @click="openPeerMedia">
          <Users class="h-4 w-4" />
          查看他人媒体库
        </Button>
        <Button variant="outline" :class="selectMode ? 'border-primary text-primary' : ''" @click="toggleSelectMode">
          <ListChecks class="h-4 w-4" />
          {{ selectMode ? '退出多选' : '批量管理' }}
        </Button>
        <Button variant="outline" :disabled="importing" @click="openCreateFolder">
          <FolderPlus class="h-4 w-4" />
          新建文件夹
        </Button>
        <Button variant="outline" @click="openOnlineDialog">
          <Link class="h-4 w-4" />
          导入B站视频
        </Button>
        <Button :disabled="importing" @click="importViaDialog">
          <Loader2 v-if="importing" class="h-4 w-4 animate-spin" />
          <Plus v-else class="h-4 w-4" />
          导入视频
        </Button>
      </div>
    </div>

    <div v-if="!serverStore.running" class="mt-4 flex items-center gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-600 dark:text-amber-400">
      <AlertTriangle class="h-4 w-4 shrink-0" />
      <span>HTTP 服务未运行，播放链接不可用。</span>
      <RouterLink to="/server" class="font-medium underline underline-offset-4 hover:text-amber-700 dark:hover:text-amber-300">
        前往启动
      </RouterLink>
    </div>

    <!-- 面包屑导航 -->
    <div class="mt-4 flex items-center gap-1 text-sm">
      <button
        type="button"
        class="flex cursor-pointer items-center gap-1 rounded px-2 py-1 font-medium hover:bg-accent"
        :class="currentFolderId === null ? 'text-foreground' : 'text-muted-foreground'"
        @click="currentFolderId = null"
      >
        <FolderIcon class="h-4 w-4" />
        根目录
      </button>
      <template v-for="crumb in breadcrumbs" :key="crumb.id">
        <ChevronRight class="h-4 w-4 text-muted-foreground/60" />
        <button
          type="button"
          class="cursor-pointer rounded px-2 py-1 font-medium hover:bg-accent"
          :class="crumb.id === currentFolderId ? 'text-foreground' : 'text-muted-foreground'"
          @click="currentFolderId = crumb.id"
        >
          {{ crumb.name }}
        </button>
      </template>
      <Button
        v-if="currentFolderId !== null"
        size="sm"
        variant="ghost"
        class="ml-1 h-7 px-2 text-xs"
        @click="goUp"
      >
        <CornerUpLeft class="h-3.5 w-3.5" />
        上一级
      </Button>
    </div>

    <div class="mt-3 flex flex-wrap items-center gap-2">
      <div class="relative w-64">
        <Search class="absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        <Input v-model="search" placeholder="搜索标题或文件名（全库）" class="pl-8" />
      </div>
      <Select :model-value="sortBy" :options="sortOptions" class="w-32" @update:model-value="setSortBy" />
      <Select :model-value="sortOrder" :options="orderOptions" class="w-24" @update:model-value="setSortOrder" />
      <Select :model-value="statusFilter" :options="statusOptions" class="w-28" @update:model-value="setStatusFilter" />
    </div>

    <!-- 空状态：搜索无结果，或根目录完全为空 -->
    <div
      v-if="
        mediaStore.loaded &&
        filtered.length === 0 &&
        (searching || (childFolders.length === 0 && currentFolderId === null))
      "
      class="mt-16 flex flex-col items-center justify-center text-center"
    >
      <template v-if="mediaStore.items.length === 0">
        <Film class="h-16 w-16 text-muted-foreground/40" />
        <h2 class="mt-4 text-lg font-medium">媒体库为空</h2>
        <p class="mt-1 text-sm text-muted-foreground">拖入视频文件或点击导入</p>
        <Button class="mt-4" :disabled="importing" @click="importViaDialog">
          <Plus class="h-4 w-4" />
          导入视频
        </Button>
      </template>
      <template v-else-if="searching">
        <Search class="h-12 w-12 text-muted-foreground/40" />
        <p class="mt-4 text-sm text-muted-foreground">没有匹配的视频</p>
      </template>
      <template v-else>
        <FolderIcon class="h-12 w-12 text-muted-foreground/40" />
        <p class="mt-4 text-sm text-muted-foreground">此文件夹为空</p>
      </template>
    </div>

    <!-- 内容区：返回上一级 / 文件夹 / 视频 同级网格展示（近似正方形卡片） -->
    <div
      v-else
      :class="[
        'mt-4 grid gap-3',
        currentFolderId === BUILTIN_INCOMPATIBLE_ID
          ? 'grid-cols-[repeat(auto-fill,minmax(300px,1fr))]'
          : 'grid-cols-[repeat(auto-fill,minmax(170px,1fr))]',
      ]"
    >
      <!-- 返回上一级（根目录不显示） -->
      <button
        v-if="!searching && currentFolderId !== null"
        type="button"
        class="flex aspect-square w-full cursor-pointer flex-col items-center justify-center gap-3 rounded-lg border border-dashed bg-card p-4 text-muted-foreground shadow-sm transition-colors hover:border-primary/40 hover:text-foreground"
        @click="goUp"
      >
        <CornerUpLeft class="h-12 w-12" />
        <div class="text-sm font-medium">返回上一级</div>
      </button>

      <!-- 文件夹 -->
      <ContextMenu v-for="f in visibleFolders" :key="f.id" :items="folderMenu(f)">
        <button
          type="button"
          class="flex aspect-square w-full cursor-pointer flex-col items-center justify-center gap-3 rounded-lg border bg-card p-4 shadow-sm transition-colors hover:border-primary/40"
          @click="enterFolder(f)"
        >
          <FolderIcon
            class="h-12 w-12"
            :class="f.id === BUILTIN_INCOMPATIBLE_ID ? 'text-destructive/70' : 'text-primary/70'"
          />
          <div class="w-full min-w-0 text-center">
            <div class="truncate text-sm font-medium" :title="f.name">{{ f.name }}</div>
            <div class="text-xs text-muted-foreground">{{ folderMediaCount(f.id) }} 个视频</div>
          </div>
        </button>
      </ContextMenu>

      <!-- 视频 -->
      <MediaCard
        v-for="item in filtered"
        :key="item.id"
        :item="item"
        :detailed="currentFolderId === BUILTIN_INCOMPATIBLE_ID"
        :selectable="selectMode"
        :selected="selectedIds.has(item.id)"
        @copy-link="openUrlDialog"
        @removed="onRemoved"
        @rename="openRenameFile"
        @move="openMoveFile"
        @goto-source="gotoSource"
        @toggle-select="toggleSelect"
      />
    </div>

    <!-- 批量管理操作栏 -->
    <div
      v-if="selectMode"
      class="fixed bottom-6 left-1/2 z-40 flex -translate-x-1/2 flex-wrap items-center gap-2 rounded-full border border-border bg-card px-4 py-2 shadow-lg"
    >
      <span class="whitespace-nowrap text-sm font-medium">
        <Check class="mr-1 inline h-4 w-4 text-primary" />已选 {{ selectedCount }} 个
      </span>
      <Button size="sm" variant="ghost" :disabled="!filtered.length" @click="selectAllFiltered">
        {{ allFilteredSelected ? '取消全选' : '全选' }}
      </Button>
      <Button size="sm" variant="outline" :disabled="!selectedCount" @click="openBatchMove">
        <FolderInput class="h-3.5 w-3.5" />
        移动到…
      </Button>
      <Button size="sm" variant="outline" :disabled="!selectedCount" @click="openBatchTranscode">
        <FileVideo class="h-3.5 w-3.5" />
        转码 MP4
      </Button>
      <Button size="sm" variant="destructive" :disabled="!selectedCount" @click="batchRemove">
        <Trash2 class="h-3.5 w-3.5" />
        移除
      </Button>
      <Button size="sm" variant="ghost" @click="toggleSelectMode">完成</Button>
    </div>

    <!-- 空文件夹提示（含返回上一级卡片时的补充说明） -->
    <div
      v-if="!searching && currentFolderId !== null && !filtered.length && !childFolders.length"
      class="mt-4 flex flex-col items-center justify-center rounded-lg border border-dashed border-border py-10 text-center"
    >
      <Film class="h-8 w-8 text-muted-foreground/40" />
      <p class="mt-2 text-sm text-muted-foreground">此文件夹为空</p>
    </div>

    <UrlDialog v-model:open="urlDialogOpen" :item="urlDialogItem" :initial-format="urlDialogFormat" />

    <!-- 查看他人媒体库：手动输入虚拟 IP/端口，EasyTier 运行时额外提供成员一键选择 -->
    <Dialog
      :open="peerPickerOpen"
      title="查看他人媒体库"
      :description="
        pickerLoading
          ? '正在读取房间成员…'
          : pickerPeers.length
            ? '选择同一房间内的成员，或手动输入对方的虚拟 IP 与端口'
            : '请输入对方的虚拟IP或启用Easytier'
      "
      @update:open="peerPickerOpen = $event"
    >
      <div class="space-y-4">
        <div class="grid grid-cols-[1fr_auto] gap-3">
          <div class="space-y-1.5">
            <label class="text-sm font-medium">对方的虚拟 IP</label>
            <Input
              v-model="manualIp"
              placeholder="例如 10.126.126.2"
              autofocus
              @keydown.enter="submitManualIp"
            />
            <p v-if="manualIp.trim() && !manualIpValid" class="text-xs text-destructive">
              请输入合法的 IPv4 地址，例如 10.126.126.2
            </p>
          </div>
          <div class="space-y-1.5">
            <label class="text-sm font-medium">端口</label>
            <Input
              v-model="peerPortInput"
              type="number"
              :min="1"
              :max="65535"
              class="w-28"
              :placeholder="`留空 ${defaultPeerPort}`"
            />
          </div>
        </div>
        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" @click="peerPickerOpen = false">取消</Button>
          <Button type="button" :disabled="manualIp.trim() !== '' && !manualIpValid" @click="submitManualIp">查看</Button>
        </div>

        <!-- 一键选择：仅在 EasyTier 运行且读取到房间成员时显示 -->
        <template v-if="pickerLoading">
          <div class="border-t border-border" />
          <div class="py-2 text-center text-sm text-muted-foreground">正在读取房间成员…</div>
        </template>
        <template v-else-if="pickerPeers.length">
          <div class="border-t border-border" />
          <div class="space-y-2">
            <p class="text-xs font-medium text-muted-foreground">房间成员（一键查看）</p>
            <div class="max-h-[42vh] space-y-2 overflow-y-auto pr-1">
              <button
                v-for="peer in pickerPeers"
                :key="peer.ipv4 + peer.hostname"
                type="button"
                class="flex w-full items-center justify-between gap-3 rounded-md border border-border p-3 text-left transition-colors hover:bg-accent"
                @click="choosePeer(peer)"
              >
                <div class="min-w-0 flex-1">
                  <p class="truncate text-sm font-medium">{{ peer.hostname || '未命名成员' }}</p>
                  <p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">{{ peer.ipv4 || '无虚拟 IP' }}</p>
                </div>
                <Badge variant="outline" class="shrink-0">{{ costText(peer.cost) }}</Badge>
              </button>
            </div>
          </div>
        </template>
      </div>
    </Dialog>

    <PeerMediaDialog v-model:open="peerMediaOpen" :peer="selectedPeer" :port="peerPort" />

    <!-- 新建/重命名文件夹 -->
    <Dialog
      :open="folderDialog.open"
      :title="folderDialog.mode === 'create' ? '新建文件夹' : '重命名文件夹'"
      description="文件夹仅用于媒体库整理，不影响磁盘文件位置"
      @update:open="folderDialog.open = $event"
    >
      <form
        class="space-y-4"
        @submit.prevent="submitFolderDialog"
      >
        <div class="space-y-1.5">
          <label class="text-sm font-medium">文件夹名</label>
          <Input v-model="folderDialog.name" placeholder="例如：电影 / 剧集" autofocus />
        </div>
        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" @click="folderDialog.open = false">取消</Button>
          <Button type="submit">{{ folderDialog.mode === 'create' ? '创建' : '保存' }}</Button>
        </div>
      </form>
    </Dialog>

    <!-- 重命名/移动视频 -->
    <Dialog
      :open="fileDialog.open"
      :title="fileDialog.mode === 'rename' ? '重命名视频' : '移动视频'"
      description="仅修改媒体库映射，不会改动磁盘上的文件；播放链接随文件名同步变化"
      @update:open="fileDialog.open = $event"
    >
      <form
        class="space-y-4"
        @submit.prevent="submitFileDialog"
      >
        <div v-if="fileDialog.mode === 'rename'" class="space-y-1.5">
          <label class="text-sm font-medium">文件名（含扩展名）</label>
          <Input v-model="fileDialog.name" placeholder="例如：我的视频.mp4" autofocus />
        </div>
        <div v-else class="space-y-1.5">
          <label class="text-sm font-medium">目标文件夹</label>
          <Select v-model="fileDialog.target" :options="moveTargetOptions" class="w-full" />
        </div>
        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" @click="fileDialog.open = false">取消</Button>
          <Button type="submit">{{ fileDialog.mode === 'rename' ? '保存' : '移动' }}</Button>
        </div>
      </form>
    </Dialog>

    <!-- 批量移动视频 -->
    <Dialog
      :open="batchMoveOpen"
      title="批量移动视频"
      :description="`将选中的 ${selectedCount} 个视频移动到目标文件夹（不会改动磁盘文件）`"
      @update:open="batchMoveOpen = $event"
    >
      <form class="space-y-4" @submit.prevent="submitBatchMove">
        <div class="space-y-1.5">
          <label class="text-sm font-medium">目标文件夹</label>
          <Select v-model="batchMoveTarget" :options="moveTargetOptions" class="w-full" autofocus />
        </div>
        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" @click="batchMoveOpen = false">取消</Button>
          <Button type="submit">移动</Button>
        </div>
      </form>
    </Dialog>

    <!-- 导入 B 站在线视频 -->
    <Dialog
      :open="onlineDialog.open"
      title="导入B站视频"
      description="粘贴 B 站视频链接，直接生成可在线播放（免登录上限 720P）的媒体卡片"
      @update:open="onlineDialog.open = $event"
    >
      <form class="space-y-4" @submit.prevent="submitOnlineImport">
        <div class="space-y-1.5">
          <label class="text-sm font-medium">B 站视频链接</label>
          <Input
            v-model="onlineDialog.url"
            placeholder="https://www.bilibili.com/video/BV..."
            autofocus
            :disabled="onlineImporting"
          />
        </div>
        <div class="rounded-md border border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          在线播放最高 720P（B 站免登录限制），播放链路为「本软件本机代理 B 站官方 CDN」。
          需要 1080P 及以上清晰度时，请用 JJdown（唧唧）下载视频，再用「导入视频」导入，即可获得本地媒体卡片与完整画质。
        </div>
        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" :disabled="onlineImporting" @click="onlineDialog.open = false">取消</Button>
          <Button type="submit" :disabled="onlineImporting">
            <Loader2 v-if="onlineImporting" class="h-4 w-4 animate-spin" />
            导入
          </Button>
        </div>
      </form>
    </Dialog>

    <!-- 批量转码设置：同一套设置应用到所有选中视频 -->
    <BatchTranscodeDialog
      v-model:open="batchTranscodeOpen"
      :count="batchTranscodeCount"
      @submit="submitBatchTranscode"
    />
  </div>
</template>
