<script setup lang="ts">
import { cn } from '../../lib/utils'

withDefaults(
  defineProps<{
    modelValue?: string | number
    class?: string
    type?: string
    placeholder?: string
    disabled?: boolean
    readonly?: boolean
    min?: number | string
    max?: number | string
  }>(),
  {
    modelValue: '',
    type: 'text',
    disabled: false,
    readonly: false,
  }
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

function onInput(e: Event) {
  emit('update:modelValue', (e.target as HTMLInputElement).value)
}
</script>

<template>
  <input
    :type="type"
    :value="modelValue"
    :placeholder="placeholder"
    :disabled="disabled"
    :readonly="readonly"
    :min="min"
    :max="max"
    @input="onInput"
    :class="
      cn(
        'flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-sm transition-colors outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50',
        $props.class
      )
    "
  />
</template>
