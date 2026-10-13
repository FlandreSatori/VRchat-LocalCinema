<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { FolderOpen, RotateCcw, Trash2, Save } from 'lucide-vue-next'
import { open } from '@tauri-apps/plugin-dialog'
import Card from '../components/ui/Card.vue'
import Button from '../components/ui/Button.vue'
import Input from '../components/ui/Input.vue'
import Label from '../components/ui/Label.vue'
import Select from '../components/ui/Select.vue'
import Switch from '../components/ui/Switch.vue'
import Separator from '../components/ui/Separator.vue'
import {
  getSettings,
  saveSettings,
  resetSettings,
  detectPlayers,
  getAppPaths,
  openDataDir,
  clearThumbnailCache,
  type AppPaths,
  type CloseBehavior,
  type ListenMode,
  type PlayerInfo,
  type Settings,
  type UrlMode,
} from '../lib/api'
import { settingsCache } from '../lib/store'
import { toast, errMsg } from '../lib/toast'

const form = reactive<Settings>({
  listenMode: 'loopback',
  port: 17001,
  defaultUrlMode: 'localhost',
  defaultPlayerPath: '',
  defaultPlayerArgs: '',
  thumbDir: '',
  autostart: false,
  closeBehavior: 'exit',
  ffmpegPath: '',
  ffprobePath: '',
})

const loading = ref(true)
const saving = ref(false)
const appPaths = ref<AppPaths | null>(null)
const players = ref<PlayerInfo[]>([])

const listenModeOptions = [
  { value: 'loopback', label: '本地模式' },
  { value: 'lan', label: '局域网模式' },
]
const urlModeOptions = [
  { value: 'localhost', label: '本地模式' },
  { value: 'lan', label: '局域网模式' },
]
const closeBehaviorOptions = [
  { value: 'exit', label: '退出程序' },
  { value: 'tray', label: '最小化到托盘' },
]

const playerOptions = computed(() => {
  const opts = [
    { value: '', label: '跟随系统默认' },
    ...players.value.map((p) => ({ value: p.path, label: `${p.name}（${p.path}）` })),
  ]
  if (form.defaultPlayerPath && !opts.some((o) => o.value === form.defaultPlayerPath)) {
    opts.push({ value: form.defaultPlayerPath, label: form.defaultPlayerPath })
  }
  return opts
})

watch(() => form.listenMode, (mode) => {
  form.defaultUrlMode = mode === 'lan' ? 'lan' : 'localhost'
})

function setListenMode(v: string) {
  form.listenMode = v as ListenMode
}
function setUrlMode(v: string) {
  form.defaultUrlMode = v as UrlMode
  form.listenMode = v === 'lan' ? 'lan' : 'loopback'
}
function setCloseBehavior(v: string) {
  form.closeBehavior = v as CloseBehavior
}

async function pickExe(title: string): Promise<string | null> {
  const selected = await open({
    multiple: false,
    title,
    filters: [{ name: '可执行文件', extensions: ['exe'] }],
  })
  return typeof selected === 'string' ? selected : null
}

