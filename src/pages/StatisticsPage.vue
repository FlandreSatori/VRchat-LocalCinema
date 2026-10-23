<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { getTrafficStats, type TrafficPoint, type TrafficRange, type TrafficStats } from '../lib/api'
import Card from '../components/ui/Card.vue'
import { formatSize } from '../lib/format'

const range = ref<TrafficRange>('1h')
const stats = ref<TrafficStats | null>(null)
const loading = ref(true)
const error = ref('')
const hovered = ref<number | null>(null)
let timer: ReturnType<typeof setInterval> | undefined
let generation = 0
let disposed = false
const width = 840
const height = 300
const pad = { left: 48, right: 16, top: 20, bottom: 38 }

const points = computed(() => {
  const source = stats.value?.points ?? []
  if (range.value === '1h') {
    const bySession = new Map<string, TrafficPoint[]>()
    for (const point of source) {
      const session = bySession.get(point.sessionId) ?? []
      session.push(point)
      bySession.set(point.sessionId, session)
    }
    return [...bySession.values()].flatMap(session => session.length <= 600 ? session : session.filter((_, i) => i % Math.ceil(session.length / 600) === 0)).sort((a, b) => a.timestamp - b.timestamp)
  }
  const bySession = new Map<string, TrafficPoint[]>()
  for (const point of source) {
    const session = bySession.get(point.sessionId) ?? []
    session.push(point)
    bySession.set(point.sessionId, session)
  }
  const result: TrafficPoint[] = []
  const targetPerSession = range.value === 'all' ? 600 : Math.ceil(600 / bySession.size)
  for (const session of bySession.values()) {
    if (session.length <= targetPerSession) {
      result.push(...session)
      continue
    }
    const bucketSize = Math.ceil(session.length / targetPerSession)
    for (let i = 0; i < session.length; i += bucketSize) {
      const bucket = session.slice(i, i + bucketSize)
      const duration = bucket.reduce((sum, p) => sum + p.durationMs, 0)
      if (!duration) continue
      result.push({
        ...bucket[0],
        durationMs: duration,
        uploadBytes: bucket.reduce((sum, p) => sum + p.uploadBytes, 0),
        downloadBytes: bucket.reduce((sum, p) => sum + p.downloadBytes, 0),
        localUploadBytes: bucket.reduce((sum, p) => sum + p.localUploadBytes, 0),
        localDownloadBytes: bucket.reduce((sum, p) => sum + p.localDownloadBytes, 0),
      })
    }
  }
  return result.sort((a, b) => a.timestamp - b.timestamp)
})

const plotted = computed(() => {
  const n = points.value.length
  const plotW = width - pad.left - pad.right
  const plotH = height - pad.top - pad.bottom
  // 计算对外上传速率（总上传 - 本机上传）作为 Y 轴最大值的参考
  const max = Math.max(1, points.value.reduce((value, p) => {
    const remoteUpload = (p.uploadBytes - p.localUploadBytes) * 8 / p.durationMs / 1000
    const download = p.downloadBytes * 8 / p.durationMs / 1000
    return Math.max(value, remoteUpload, download)
  }, 0))
  const firstTimestamp = points.value[0]?.timestamp ?? 0
  const lastTimestamp = points.value[n - 1]?.timestamp ?? firstTimestamp
  return points.value.map(p => {
    const remoteUploadBytes = p.uploadBytes - p.localUploadBytes
    return {
      ...p,
      remoteUploadBytes,
      x: pad.left + (lastTimestamp === firstTimestamp ? plotW / 2 : (p.timestamp - firstTimestamp) / (lastTimestamp - firstTimestamp) * plotW),
      remoteUp: pad.top + plotH - (remoteUploadBytes * 8 / p.durationMs / 1000) / max * plotH,
      down: pad.top + plotH - (p.downloadBytes * 8 / p.durationMs / 1000) / max * plotH,
      localUp: pad.top + plotH - (p.localUploadBytes * 8 / p.durationMs / 1000) / max * plotH,
    }
  })
})

const yMax = computed(() => Math.max(1, plotted.value.reduce((value, p) => {
  const remoteUpload = p.remoteUploadBytes * 8 / p.durationMs / 1000
  const download = p.downloadBytes * 8 / p.durationMs / 1000
  return Math.max(value, remoteUpload, download)
}, 0)))
const yTicks = computed(() => [0, 1, 2, 3].map(i => yMax.value * (3 - i) / 3))
const segments = computed(() => {
  const out: { session: string; points: typeof plotted.value }[] = []
  for (const point of plotted.value) {
    let group = out[out.length - 1]
    if (!group || group.session !== point.sessionId) out.push(group = { session: point.sessionId, points: [] })
    group.points.push(point)
  }
  return out
})
const breaks = computed(() => segments.value.flatMap((group, i) => i === 0 ? [] : [{ x: group.points[0]?.x ?? pad.left, timestamp: group.points[0]?.timestamp ?? 0 }]))
const activePoint = computed(() => hovered.value === null ? null : plotted.value[hovered.value] ?? null)
const ranges = [{ value: '1h', label: '1 小时' }, { value: '24h', label: '24 小时' }, { value: 'all', label: '自部署以来' }] as const

