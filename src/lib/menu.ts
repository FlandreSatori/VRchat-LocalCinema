import type { Component } from 'vue'

export type MenuEntry =
  | {
      type: 'item'
      label: string
      icon?: Component
      danger?: boolean
      disabled?: boolean
      onSelect?: () => void
    }
  | { type: 'separator' }
