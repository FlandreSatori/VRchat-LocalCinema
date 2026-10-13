<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Copy, Check, ExternalLink, Laptop, Globe } from 'lucide-vue-next'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import Dialog from './ui/Dialog.vue'
import Button from './ui/Button.vue'
import Input from './ui/Input.vue'
import Label from './ui/Label.vue'
import {
  buildMediaUrl,
  openUrl,
  type MediaItem,
  type UrlMode,
} from '../lib/api'
import { loadSettings } from '../lib/store'
import { toast, errMsg } from '../lib/toast'
import { cn } from '../lib/utils'

const props = defineProps<{
  open: boolean
  item: MediaItem | null
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

const mode = ref<UrlMode>('localhost')
const url = ref('')
const urlError = ref('')
const copied = ref(false)

const modeOptions: { value: UrlMode; label: string; icon: typeof Laptop }[] = [
  { value: 'localhost', label: '本地模式', icon: Laptop },
  { value: 'lan', label: '局域网模式', icon: Globe },
]

const modeHint = computed(() => {
  switch (mode.value) {
    case 'localhost':
      return '127.0.0.1 指向运行播放器的电脑本身。仅当观众本机也运行了本服务时可用。'
    case 'lan':
      return '同一局域网内的设备可访问；请留意系统防火墙可能会阻止连接。'
  }
})

watch(
  () => props.open,
  async (isOpen) => {
    if (!isOpen || !props.item) return
    copied.value = false
    urlError.value = ''
    url.value = ''
    try {
      const settings = await loadSettings()
      mode.value = settings.defaultUrlMode
    } catch {
      mode.value = 'localhost'
    }
    await rebuildUrl()
  }
)

watch(mode, () => {
  if (props.open) rebuildUrl()
})

async function rebuildUrl() {
  url.value = ''
  urlError.value = ''
  if (!props.item) return
  try {
    url.value = await buildMediaUrl(props.item.id, mode.value)
  } catch (e) {
    urlError.value = errMsg(e)
  }
}

async function copy() {
  if (!url.value) return
  try {
    await writeText(url.value)
    copied.value = true
    toast.success('已复制播放链接')
    setTimeout(() => {
      copied.value = false
    }, 1500)
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
          <Check v-if="copied" class="h-4 w-4" />
          <Copy v-else class="h-4 w-4" />
          {{ copied ? '已复制' : '复制' }}
        </Button>
      </div>
    </div>
  </Dialog>
</template>