function line(group: typeof segments.value[number], key: 'remoteUp' | 'down' | 'localUp') {
  return group.points.map((p, i) => `${i ? 'L' : 'M'}${p.x},${p[key]}`).join(' ')
}
function rate(bytes: number, duration: number) { return duration > 0 ? (bytes * 8 / duration / 1000).toFixed(2) : '0.00' }
function durationLabel(ms: number) {
  const total = Math.floor(ms / 1000)
  return `${Math.floor(total / 3600)} 小时 ${Math.floor(total % 3600 / 60)} 分钟`
}
function stamp(value: number) { return new Date(value).toLocaleString() }

async function refresh() {
  const ticket = ++generation
  loading.value = !stats.value
  try {
    const next = await getTrafficStats(range.value)
    if (disposed || ticket !== generation) return
    stats.value = next
    error.value = ''
  } catch (e) {
    if (disposed || ticket !== generation) return
    error.value = String(e)
  } finally {
    if (!disposed && ticket === generation) loading.value = false
  }
}
watch(range, () => { stats.value = null; hovered.value = null; void refresh() })
onMounted(() => { void refresh(); timer = setInterval(() => void refresh(), 5000) })
onUnmounted(() => { disposed = true; generation++; if (timer) clearInterval(timer) })
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-5 p-6">
    <header class="flex flex-wrap items-center justify-between gap-3">
      <h1 class="text-xl font-semibold">流量统计</h1>
      <div role="group" aria-label="统计区间" class="flex rounded-md border border-border p-0.5">
        <button v-for="opt in ranges" :key="opt.value" type="button" class="rounded px-3 py-1.5 text-sm" :class="range === opt.value ? 'bg-primary text-primary-foreground' : 'text-muted-foreground hover:bg-accent'" :aria-pressed="range === opt.value" @click="range = opt.value">{{ opt.label }}</button>
      </div>
    </header>
    <div v-if="loading" class="py-16 text-center text-sm text-muted-foreground" role="status">正在加载流量统计…</div>
    <div v-else-if="error" class="rounded-md border border-destructive/40 bg-destructive/5 p-4 text-sm text-destructive" role="alert">加载失败：{{ error }} <button class="ml-2 underline" @click="refresh">重试</button></div>
    <template v-else-if="stats">
      <div class="grid gap-3 sm:grid-cols-4">
        <Card class="p-4"><div class="text-xs text-muted-foreground">累计对外上传</div><div class="mt-1 text-lg font-semibold">{{ formatSize(stats.totalUploadBytes - stats.totalLocalUploadBytes) }}</div></Card>
        <Card class="p-4"><div class="text-xs text-muted-foreground">累计下载</div><div class="mt-1 text-lg font-semibold">{{ formatSize(stats.totalDownloadBytes) }}</div></Card>
        <Card class="p-4"><div class="text-xs text-muted-foreground">本机上传</div><div class="mt-1 text-lg font-semibold text-amber-600">{{ formatSize(stats.totalLocalUploadBytes) }}</div></Card>
        <Card class="p-4"><div class="text-xs text-muted-foreground">有效运行时长</div><div class="mt-1 text-lg font-semibold">{{ durationLabel(stats.activeDurationMs) }}</div></Card>
      </div>
      <Card class="p-4">
        <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
          <div class="flex items-center gap-4 text-xs"><span class="flex items-center gap-1.5"><i class="h-2 w-2 rounded-full bg-primary"/>对外上传 Mbps</span><span class="flex items-center gap-1.5"><i class="h-2 w-2 rounded-full bg-emerald-500"/>下载 Mbps</span><span class="flex items-center gap-1.5"><i class="h-2 w-2 rounded-full bg-amber-500"/>本机上传 Mbps</span></div>
          <div class="text-xs text-muted-foreground">历史最大上传速率 {{ stats.uploadBaselineMbps.toFixed(2) }} Mbps · 提醒阈值 {{ (stats.uploadBaselineMbps * .7).toFixed(2) }} Mbps</div>
        </div>
        <div v-if="!plotted.length" class="flex h-64 items-center justify-center text-sm text-muted-foreground">所选区间暂无有效流量样本</div>
        <div v-else class="relative overflow-x-auto">
          <svg :viewBox="`0 0 ${width} ${height}`" class="h-64 min-w-[640px] w-full" role="img" aria-label="对外上传与下载速率趋势图；键盘聚焦数据点可查看详情">
            <line :x1="pad.left" :x2="width-pad.right" :y1="height-pad.bottom" :y2="height-pad.bottom" class="stroke-border"/>
            <g v-for="(tick, i) in yTicks" :key="i"><line :x1="pad.left" :x2="width-pad.right" :y1="pad.top+(height-pad.top-pad.bottom)*i/3" :y2="pad.top+(height-pad.top-pad.bottom)*i/3" class="stroke-border/60" stroke-dasharray="3 5"/><text :x="pad.left-6" :y="pad.top+(height-pad.top-pad.bottom)*i/3+4" text-anchor="end" class="fill-muted-foreground" font-size="10">{{ tick.toFixed(1) }}</text></g>
            <g v-for="(group, gi) in segments" :key="`${gi}-${group.session}`">
              <path v-if="group.points.length > 1" :d="line(group,'remoteUp')" fill="none" class="stroke-primary" stroke-width="2"/>
              <path v-if="group.points.length > 1" :d="line(group,'down')" fill="none" class="stroke-emerald-500" stroke-width="2"/>
              <path v-if="group.points.length > 1" :d="line(group,'localUp')" fill="none" class="stroke-amber-500" stroke-width="2"/>
              <g v-for="p in group.points" :key="p.timestamp" tabindex="0" role="button" :aria-label="`${stamp(p.timestamp)}，对外上传 ${rate(p.remoteUploadBytes,p.durationMs)} Mbps，下载 ${rate(p.downloadBytes,p.durationMs)} Mbps，本机上传 ${rate(p.localUploadBytes,p.durationMs)} Mbps`" @focus="hovered = plotted.indexOf(p)" @blur="hovered = null" @mouseenter="hovered = plotted.indexOf(p)" @mouseleave="hovered = null" @keydown.left.prevent="hovered = Math.max(0, plotted.indexOf(p)-1)" @keydown.right.prevent="hovered = Math.min(plotted.length-1, plotted.indexOf(p)+1)">
                <circle :cx="p.x" :cy="p.remoteUp" r="4" class="fill-primary"/><circle :cx="p.x" :cy="p.down" r="4" class="fill-emerald-500"/><circle :cx="p.x" :cy="p.localUp" r="4" class="fill-amber-500"/><circle :cx="p.x" :cy="(p.remoteUp+p.down+p.localUp)/3" r="10" fill="transparent"/>
              </g>
            </g>
            <text x="8" :y="height/2" transform="rotate(-90 8 150)" class="fill-muted-foreground" font-size="10">Mbps</text>
            <g v-for="(br, bi) in breaks" :key="`break-${bi}`"><line :x1="br.x" :x2="br.x" :y1="pad.top" :y2="height-pad.bottom" stroke="#d97706" stroke-dasharray="4 4" stroke-width="1.5"/><text :x="Math.min(br.x+4,width-pad.right-8)" :y="pad.top+12" class="fill-amber-600" font-size="10">断点</text></g>
            <text :x="pad.left" :y="height-10" class="fill-muted-foreground" font-size="11">{{ plotted[0] ? stamp(plotted[0].timestamp) : '' }}</text>
            <text :x="width-pad.right" :y="height-10" text-anchor="end" class="fill-muted-foreground" font-size="11">{{ plotted.length ? stamp(plotted[plotted.length-1].timestamp) : '' }}</text>
          </svg>
          <div v-if="activePoint" class="pointer-events-none absolute right-3 top-3 rounded border border-border bg-popover px-3 py-2 text-xs shadow" role="status">
            <div class="mb-1 font-medium">{{ stamp(activePoint.timestamp) }}</div><div class="text-primary">对外上传 {{ rate(activePoint.remoteUploadBytes,activePoint.durationMs) }} Mbps</div><div class="text-emerald-600">下载 {{ rate(activePoint.downloadBytes,activePoint.durationMs) }} Mbps</div><div class="text-amber-600">本机上传 {{ rate(activePoint.localUploadBytes,activePoint.durationMs) }} Mbps</div>
          </div>
        </div>
      </Card>
      <p class="text-xs text-muted-foreground">统计自 {{ stamp(stats.startedAt) }} 开始；仅包含 HTTP 媒体服务输出、对端媒体下载与 B 站代理上游流量，不代表全网卡流量。停机区间不生成样本，图中仅连接同一运行会话的相邻有效点。</p>
    </template>
  </div>
</template>
