import type { Component } from 'vue'

export type MenuEntry =
  | {
      type: 'item'
      label: string
      icon?: Component
      danger?: boolean
      disabled?: boolean
      /** 悬停提示；禁用项用来说明不可用原因 */
      title?: string
      onSelect?: () => void
    }
  | { type: 'separator' }
