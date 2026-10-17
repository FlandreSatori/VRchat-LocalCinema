<script setup lang="ts">
import { computed } from 'vue'
import Dialog from './ui/Dialog.vue'
import Badge from './ui/Badge.vue'
import { formatSize, formatDuration } from '../lib/format'
import type { MediaItem } from '../lib/api'

const props = defineProps<{
  open: boolean
  item: MediaItem | null
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

const rows = computed(() => {
  const it = props.item
  if (!it) return []
  // 在线（B 站）条目没有本机文件，展示来源页面而非内部占位路径
  const sourceRow =
    it.sourceKind === 'online'
      ? { label: '视频来源', value: it.sourceUrl ?? 'B 站在线视频' }
      : { label: '文件路径', value: it.filePath }
  return [
    { label: '文件名', value: it.fileName },
    { label: '容器格式', value: it.container ? it.container.toUpperCase() : '未知' },
    { label: '视频编码', value: it.videoCodec ?? '未知' },
    {
      label: '分辨率',
      value: it.width && it.height ? `${it.width} × ${it.height}` : '未知',
    },
    { label: '像素格式', value: it.pixFmt ?? '未知' },
    { label: '视频位深', value: it.bitDepth != null ? `${it.bitDepth}-bit` : '未知' },
    { label: '音频编码', value: it.audioCodec ?? '未知' },
    {
      label: '采样率',
      value: it.audioSampleRate ? `${it.audioSampleRate} Hz` : '未知',
    },
    {
      label: '声道数',
      value: it.audioChannels != null ? String(it.audioChannels) : '未知',
    },
    { label: '时长', value: formatDuration(it.durationMs) },
    { label: '文件大小', value: formatSize(it.fileSize) },
    sourceRow,
  ]
})

const compatText = computed(() => {
  switch (props.item?.compatStatus) {
    case 'supported':
      return '支持'
    case 'unsupported':
      return '不支持'
    default:
      return '未检测'
  }
})
const compatVariant = computed(() => {
  switch (props.item?.compatStatus) {
    case 'supported':
      return 'success' as const
    case 'unsupported':
      return 'destructive' as const
    default:
      return 'outline' as const
  }
})
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)" title="视频信息" :description="item?.title">
    <div class="max-h-[60vh] space-y-1 overflow-y-auto pr-1">
      <div class="mb-3 flex items-center gap-2">
        <span class="text-sm text-muted-foreground">兼容性判定：</span>
        <Badge :variant="compatVariant">{{ compatText }}</Badge>
      </div>
      <div
        v-if="item?.compatStatus === 'unsupported' && item.compatReason"
        class="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        {{ item.compatReason }}
      </div>
      <div
        v-for="row in rows"
        :key="row.label"
        class="flex items-start justify-between gap-4 rounded px-2 py-1.5 text-sm odd:bg-accent/40"
      >
        <span class="shrink-0 text-muted-foreground">{{ row.label }}</span>
        <span class="break-all text-right font-medium" :title="row.value">{{ row.value }}</span>
      </div>
    </div>
  </Dialog>
</template>
