<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Play, Square, RotateCw, RefreshCw } from 'lucide-vue-next'
import Card from '../components/ui/Card.vue'
import Button from '../components/ui/Button.vue'
import Badge from '../components/ui/Badge.vue'
import CollapsibleCard from '../components/ui/CollapsibleCard.vue'
import CloudflareTunnelCard from '../components/CloudflareTunnelCard.vue'
import FrpCard from '../components/FrpCard.vue'
import { startServer, stopServer, restartServer, getRecentLogs, type LogEntry } from '../lib/api'
import { serverStore, applyServerStatus } from '../lib/store'
import { formatTime } from '../lib/format'
import { toast, errMsg } from '../lib/toast'

const operating = ref<'' | 'start' | 'stop' | 'restart'>('')
const logs = ref<LogEntry[]>([])
const logsLoading = ref(false)

/// 「公网隧道」分组默认展开；两条隧道（Cloudflare / frp）以分隔线并列，同时只能运行一个
const tunnelsOpen = ref(true)

const statusBadge = computed(() => {
  if (serverStore.error && !serverStore.running) return { variant: 'destructive' as const, text: '错误' }
  return serverStore.running
    ? { variant: 'success' as const, text: '运行中' }
    : { variant: 'secondary' as const, text: '已停止' }
})

async function operate(kind: 'start' | 'stop' | 'restart') {
  if (operating.value) return
  operating.value = kind
  try {
    const status =
      kind === 'start' ? await startServer() : kind === 'stop' ? await stopServer() : await restartServer()
    applyServerStatus(status)
    toast.success(kind === 'start' ? '服务已启动' : kind === 'stop' ? '服务已停止' : '服务已重启')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    operating.value = ''
  }
}

async function loadLogs() {
  logsLoading.value = true
  try {
    logs.value = await getRecentLogs(100)
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    logsLoading.value = false
  }
}

function levelVariant(level: string) {
  switch (level.toUpperCase()) {
    case 'ERROR':
      return 'destructive' as const
    case 'WARN':
      return 'warning' as const
    case 'DEBUG':
    case 'TRACE':
      return 'outline' as const
    default:
      return 'secondary' as const
  }
}

onMounted(() => {
  loadLogs()
})
</script>

<template>
  <div class="mx-auto max-w-4xl space-y-4 p-6">
    <h1 class="text-xl font-semibold">HTTP 服务</h1>

    <Card class="p-5">
      <div class="flex items-center justify-between">
        <div class="space-y-1.5">
          <div class="flex items-center gap-2">
            <Badge :variant="statusBadge.variant" class="px-3 py-1 text-sm">{{ statusBadge.text }}</Badge>
          </div>
          <p v-if="serverStore.running" class="text-sm text-muted-foreground">
            监听地址 <span class="font-mono text-foreground">{{ serverStore.bindAddr }}</span>
          </p>
          <p v-else class="text-sm text-muted-foreground">端口 {{ serverStore.port }}</p>
          <p v-if="serverStore.error" class="text-sm text-destructive">{{ serverStore.error }}</p>
        </div>
        <div class="flex gap-2">
          <Button :disabled="serverStore.running || operating !== ''" @click="operate('start')">
            <Play class="h-4 w-4" />
            {{ operating === 'start' ? '启动中…' : '启动' }}
          </Button>
          <Button variant="outline" :disabled="!serverStore.running || operating !== ''" @click="operate('restart')">
            <RotateCw class="h-4 w-4" />
            {{ operating === 'restart' ? '重启中…' : '重启' }}
          </Button>
          <Button variant="destructive" :disabled="!serverStore.running || operating !== ''" @click="operate('stop')">
            <Square class="h-4 w-4" />
            {{ operating === 'stop' ? '停止中…' : '停止' }}
          </Button>
        </div>
      </div>
    </Card>

    <CollapsibleCard v-model:open="tunnelsOpen" title="公网隧道（二选一）">
      <CloudflareTunnelCard />
      <div class="border-t border-border" />
      <FrpCard />
    </CollapsibleCard>

    <Card class="p-5">
      <div class="flex items-center justify-between">
        <h2 class="font-medium">最近日志</h2>
        <Button variant="ghost" size="sm" :disabled="logsLoading" @click="loadLogs">
          <RefreshCw class="h-4 w-4" :class="logsLoading ? 'animate-spin' : ''" />
          刷新
        </Button>
      </div>
      <div v-if="logs.length === 0" class="mt-4 text-center text-sm text-muted-foreground">暂无日志</div>
      <div v-else class="mt-3 max-h-96 space-y-1 overflow-y-auto font-mono text-xs">
        <div
          v-for="log in logs"
          :key="`${log.ts}·${log.level}·${log.target}·${log.message}`"
          class="flex items-start gap-2 rounded px-1.5 py-1 hover:bg-accent/50"
        >
          <span class="shrink-0 text-muted-foreground">{{ formatTime(log.ts) }}</span>
          <Badge :variant="levelVariant(log.level)" class="shrink-0 px-1.5 py-0 text-[10px]">{{ log.level }}</Badge>
          <span class="break-all leading-snug">{{ log.message }}</span>
        </div>
      </div>
    </Card>
  </div>
</template>
