import {
  loadEstimateSource, probeTranscodeEstimate, cancelTranscodeEstimate,
  type EstimateSource, type TranscodeOptions,
} from './api'

export interface TranscodeEstimate {
  layer: 'L1' | 'L2'
  seconds: number | null
  fps: number | null
  duration: number | null
  status: 'loading' | 'waiting' | 'probing' | 'complete' | 'failed'
  /** 经验估算与短样本都不保证整片精度。 */
  confidence: 'heuristic' | 'sampled' | 'unavailable'
  reason: string
  calibrationK: number
  effectiveOptions: TranscodeOptions
}
export interface EstimatorWeights {
  baseFps: number
  codec: Record<string, number>
  preset: Record<string, number>
  tenBit: number
  burn: number
  audioRealtime: number
  copyBytesPerSecond: number
}
/** FPS 乘数；AV1 为实际 libaom-av1 cpu-used=6 的保守起点，不套用 SVT-AV1 或 x264 preset。 */
export const DEFAULT_ESTIMATOR_WEIGHTS: EstimatorWeights = {
  baseFps: 120,
  codec: { h264: 1, h265: 0.32, hevc: 0.32, av1: 1 / 12 },
  preset: { ultrafast: 3.2, superfast: 2.4, veryfast: 1.8, faster: 1.4, fast: 1.2, medium: 1, slow: 0.62, slower: 0.35, veryslow: 0.18 },
  tenBit: 0.75, burn: 0.85, audioRealtime: 80, copyBytesPerSecond: 300_000_000,
}
const positive = (n: number | null | undefined): n is number => typeof n === 'number' && Number.isFinite(n) && n > 0
const videoMode = (o: TranscodeOptions) => o.subtitleMode === 'burn' && o.videoMode === 'copy' ? 'h264' : o.videoMode ?? 'h264'

/** 与后端有效参数清理一致；缓存 profile 不缓存上次请求的编码选项。 */
function withOptions(source: EstimateSource, requested: TranscodeOptions): EstimateSource {
  const o = { ...requested }
  const streams = source.streams
  const video = streams.find(s => s.kind === 'video' && s.index === o.videoStream) ?? streams.find(s => s.kind === 'video')
  const audio = streams.find(s => s.kind === 'audio' && s.index === o.audioStream) ?? streams.find(s => s.kind === 'audio')
  if (o.videoMode === 'copy' && !video?.copyable) o.videoMode = 'h264'
  if (o.includeAudio !== false && o.audioMode === 'copy' && !audio?.copyable) o.audioMode = 'aac'
  if (o.subtitleMode && o.subtitleMode !== 'drop') {
    const sub = streams.find(s => s.kind === 'subtitle' && s.index === o.subtitleStream)
      ?? streams.find(s => s.kind === 'subtitle' && s.textSubtitle)
    if (sub?.textSubtitle) o.subtitleStream = sub.index
    else { o.subtitleStream = null; o.subtitleMode = 'drop' }
  }
  return { ...source, effectiveOptions: o }
}

/** profile 已加载时同步估算；K = actual FPS / raw FPS。 */
export function estimateL1(source: EstimateSource, weights = DEFAULT_ESTIMATOR_WEIGHTS, calibrationK = 1): TranscodeEstimate {
  const p = source.profile
  const o = source.effectiveOptions
  const mode = videoMode(o)
  const duration = positive(p.duration) ? p.duration : positive(p.totalFrames) && positive(p.fps) ? p.totalFrames / p.fps : null
  const audio = o.includeAudio !== false && source.streams.some(s => s.kind === 'audio') && o.audioMode !== 'copy'
  const audioSeconds = audio && duration !== null ? duration / weights.audioRealtime : 0
  const k = positive(calibrationK) ? calibrationK : 1
  let seconds: number | null = null
  let fps: number | null = null
  if (mode === 'copy') {
    if (positive(p.fileSize) && (!audio || duration !== null)) seconds = p.fileSize / weights.copyBytesPerSecond + audioSeconds
  } else if (positive(p.width) && positive(p.height) && positive(p.totalFrames) && (!audio || duration !== null)) {
    const scaling = !!o.resolution && o.resolution !== 'original'
    const targetHeight = scaling ? Number(o.resolution) : p.height
    if (positive(targetHeight)) {
      const pixels = p.width * p.height * (targetHeight / p.height) ** 2
      const pixFmt = o.pixFmt === 'auto' ? p.pixFmt : o.pixFmt ?? (mode === 'h265' ? 'yuv420p10le' : 'yuv420p')
      const preset = mode === 'av1' ? 1 : weights.preset[o.preset ?? 'medium'] ?? 1
      const crf = Math.max(0.75, Math.min(1.25, 1 + ((o.crf ?? 20) - 23) * 0.015))
      const decode = ({ h264: 1, hevc: 0.9, h265: 0.9, av1: 0.85, vp9: 0.85 } as Record<string, number>)[p.videoCodec.toLowerCase()] ?? 1
      const throughput = weights.baseFps * (2_073_600 / pixels) ** 0.85 * (weights.codec[mode] ?? 1) * preset
        * (pixFmt?.includes('10') ? weights.tenBit : 1) * crf * (scaling ? 0.92 : 1)
        * (o.subtitleMode === 'burn' ? weights.burn : 1) * decode * (p.width * p.height >= 8_294_400 ? 0.8 : 1) * k
      if (positive(throughput)) { fps = throughput; seconds = p.totalFrames / throughput + audioSeconds }
    }
  }
  if (!positive(seconds)) seconds = null
  const reason = seconds === null ? '源参数不足，无法估算耗时'
    : mode === 'copy' ? '直拷按 300 MB/s + AAC 时长 / 80 估算；跳过编码采样'
      : `经验估算 ±35%（参考范围，非精度保证）${mode === 'av1' ? '；适配 libaom-av1 cpu-used=6，非 SVT-AV1' : ''}`
  return { layer: 'L1', seconds, fps, duration, status: 'complete', confidence: seconds === null ? 'unavailable' : 'heuristic',
    reason, calibrationK: k, effectiveOptions: o }
}

