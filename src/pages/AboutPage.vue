<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Clapperboard } from 'lucide-vue-next'
import Card from '../components/ui/Card.vue'
import Badge from '../components/ui/Badge.vue'
import { getAppPaths, type AppPaths } from '../lib/api'
import { APP_NAME, APP_VERSION, APP_DESCRIPTION } from '../lib/appinfo'
import { toast, errMsg } from '../lib/toast'

const appPaths = ref<AppPaths | null>(null)

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
        <div class="flex h-14 w-14 items-center justify-center rounded-xl bg-primary/10 text-primary">
          <Clapperboard class="h-7 w-7" />
        </div>
        <div>
          <h2 class="text-lg font-semibold">{{ APP_NAME }}</h2>
          <p class="text-sm text-muted-foreground">版本 {{ APP_VERSION }}</p>
        </div>
      </div>
      <p class="mt-4 text-sm leading-relaxed text-muted-foreground">{{ APP_DESCRIPTION }}</p>
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
        <div class="flex justify-between gap-4">
          <span class="shrink-0 text-muted-foreground">数据目录</span>
          <span class="break-all font-mono text-xs">{{ appPaths.dataDir }}</span>
        </div>
        <div class="flex justify-between gap-4">
          <span class="shrink-0 text-muted-foreground">封面缓存</span>
          <span class="break-all font-mono text-xs">{{ appPaths.thumbDir }}</span>
        </div>
        <div class="flex justify-between gap-4">
          <span class="shrink-0 text-muted-foreground">日志目录</span>
          <span class="break-all font-mono text-xs">{{ appPaths.logDir }}</span>
        </div>
        <div class="flex justify-between gap-4">
          <span class="shrink-0 text-muted-foreground">便携模式</span>
          <span class="font-mono text-xs">{{ appPaths.portable ? '是' : '否' }}</span>
        </div>
      </div>
      <p v-else class="mt-3 text-sm text-muted-foreground">加载中…</p>
    </Card>

    <Card class="p-5">
      <h2 class="font-medium">声明</h2>
      <p class="mt-2 text-sm leading-relaxed text-muted-foreground">
        本软件仅用于本地媒体分享，请在可信网络环境中使用，勿用于分享未授权内容。
      </p>
    </Card>
  </div>
</template>
