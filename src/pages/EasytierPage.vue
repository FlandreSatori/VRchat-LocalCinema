<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from 'vue'
import {
  Play,
  Square,
  RefreshCw,
  Save,
  RotateCcw,
  Users,
  Layers,
  Download,
  ExternalLink,
  AlertTriangle,
} from 'lucide-vue-next'
import Card from '../components/ui/Card.vue'
import Button from '../components/ui/Button.vue'
import Input from '../components/ui/Input.vue'
import Label from '../components/ui/Label.vue'
import Switch from '../components/ui/Switch.vue'
import Badge from '../components/ui/Badge.vue'
import Separator from '../components/ui/Separator.vue'
import PeerMediaDialog from '../components/PeerMediaDialog.vue'
import {
  getSettings,
  saveSettings,
  getEasytierStatus,
  startEasytier,
  stopEasytier,
  getRoomInfo,
  resetNetworkAdapter,
  getComponentsStatus,
  downloadComponent,
  openComponentPage,
  onEasytierStatusChanged,
  onComponentDownloadProgress,
  type ComponentStatus,
  type EasytierConfig,
  type EasytierStatus,
  type PeerInfo,
  type RoomInfo,
  type Settings,
} from '../lib/api'
import { settingsCache, mediaStore } from '../lib/store'
import { toast, errMsg } from '../lib/toast'
import type { UnlistenFn } from '@tauri-apps/api/event'

const PROTOCOLS = ['tcp', 'udp', 'ws', 'wss', 'quic']

const fullSettings = ref<Settings | null>(null)
const cfg = reactive<EasytierConfig>({
  protocols: ['tcp', 'ws'],
  peerUrl: '',
  roomName: 'VRchat',
  roomPassword: '123',
  memberName: '',
  ipv4: '',
  ipv6: '',
  dhcp: true,
  kcpProxy: true,
  kcpInput: false,
  quicProxy: false,
  quicInput: false,
  latencyFirst: false,
  compression: 'none',
  upnp: true,
  multiThread: true,
  listenPort: 11010,
})

const status = ref<EasytierStatus>({
  running: false,
  pid: null,
  rpcPortal: '',
  error: null,
  args: [],
})
const room = ref<RoomInfo>({ peers: [], raw: '', error: null })
const easytierComponent = ref<ComponentStatus | null>(null)
const loading = ref(true)
const saving = ref(false)
const operating = ref<'' | 'start' | 'stop' | 'reset'>('')
const roomLoading = ref(false)
const showRaw = ref(false)
const installing = ref(false)
const downloadProgress = ref<{ downloaded: number; total: number | null } | null>(null)

const dialogOpen = ref(false)
const selectedPeer = ref<PeerInfo | null>(null)

const localCount = computed(() => mediaStore.items.length)
const installed = computed(() => easytierComponent.value?.installed ?? false)

const listenHint = computed(() => {
  if (cfg.protocols.length === 0) return '至少启用一种协议，否则无法被其他成员连接。'
  return cfg.protocols
    .map((p) => `${p}://0.0.0.0:${cfg.listenPort}${p === 'ws' ? '+1' : p === 'wss' ? '+2' : p === 'quic' ? '+3' : ''}`)
    .join('，')
})

const installProgressText = computed(() => {
  if (!installing.value) return ''
  const p = downloadProgress.value
  if (!p) return '正在获取最新版本信息…'
  if (!p.total) return `已下载 ${(p.downloaded / 1048576).toFixed(1)} MB`
  const pct = Math.min(100, Math.round((p.downloaded / p.total) * 100))
  return `已下载 ${(p.downloaded / 1048576).toFixed(1)} / ${(p.total / 1048576).toFixed(1)} MB（${pct}%）`
})

let unlisteners: UnlistenFn[] = []
let timer: number | null = null

function toggleProtocol(name: string) {
  const idx = cfg.protocols.indexOf(name)
  if (idx >= 0) cfg.protocols.splice(idx, 1)
  else cfg.protocols.push(name)
}

// KCP 与 QUIC 代理互斥（对齐 EasytierGame）：启用其一会自动关闭另一个
function setKcpProxy(v: boolean) {
  cfg.kcpProxy = v
  if (v) cfg.quicProxy = false
}
function setQuicProxy(v: boolean) {
  cfg.quicProxy = v
  if (v) cfg.kcpProxy = false
}

