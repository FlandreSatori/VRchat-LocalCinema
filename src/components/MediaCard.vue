<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  Film,
  Link,
  Copy,
  Play,
  MonitorPlay,
  FolderOpen,
  FolderInput,
  Pencil,
  RefreshCw,
  Image as ImageIcon,
  Trash2,
  Info,
  FileVideo,
  ListVideo,
  CornerUpLeft,
  Scissors,
  Check,
  TriangleAlert,
} from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { open } from '@tauri-apps/plugin-dialog'
import { useRouter } from 'vue-router'
import ContextMenu from './ui/ContextMenu.vue'
import Button from './ui/Button.vue'
import Badge from './ui/Badge.vue'
import MediaInfoDialog from './MediaInfoDialog.vue'
import TranscodeDialog from './TranscodeDialog.vue'
import DecoderMissingDialog from './DecoderMissingDialog.vue'
import type { MenuEntry } from '../lib/menu'
import {
  buildMediaUrl,
  openInPlayer,
  openWithSystemDefault,
  openInFolder,
  refreshMedia,
  setCustomThumbnail,
  removeMedia,
  generateSample,
  audioDecoderMissingCached,
  type MediaItem,
  type UrlMode,
} from '../lib/api'
import { toast, errMsg } from '../lib/toast'
import { confirmDialog } from '../lib/confirm'
import { formatSize, formatDuration } from '../lib/format'
import { serverStore, convertStore, convertTaskStore } from '../lib/store'

const props = withDefaults(
  defineProps<{
    item: MediaItem
    /** 批量管理模式：显示勾选框，点击卡片切换选中 */
    selectable?: boolean
    selected?: boolean
  }>(),
  { selectable: false, selected: false }
)

const emit = defineEmits<{
  'copy-link': [item: MediaItem]
  removed: [id: string]
  rename: [item: MediaItem]
  move: [item: MediaItem]
  'goto-source': [sourceId: string]
  'toggle-select': [id: string]
}>()

const router = useRouter()

const imgError = ref(false)
const infoOpen = ref(false)
const transcodeOpen = ref(false)
/// 打开转码对话框时默认套用的方式（从「缺解码器」提示进入时预设为「仅转音频」）
const transcodeStrategy = ref<'smart' | 'audio' | 'full' | 'custom'>('smart')
const decoderOpen = ref(false)

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

/// 当前卡片的转码/合成任务（HLS 与转码共用 id）
const job = computed(() => convertStore.jobs[props.item.id])

/// 转码排队/进行中或边转边播时，禁用该视频的所有操作
const busy = computed(
  () =>
    job.value !== undefined ||
    convertTaskStore.tasks.some(
      (t) => t.mediaId === props.item.id && (t.state === 'queued' || t.state === 'running')
    )
)

const unsupported = computed(() => props.item.compatStatus === 'unsupported')

/// 本机缺少音频解码器（如 AC-3）：白名单兼容但 VRChat 仍可能无法播放音轨，弹出提示引导安装或转码
const decoderWarning = ref<string | null>(null)
watch(
  () => [props.item.id, props.item.audioCodec, props.item.compatStatus] as const,
  async () => {
    decoderWarning.value = null
    const codec = props.item.audioCodec
    if (!codec || props.item.compatStatus !== 'supported') return
    if (codec !== 'ac3' && codec !== 'eac3') return
    try {
      if (await audioDecoderMissingCached(codec)) {
        decoderWarning.value =
          codec === 'ac3'
            ? '本机缺少 AC-3 解码器，VRChat 将无法播放该视频的音轨。可转码为 AAC，或安装 Dolby 解码器后重启 VRChat 解决。'
            : '本机缺少 E-AC-3 (Dolby Digital Plus) 解码器，VRChat 将无法播放该视频的音轨。可转码为 AAC，或安装 Dolby 解码器后重启 VRChat 解决。'
      }
    } catch {
      // 非 Tauri 环境忽略
    }
  },
  { immediate: true }
)

/// 转码产物的「快捷方式」卡片：只允许复制链接 / 跳转源视频位置，不允许再次转码
const isShortcut = computed(() => props.item.sourceMediaId !== null)