export interface EstimatorDependencies {
  load: typeof loadEstimateSource
  probe: typeof probeTranscodeEstimate
  cancel: typeof cancelTranscodeEstimate
}
export interface EstimatorConfig {
  weights?: Partial<EstimatorWeights>
  /** 测试或宿主可注入 IPC，无 Vue 依赖。 */
  dependencies?: EstimatorDependencies
}

/** 首次读取 profile；后续参数修改同步发布 L1，最后一次修改 400ms 后采样。 */
export function createTranscodeEstimator(config: EstimatorConfig = {}) {
  const weights: EstimatorWeights = { ...DEFAULT_ESTIMATOR_WEIGHTS, ...config.weights,
    codec: { ...DEFAULT_ESTIMATOR_WEIGHTS.codec, ...config.weights?.codec },
    preset: { ...DEFAULT_ESTIMATOR_WEIGHTS.preset, ...config.weights?.preset } }
  const deps = config.dependencies ?? { load: loadEstimateSource, probe: probeTranscodeEstimate, cancel: cancelTranscodeEstimate }
  let generation = 0
  let disposed = false
  let timer: ReturnType<typeof setTimeout> | undefined
  let settle: ((result: TranscodeEstimate | null) => void) | undefined
  let active: { id: string; key: string; kind: 'load' | 'probe' } | undefined
  let pending: { key: string; promise: Promise<EstimateSource | null> } | undefined
  let queue: Promise<unknown> = Promise.resolve()
  const profiles = new Map<string, EstimateSource>()
  const calibrations = new Map<string, number>()
  const instanceId = crypto.randomUUID()
  let sequence = 0
  const sourceKey = (id: string, o: TranscodeOptions) => JSON.stringify([id, o.videoStream ?? null])
  const calibrationKey = (id: string, source: EstimateSource) => JSON.stringify([id, source.profile.videoStream, videoMode(source.effectiveOptions)])

  function cancelActive() {
    const job = active
    if (!job) return
    // 队列等待取消应答和旧操作退出，避免旧 cancel 或迟到 begin 影响新探测。
    const cancelled = deps.cancel(job.id).catch(() => {})
    queue = Promise.all([queue, cancelled]).then(() => {})
  }
  function stop(keepSource?: string) {
    generation++
    clearTimeout(timer)
    timer = undefined
    if (active && (active.kind === 'probe' || active.key !== keepSource)) cancelActive()
    if (pending?.key !== keepSource) pending = undefined
    settle?.(null)
    settle = undefined
    return queue.then(() => {})
  }
  function enqueue<T>(key: string, kind: 'load' | 'probe', valid: () => boolean, run: (requestId: string) => Promise<T>): Promise<T | null> {
    const result = queue.then(async () => {
      if (!valid()) return null
      const job = { id: `${instanceId}-${++sequence}`, key, kind }
      active = job
      try { return await run(job.id) }
      finally { if (active === job) active = undefined }
    })
    queue = result.catch(() => {})
    return result
  }
  function estimate(id: string, options: TranscodeOptions, onUpdate?: (result: TranscodeEstimate) => void): Promise<TranscodeEstimate | null> {
    const key = sourceKey(id, options)
    void stop(key)
    if (disposed) return Promise.resolve(null)
    const ticket = generation
    const current = () => !disposed && generation === ticket
    const requested = { ...options }
    const cached = profiles.get(key)
    if (!cached && pending?.key !== key) {
      const entry = { key, promise: Promise.resolve<EstimateSource | null>(null) }
      pending = entry
      entry.promise = enqueue(key, 'load', () => !disposed && pending === entry, requestId => deps.load(id, requested, requestId))
        .then(source => {
          if (source && pending === entry && !disposed) {
            profiles.set(key, source)
            // 自动视频轨和明确指定同一轨共用 profile。
            profiles.set(sourceKey(id, { videoStream: source.profile.videoStream }), source)
          }
          return source
        })
    }
    const ready = cached ? Promise.resolve(cached) : pending!.promise
    const local = (source: EstimateSource) => {
      const normalized = withOptions(source, requested)
      return estimateL1(normalized, weights, calibrations.get(calibrationKey(id, normalized)) ?? 1)
    }
    return new Promise(resolve => {
      settle = resolve
      const publish = (value: TranscodeEstimate) => { if (current()) onUpdate?.(value) }
      const finish = (value: TranscodeEstimate) => {
        publish(value)
        if (current()) { settle = undefined; resolve(value) }
      }
      const initial = (source: EstimateSource) => {
        const l1 = local(source)
        const copy = videoMode(l1.effectiveOptions) === 'copy'
        publish({ ...l1, status: copy ? 'complete' : 'waiting' })
      }
      if (cached) initial(cached)
      else publish({ layer: 'L1', seconds: null, fps: null, duration: null, status: 'loading', confidence: 'unavailable',
        reason: '正在读取源参数', calibrationK: 1, effectiveOptions: requested })
      void ready.then(source => { if (source && !cached && current()) initial(source) }).catch(() => {})
      timer = setTimeout(() => {
        timer = undefined
        void (async () => {
          let source: EstimateSource | null = null
          try {
            source = await ready
            if (!source || !current()) return
            const l1 = local(source)
            if (videoMode(l1.effectiveOptions) === 'copy') { finish(l1); return }
            publish({ ...l1, status: 'probing' })
            const probe = await enqueue(key, 'probe', current, requestId => deps.probe(id, requestId, requested))
            if (!probe || !current()) return
            if (!positive(probe.fps) || !positive(source.profile.totalFrames) || !probe.samples.length || probe.reason) {
              finish({ ...l1, status: 'failed', reason: `采样失败，回落经验估算：${probe.reason || '无有效 FPS / 帧数 / 样本'}` })
              return
            }
            const normalized = withOptions(source, probe.effectiveOptions)
            const baseline = estimateL1(normalized, weights)
            if (!positive(baseline.fps) || baseline.seconds === null) {
              finish({ ...l1, status: 'failed', reason: '源参数不足，无法将采样 FPS 换算为整片耗时' })
              return
            }
            const calibration = calibrationKey(id, normalized)
            const observedK = probe.fps / baseline.fps
            const previous = calibrations.get(calibration)
            const nextK = previous === undefined ? observedK : previous * 0.7 + observedK * 0.3
            calibrations.set(calibration, nextK)
            const audioSeconds = Math.max(0, baseline.seconds - source.profile.totalFrames / baseline.fps)
            finish({ ...baseline, layer: 'L2', seconds: source.profile.totalFrames / probe.fps + audioSeconds, fps: probe.fps,
              status: 'complete', confidence: 'sampled', calibrationK: nextK,
              reason: `短样本参考 ±10%（${probe.samples.length} 个有效点），非整片精度保证；未覆盖全部内容与封装 I/O${videoMode(normalized.effectiveOptions) === 'av1' ? '；libaom-av1，非 SVT-AV1' : ''}` })
          } catch (error) {
            if (!current()) return
            const fallback = source ? local(source) : { layer: 'L1' as const, seconds: null, fps: null, duration: null,
              confidence: 'unavailable' as const, calibrationK: 1, effectiveOptions: requested }
            finish({ ...fallback, status: 'failed', reason: `${source ? '采样失败，回落经验估算' : '无法读取源信息'}：${String(error)}` })
            if (!source && pending?.key === key) pending = undefined
          }
        })()
      }, 400)
    })
  }
  return {
    estimate,
    cancel: () => stop(),
    dispose() { disposed = true; profiles.clear(); calibrations.clear(); return stop() },
  }
}
