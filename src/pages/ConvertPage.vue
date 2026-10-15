<script setup lang="ts">
import { computed, onMounted } from 'vue'
import {
  ListVideo,
  Play,
  Pause,
  Ban,
  Trash2,
  Eraser,
  Clock,
  Loader2,
  CheckCircle2,
  XCircle,
} from 'lucide-vue-next'
import Button from '../components/ui/Button.vue'
import Badge from '../components/ui/Badge.vue'
import { convertTaskStore, applyConvertTasks } from '../lib/store'
import {
  listConvertTasks,
  convertTaskStart,
  convertTaskCancel,
  convertTaskRemove,
  convertTaskClear,
  downloadTaskPause,
  downloadTaskResume,
  type ConvertTask,
} from '../lib/api'
import { toast, errMsg } from '../lib/toast'

const tasks = computed(() =>
  [...convertTaskStore.tasks].sort((a, b) => a.createdAt - b.createdAt)
)
const finishedCount = computed(
  () => tasks.value.filter((t) => t.state !== 'queued' && t.state !== 'running').length
)

const stateMeta: Record<ConvertTask['state'], { label: string; cls: string }> = {
  queued: { label: '排队中', cls: 'text-muted-foreground' },
  running: { label: '进行中', cls: 'text-primary' },
  paused: { label: '已暂停', cls: 'text-muted-foreground' },
  done: { label: '已完成', cls: 'text-emerald-600 dark:text-emerald-400' },
  error: { label: '失败', cls: 'text-destructive' },
  cancelled: { label: '已取消', cls: 'text-muted-foreground' },
}

const kindLabel: Record<ConvertTask['kind'], string> = {
  transcode: '转码 MP4',
  segment: '分段转码',
  hls: '边转边播（已停用）',
  download: '下载',
}

function stateIcon(state: ConvertTask['state']) {
  switch (state) {
    case 'running':
      return Loader2
    case 'done':
      return CheckCircle2
    case 'error':
      return XCircle
    case 'cancelled':
      return Ban
    case 'paused':
      return Pause
    default:
      return Clock
  }
}

function formatSize(bytes: number): string {
  if (!bytes) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let i = 0
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024
    i += 1
  }
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return '计算中'
  const total = Math.ceil(seconds)
  if (total < 60) return `${total} 秒`
  const minutes = Math.floor(total / 60)
  const rest = total % 60
  return rest ? `${minutes} 分 ${rest} 秒` : `${minutes} 分钟`
}

function formatTaskTelemetry(t: ConvertTask): string | null {
  if (t.state !== 'running') return null
  if (t.kind === 'download' && t.speed && t.speed > 0) {
    return `${formatSize(t.speed)}/s · 剩余 ${formatDuration(t.remainingSeconds)}`
  }
  if (t.kind === 'transcode' || t.kind === 'segment') {
    const rates = [
      t.speed && t.speed > 0 ? `${t.speed.toFixed(1)}x` : null,
      t.fps && t.fps > 0 ? `${Math.round(t.fps)} fps` : null,
    ].filter(Boolean).join(' · ')
    return `${rates || '速度计算中'} · 剩余 ${formatDuration(t.remainingSeconds)}`
  }
  return null
}

