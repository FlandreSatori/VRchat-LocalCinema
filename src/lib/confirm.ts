import { reactive } from 'vue'

interface ConfirmOptions {
  title: string
  description: string
  confirmText?: string
  danger?: boolean
}

interface ConfirmState {
  open: boolean
  title: string
  description: string
  confirmText: string
  danger: boolean
  resolve: ((v: boolean) => void) | null
}

export const confirmState = reactive<ConfirmState>({
  open: false,
  title: '',
  description: '',
  confirmText: '确定',
  danger: false,
  resolve: null,
})

/// 应用内确认对话框（替代原生 window.confirm —— Tauri 下原生 confirm 返回 Promise 且不可控）
export function confirmDialog(opts: ConfirmOptions): Promise<boolean> {
  // 已有未决弹窗时按取消处理，避免悬挂
  confirmState.resolve?.(false)
  return new Promise((resolve) => {
    confirmState.title = opts.title
    confirmState.description = opts.description
    confirmState.confirmText = opts.confirmText ?? '确定'
    confirmState.danger = opts.danger ?? false
    confirmState.resolve = resolve
    confirmState.open = true
  })
}

function settle(v: boolean) {
  confirmState.open = false
  confirmState.resolve?.(v)
  confirmState.resolve = null
}

export function acceptConfirm() {
  settle(true)
}

export function cancelConfirm() {
  settle(false)
}
