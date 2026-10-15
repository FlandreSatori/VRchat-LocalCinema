<script setup lang="ts">
import { ref, watch } from 'vue'
import Dialog from './ui/Dialog.vue'
import Select, { type SelectOption } from './ui/Select.vue'
import Button from './ui/Button.vue'
import type { TranscodeOptions } from '../lib/api'

const props = defineProps<{
  open: boolean
  /// 选中数量，仅用于文案
  count: number
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
  submit: [options: TranscodeOptions]
}>()

// ===== 表单状态（全部为字符串，便于与 Select 直接绑定）=====
/// 转码方式预设：完整转码 / 仅转音频 / 自定义
const strategy = ref('full')
const videoMode = ref('h264')
const resolution = ref('original')
const crf = ref('20')
const preset = ref('medium')
const pixFmt = ref('auto')
const audioMode = ref('aac')
const audioBitrate = ref('192k')
const audioChannels = ref('0')
const audioSampleRate = ref('0')
const faststart = ref('on')
const subtitleMode = ref('drop')

/// 正在套用预设（此时各字段的变更不应把方式改回「自定义」）
let applying = false

// ===== 选项列表 =====
const strategyOptions: SelectOption[] = [
  { value: 'full', label: '完整转码（H.264 + AAC）' },
  { value: 'audio', label: '仅转音频（视频直拷，画质无损、速度快）' },
  { value: 'custom', label: '自定义（下面各项自行选择）' },
]
const videoModeOptions: SelectOption[] = [
  { value: 'copy', label: '直拷（不改画面，需可直拷）' },
  { value: 'h264', label: 'H.264（兼容性最好）' },
  { value: 'h265', label: 'H.265 / HEVC（更小体积）' },
  { value: 'av1', label: 'AV1（体积最小，编码慢）' },
]
const resolutionOptions: SelectOption[] = [
  { value: 'original', label: '原始分辨率（不缩放）' },
  { value: '2160', label: '2160p (4K)' },
  { value: '1440', label: '1440p (2K)' },
  { value: '1080', label: '1080p (全高清)' },
  { value: '720', label: '720p (高清)' },
  { value: '480', label: '480p (流畅)' },
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
const pixFmtOptions: SelectOption[] = [
  { value: 'auto', label: '自动（按编码器默认）' },
  { value: 'yuv420p', label: 'yuv420p（8bit，兼容性最好）' },
  { value: 'yuv420p10le', label: 'yuv420p10le（10bit，H.265 用）' },
]
const audioModeOptions: SelectOption[] = [
  { value: 'copy', label: '直拷（不改音频，需可直拷）' },
  { value: 'aac', label: 'AAC（推荐，兼容性好）' },
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
const faststartOptions: SelectOption[] = [
  { value: 'on', label: '开启（推荐，网络播放更流畅）' },
  { value: 'off', label: '关闭' },
]
const subtitleOptions: SelectOption[] = [
  { value: 'drop', label: '不烧录字幕' },
  { value: 'burn', label: '烧录字幕（自动选第一条可用字幕轨）' },
]

// ===== 预设套用 =====
function applyStrategy(s: string) {
  applying = true
  strategy.value = s
  if (s === 'audio') {
    videoMode.value = 'copy'
    audioMode.value = 'aac'
    // 直拷不改画面，分辨率必须保持原始
    resolution.value = 'original'
    subtitleMode.value = 'drop'
  } else if (s === 'full') {
    videoMode.value = 'h264'
    audioMode.value = 'aac'
    subtitleMode.value = 'drop'
  }
  applying = false
}

function resetToDefaults() {
  applying = true
  strategy.value = 'full'
  videoMode.value = 'h264'
  resolution.value = 'original'
  crf.value = '20'
  preset.value = 'medium'
  pixFmt.value = 'auto'
  audioMode.value = 'aac'
  audioBitrate.value = '192k'
  audioChannels.value = '0'
  audioSampleRate.value = '0'
  faststart.value = 'on'
  subtitleMode.value = 'drop'
  applying = false
}

// 用户改动任一选项即视为「自定义」
watch(
  [videoMode, resolution, crf, preset, pixFmt, audioMode, audioBitrate, audioChannels, audioSampleRate, faststart, subtitleMode],
  () => {
    if (!applying) strategy.value = 'custom'
  },
  { flush: 'sync' }
)
watch(
  strategy,
  (s) => {
    if (applying) return
    if (!['full', 'audio'].includes(s)) return
    applyStrategy(s)
  },
  { flush: 'sync' }
)

/// 直拷与缩放不能并存：直拷要求画面保持原样，改变分辨率必须重编码
const resolutionLocked = ref(false)
watch(videoMode, (mode) => {
  resolutionLocked.value = mode === 'copy'
  if (mode === 'copy') resolution.value = 'original'
}, { flush: 'sync' })

// 每次打开都重置为默认值，避免残留上一次的自定义设置
watch(() => props.open, (open) => {
  if (open) resetToDefaults()
})

// ===== 提交 =====
function submit() {
  emit('submit', {
    videoStream: null,
    audioStream: null,
    includeAudio: true,
    subtitleMode: subtitleMode.value as TranscodeOptions['subtitleMode'],
    subtitleStream: null,
    subtitleGraphic: null,
    videoMode: videoMode.value as TranscodeOptions['videoMode'],
    resolution: videoMode.value === 'copy' ? 'original' : resolution.value,
    crf: Number(crf.value),
    preset: preset.value,
    pixFmt: pixFmt.value,
    audioMode: audioMode.value as TranscodeOptions['audioMode'],
    audioBitrate: audioBitrate.value,
    audioChannels: Number(audioChannels.value),
    audioSampleRate: Number(audioSampleRate.value),
    faststart: faststart.value === 'on',
  })
  emit('update:open', false)
}
</script>

<template>
  <Dialog
    :open="open"
    title="批量转码设置"
    :description="`将对选中的 ${count} 个视频应用同一套设置`"
    class="max-w-2xl"
    @update:open="$event ? emit('update:open', true) : emit('update:open', false)"
  >
    <div class="space-y-4">
      <div>
        <div class="mb-1 text-xs font-medium text-muted-foreground">转码方式</div>
        <Select v-model="strategy" :options="strategyOptions" />
      </div>

      <!-- 视频参数 -->
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">视频编码</div>
          <Select v-model="videoMode" :options="videoModeOptions" />
        </div>
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">分辨率</div>
          <Select v-model="resolution" :options="resolutionOptions" :disabled="resolutionLocked" />
          <p v-if="resolutionLocked" class="mt-1 text-xs text-muted-foreground">视频直拷时画面不变，无法缩放分辨率。</p>
        </div>
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">画质（CRF）</div>
          <Select v-model="crf" :options="crfOptions" />
        </div>
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">编码预设</div>
          <Select v-model="preset" :options="presetOptions" />
        </div>
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">像素格式</div>
          <Select v-model="pixFmt" :options="pixFmtOptions" />
        </div>
        <div>
          <div class="mb-1 text-xs font-medium text-muted-foreground">网络边下边播优化（faststart）</div>
          <Select v-model="faststart" :options="faststartOptions" />
        </div>
      </div>

      <!-- 音频参数 -->
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
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
      </div>

      <!-- 字幕 -->
      <div>
        <div class="mb-1 text-xs font-medium text-muted-foreground">字幕处理</div>
        <Select v-model="subtitleMode" :options="subtitleOptions" />
      </div>

      <div class="flex justify-end gap-2 pt-1">
        <Button variant="outline" size="sm" @click="emit('update:open', false)">取消</Button>
        <Button size="sm" @click="submit">开始批量转码</Button>
      </div>
    </div>
  </Dialog>
</template>
