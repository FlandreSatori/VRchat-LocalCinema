<script setup lang="ts">
import { computed } from 'vue'
import { AlertTriangle, Download, ExternalLink, FileVideo, ShieldAlert } from 'lucide-vue-next'
import Dialog from './ui/Dialog.vue'
import Button from './ui/Button.vue'
import {
  DOLBY_DECODER_URL,
  DOLBY_DECODER_FORUM_URL,
  openUrl,
  type MediaItem,
} from '../lib/api'
import { toast, errMsg } from '../lib/toast'

const props = defineProps<{
  open: boolean
  item: MediaItem | null
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
  /** 用户选择「转码为 AAC」：由父组件打开转码对话框并预设为「仅转音频」 */
  transcode: []
}>()

const codecName = computed(() =>
  props.item?.audioCodec?.toLowerCase() === 'eac3'
    ? 'E-AC-3（Dolby Digital Plus）'
    : 'AC-3（Dolby Digital）'
)

async function openPage(url: string) {
  try {
    await openUrl(url)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

function doTranscode() {
  emit('update:open', false)
  emit('transcode')
}
</script>

<template>
  <Dialog
    :open="open"
    title="本机缺少音频解码器"
    :description="item?.title ?? ''"
    class="max-w-xl"
    @update:open="emit('update:open', $event)"
  >
    <div class="space-y-4 text-sm">
      <div
        class="flex items-start gap-2.5 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-amber-600"
      >
        <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0" />
        <p>
          该视频的音轨是 <span class="font-medium">{{ codecName }}</span>。Windows 11 24H2
          起系统不再自带此类解码器，VRChat 通过系统解码器播放时会失败（表现为打开后约数秒报错）。
        </p>
      </div>

      <!-- 方案一：用本软件转码为 AAC（保底） -->
      <section class="rounded-md border border-border p-3">
        <h3 class="mb-1 flex items-center gap-1.5 font-medium">
          <FileVideo class="h-4 w-4" />
          方案一 · 用本软件转码为 AAC（推荐，100% 可靠）
        </h3>
        <p class="text-muted-foreground">
          把音轨转成 AAC、视频原样直拷（画质无损、速度快）。转出的新视频可直接在 VRChat 播放，
          无需安装任何系统组件。
        </p>
        <Button size="sm" class="mt-2" @click="doTranscode">
          <FileVideo class="h-3.5 w-3.5" />
          转码为 AAC
        </Button>
      </section>

      <!-- 方案二：自行安装 Dolby 解码器 -->
      <section class="rounded-md border border-border p-3">
        <h3 class="mb-1 flex items-center gap-1.5 font-medium">
          <Download class="h-4 w-4" />
          方案二 · 自行安装 Dolby 解码器
        </h3>
        <p class="text-muted-foreground">
          需要安装微软官方的解码器包
          <code class="rounded bg-muted px-1 py-0.5">Microsoft.DolbyAudioExtensions</code>。但
          <span class="font-medium text-foreground">微软商店搜不到该包</span
          >（只能搜到 OEM 版，实测无效），只能从下面的第三方页面自行下载。
        </p>
        <div class="mt-2 flex flex-wrap gap-2">
          <Button size="sm" variant="outline" @click="openPage(DOLBY_DECODER_URL)">
            <ExternalLink class="h-3.5 w-3.5" />
            打开下载页面
          </Button>
          <Button size="sm" variant="outline" @click="openPage(DOLBY_DECODER_FORUM_URL)">
            <ExternalLink class="h-3.5 w-3.5" />
            查看说明帖
          </Button>
        </div>
        <div
          class="mt-2 flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/5 px-2.5 py-2 text-xs text-muted-foreground"
        >
          <ShieldAlert class="mt-0.5 h-3.5 w-3.5 shrink-0 text-destructive" />
          <p>
            <span class="font-medium text-destructive">安全提示：</span
            >以上页面均为第三方站点，非本软件官方来源，其内容与所下载文件的安全性未经本软件验证，
            请自行判断风险。下载到的 <code class="rounded bg-muted px-1 py-0.5">.appx</code>
            建议先校验数字签名（签名者应为 Microsoft Corporation）再安装。安装后重启 VRChat 即可生效。
          </p>
        </div>
      </section>

      <div class="flex justify-end">
        <Button variant="outline" size="sm" @click="emit('update:open', false)">关闭</Button>
      </div>
    </div>
  </Dialog>
</template>
