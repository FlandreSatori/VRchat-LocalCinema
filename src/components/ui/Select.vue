<script setup lang="ts">
import { ChevronDown } from 'lucide-vue-next'
import { cn } from '../../lib/utils'

export interface SelectOption {
  value: string
  label: string
  disabled?: boolean
}

defineProps<{
  modelValue: string
  options: SelectOption[]
  class?: string
  disabled?: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

function onChange(e: Event) {
  emit('update:modelValue', (e.target as HTMLSelectElement).value)
}
</script>

<template>
  <div :class="cn('relative', $props.class)">
    <select
      :value="modelValue"
      :disabled="disabled"
      @change="onChange"
      class="h-9 w-full cursor-pointer appearance-none rounded-md border border-input bg-background px-3 pr-8 text-sm shadow-sm transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50"
    >
      <option v-for="opt in options" :key="opt.value" :value="opt.value" :disabled="opt.disabled">{{ opt.label }}</option>
    </select>
    <ChevronDown class="pointer-events-none absolute right-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
  </div>
</template>