// 对齐 EasytierGame：中转方式 p2p = 直连，relay = 中转
const costBadge = (cost: string) =>
  cost.startsWith('p2p')
    ? { variant: 'success' as const, text: '直连' }
    : cost.startsWith('relay')
      ? { variant: 'secondary' as const, text: '中转' }
      : { variant: 'outline' as const, text: cost || '-' }

async function loadComponents() {
  try {
    const list = await getComponentsStatus()
    easytierComponent.value = list.find((c) => c.kind === 'easytier') ?? null
  } catch {
    easytierComponent.value = null
  }
}

async function loadRoom() {
  if (!status.value.running || !installed.value) return
  roomLoading.value = true
  try {
    room.value = await getRoomInfo()
  } catch (e) {
    room.value = { peers: [], raw: '', error: errMsg(e) }
  } finally {
    roomLoading.value = false
  }
}

function startPolling() {
  stopPolling()
  timer = window.setInterval(() => {
    if (status.value.running) loadRoom()
  }, 5000)
}

function stopPolling() {
  if (timer !== null) {
    window.clearInterval(timer)
    timer = null
  }
}

async function applyConfig() {
  const base = fullSettings.value
  if (!base) return
  const updated: Settings = { ...base, easytier: { ...cfg, protocols: [...cfg.protocols] } }
  await saveSettings(updated)
  fullSettings.value = updated
  settingsCache.value = updated
}

async function save() {
  if (saving.value) return
  saving.value = true
  try {
    await applyConfig()
    toast.success('组网配置已保存')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    saving.value = false
  }
}

async function operate(kind: 'start' | 'stop' | 'reset') {
  if (operating.value) return
  operating.value = kind
  try {
    if (kind === 'start') {
      await applyConfig()
      status.value = await startEasytier()
      toast.success('已启动 EasyTier 组网')
      await loadRoom()
    } else if (kind === 'stop') {
      status.value = await stopEasytier()
      room.value = { peers: [], raw: '', error: null }
      toast.success('已停止 EasyTier 组网')
    } else {
      status.value = await resetNetworkAdapter()
      toast.success('已重置网卡并重新协商')
      await loadRoom()
    }
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    operating.value = ''
  }
}

async function install() {
  if (installing.value) return
  installing.value = true
  downloadProgress.value = null
  try {
    await downloadComponent('easytier')
    await loadComponents()
    toast.success('EasyTier 组件已安装到内置目录')
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    installing.value = false
    downloadProgress.value = null
  }
}

async function openManual(kind: 'ffmpeg' | 'easytier') {
  try {
    await openComponentPage(kind)
  } catch (e) {
    toast.error(errMsg(e))
  }
}

function openPeer(peer: PeerInfo) {
  selectedPeer.value = peer
  dialogOpen.value = true
}

function statusBadge() {
  if (status.value.error && !status.value.running) return { variant: 'destructive' as const, text: '异常' }
  return status.value.running
    ? { variant: 'success' as const, text: '组网中' }
    : { variant: 'secondary' as const, text: '未启动' }
}

function formatLatency(v: number | null): string {
  return v === null ? '-' : `${v.toFixed(0)} ms`
}
function formatLoss(v: number | null): string {
  return v === null ? '-' : `${v.toFixed(1)}%`
}

onMounted(async () => {
  try {
    const [s, st] = await Promise.all([getSettings(), getEasytierStatus()])
    fullSettings.value = s
    Object.assign(cfg, s.easytier, { protocols: [...s.easytier.protocols] })
    // 兼容历史配置：KCP 与 QUIC 代理互斥，同时开启时保留 KCP
    if (cfg.kcpProxy && cfg.quicProxy) cfg.quicProxy = false
    status.value = st
    await loadComponents()
    if (st.running) await loadRoom()
    startPolling()
  } catch (e) {
    toast.error(errMsg(e))
  } finally {
    loading.value = false
  }

  unlisteners = [
    await onEasytierStatusChanged((s) => {
      status.value = s
    }),
    await onComponentDownloadProgress((p) => {
      if (p.kind === 'easytier') downloadProgress.value = { downloaded: p.downloaded, total: p.total }
    }),
  ]
})

