<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Github } from 'lucide-vue-next'
import brandIcon from '../../watching_video.png'
import Card from '../components/ui/Card.vue'
import Badge from '../components/ui/Badge.vue'
import Button from '../components/ui/Button.vue'
import DisclaimerDialog from '../components/DisclaimerDialog.vue'
import { getAppPaths, openUrl, openDir, type AppPaths } from '../lib/api'
import { APP_NAME, APP_VERSION, APP_DESCRIPTION } from '../lib/appinfo'
import { toast, errMsg } from '../lib/toast'

const appPaths = ref<AppPaths | null>(null)
const showDisclaimer = ref(false)

const links = [
  {
    name: 'bilibili',
    url: 'https://space.bilibili.com/2199618',
    // bilibili 官方图标（Simple Icons），lucide 未收录
    icon: 'bilibili',
  },
  {
    name: 'GitHub',
    url: 'https://github.com/FlandreSatori/VRchat-LocalCinema',
    icon: 'github',
  },
]

async function openLink(url: string) {
  try {
    await openUrl(url)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

/// 数据路径展示：便携模式显示相对 exe 同级的 ./data 路径；
/// 非便携模式把用户绝对路径前缀替换为环境变量占位符（不泄露具体用户名）
function displayPath(full: string): string {
  const norm = full.replace(/\\/g, '/')
  if (appPaths.value?.portable) {
    const lower = norm.toLowerCase()
    const idx = lower.lastIndexOf('/data/')
    if (idx >= 0) return '.' + norm.slice(idx)
    if (lower.endsWith('/data')) return './data'
    return norm
  }
  const m = /^(.*?)\/AppData\/(Roaming|LocalLow|Local)\//i.exec(norm)
  if (m) {
    const seg = m[2].toLowerCase()
    const prefix =
      seg === 'local' ? '%LOCALAPPDATA%' : seg === 'locallow' ? '%LOCALAPPDATA%/../LocalLow' : '%APPDATA%'
    return `${prefix}/${norm.slice(m[0].length)}`
  }
  return norm
}

/// 各数据存储路径（「打开目录」一键定位）
const pathRows = computed(() =>
  appPaths.value
    ? [
        { label: '数据目录', path: appPaths.value.dataDir },
        { label: '封面缓存', path: appPaths.value.thumbDir },
        { label: '日志目录', path: appPaths.value.logDir },
      ]
    : []
)

async function doOpenDir(path: string) {
  try {
    await openDir(path)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

const techStack = ['Tauri 2', 'Vue 3', 'TypeScript', 'Vite', 'Tailwind CSS 4', 'reka-ui', 'FFmpeg']

onMounted(async () => {
  try {
    appPaths.value = await getAppPaths()
  } catch (e) {
    toast.error(errMsg(e))
  }
})
</script>

<template>
  <div class="mx-auto max-w-3xl space-y-4 p-6">
    <h1 class="text-xl font-semibold">关于</h1>

    <Card class="p-6">
      <div class="flex items-center gap-4">
        <img
          :src="brandIcon"
          :alt="APP_NAME"
          draggable="false"
          class="h-14 w-14 rounded-xl object-contain"
        />
        <div>
          <h2 class="text-lg font-semibold">{{ APP_NAME }}</h2>
          <p class="text-sm text-muted-foreground">版本 {{ APP_VERSION }}</p>
        </div>
      </div>
      <p class="mt-4 text-sm leading-relaxed text-muted-foreground">{{ APP_DESCRIPTION }}</p>
      <div class="mt-4 flex items-center gap-2">
        <button
          v-for="link in links"
          :key="link.name"
          type="button"
          class="flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          :title="link.url"
          @click="openLink(link.url)"
        >
          <svg v-if="link.icon === 'bilibili'" viewBox="0 0 24 24" fill="currentColor" class="h-3.5 w-3.5" aria-hidden="true">
            <path d="M17.813 4.653h.854c1.51.054 2.769.578 3.773 1.574 1.004.995 1.524 2.249 1.56 3.76v7.36c-.036 1.51-.556 2.769-1.56 3.773s-2.262 1.524-3.773 1.56H5.333c-1.51-.036-2.769-.556-3.773-1.56S.036 18.858 0 17.347v-7.36c.036-1.511.556-2.765 1.56-3.76 1.004-.996 2.262-1.52 3.773-1.574h.774l-1.174-1.12a1.234 1.234 0 0 1-.373-.906c0-.356.124-.658.373-.907l.027-.027c.267-.249.573-.373.92-.373.347 0 .653.124.92.373L9.653 4.44c.071.071.134.142.187.213h4.267a.836.836 0 0 1 .16-.213l2.853-2.747c.267-.249.573-.373.92-.373.347 0 .662.151.929.4.267.249.391.551.391.907 0 .355-.124.657-.373.906zM5.333 7.24c-.746.018-1.373.276-1.88.773-.506.498-.769 1.13-.786 1.894v7.52c.017.764.28 1.395.786 1.893.507.498 1.134.756 1.88.773h13.334c.746-.017 1.373-.275 1.88-.773.506-.498.769-1.129.786-1.893v-7.52c-.017-.765-.28-1.396-.786-1.894-.507-.497-1.134-.755-1.88-.773zM8 11.107c.373 0 .684.124.933.373.25.249.383.569.4.96v1.173c-.017.391-.15.711-.4.96-.249.25-.56.374-.933.374s-.684-.125-.933-.374c-.25-.249-.383-.569-.4-.96V12.44c0-.373.129-.689.386-.947.258-.257.574-.386.947-.386zm8 0c.373 0 .684.124.933.373.25.249.383.569.4.96v1.173c-.017.391-.15.711-.4.96-.249.25-.56.374-.933.374s-.684-.125-.933-.374c-.25-.249-.383-.569-.4-.96V12.44c.017-.391.15-.711.4-.96.249-.249.56-.373.933-.373Z" />
          </svg>
          <Github v-else class="h-3.5 w-3.5" aria-hidden="true" />
          {{ link.name }}
        </button>
      </div>
    </Card>

    <Card class="p-5">
      <h2 class="font-medium">技术栈</h2>
      <div class="mt-3 flex flex-wrap gap-2">
        <Badge v-for="t in techStack" :key="t" variant="secondary">{{ t }}</Badge>
      </div>
    </Card>

    <Card class="p-5">
      <h2 class="font-medium">数据位置</h2>
      <div v-if="appPaths" class="mt-3 space-y-2 text-sm">
        <div v-for="row in pathRows" :key="row.label" class="flex items-start justify-between gap-3">
          <span class="shrink-0 pt-1 text-muted-foreground">{{ row.label }}</span>
          <div class="flex min-w-0 items-center gap-2">
            <span class="break-all pt-1 font-mono text-xs">{{ displayPath(row.path) }}</span>
            <Button variant="outline" size="sm" class="shrink-0" @click="doOpenDir(row.path)">
              打开目录
            </Button>
          </div>
        </div>
        <div class="flex justify-between gap-4">
          <span class="shrink-0 text-muted-foreground">便携模式</span>
          <span class="font-mono text-xs">{{ appPaths.portable ? '是' : '否' }}</span>
        </div>
      </div>
      <p v-else class="mt-3 text-sm text-muted-foreground">加载中…</p>
    </Card>

    <Card class="p-5">
      <div class="flex items-center justify-between">
        <h2 class="font-medium">免责声明</h2>
        <Button variant="outline" size="sm" @click="showDisclaimer = true">
          查看详情
        </Button>
      </div>
      <p class="mt-3 text-sm leading-relaxed text-muted-foreground">
        本软件为独立开源项目，仅用于本地媒体分享。请在可信网络环境中使用，勿用于分享未授权内容。使用本软件即表示您已阅读并同意完整的免责声明。
      </p>
    </Card>
  </div>

  <DisclaimerDialog v-model:open="showDisclaimer" />
</template>
