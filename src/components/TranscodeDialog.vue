<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { createTranscodeEstimator, type TranscodeEstimate } from '../lib/transcodeEstimator'
import { ChevronDown, Subtitles, Info } from 'lucide-vue-next'
import Dialog from './ui/Dialog.vue'
import Select, { type SelectOption } from './ui/Select.vue'
import Button from './ui/Button.vue'
import {
  getMediaStreams,
  transcodeMedia,
  transcodeOutputExists,
  segmentTranscode,
  type MediaItem,
  type MediaStreamInfo,
  type TranscodeOptions,
} from '../lib/api'
import { toast, errMsg } from '../lib/toast'

const props = defineProps<{
  open: boolean
  item: MediaItem | null
  /** 打开时默认套用的转码方式（如从「缺解码器」提示进入时预设为 'audio'） */
  defaultStrategy?: 'smart' | 'audio' | 'full' | 'custom'
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

const loading = ref(false)
const submitting = ref(false)
const estimate = ref<TranscodeEstimate | null>(null)
const estimator = createTranscodeEstimator()
let loadGeneration = 0
let submitGeneration = 0
const loadError = ref<string | null>(null)
const streams = ref<MediaStreamInfo[]>([])
/// 已存在转码产物 → 展示「覆盖」开关
const outputExists = ref(false)
const advancedOpen = ref(false)

// ===== 表单状态（全部为字符串，便于与 Select 直接绑定）=====
/// 分段与全部转码共用同一套参数
const scope = ref('full')
/// 分段转码第一段时长（分钟），默认按视频时长的 10% 向上取整到 5 分钟
const firstDuration = ref('recommended')
/// 转码方式：智能推荐 / 仅转音频 / 完整转码 / 自定义
const strategy = ref('smart')
/// 视频轨：'' = 自动（第一路视频）
const videoStream = ref('')
/// 音频轨：'' = 自动（默认音轨）；'none' = 不保留音频
const audioStream = ref('')
/// 主选择仅负责烧录，其他输出方式在高级参数中选择
const subtitleChoice = ref('drop')
const videoMode = ref('h264')
const preset = ref('medium')
const crf = ref('20')
const resolution = ref('original')
const pixFmt = ref('auto')
const audioMode = ref('aac')
const audioBitrate = ref('192k')
const audioChannels = ref('0')
const audioSampleRate = ref('0')
const faststart = ref('on')
/// 已存在产物时是否覆盖：'yes' | 'no'（统一用下拉，避免引入开关组件）
const overwrite = ref('no')

/// 正在套用预设（此时各字段的变更不应把方式改回「自定义」）
let applying = false

const videoStreams = computed(() => streams.value.filter((s) => s.kind === 'video'))
const audioStreams = computed(() => streams.value.filter((s) => s.kind === 'audio'))
const subtitleStreams = computed(() => streams.value.filter((s) => s.kind === 'subtitle'))

/// 流的展示文本（镜像后端 StreamInfo::describe 的格式；invoke 返回的是纯 JSON，没有方法可调）
function describeStream(s: MediaStreamInfo): string {
  const parts: string[] = []
  if (s.kind === 'video') {
    let c = (s.codec || '').toUpperCase()
    if (c === 'HEVC') c = 'H.265'
    else if (c === 'H264') c = 'H.264'
    else if (c === 'AV1') c = 'AV1'
    parts.push(c)
    if (s.profile) parts.push(s.profile)
    if (s.width && s.height) parts.push(`${s.width}×${s.height}`)
    if (s.pixFmt) parts.push(s.pixFmt)
  } else if (s.kind === 'audio') {
    parts.push((s.codec || '').toUpperCase())
    if (s.profile) parts.push(s.profile)
    if (s.channels) parts.push(`${s.channels}ch`)
    if (s.sampleRate) parts.push(`${s.sampleRate}Hz`)
  } else {
    parts.push(s.codec || 'subtitle')
    if (s.language) parts.push(s.language)
    if (s.title) parts.push(s.title)
  }
  return parts.filter(Boolean).join(' ')
}

function currentVideo(): MediaStreamInfo | undefined {
  return videoStream.value !== ''
    ? videoStreams.value.find((s) => String(s.index) === videoStream.value)
    : videoStreams.value[0]
}
function currentAudio(): MediaStreamInfo | undefined {
  return audioStream.value !== '' && audioStream.value !== 'none'
    ? audioStreams.value.find((s) => String(s.index) === audioStream.value)
    : audioStreams.value.find((s) => s.isDefault) ?? audioStreams.value[0]
}
const videoCopyable = computed(() => currentVideo()?.copyable ?? false)

// ===== 选项列表 =====
const totalMs = computed(() => {
  const duration = props.item?.durationMs
  return duration != null && Number.isFinite(duration) && duration > 0 ? duration : 0
})
const recommendedMs = computed(() => totalMs.value
  ? Math.min(totalMs.value, Math.ceil(totalMs.value * 0.1 / 300_000) * 300_000)
  : 0)
const firstMs = computed(() => firstDuration.value === 'recommended' ? recommendedMs.value : Number(firstDuration.value))
function durationLabel(ms: number): string {
  const seconds = Math.ceil(ms / 1000)
  const minutes = Math.floor(seconds / 60)
  return minutes ? `${minutes} 分钟${seconds % 60 ? ` ${seconds % 60} 秒` : ''}` : `${seconds} 秒`
}
const firstDurationOptions = computed<SelectOption[]>(() => {
  if (!totalMs.value) return []
  const values = [1, 2, 3, 5, 10, 15, 20, 30, 45, 60, 90, 120].map(m => m * 60_000)
  if (totalMs.value < 300_000) values.push(...[1, 2, 3, 5, 10, 15, 20, 30, 45].map(s => s * 1000))
  values.push(totalMs.value)
  return [
    { value: 'recommended', label: `推荐 · ${durationLabel(recommendedMs.value)}（总长 10% 向上取 5 分钟，不超过总长）` },
    ...[...new Set(values)].filter(ms => ms <= totalMs.value).sort((a, b) => a - b)
      .map(ms => ({ value: String(ms), label: `${durationLabel(ms)}${ms === totalMs.value ? '（全长）' : ''}` })),
  ]
})
watch(firstDurationOptions, options => {
  if (!options.some(option => option.value === firstDuration.value)) firstDuration.value = 'recommended'
})
const scopeOptions = computed<SelectOption[]>(() => [
  { value: 'full', label: '全部转码（整个视频一次转完）' },
  { value: 'segment', label: totalMs.value ? '分段转码（首段完成即可观看，后台转余下）' : '分段转码（时长未知，请先刷新媒体信息）' },
])


const strategyOptions: SelectOption[] = [
  { value: 'smart', label: '智能推荐（自动选最优方案）' },
  { value: 'audio', label: '仅转音频（视频直拷，画质无损、速度快）' },
  { value: 'full', label: '完整转码（H.264 + AAC）' },
  { value: 'custom', label: '自定义（下面各项自行选择）' },
]

const videoOptions = computed<SelectOption[]>(() => {
  if (videoStreams.value.length === 0) return [{ value: '', label: '（无流信息，按自动处理）' }]
  if (videoStreams.value.length === 1)
    return [{ value: '', label: `自动（${describeStream(videoStreams.value[0])}）` }]
  return [
    { value: '', label: '自动（第一路视频）' },
    ...videoStreams.value.map((s) => ({
      value: String(s.index),
      label: `#${s.index} ${describeStream(s)}${s.copyable ? '' : '（需重编码）'}`,
    })),
  ]
})

const audioOptions = computed<SelectOption[]>(() => {
  if (audioStreams.value.length === 0) return [{ value: 'none', label: '无音轨' }]
  return [
    { value: '', label: '自动（默认音轨）' },
    ...audioStreams.value.map((s) => ({
      value: String(s.index),
      label: `#${s.index} ${describeStream(s)}${s.isDefault ? ' · 默认' : ''}${s.copyable ? '' : ' · 需转码'}`,
    })),
    { value: 'none', label: '不保留音频' },
  ]
})

const subtitleOptions = computed<SelectOption[]>(() => [
  { value: 'drop', label: '不烧录字幕' },
  ...subtitleStreams.value.filter(s => s.textSubtitle || s.graphicSubtitle).map(s => ({
    value: `burn:${s.index}`, label: `烧录 · #${s.index} ${describeStream(s)}`,
  })),
])

// ===== 高级参数下拉选项 =====
const presetOptions: SelectOption[] = [
  { value: 'ultrafast', label: 'ultrafast（最快，体积最大）' },
  { value: 'superfast', label: 'superfast（极快）' },
  { value: 'veryfast', label: 'veryfast（很快）' },
  { value: 'faster', label: 'faster' },
  { value: 'fast', label: 'fast' },
  { value: 'medium', label: 'medium（默认，均衡）' },
  { value: 'slow', label: 'slow（更清晰，更慢）' },
  { value: 'slower', label: 'slower（画质优先）' },
  { value: 'veryslow', label: 'veryslow（最慢，体积最小）' },
]
const crfOptions: SelectOption[] = [
  { value: '16', label: '16（接近无损，体积大）' },
  { value: '18', label: '18（高质量，推荐）' },
  { value: '20', label: '20（默认）' },
  { value: '22', label: '22（均衡）' },
  { value: '24', label: '24（体积较小）' },
  { value: '26', label: '26（省空间）' },
  { value: '28', label: '28（体积优先）' },
  { value: '32', label: '32（很低画质）' },
]
const resolutionOptions: SelectOption[] = [
  { value: 'original', label: '原始分辨率（不缩放）' },
  { value: '2160', label: '2160p (4K)' },
  { value: '1440', label: '1440p (2K)' },
  { value: '1080', label: '1080p (全高清)' },
  { value: '720', label: '720p (高清)' },
  { value: '480', label: '480p (流畅)' },
]
const pixFmtOptions: SelectOption[] = [
  { value: 'auto', label: '自动（按编码器默认）' },
  { value: 'yuv420p', label: 'yuv420p（8bit，兼容性最好）' },
  { value: 'yuv420p10le', label: 'yuv420p10le（10bit，H.265 用）' },
]
const audioBitrateOptions: SelectOption[] = [
  { value: '96k', label: '96 kbps（省空间）' },
  { value: '128k', label: '128 kbps' },
  { value: '160k', label: '160 kbps' },
  { value: '192k', label: '192 kbps（默认）' },
  { value: '256k', label: '256 kbps（高质量）' },
  { value: '320k', label: '320 kbps（最高）' },
]
const audioChannelsOptions: SelectOption[] = [
  { value: '0', label: '保持原样' },
  { value: '1', label: '单声道 (1.0)' },
  { value: '2', label: '立体声 (2.0)' },
  { value: '6', label: '5.1 环绕' },
]
const audioSampleRateOptions: SelectOption[] = [
  { value: '0', label: '保持原样' },
  { value: '48000', label: '48000 Hz' },
  { value: '44100', label: '44100 Hz' },
  { value: '32000', label: '32000 Hz' },
  { value: '22050', label: '22050 Hz' },
]
const videoModeOptions = computed<SelectOption[]>(() => [
  { value: 'copy', label: videoCopyable.value ? '直拷（不改画面，需可直拷）' : '直拷（当前视频不可直拷，将自动重编码）' },
  { value: 'h264', label: 'H.264（兼容性最好）' },
  { value: 'h265', label: 'H.265 / HEVC（更小体积）' },
  { value: 'av1', label: 'AV1（体积最小，编码慢）' },
])
const audioModeOptions: SelectOption[] = [
  { value: 'copy', label: '直拷（不改音频，需可直拷）' },
  { value: 'aac', label: 'AAC（推荐，兼容性好）' },
]
const faststartOptions: SelectOption[] = [
  { value: 'on', label: '开启（推荐，网络播放更流畅）' },
  { value: 'off', label: '关闭' },
]
const overwriteOptions: SelectOption[] = [
  { value: 'no', label: '不覆盖（生成新文件）' },
  { value: 'yes', label: '覆盖已有产物' },
]

// ===== 预设套用 =====
function applyStrategy(s: string) {
  applying = true
  strategy.value = s
  videoStream.value = ''
  audioStream.value = ''
  if (s === 'smart') {
    // 让 VRChat 看到字幕只能烧录，而烧录必须重编码视频；文本字幕优先，无文本时退而用图形字幕
    const textSub = subtitleStreams.value.find(s => s.textSubtitle)
    const burnSub = textSub ?? subtitleStreams.value.find(s => s.graphicSubtitle)
    if (burnSub) {
      subtitleChoice.value = `burn:${burnSub.index}`
      videoMode.value = 'h264'
      resolution.value = 'original'
    } else {
      subtitleChoice.value = 'drop'
      videoMode.value = videoCopyable.value ? 'copy' : 'h264'
      resolution.value = 'original'
    }
    audioMode.value = currentAudio()?.copyable ? 'copy' : 'aac'
  } else if (s === 'audio') {
    videoMode.value = 'copy'
    audioMode.value = 'aac'
    subtitleChoice.value = 'drop'
    // 直拷不改画面，分辨率必须保持原始
    resolution.value = 'original'
  } else if (s === 'full') {
    videoMode.value = 'h264'
    audioMode.value = 'aac'
    subtitleChoice.value = 'drop'
  }
  applying = false
}

// 用户改动任一选项即视为「自定义」
watch(
  [
    videoStream,
    audioStream,
    subtitleChoice,
    videoMode,
    preset,
    crf,
    resolution,
    pixFmt,
    audioMode,
    audioBitrate,
    audioChannels,
    audioSampleRate,
    faststart,
  ],
  () => {
    if (!applying) strategy.value = 'custom'
  },
  { flush: 'sync' }
)
watch(
  strategy,
  (s) => {
    if (applying) return
    if (!['smart', 'audio', 'full'].includes(s)) return
    applyStrategy(s)
  },
  { flush: 'sync' }
)

/// 直拷与缩放不能并存：直拷要求画面保持原样，改变分辨率必须重编码
const resolutionLocked = computed(() => videoMode.value === 'copy')
watch(videoMode, (mode) => {
  if (mode === 'copy') resolution.value = 'original'
}, { flush: 'sync' })

// 打开、切媒体和关闭均使旧请求失效；迟到结果不能覆盖当前表单。
watch(
  [() => props.open, () => props.item?.id],
  async ([open, id]) => {
    const ticket = ++loadGeneration
    ++submitGeneration
    submitting.value = false
    void estimator.cancel()
    estimate.value = null
    loading.value = false
    if (!open || !id) return
    const current = () => ticket === loadGeneration && props.open && props.item?.id === id
    loading.value = true
    loadError.value = null
    streams.value = []
    outputExists.value = false
    advancedOpen.value = false
    overwrite.value = 'no'
    scope.value = 'full'
    firstDuration.value = 'recommended'
    const [streamResult, exists] = await Promise.all([
      getMediaStreams(id).then(value => ({ value, error: null as string | null }))
        .catch(error => ({ value: [] as MediaStreamInfo[], error: errMsg(error) })),
      transcodeOutputExists(id).catch(() => false),
    ])
    if (!current()) return
    streams.value = streamResult.value
    loadError.value = streamResult.error
    outputExists.value = exists
    applyStrategy(props.defaultStrategy ?? 'smart')
    loading.value = false
  },
  { immediate: true }
)

// ===== 结果说明 =====
const planSummary = computed(() => {
  const parts: string[] = [scope.value === 'segment' ? '分段转码' : '整段转码']
  const v = videoMode.value === 'copy' ? '视频直拷（无损）' : `视频 ${videoMode.value === 'h265' ? 'H.265' : videoMode.value.toUpperCase()}`
  parts.push(videoMode.value === 'copy' && subtitleChoice.value.startsWith('burn') ? '视频 H.264（字幕烧录需重编码）' : v)
  if (audioStream.value === 'none') parts.push('无音频')
  else parts.push(audioMode.value === 'copy' ? '音频直拷' : `音频 AAC ${audioBitrate.value}`)
  const [mode, index] = subtitleChoice.value.split(':')
  parts.push(
    mode === 'burn'
      ? '字幕烧录进画面'
      : '不带字幕'
  )
  if (mode !== 'drop') parts.push(`字幕 #${index}`)
  return parts.join(' · ')
})

const burnNeedsReencode = computed(
  () => subtitleChoice.value.startsWith('burn') && videoMode.value === 'copy'
)

// ===== 提交 =====
function collect(): TranscodeOptions {
  const [mode, idx] = subtitleChoice.value.split(':')
  const isDrop = mode !== 'burn'
  const subStream = isDrop
    ? undefined
    : subtitleStreams.value.find((s) => String(s.index) === idx)
  return {
    videoStream: videoStream.value === '' ? null : Number(videoStream.value),
    includeAudio: audioStream.value !== 'none',
    audioStream: audioStream.value === '' || audioStream.value === 'none' ? null : Number(audioStream.value),
    subtitleMode: isDrop ? 'drop' : 'burn',
    subtitleStream: isDrop ? null : Number(idx),
    subtitleGraphic: subStream?.graphicSubtitle ?? null,
    videoMode: videoMode.value as TranscodeOptions['videoMode'],
    resolution: videoMode.value === 'copy' ? 'original' : resolution.value,
    audioMode: audioMode.value as TranscodeOptions['audioMode'],
    crf: Number(crf.value),
    preset: preset.value,
    pixFmt: pixFmt.value,
    audioBitrate: audioBitrate.value,
    audioChannels: Number(audioChannels.value),
    audioSampleRate: Number(audioSampleRate.value),
    faststart: faststart.value === 'on',
  }
}

const collectedOptions = computed(collect)
watch(
  [() => props.open, () => props.item?.id, loading, submitting, collectedOptions],
  ([open, id, busy, sending, options]) => {
    if (!open || !id || busy || sending) {
      void estimator.cancel()
      return
    }
    void estimator.estimate(id, options, value => { estimate.value = value })
  },
  { immediate: true }
)
const estimateStatus = computed(() => {
  if (!estimate.value) return '等待源信息'
  return { loading: '读取源参数中', waiting: '经验估算 · 等待采样', probing: '经验估算 · 正在探测', complete: estimate.value.layer === 'L2' ? '短样本参考' : '经验估算', failed: '探测不可用' }[estimate.value.status]
})
const estimateSpeed = computed(() => estimate.value?.duration && estimate.value.seconds
  ? estimate.value.duration / estimate.value.seconds : null)
function estimateTime(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return '暂不可用'
  const rounded = Math.max(1, Math.ceil(seconds))
  if (rounded >= 3600) return `${Math.floor(rounded / 3600)} 小时 ${Math.ceil(rounded % 3600 / 60)} 分钟`
  if (rounded >= 60) return `${Math.floor(rounded / 60)} 分 ${rounded % 60} 秒`
  return `${rounded} 秒`
}
const estimateRange = computed(() => {
  if (estimate.value?.seconds == null) return ''
  const margin = estimate.value.layer === 'L2' ? 0.1 : 0.35
  return `${estimateTime(estimate.value.seconds * (1 - margin))} – ${estimateTime(estimate.value.seconds * (1 + margin))}`
})
const firstEstimateSeconds = computed(() => {
  const value = estimate.value
  return value?.seconds != null && value.duration && firstMs.value > 0
    ? value.seconds * Math.min(1, firstMs.value / 1000 / value.duration) : null
})
function closeDialog() {
  ++loadGeneration
  ++submitGeneration
  void estimator.cancel()
  emit('update:open', false)
}
onBeforeUnmount(() => {
  ++loadGeneration
  ++submitGeneration
  void estimator.dispose()
})

async function submit() {
  if (!props.item || loading.value || submitting.value) return
  const id = props.item.id
  const segmented = scope.value === 'segment'
  const first = firstMs.value
  if (segmented && (!Number.isFinite(first) || first <= 0)) {
    toast.error('视频时长未知，无法分段转码，请先「刷新媒体信息」')
    return
  }
  const options = collect()
  const replace = overwrite.value === 'yes'
  const ticket = ++submitGeneration
  const current = () => ticket === submitGeneration && props.open && props.item?.id === id
  submitting.value = true
  try {
    // 等待旧探测退出后再提交，避免转码与残留采样争抢 CPU。
    await estimator.cancel()
    if (!current()) return
    if (segmented) {
      await segmentTranscode(id, first, replace, options)
    } else {
      await transcodeMedia(id, replace, options)
    }
    toast.info(segmented ? '已加入转码队列：第一段完成后会自动弹出复制链接窗口' : '已加入转码队列，可在「任务」页查看进度或调整顺序')
    if (current()) emit('update:open', false)
  } catch (error) {
    if (current()) {
      toast.error(errMsg(error))
      submitting.value = false
    }
  }
}
</script>

<template>
  <Dialog
    :open="open"
    title="转码设置"
    :description="item?.title ?? ''"
    class="max-w-2xl"
    @update:open="$event ? emit('update:open', true) : closeDialog()"
  >
    <div class="space-y-4">
      <p v-if="loading" class="text-sm text-muted-foreground">正在读取视频的音轨 / 字幕轨信息…</p>

      <!-- 转码范围 -->
      <div>
        <div class="mb-1 text-xs font-medium text-muted-foreground">转码范围</div>
        <Select v-model="scope" :options="scopeOptions" :disabled="loading" />
      </div>

      <fieldset :disabled="loading || submitting" class="min-w-0 space-y-4">
        <!-- 转码方式 -->
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">转码方式</div>
          <Select v-model="strategy" :options="strategyOptions" />
        </div>

        <p v-if="loadError" class="rounded-md border border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          {{ loadError }}（下方选项按「自动」处理）
        </p>

      <!-- 轨道选择 -->
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">视频轨</div>
          <Select v-model="videoStream" :options="videoOptions" :disabled="loading" />
        </div>
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">音频轨</div>
          <Select v-model="audioStream" :options="audioOptions" :disabled="loading" />
        </div>
        <div class="sm:col-span-2">
          <div class="mb-1 flex items-center gap-1 text-xs font-medium text-muted-foreground">
            <Subtitles class="h-3.5 w-3.5" />
            字幕处理
          </div>
          <Select v-model="subtitleChoice" :options="subtitleOptions" :disabled="loading" />
          <p class="mt-1 flex items-start gap-1 text-xs text-muted-foreground">
        <Info class="mt-0.5 h-3 w-3 shrink-0" />
        VRChat 不能选择字幕轨，只有「烧录进画面」才能在 VRChat 中看到字幕。
      </p>
        </div>
      </div>

      <div v-if="scope === 'segment'" class="rounded-md border border-border px-3 py-2">
        <div class="mb-1 text-xs font-medium text-muted-foreground">第一段时长</div>
        <Select v-model="firstDuration" :options="firstDurationOptions" :disabled="loading" />
        <p class="mt-1 text-xs text-muted-foreground">默认按总时长 10% 向上取整到 5 分钟，不超过全长；可手动调整。</p>
      </div>

      <!-- 高级参数 -->
      <div class="rounded-md border border-border">
        <button
          type="button"
          class="flex w-full items-center justify-between px-3 py-2 text-sm font-medium"
          @click="advancedOpen = !advancedOpen"
        >
          <span>高级参数</span>
          <ChevronDown class="h-4 w-4 transition-transform" :class="advancedOpen ? 'rotate-180' : ''" />
        </button>
        <div v-show="advancedOpen" class="grid grid-cols-1 gap-3 border-t border-border p-3 sm:grid-cols-2">
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">视频编码</div>
            <Select v-model="videoMode" :options="videoModeOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">编码预设</div>
            <Select v-model="preset" :options="presetOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">画质（CRF）</div>
            <Select v-model="crf" :options="crfOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">分辨率</div>
            <Select v-model="resolution" :options="resolutionOptions" :disabled="resolutionLocked" />
            <p v-if="resolutionLocked" class="mt-1 text-xs text-muted-foreground">视频直拷时画面不变，无法缩放分辨率。</p>
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">像素格式</div>
            <Select v-model="pixFmt" :options="pixFmtOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">音频编码</div>
            <Select v-model="audioMode" :options="audioModeOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">音频码率</div>
            <Select v-model="audioBitrate" :options="audioBitrateOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">声道</div>
            <Select v-model="audioChannels" :options="audioChannelsOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">采样率</div>
            <Select v-model="audioSampleRate" :options="audioSampleRateOptions" />
          </div>
          <div>
            <div class="mb-1 text-xs font-medium text-muted-foreground">网络边下边播优化（faststart）</div>
            <Select v-model="faststart" :options="faststartOptions" />
          </div>
        </div>
      </div>
      </fieldset>

      <!-- 覆盖 / 提示 -->
      <div v-if="outputExists" class="grid grid-cols-1 items-center gap-1 rounded-md border border-border px-3 py-2 sm:grid-cols-[1fr_auto]">
        <div class="text-xs text-muted-foreground">已存在转码产物，是否覆盖（不覆盖则生成新文件）</div>
        <Select v-model="overwrite" :options="overwriteOptions" class="w-44" />
      </div>

      <div class="space-y-1">
        <p class="flex items-start gap-1 rounded-md bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          <Info class="mt-0.5 h-3 w-3 shrink-0" />
          将执行：{{ planSummary }}
        </p>
        <p
          v-if="burnNeedsReencode"
          class="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-600"
        >
          烧录字幕需要重新编码视频，将改用 H.264 而非直拷。
        </p>
      </div>

      <div v-if="estimate" class="rounded-md border border-border px-3 py-2 text-xs">
        <div class="flex items-center justify-between gap-3">
          <span class="font-medium">预计转码耗时</span>
          <span class="text-muted-foreground">{{ estimateStatus }}</span>
        </div>
        <div v-if="estimate.seconds != null" class="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-muted-foreground">
          <span>{{ estimateTime(estimate.seconds) }}</span>
          <span v-if="estimateRange">参考 {{ estimateRange }}</span>
          <span v-if="estimateSpeed">约 {{ estimateSpeed.toFixed(1) }} 倍速</span>
          <span v-if="estimate.fps">{{ Math.round(estimate.fps) }} fps</span>
        </div>
        <p class="mt-1 text-muted-foreground">{{ estimate.reason }}</p>
        <p v-if="scope === 'segment' && firstEstimateSeconds != null" class="mt-1 text-muted-foreground">
          第一段预计 {{ estimateTime(firstEstimateSeconds) }}
        </p>
      </div>

      <div class="flex justify-end gap-2 pt-1">
        <Button variant="outline" size="sm" @click="emit('update:open', false)">取消</Button>
        <Button size="sm" :disabled="loading" @click="submit">开始转码</Button>
      </div>
    </div>
  </Dialog>
</template>