onUnmounted(() => {
  stopPolling()
  unlisteners.forEach((u) => u())
  unlisteners = []
})
</script>

<template>
  <div class="mx-auto max-w-4xl space-y-4 p-6 pb-12">
    <div class="flex items-center justify-between">
      <h1 class="text-xl font-semibold">EasyTier 组网</h1>
      <Button :disabled="loading || saving" @click="save">
        <Save class="h-4 w-4" />
        {{ saving ? '保存中…' : '保存配置' }}
      </Button>
    </div>

    <Card v-if="!loading && !installed" class="space-y-3 border-amber-500/40 p-5">
      <div class="flex items-start gap-2">
        <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-amber-500" />
        <div>
          <h2 class="font-medium">似乎未安装 EasyTier</h2>
          <p class="mt-0.5 text-sm text-muted-foreground">
            未在系统路径和内置组件目录中检测到 easytier-core / easytier-cli，需要手动填入路径或下载吗？
          </p>
          <p v-if="easytierComponent?.missing.length" class="mt-1 font-mono text-xs text-muted-foreground">
            缺少：{{ easytierComponent.missing.join(', ') }}
          </p>
        </div>
      </div>
      <div v-if="installProgressText" class="text-xs text-muted-foreground">{{ installProgressText }}</div>
      <div class="flex flex-wrap gap-2">
        <Button :disabled="installing" @click="install">
          <Download class="h-4 w-4" />
          {{ installing ? '下载中…' : '自动下载最新版' }}
        </Button>
        <Button variant="outline" @click="openManual('easytier')">
          <ExternalLink class="h-4 w-4" />
          手动下载
        </Button>
      </div>
    </Card>

    <Card class="p-5">
      <div class="flex items-center justify-between gap-4">
        <div class="space-y-1.5">
          <div class="flex items-center gap-2">
            <Badge :variant="statusBadge().variant" class="px-3 py-1 text-sm">{{ statusBadge().text }}</Badge>
            <span v-if="status.rpcPortal" class="font-mono text-xs text-muted-foreground">
              RPC {{ status.rpcPortal }}
            </span>
          </div>
          <p v-if="status.error" class="text-sm text-destructive">{{ status.error }}</p>
          <p v-else-if="status.running" class="text-sm text-muted-foreground">
            房间 <span class="font-medium text-foreground">{{ cfg.roomName }}</span> · 成员
            <span class="font-medium text-foreground">{{ room.peers.length || 1 }}</span>
          </p>
          <p v-else class="text-sm text-muted-foreground">启动后可与同一房间的成员互相访问媒体库</p>
        </div>
        <div class="flex flex-wrap gap-2">
          <Button :disabled="status.running || operating !== ''" @click="operate('start')">
            <Play class="h-4 w-4" />
            {{ operating === 'start' ? '启动中…' : '启动组网' }}
          </Button>
          <Button variant="destructive" :disabled="!status.running || operating !== ''" @click="operate('stop')">
            <Square class="h-4 w-4" />
            {{ operating === 'stop' ? '停止中…' : '停止' }}
          </Button>
          <Button variant="outline" :disabled="operating !== ''" @click="operate('reset')">
            <RotateCcw class="h-4 w-4" />
            {{ operating === 'reset' ? '重置中…' : '重置网卡' }}
          </Button>
        </div>
      </div>
      <p class="mt-3 text-xs text-muted-foreground">
        重置网卡会停止组网、卸载 EasyTier 虚拟网卡设备并清理本地缓存；若组网正在进行，会自动重启并重装网卡（需要管理员权限）。
      </p>
    </Card>

    <Card class="space-y-4 p-5">
      <div class="flex items-center gap-2">
        <Layers class="h-4 w-4 text-muted-foreground" />
        <h2 class="font-medium">基础配置</h2>
      </div>

      <div class="space-y-1.5">
        <Label>启用的协议类型</Label>
        <div class="flex flex-wrap gap-2">
          <button
            v-for="p in PROTOCOLS"
            :key="p"
            type="button"
            class="rounded-md border px-3 py-1.5 text-xs font-medium uppercase transition-colors"
            :class="
              cfg.protocols.includes(p)
                ? 'border-primary bg-primary text-primary-foreground'
                : 'border-border bg-background text-muted-foreground hover:bg-accent hover:text-foreground'
            "
            @click="toggleProtocol(p)"
          >
            {{ p }}
          </button>
        </div>
        <p class="text-xs text-muted-foreground">{{ listenHint }}</p>
      </div>

      <div class="grid grid-cols-2 gap-4">
        <div class="space-y-1.5">
          <Label>中转服务器地址</Label>
          <Input v-model="cfg.peerUrl" placeholder="例如 tcp://public.easytier.top:11010" />
        </div>
        <div class="space-y-1.5">
          <Label>监听端口</Label>
          <Input
            type="number"
            :min="1"
            :max="65535"
            :model-value="String(cfg.listenPort)"
            @update:model-value="cfg.listenPort = Math.round(Number($event)) || 0"
          />
        </div>
        <div class="space-y-1.5">
          <Label>房间名</Label>
          <Input v-model="cfg.roomName" placeholder="VRchat" />
        </div>
        <div class="space-y-1.5">
          <Label>房间密码</Label>
          <Input v-model="cfg.roomPassword" placeholder="123" />
        </div>
        <div class="space-y-1.5">
          <Label>成员名</Label>
          <Input v-model="cfg.memberName" placeholder="留空使用本机主机名" />
        </div>
        <div class="space-y-1.5">
          <div class="flex items-center justify-between gap-2">
            <Label>虚拟 IPv4</Label>
            <label class="flex items-center gap-1.5 text-xs text-muted-foreground">
              自动分配（DHCP）
              <Switch :checked="cfg.dhcp" @update:checked="cfg.dhcp = $event" />
            </label>
          </div>
          <Input v-model="cfg.ipv4" :disabled="cfg.dhcp" placeholder="例如 10.126.126.1，DHCP 开启时自动分配" />
        </div>
        <div class="space-y-1.5 col-span-2">
          <Label>虚拟 IPv6</Label>
          <Input v-model="cfg.ipv6" placeholder="可选，留空表示不使用 IPv6" />
        </div>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <h2 class="font-medium">高级选项</h2>
      <div class="grid grid-cols-2 gap-x-6 gap-y-3">
        <div class="flex items-center justify-between gap-4">
          <div class="flex items-center gap-1">
            <Label>KCP 代理</Label>
            <span
              class="cursor-help text-xs text-muted-foreground"
              title="（不可与QUIC代理同时开启）将TCP流量转为KCP流量，降低传输延迟，提升传输速度"
            >?</span>
          </div>
          <Switch :checked="cfg.kcpProxy" @update:checked="setKcpProxy" />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div class="flex items-center gap-1">
            <Label>禁用 KCP 输入</Label>
            <span
              class="cursor-help text-xs text-muted-foreground"
              title="不允许其他节点使用 KCP 代理 TCP 流到此节点。开启 KCP 代理的节点访问此节点时，依然使用原始 TCP 连接"
            >?</span>
          </div>
          <Switch v-model:checked="cfg.kcpInput" />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div class="flex items-center gap-1">
            <Label>QUIC 代理</Label>
            <span
              class="cursor-help text-xs text-muted-foreground"
              title="（不可与KCP代理同时开启）使用 QUIC 代理 TCP 流，提高在 UDP 丢包网络上的延迟和吞吐量"
            >?</span>
          </div>
          <Switch :checked="cfg.quicProxy" @update:checked="setQuicProxy" />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div class="flex items-center gap-1">
            <Label>禁用 QUIC 输入</Label>
            <span
              class="cursor-help text-xs text-muted-foreground"
              title="不允许其他节点使用 QUIC 代理 TCP 流到此节点。开启 QUIC 代理的节点访问此节点时，依然使用原始 TCP 连接"
            >?</span>
          </div>
          <Switch v-model:checked="cfg.quicInput" />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div>
            <Label>低延迟模式</Label>
            <p class="mt-0.5 text-xs text-muted-foreground">优先选择低延迟路径，弱网下延迟可能波动</p>
          </div>
          <Switch v-model:checked="cfg.latencyFirst" />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div>
            <Label>传输数据压缩</Label>
            <p class="mt-0.5 text-xs text-muted-foreground">zstd 可省带宽，略耗 CPU</p>
          </div>
          <Select
            :model-value="cfg.compression"
            :options="[
              { value: 'none', label: '不压缩（推荐）' },
              { value: 'zstd', label: 'zstd 算法' },
            ]"
            class="w-40"
            @update:model-value="cfg.compression = $event"
          />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div>
            <Label>UPnP 端口映射</Label>
            <p class="mt-0.5 text-xs text-muted-foreground">自动向路由器请求端口映射（需 EasyTier 2.6.2+）</p>
          </div>
          <Switch v-model:checked="cfg.upnp" />
        </div>
        <div class="flex items-center justify-between gap-4">
          <div>
            <Label>多线程</Label>
            <p class="mt-0.5 text-xs text-muted-foreground">使用多线程运行时提升吞吐</p>
          </div>
          <Switch v-model:checked="cfg.multiThread" />
        </div>
        <p class="col-span-2 text-xs text-muted-foreground">KCP 与 QUIC 代理不能同时开启，启用其一会自动关闭另一个。</p>
      </div>
      <Separator />
      <div>
        <p class="text-xs text-muted-foreground">当前启动参数</p>
        <p class="mt-1 break-all font-mono text-xs">{{ status.args.length ? status.args.join(' ') : '（启动后显示）' }}</p>
      </div>
    </Card>

    <Card class="space-y-4 p-5">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          <Users class="h-4 w-4 text-muted-foreground" />
          <h2 class="font-medium">房间信息</h2>
        </div>
        <Button variant="ghost" size="sm" :disabled="roomLoading || !status.running" @click="loadRoom">
          <RefreshCw class="h-4 w-4" :class="roomLoading ? 'animate-spin' : ''" />
          刷新
        </Button>
      </div>

      <p v-if="!status.running" class="text-sm text-muted-foreground">启动组网后可查看房间成员</p>
      <div v-else-if="room.error" class="space-y-2">
        <p class="text-sm text-destructive">{{ room.error }}</p>
        <Button variant="outline" size="sm" @click="showRaw = !showRaw">
          {{ showRaw ? '隐藏' : '显示' }}原始输出
        </Button>
        <pre v-if="showRaw" class="max-h-52 overflow-auto rounded-md bg-muted p-3 font-mono text-xs">{{ room.raw }}</pre>
      </div>
      <div v-else-if="room.peers.length === 0" class="text-sm text-muted-foreground">暂无其他成员</div>
      <div v-else class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead>
            <tr class="border-b border-border text-left text-xs text-muted-foreground">
              <th class="py-2 pr-3 font-medium">成员名</th>
              <th class="py-2 pr-3 font-medium">方式</th>
              <th class="py-2 pr-3 font-medium">虚拟 IP</th>
              <th class="py-2 pr-3 font-medium">延迟</th>
              <th class="py-2 pr-3 font-medium">丢包率</th>
              <th class="py-2 pr-3 font-medium">媒体库</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="peer in room.peers" :key="peer.ipv4 + peer.hostname" class="border-b border-border/50">
              <td class="py-2 pr-3">{{ peer.hostname || '-' }}</td>
              <td class="py-2 pr-3">
                <Badge :variant="costBadge(peer.cost).variant">{{ costBadge(peer.cost).text }}</Badge>
              </td>
              <td class="py-2 pr-3 font-mono text-xs">{{ peer.ipv4 || '-' }}</td>
              <td class="py-2 pr-3">{{ formatLatency(peer.latencyMs) }}</td>
              <td class="py-2 pr-3">{{ formatLoss(peer.loss) }}</td>
              <td class="py-2 pr-3">
                <Button variant="outline" size="sm" :disabled="!peer.ipv4" @click="openPeer(peer)">
                  查看媒体
                </Button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="text-xs text-muted-foreground">
        本地媒体库共 {{ localCount }} 个媒体，已计算哈希的条目可直接与对方比对。
      </p>
    </Card>

    <PeerMediaDialog
      v-model:open="dialogOpen"
      :peer="selectedPeer"
      :port="fullSettings?.port ?? 17001"
    />
  </div>
</template>