async function refresh() {
  try {
    applyConvertTasks(await listConvertTasks())
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function start(t: ConvertTask) {
  try {
    await convertTaskStart(t.id)
    toast.success('已置于队列最前，当前任务完成后立即执行')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function cancel(t: ConvertTask) {
  try {
    await convertTaskCancel(t.id)
    toast.success(t.state === 'running' ? '正在取消，即将终止任务' : '已取消任务')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function pauseDownload(t: ConvertTask) {
  try {
    await downloadTaskPause(t.id)
    toast.success('已请求暂停，进度将保存')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function resumeDownload(t: ConvertTask) {
  try {
    await downloadTaskResume(t.id)
    toast.success('已继续下载（断点续传）')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function remove(t: ConvertTask) {
  try {
    await convertTaskRemove(t.id)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

async function clearFinished() {
  try {
    await convertTaskClear()
    toast.success('已清除全部已完成记录')
  } catch (e) {
    toast.error(errMsg(e))
  }
}

onMounted(refresh)
</script>

<template>
  <div class="mx-auto max-w-6xl p-6">
    <div class="flex items-center justify-between gap-4">
      <div>
        <h1 class="text-xl font-semibold">任务</h1>
        <p class="mt-0.5 text-sm text-muted-foreground">
          转码与分段转码按队列顺序执行；下载任务后台执行，完成后自动加入媒体库
        </p>
      </div>
      <Button v-if="finishedCount > 0" variant="outline" @click="clearFinished">
        <Eraser class="h-4 w-4" />
        清除已完成记录
      </Button>
    </div>

    <!-- 空状态 -->
    <div
      v-if="tasks.length === 0"
      class="mt-16 flex flex-col items-center justify-center text-center"
    >
      <ListVideo class="h-16 w-16 text-muted-foreground/40" />
      <h2 class="mt-4 text-lg font-medium">暂无任务</h2>
      <p class="mt-1 text-sm text-muted-foreground">
        在媒体库中右键视频可发起转码；在成员媒体库中点击「下载」可获取对端文件
      </p>
    </div>

    <!-- 任务列表 -->
    <div v-else class="mt-4 space-y-2">
      <div v-for="t in tasks" :key="t.id" class="rounded-lg border bg-card p-3 shadow-sm">
        <div class="flex items-center gap-3">
          <component
            :is="stateIcon(t.state)"
            class="h-4 w-4 shrink-0"
            :class="[stateMeta[t.state].cls, t.state === 'running' ? 'animate-spin' : '']"
          />
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2">
              <div class="truncate text-sm font-medium" :title="t.title">{{ t.title }}</div>
              <Badge variant="secondary" class="shrink-0">{{ kindLabel[t.kind] }}</Badge>
              <Badge v-if="t.kind === 'transcode' && t.overwrite" variant="warning" class="shrink-0">覆盖模式</Badge>
            </div>
            <div class="text-xs" :class="stateMeta[t.state].cls">
              {{ stateMeta[t.state].label }}
              <template v-if="t.state === 'running' && t.percent !== null">
                · {{ Math.round(t.percent) }}%
              </template>
              <template v-if="t.kind === 'download' && (t.state === 'running' || t.state === 'paused') && t.received !== null && t.received !== undefined">
                · {{ formatSize(t.received) }}<template v-if="t.total"> / {{ formatSize(t.total) }}</template>
              </template>
              <template v-if="formatTaskTelemetry(t)">
                · {{ formatTaskTelemetry(t) }}
              </template>
            </div>
          </div>
          <div class="flex shrink-0 gap-1.5">
            <!-- 下载任务：暂停 / 继续 / 取消 -->
            <template v-if="t.kind === 'download'">
              <Button v-if="t.state === 'running'" size="sm" variant="outline" @click="pauseDownload(t)">
                <Pause class="h-3.5 w-3.5" />
                暂停
              </Button>
              <Button
                v-if="t.state === 'paused' || t.state === 'error'"
                size="sm"
                variant="outline"
                @click="resumeDownload(t)"
              >
                <Play class="h-3.5 w-3.5" />
                继续
              </Button>
              <Button v-if="t.state === 'running' || t.state === 'paused'" size="sm" variant="outline" @click="cancel(t)">
                <Ban class="h-3.5 w-3.5" />
                取消
              </Button>
              <Button v-else size="sm" variant="ghost" @click="remove(t)">
                <Trash2 class="h-3.5 w-3.5" />
                移除记录
              </Button>
            </template>
            <!-- 转码任务 -->
            <template v-else>
              <Button v-if="t.state === 'queued'" size="sm" variant="outline" @click="start(t)">
                <Play class="h-3.5 w-3.5" />
                开始
              </Button>
              <Button
                v-if="t.state === 'queued' || t.state === 'running'"
                size="sm"
                variant="outline"
                @click="cancel(t)"
              >
                <Ban class="h-3.5 w-3.5" />
                取消
              </Button>
              <Button v-else size="sm" variant="ghost" @click="remove(t)">
                <Trash2 class="h-3.5 w-3.5" />
                移除记录
              </Button>
            </template>
          </div>
        </div>
        <div
          v-if="t.state === 'running'"
          class="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-muted"
        >
          <div
            class="h-full bg-primary transition-all"
            :style="{ width: t.percent !== null ? `${t.percent}%` : '30%' }"
            :class="t.percent === null ? 'animate-pulse' : ''"
          />
        </div>
        <div v-if="t.error" class="mt-2 truncate text-xs text-destructive" :title="t.error">
          {{ t.error }}
        </div>
      </div>
    </div>
  </div>
</template>
