<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'
import { Copy, Check, ExternalLink, Laptop, Globe, Loader2 } from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import Dialog from './ui/Dialog.vue'
import Button from './ui/Button.vue'
import Input from './ui/Input.vue'
import Label from './ui/Label.vue'
import Badge from './ui/Badge.vue'
import {
  buildMediaUrl,
  listSegments,
  openUrl,
  type MediaItem,
  type UrlMode,
  type UnlistenFn,
} from '../lib/api'
import { loadSettings, convertTaskStore } from '../lib/store'
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
  id: string
  label: string
  url: string
}

const mode = ref<UrlMode>('localhost')
const format = ref<'whole' | 'segment'>('whole')
const url = ref('')
const urlError = ref('')
const segments = ref<SegmentEntry[]>([])
const segmentHint = ref('')
const copiedId = ref('')
let unlisten: UnlistenFn | null = null
/// 每次重建自增；开关弹窗、切换媒体/模式/格式时旧请求失效，迟到结果被丢弃
let rebuildGeneration = 0

const modeOptions: { value: UrlMode; label: string; icon: typeof Laptop }[] = [
  { value: 'localhost', label: '本地模式', icon: Laptop },
  { value: 'lan', label: '局域网模式', icon: Globe },
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
  }
})

/// 分段转码任务的进行中进度（用于第二段状态提示）
const segmentTask = computed(() =>
  convertTaskStore.tasks.find(
    (t) => sourceId.value !== null && t.mediaId === sourceId.value && t.kind === 'segment'
  )
)

const sourceId = computed(() => props.item?.sourceMediaId ?? props.item?.id ?? null)

onUnmounted(() => {
  unlisten?.()
})

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
    await rebuild()
  }
)

watch([mode, format], () => {
  if (props.open) rebuild()
})

async function rebuild() {
  const ticket = ++rebuildGeneration
  url.value = ''
  urlError.value = ''
  segments.value = []
  segmentHint.value = ''
  const id = props.item?.id
  if (!id) return
  try {
    if (format.value === 'whole') {
      const result = await buildMediaUrl(id, mode.value)
      if (ticket !== rebuildGeneration) return
      url.value = result
    } else {
      const sid = sourceId.value
      if (!sid) return
      const list = await listSegments(sid)
      // 并发获取各段链接；完毕后若已被更新请求取代则整体丢弃
      const entries: SegmentEntry[] = await Promise.all(
        list.map(async (seg, i) => ({
          id: seg.id,
          label: `第 ${i + 1} 段`,
          url: await buildMediaUrl(seg.id, mode.value),
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

async function copy() {
  if (!url.value) return
  try {
    await writeText(url.value)
    copiedId.value = 'main'
    toast.success('已复制播放链接')
    // 复制后自动关闭弹窗返回主界面
    setTimeout(() => {
      emit('update:open', false)
    }, 500)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function copySegment(seg: SegmentEntry) {
  try {
    await writeText(seg.url)
    copiedId.value = seg.id
    toast.success(`已复制${seg.label}链接`)
    // 复制后自动关闭弹窗返回主界面
    setTimeout(() => {
      emit('update:open', false)
    }, 500)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openInBrowser() {
  if (!url.value) return
  try {
    await openUrl(url.value)
  } catch (e) {
    toast.error(errMsg(e))
  }
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)" title="复制播放链接" :description="item?.title">
    <div class="space-y-4">
      <div>
        <Label class="mb-2 block">地址模式</Label>
        <div class="grid grid-cols-2 gap-2">
          <button
            v-for="opt in modeOptions"
            :key="opt.value"
            type="button"
            @click="mode = opt.value"
            :class="
              cn(
                'flex cursor-pointer items-center justify-center gap-1.5 rounded-md border px-3 py-2 text-sm transition-colors',
                mode === opt.value
                  ? 'border-primary bg-primary/10 text-primary'
                  : 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'
              )
            "
          >
            <component :is="opt.icon" class="h-4 w-4" />
            {{ opt.label }}
          </button>
        </div>
        <p class="mt-2 text-xs text-muted-foreground">{{ modeHint }}</p>
      </div>

      <div>
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
          <div v-for="seg in segments" :key="seg.id">
            <div class="mb-1 flex items-center gap-2">
              <Badge variant="secondary">{{ seg.label }}</Badge>
            </div>
            <div class="flex gap-2">
              <Input :model-value="seg.url" readonly class="flex-1 font-mono text-xs" />
              <Button size="sm" class="h-9 shrink-0 px-3" @click="copySegment(seg)">
                <Check v-if="copiedId === seg.id" class="h-4 w-4" />
                <Copy v-else class="h-4 w-4" />
                {{ copiedId === seg.id ? '已复制' : '复制' }}
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
