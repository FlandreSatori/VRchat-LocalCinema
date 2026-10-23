<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Copy, Check, ExternalLink, Laptop, Globe, Cloud, Loader2 } from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import Dialog from './ui/Dialog.vue'
import Button from './ui/Button.vue'
import Input from './ui/Input.vue'
import Label from './ui/Label.vue'
import Badge from './ui/Badge.vue'
import {
  buildMediaUrl,
  buildTunnelUrl,
  listSegments,
  openUrl,
  type MediaItem,
  type UrlMode,
} from '../lib/api'
import { loadSettings, convertTaskStore, tunnelStore, lanListenAvailable, lanUnavailableReason } from '../lib/store'
import { maybeRemindAfterCopy } from '../lib/copyReminder'
import { toast, errMsg } from '../lib/toast'
import { cn } from '../lib/utils'

const props = defineProps<{
  open: boolean
  item: MediaItem | null
  /** 打开时默认选中的链接格式（分段转码完成弹窗用 'segment'） */
  initialFormat?: 'whole' | 'segment'
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

interface SegmentEntry {
  item: MediaItem
  label: string
  url: string
}

const mode = ref<LinkMode>('localhost')
const format = ref<'whole' | 'segment'>('whole')
const url = ref('')
const urlError = ref('')
const segments = ref<SegmentEntry[]>([])
const segmentHint = ref('')
const copiedId = ref('')
/// 每次重建自增；开关弹窗、切换媒体/模式/格式时旧请求失效，迟到结果被丢弃
let rebuildGeneration = 0

/// 链接地址模式：本地 / 局域网 / 公网隧道（Cloudflare 快速隧道）
type LinkMode = UrlMode | 'tunnel'

const modeOptions: { value: LinkMode; label: string; icon: typeof Laptop }[] = [
  { value: 'localhost', label: '本地模式', icon: Laptop },
  { value: 'lan', label: '局域网模式', icon: Globe },
  { value: 'tunnel', label: '公网隧道', icon: Cloud },
]

const formatOptions: { value: 'whole' | 'segment'; label: string }[] = [
  { value: 'whole', label: '整段播放' },
  { value: 'segment', label: '分段播放' },
]

const modeHint = computed(() => {
  switch (mode.value) {
    case 'localhost':
      return '127.0.0.1 指向运行播放器的电脑本身。仅当观众本机也运行了本服务时可用。'
    case 'lan':
      return '同一局域网内的设备可访问；EasyTier 组网运行时自动使用虚拟 IP，也可在设置中手动指定网卡。'
    case 'tunnel':
      return '通过 Cloudflare 快速隧道暴露为公网 HTTPS 地址，观众无需安装本程序或组网；需先在「HTTP 服务」页启动隧道。'
  }
})

/// 局域网模式依赖服务实际绑定到局域网；未生效时生成的局域网链接其他设备必然连不上，故不可选
const lanAvailable = lanListenAvailable
const lanDisabledHint = lanUnavailableReason

function modeUnavailable(v: LinkMode): boolean {
  return v === 'lan' && !lanAvailable.value
}

function modeButtonClass(v: LinkMode) {
  if (modeUnavailable(v)) {
    return 'flex cursor-not-allowed items-center justify-center gap-1.5 rounded-md border border-border px-3 py-2 text-sm text-muted-foreground opacity-50'
  }
  return cn(
    'flex cursor-pointer items-center justify-center gap-1.5 rounded-md border px-3 py-2 text-sm transition-colors',
    mode.value === v
      ? 'border-primary bg-primary/10 text-primary'
      : 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'
  )
}

/// 分段转码任务的进行中进度（用于第二段状态提示）
const segmentTask = computed(() =>
  convertTaskStore.tasks.find(
    (t) => sourceId.value !== null && t.mediaId === sourceId.value && t.kind === 'segment'
  )
)

const sourceId = computed(() => props.item?.sourceMediaId ?? props.item?.id ?? null)

/// 在线（B 站）条目：本地模式用本机代理链接，局域网模式用 B 站 CDN 直链；没有分段概念
const isOnline = computed(() => props.item?.sourceKind === 'online')

watch(
  [() => props.open, () => props.item?.id],
  async ([isOpen]) => {
    // 关闭或切换媒体：使此前所有在途请求失效
    const ticket = ++rebuildGeneration
    copiedId.value = ''
    urlError.value = ''
    url.value = ''
    segments.value = []
    segmentHint.value = ''
    if (!isOpen || !props.item) return
    format.value = props.initialFormat ?? 'whole'
    try {
      const settings = await loadSettings()
      if (ticket !== rebuildGeneration) return
      mode.value = settings.defaultUrlMode
    } catch {
      if (ticket !== rebuildGeneration) return
      mode.value = 'localhost'
    }
    // 在线视频没有分段：按当前默认地址模式生成。
    // 局域网模式下后端直接返回 B 站官方 CDN 直链（不依赖本机 HTTP 服务），故不做局域网可用性回落。
    if (isOnline.value) {
      format.value = 'whole'
      await rebuild()
      return
    }
    // 仅本机监听时局域网链接不可用，回落到本机模式
    if (mode.value === 'lan' && !lanAvailable.value) mode.value = 'localhost'
    await rebuild()
  }
)

watch([mode, format], () => {
  if (props.open) rebuild()
})

watch(() => tunnelStore.revision, () => {
  if (props.open && mode.value === 'tunnel') rebuild()
})

async function rebuild() {
  const ticket = ++rebuildGeneration
  url.value = ''
  urlError.value = ''
  segments.value = []
  segmentHint.value = ''
  const id = props.item?.id
  if (!id) return
  const useTunnel = mode.value === 'tunnel'
  const lanMode: UrlMode = mode.value === 'tunnel' ? 'localhost' : mode.value
  try {
    if (format.value === 'whole') {
      const result = useTunnel ? await buildTunnelUrl(id) : await buildMediaUrl(id, lanMode)
      if (ticket !== rebuildGeneration) return
      url.value = result
    } else {
      const sid = sourceId.value
      if (!sid) return
      const list = await listSegments(sid)
      // 并发获取各段链接；完毕后若已被更新请求取代则整体丢弃
      const entries: SegmentEntry[] = await Promise.all(
        list.map(async (seg, i) => ({
          item: seg,
          label: `第 ${i + 1} 段`,
          url: useTunnel ? await buildTunnelUrl(seg.id) : await buildMediaUrl(seg.id, lanMode),
        }))
      )
      if (ticket !== rebuildGeneration) return
      segments.value = entries
      if (!entries.length) {
        segmentHint.value = '分段尚未生成：第一段转码完成后会自动弹出本窗口'
      } else if (entries.length < 2 && segmentTask.value) {
        segmentHint.value =
          segmentTask.value.state === 'running'
            ? `第二段转码中（总进度 ${Math.round(segmentTask.value.percent ?? 0)}%），完成后即可复制`
            : '第二段排队等待转码中'
      }
    }
  } catch (e) {
    if (ticket !== rebuildGeneration) return
    urlError.value = errMsg(e)
  }
}

async function copyNow() {
  if (!url.value) return
  try {
    const value = mode.value === 'tunnel' && props.item
      ? await buildTunnelUrl(props.item.id) : url.value
    await writeText(value)
    copiedId.value = 'main'
    toast.success('已复制播放链接')
    const reminderItem = props.item
    const reminderMode = mode.value === 'tunnel' ? 'tunnel' : mode.value
    emit('update:open', false)
    if (reminderItem) setTimeout(() => void maybeRemindAfterCopy(reminderItem, reminderMode), 0)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function copy() {
  await copyNow()
}

async function copySegment(seg: SegmentEntry) {
  try {
    const value = mode.value === 'tunnel' ? await buildTunnelUrl(seg.item.id) : seg.url
    await writeText(value)
    copiedId.value = seg.item.id
    toast.success(`已复制${seg.label}链接`)
    const reminderItem = props.item ?? seg.item
    const reminderMode = mode.value === 'tunnel' ? 'tunnel' : mode.value
    emit('update:open', false)
    setTimeout(() => void maybeRemindAfterCopy(reminderItem, reminderMode, seg.item), 0)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openInBrowser() {
  if (!url.value) return
  try {
    const value = mode.value === 'tunnel' && props.item
      ? await buildTunnelUrl(props.item.id) : url.value
    await openUrl(value)
  } catch (e) {
    toast.error(errMsg(e))
  }
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)" title="复制播放链接" :description="item?.title">
    <div class="space-y-4">
      <div v-if="isOnline" class="rounded-md border border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
        <template v-if="mode === 'lan'">
          直链，约 2 小时后过期。
        </template>
        <template v-else>
          本地解析，画质上限 720P；如需 1080P+ 请用 JJdown 下载后导入。
        </template>
      </div>

      <div v-if="!isOnline">
        <Label class="mb-2 block">地址模式</Label>
        <div class="grid grid-cols-3 gap-2">
          <button
            v-for="opt in modeOptions"
            :key="opt.value"
            type="button"
            :disabled="modeUnavailable(opt.value)"
            :title="modeUnavailable(opt.value) ? lanDisabledHint : undefined"
            @click="mode = opt.value"
            :class="modeButtonClass(opt.value)"
          >
            <component :is="opt.icon" class="h-4 w-4" />
            {{ opt.label }}
          </button>
        </div>
        <p class="mt-2 text-xs text-muted-foreground">{{ modeHint }}</p>
        <p v-if="!lanAvailable" class="mt-1 text-xs text-amber-600 dark:text-amber-500">
          {{ lanDisabledHint }}
        </p>
      </div>

      <div v-if="!isOnline">
        <Label class="mb-2 block">链接格式</Label>
        <div class="grid grid-cols-2 gap-2">
          <button
            v-for="opt in formatOptions"
            :key="opt.value"
            type="button"
            @click="format = opt.value"
            :class="
              cn(
                'cursor-pointer rounded-md border px-3 py-2 text-sm transition-colors',
                format === opt.value
                  ? 'border-primary bg-primary/10 text-primary'
                  : 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'
              )
            "
          >
            {{ opt.label }}
          </button>
        </div>
      </div>

      <!-- 整段播放 -->
      <template v-if="format === 'whole'">
        <div>
          <Label class="mb-2 block">链接预览</Label>
          <Input :model-value="url" readonly class="font-mono text-xs" placeholder="生成失败" />
          <p v-if="urlError" class="mt-1.5 text-xs text-destructive">{{ urlError }}</p>
        </div>
        <div class="flex justify-end gap-2">
          <Button variant="outline" :disabled="!url" @click="openInBrowser">
            <ExternalLink class="h-4 w-4" />
            在浏览器打开
          </Button>
          <Button :disabled="!url" @click="copy">
            <Check v-if="copiedId === 'main'" class="h-4 w-4" />
            <Copy v-else class="h-4 w-4" />
            {{ copiedId === 'main' ? '已复制' : '复制' }}
          </Button>
        </div>
      </template>

      <!-- 分段播放 -->
      <template v-else>
        <div class="space-y-2">
          <div v-if="segmentHint" class="flex items-center gap-2 rounded-md border border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
            <Loader2 v-if="segmentTask?.state === 'running'" class="h-3.5 w-3.5 animate-spin" />
            {{ segmentHint }}
          </div>
          <div v-for="seg in segments" :key="seg.item.id">
            <div class="mb-1 flex items-center gap-2">
              <Badge variant="secondary">{{ seg.label }}</Badge>
            </div>
            <div class="flex gap-2">
              <Input :model-value="seg.url" readonly class="flex-1 font-mono text-xs" />
              <Button size="sm" class="h-9 shrink-0 px-3" @click="copySegment(seg)">
                <Check v-if="copiedId === seg.item.id" class="h-4 w-4" />
                <Copy v-else class="h-4 w-4" />
                {{ copiedId === seg.item.id ? '已复制' : '复制' }}
              </Button>
            </div>
          </div>
          <p v-if="urlError" class="text-xs text-destructive">{{ urlError }}</p>
          <p class="text-xs text-muted-foreground">分段为已转码的 H.264 + AAC MP4，可完整拖动进度条。</p>
        </div>
      </template>
    </div>
  </Dialog>
</template>