async function browsePlayer() {
  try {
    const p = await pickExe('选择默认播放器')
    if (p) form.defaultPlayerPath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseFfmpeg() {
  try {
    const p = await pickExe('选择 ffmpeg 可执行文件')
    if (p) form.ffmpegPath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}
async function browseFfprobe() {
  try {
    const p = await pickExe('选择 ffprobe 可执行文件')
    if (p) form.ffprobePath = p
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function save() {
  if (saving.value) return
  if (form.port < 1 || form.port > 65535) {
    toast.error('端口必须在 1-65535 之间')
    return
  }
  saving.value = true
  try {
    await saveSettings({ ...form })
    settingsCache.value = { ...form }
    toast.success('设置已保存')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    saving.value = false
  }
}

async function reset() {
  if (!confirm('确定恢复默认设置吗？所有自定义配置将被重置。')) return
  try {
    const s = await resetSettings()
    Object.assign(form, s)
    settingsCache.value = { ...s }
    toast.success('已恢复默认设置')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function clearCache() {
  try {
    const n = await clearThumbnailCache()
    toast.success(`已清理 ${n} 个未使用的封面缓存文件`)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function openDir() {
  try {
    await openDataDir()
  } catch (e) {
    toast.error(errMsg(e))
  }
}

onMounted(async () => {
  try {
    const [s, paths, detected] = await Promise.all([
      getSettings(),
      getAppPaths().catch(() => null),
      detectPlayers().catch(() => [] as PlayerInfo[]),
    ])
    Object.assign(form, s)
    appPaths.value = paths
    players.value = detected
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <div class="mx-auto max-w-3xl space-y-4 p-6 pb-12">
    <div class="flex items-center justify-between">
      <h1 class="text-xl font-semibold">设置</h1>
      <Button :disabled="loading || saving" @click="save">
        <Save class="h-4 w-4" />
        {{ saving ? '保存中…' : '保存设置' }}
      </Button>
    </div>

    <Card class="space-y-4 p-5">
      <div>
        <h2 class="font-medium">HTTP 服务</h2>
        <p class="mt-0.5 text-xs text-muted-foreground">修改监听或端口后服务将自动重启</p>
      </div>
      <div class="grid grid-cols-2 gap-4">
        <div class="space-y-1.5">
          <Label>监听模式</Label>
          <Select :model-value="form.listenMode" :options="listenModeOptions" class="w-full" @update:model-value="setListenMode" />
        </div>
        <div class="space-y-1.5">
          <Label>端口</Label>
          <Input
            type="number"
            :min="1"
            :max="65535"
            :model-value="String(form.port)"
            @update:model-value="form.port = Math.round(Number($event)) || 0"
          />
        </div>
      </div>
      <p class="text-xs text-muted-foreground">
        本地模式仅允许本机访问；局域网模式会自动选择本机可用的物理网卡地址，其他同网络设备可访问。
      </p>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">播放链接</h2>
      <div class="space-y-1.5">
        <Label>默认地址模式</Label>
        <Select :model-value="form.defaultUrlMode" :options="urlModeOptions" class="w-full" @update:model-value="setUrlMode" />
      </div>
      <p class="text-xs text-muted-foreground">局域网模式自动使用本机可用地址；没有可用地址时会提示，不会改用本地地址。</p>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">外部播放器</h2>
      <div class="space-y-1.5">
        <Label>默认播放器</Label>
        <div class="flex gap-2">
          <Select v-model="form.defaultPlayerPath" :options="playerOptions" class="flex-1" />
          <Button variant="outline" @click="browsePlayer">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
      <div class="space-y-1.5">
        <Label>启动参数</Label>
        <Input v-model="form.defaultPlayerArgs" placeholder="{path} 将被替换为文件路径" />
        <p class="text-xs text-muted-foreground">留空则使用播放器默认参数；{path} 占位符会被替换为视频文件路径。</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">封面与缓存</h2>
      <div class="space-y-1.5">
        <Label>封面缓存目录</Label>
        <Input v-model="form.thumbDir" :placeholder="appPaths?.thumbDir ?? '默认目录'" />
        <p class="text-xs text-muted-foreground">留空使用默认目录{{ appPaths ? `：${appPaths.thumbDir}` : '' }}</p>
      </div>
      <Button variant="outline" size="sm" @click="clearCache">
        <Trash2 class="h-4 w-4" />
        清理未使用封面缓存
      </Button>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">FFmpeg</h2>
      <div class="space-y-1.5">
        <Label>ffmpeg 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.ffmpegPath" placeholder="留空自动从 PATH 检测" class="flex-1" />
          <Button variant="outline" @click="browseFfmpeg">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
      <div class="space-y-1.5">
        <Label>ffprobe 路径</Label>
        <div class="flex gap-2">
          <Input v-model="form.ffprobePath" placeholder="留空自动从 PATH 检测" class="flex-1" />
          <Button variant="outline" @click="browseFfprobe">
            <FolderOpen class="h-4 w-4" />
            浏览…
          </Button>
        </div>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">系统</h2>
      <div class="flex items-center justify-between">
        <div>
          <Label>开机启动</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">登录 Windows 后自动启动本应用</p>
        </div>
        <Switch v-model:checked="form.autostart" />
      </div>
      <Separator />
      <div class="flex items-center justify-between gap-4">
        <div>
          <Label>关闭窗口时</Label>
          <p class="mt-0.5 text-xs text-muted-foreground">点击窗口关闭按钮后的行为</p>
        </div>
        <Select :model-value="form.closeBehavior" :options="closeBehaviorOptions" class="w-40" @update:model-value="setCloseBehavior" />
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">其他</h2>
      <div class="flex flex-wrap gap-2">
        <Button variant="outline" @click="openDir">
          <FolderOpen class="h-4 w-4" />
          打开配置目录
        </Button>
        <Button variant="destructive" @click="reset">
          <RotateCcw class="h-4 w-4" />
          恢复默认设置
        </Button>
      </div>
    </Card>
  </div>
</template>
