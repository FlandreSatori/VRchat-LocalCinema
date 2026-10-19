import { reactive } from 'vue'

export type ToastType = 'success' | 'error' | 'info'

export interface ToastItem {
  id: number
  type: ToastType
  message: string
}

export const toasts = reactive<ToastItem[]>([])

let nextId = 1

/// 停留时长按字数计算：字越多看得越久（每字 130ms，最短 2s，最长 10s）
function durationFor(message: string): number {
  return Math.min(10_000, Math.max(2_000, message.length * 130))
}

function push(type: ToastType, message: string) {
  const id = nextId++
  toasts.push({ id, type, message })
  setTimeout(() => {
    const idx = toasts.findIndex((t) => t.id === id)
    if (idx !== -1) toasts.splice(idx, 1)
  }, durationFor(message))
}

export const toast = {
  success: (message: string) => push('success', message),
  error: (message: string) => push('error', message),
  info: (message: string) => push('info', message),
}

export function errMsg(e: unknown): string {
  return typeof e === 'string' ? e : e instanceof Error ? e.message : String(e)
}
