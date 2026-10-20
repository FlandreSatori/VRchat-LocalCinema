<script setup lang="ts">
import { ChevronDown } from 'lucide-vue-next'
import Card from './Card.vue'
import { cn } from '../../lib/utils'

const props = defineProps<{
  title: string
  open: boolean
  class?: string
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()
</script>

<template>
  <Card :class="cn('overflow-hidden border-l-[3px] border-l-primary/60 p-0', $props.class)">
    <div class="flex items-center justify-between gap-2 bg-muted/50 pr-3">
      <button
        type="button"
        class="flex min-w-0 flex-1 cursor-pointer items-center gap-3 px-4 py-3.5 text-left transition-colors hover:bg-muted/80"
        @click="emit('update:open', !props.open)"
      >
        <h2 class="min-w-0 flex-1 text-base font-semibold tracking-tight text-foreground">{{ title }}</h2>
        <ChevronDown
          class="h-4 w-4 shrink-0 text-muted-foreground transition-transform duration-200"
          :class="open ? 'rotate-180' : ''"
        />
      </button>
      <div v-if="$slots.actions" class="shrink-0">
        <slot name="actions" />
      </div>
    </div>
    <div v-show="open" class="space-y-4 border-t border-border px-4 py-4">
      <slot />
    </div>
  </Card>
</template>
