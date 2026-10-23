import { reactive } from 'vue'
import type { MediaItem } from './api'
import { transcodeStreamingItem, tunnelStore, frpStore, lanListenAvailable, settingsCache } from './store'
import { getUploadBaseline } from './api'
import { toast, errMsg } from './toast'

export const copyReminder = reactive({ open: false, busy: false, item: null as MediaItem | null, bitrate: 0, baseline: 10, threshold: 7 })
let reminderPending = false

export async function maybeRemindAfterCopy(item: MediaItem, mode: 'localhost' | 'lan' | 'tunnel', actualItem = item) {
  if (reminderPending || copyReminder.open || item.sourceKind === 'online' || actualItem.sourceKind === 'online') return
  const shareable = lanListenAvailable.value || settingsCache.value?.listenMode === 'lan' || tunnelStore.running || frpStore.running || mode === 'lan' || mode === 'tunnel'
  if (!shareable || !actualItem.durationMs || actualItem.durationMs <= 0 || !actualItem.fileSize) return
  reminderPending = true
  try {
    let baseline = 10
    try {
      const configuredBaseline = await getUploadBaseline()
      if (Number.isFinite(configuredBaseline) && configuredBaseline > 0) baseline = configuredBaseline
    } catch { /* default baseline */ }
    const bitrate = actualItem.fileSize * 8 / (actualItem.durationMs * 1000)
    if (bitrate <= baseline * 0.7) return
    copyReminder.item = item
    copyReminder.bitrate = bitrate
    copyReminder.baseline = baseline
    copyReminder.threshold = baseline * 0.7
    copyReminder.open = true
  } finally {
    reminderPending = false
  }
}

export async function compressFromReminder() {
  const item = copyReminder.item
  if (!item || copyReminder.busy) return
  copyReminder.busy = true
  copyReminder.open = false
  try {
    await transcodeStreamingItem(item)
    copyReminder.open = false
    toast.info('已加入一键压制队列，完成后请使用新媒体卡片')
  } catch (e) { toast.error(errMsg(e)) }
  finally { copyReminder.busy = false }
}