async function copyUrl(mode: UrlMode, label: string) {
  try {
    const url = await buildMediaUrl(props.item.id, mode)
    await writeText(url)
    toast.success(`已复制${label}`)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

/// 打开转码设置对话框（转码范围 / 音轨 / 字幕轨选择、转码方式与高级参数均在其中）。
/// 延迟到下一帧再打开：先让右键菜单完全关闭，避免菜单收起的同一次点击事件
/// 把刚弹出的对话框立即关掉（表现为对话框一闪而过、屏幕只剩灰色遮罩）。
function doTranscode() {
  transcodeStrategy.value = 'smart'
  setTimeout(() => {
    transcodeOpen.value = true
  }, 0)
}

/// 从「缺解码器」提示进入转码：预设为「仅转音频」（视频直拷 + 音频 AAC）
function doTranscodeAudio() {
  transcodeStrategy.value = 'audio'
  setTimeout(() => {
    transcodeOpen.value = true
  }, 0)
}

/// 生成样本：截取视频正中间的 20 秒保存到源视频所在目录，完成后自动打开资源管理器定位
async function doSample() {
  try {
    await generateSample(props.item.id)
    toast.info('正在生成样本（中间 20 秒），完成后将打开所在目录')
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
  const ok = await confirmDialog({
    title: '从媒体库移除？',
    description: `确定移除「${props.item.title}」吗？仅移除记录，不会删除磁盘文件。`,
    confirmText: '移除',
    danger: true,
  })
  if (!ok) return
  try {
    await removeMedia(props.item.id)
    emit('removed', props.item.id)
    toast.success('已从媒体库移除')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

const menuItems = computed<MenuEntry[]>(() => {
  // 转码产物的「快捷方式」卡片：仅复制链接 / 查看信息 / 跳转源位置，不得再次转码
  if (isShortcut.value) {
    return [
      { type: 'item', label: '复制本机 URL', icon: Copy, onSelect: () => copyUrl('localhost', '本机 URL') },
      { type: 'item', label: '复制局域网 URL', icon: Copy, onSelect: () => copyUrl('lan', '局域网 URL') },
      { type: 'separator' },
      { type: 'item', label: '查看视频信息', icon: Info, onSelect: () => (infoOpen.value = true) },
      { type: 'item', label: '打开文件所在目录', icon: FolderOpen, onSelect: openFolder },
      {
        type: 'item',
        label: '跳转到源视频位置',
        icon: CornerUpLeft,
        onSelect: () => emit('goto-source', props.item.sourceMediaId!),
      },
      { type: 'separator' },
      { type: 'item', label: '从媒体库移除', icon: Trash2, danger: true, onSelect: doRemove },
    ]
  }
  // 转码排队/进行中或边转边播时禁用所有操作
  if (busy.value) {
    return [
      { type: 'item', label: '查看视频信息', icon: Info, onSelect: () => (infoOpen.value = true) },
      {
        type: 'item',
        label: '打开任务页',
        icon: ListVideo,
        onSelect: () => navigateToConvert(),
      },
    ]
  }
  return [
    { type: 'item', label: '复制本机 URL', icon: Copy, onSelect: () => copyUrl('localhost', '本机 URL') },
    { type: 'item', label: '复制局域网 URL', icon: Copy, onSelect: () => copyUrl('lan', '局域网 URL') },
    { type: 'separator' },
    { type: 'item', label: '系统默认播放器打开', icon: Play, onSelect: openDefault },
    { type: 'item', label: '使用指定播放器打开', icon: MonitorPlay, onSelect: openPlayer },
    { type: 'item', label: '打开文件所在目录', icon: FolderOpen, onSelect: openFolder },
    { type: 'separator' },
    { type: 'item', label: '查看视频信息', icon: Info, onSelect: () => (infoOpen.value = true) },
    {
      type: 'item',
      label: '转码为…',
      icon: FileVideo,
      onSelect: doTranscode,
    },
    // 本机缺少 AC-3/E-AC-3 解码器时提供引导（转码为 AAC / 安装解码器）
    ...(decoderWarning.value
      ? [
          {
            type: 'item' as const,
            label: '缺少音频解码器，如何解决…',
            icon: TriangleAlert,
            onSelect: () => (decoderOpen.value = true),
          },
        ]
      : []),
    { type: 'item', label: '生成样本（中间 20 秒）', icon: Scissors, onSelect: doSample },
    { type: 'item', label: '刷新媒体信息', icon: RefreshCw, onSelect: doRefresh },
    { type: 'item', label: '更换封面', icon: ImageIcon, onSelect: changeThumbnail },
    { type: 'item', label: '重命名', icon: Pencil, onSelect: () => emit('rename', props.item) },
    { type: 'item', label: '移动到…', icon: FolderInput, onSelect: () => emit('move', props.item) },
    { type: 'separator' },
    { type: 'item', label: '从媒体库移除', icon: Trash2, danger: true, onSelect: doRemove },
  ]
})

function navigateToConvert() {
  void router.push('/convert')
}

</script>

<template>
  <ContextMenu :items="menuItems">
    <div
      class="group overflow-hidden rounded-lg border bg-card shadow-sm transition-colors hover:border-primary/40"
      :class="[unsupported ? 'border-destructive/40' : 'border-border', selectable ? 'cursor-pointer' : '']"
      :style="selected ? 'border-color: hsl(var(--primary))' : ''"
      @click="selectable && emit('toggle-select', item.id)"
    >
      <div class="relative aspect-square w-full overflow-hidden bg-zinc-900">
        <!-- 批量管理勾选框 -->
        <button
          v-if="selectable"
          type="button"
          class="absolute left-2 top-2 z-10 flex h-6 w-6 items-center justify-center rounded-full border-2 backdrop-blur-sm transition-colors"
          :class="selected ? 'border-primary bg-primary text-primary-foreground' : 'border-white/70 bg-black/40 text-transparent'"
          :title="selected ? '取消选择' : '选择'"
          @click.stop="emit('toggle-select', item.id)"
        >
          <Check class="h-4 w-4" />
        </button>
        <img
          v-if="showImage"
          :src="thumbUrl!"
          :alt="item.title"
          class="h-full w-full object-cover"
          draggable="false"
          loading="lazy"
          @error="imgError = true"
        />
        <div v-else class="flex h-full w-full items-center justify-center text-zinc-600">
          <Film class="h-10 w-10" />
        </div>
        <Badge v-if="item.status === 'processing'" variant="warning" class="absolute right-2 top-2">处理中</Badge>
        <Badge v-else-if="item.status === 'missing'" variant="destructive" class="absolute right-2 top-2">文件丢失</Badge>
        <Badge
          v-else-if="unsupported"
          variant="destructive"
          class="absolute right-2 top-2 cursor-help"
          :title="item.compatReason ?? '不支持该视频格式或编码组合'"
        >
          不支持
        </Badge>
        <Badge
          v-if="isShortcut"
          variant="secondary"
          class="absolute top-2 cursor-help"
          :class="selectable ? 'left-10' : 'left-2'"
          title="这是源视频的转码产物（快捷方式）；源视频仍在原位置，可通过右键菜单跳转"
        >
          转码产物
        </Badge>
        <Badge
          v-if="decoderWarning"
          variant="warning"
          class="absolute right-2 cursor-pointer"
          :class="item.status === 'ok' && !unsupported ? 'top-2' : 'top-9'"
          :title="decoderWarning"
          @click.stop="decoderOpen = true"
        >
          <TriangleAlert class="mr-1 h-3 w-3" />
          缺解码器
        </Badge>
        <span
          v-if="item.durationMs !== null"
          class="absolute bottom-2 right-2 rounded bg-black/70 px-1.5 py-0.5 text-xs text-white"
        >
          {{ formatDuration(item.durationMs) }}
        </span>
        <div v-if="job" class="absolute inset-x-0 bottom-0">
          <div class="bg-black/70 px-2 py-1 text-[11px] text-white">
            {{ job.status === 'queued' ? '排队等待转码' : job.kind === 'segment' ? '分段转码中' : job.kind === 'transcode' ? '转码中' : '边转边播中' }}
            <template v-if="job.percent !== null">（{{ Math.round(job.percent) }}%）</template>
          </div>
          <div class="h-1 w-full bg-white/20">
            <div
              class="h-full bg-primary transition-all"
              :style="{ width: job.percent !== null ? `${job.percent}%` : '30%' }"
              :class="job.percent === null ? 'animate-pulse' : ''"
            />
          </div>
        </div>
      </div>
      <div class="p-2">
        <div class="line-clamp-2 break-all text-sm font-medium" :title="item.title">{{ item.title }}</div>
        <div class="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground">
          <span>{{ formatSize(item.fileSize) }}</span>
          <span>·</span>
          <span class="uppercase">{{ item.format }}</span>
        </div>
        <div v-if="unsupported && !isShortcut" class="mt-0.5 line-clamp-2 break-all text-xs text-destructive" :title="item.compatReason ?? ''">
          {{ item.compatReason ?? '不支持该视频格式或编码组合' }}
        </div>
        <div v-if="unsupported && !isShortcut" class="mt-2 flex gap-1.5">
          <Button size="sm" variant="outline" :disabled="busy" class="h-7 flex-1 px-2 text-xs" title="打开转码设置：可选全部转码或分段转码、音轨与字幕" @click="doTranscode">
            <FileVideo class="h-3 w-3" />
            转码为…
          </Button>
        </div>
        <div v-else-if="isShortcut" class="mt-2 flex gap-1.5">
          <Button size="sm" class="h-7 flex-1 px-2 text-xs" title="复制已转码视频的播放链接" @click="emit('copy-link', item)">
            <Link class="h-3 w-3" />
            复制链接
          </Button>
          <Button size="sm" variant="outline" class="h-7 px-2" title="跳转到源视频所在位置" @click="emit('goto-source', item.sourceMediaId!)">
            <CornerUpLeft class="h-3 w-3" />
          </Button>
        </div>
        <div v-else class="mt-2 flex gap-1.5">
          <Button size="sm" :disabled="busy" class="h-7 flex-1 px-2 text-xs" @click="emit('copy-link', item)">
            <Link class="h-3 w-3" />
            复制链接
          </Button>
        </div>
      </div>
    </div>
    <MediaInfoDialog v-model:open="infoOpen" :item="props.item" />
    <DecoderMissingDialog
      v-model:open="decoderOpen"
      :item="props.item"
      @transcode="doTranscodeAudio"
    />
  </ContextMenu>
  <!-- 转码对话框放在菜单触发器之外：避免与右键菜单的打开/关闭相互干扰 -->
  <TranscodeDialog
    v-model:open="transcodeOpen"
    :item="props.item"
    :default-strategy="transcodeStrategy"
  />
</template>
