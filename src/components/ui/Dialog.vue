<script setup lang="ts">
import {
  DialogRoot,
  DialogPortal,
  DialogOverlay,
  DialogContent,
  DialogTitle,
  DialogDescription,
  DialogClose,
} from 'reka-ui'
import { X } from 'lucide-vue-next'
import { cn } from '../../lib/utils'

defineProps<{
  open: boolean
  title: string
  description?: string
  class?: string
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()
</script>

<template>
  <DialogRoot :open="open" @update:open="emit('update:open', $event)">
    <DialogPortal>
      <DialogOverlay
        class="fixed inset-0 z-50 bg-black/60 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=closed]:animate-out data-[state=closed]:fade-out-0"
      />
      <DialogContent
        :class="
          cn(
            'fixed left-1/2 top-1/2 z-50 max-h-[90vh] w-full max-w-lg -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-lg border border-border bg-card p-6 shadow-lg duration-200 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95',
            $props.class
          )
        "
      >
        <div class="flex items-start justify-between gap-4">
          <DialogTitle class="text-lg font-semibold">{{ title }}</DialogTitle>
          <DialogClose
            class="cursor-pointer rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <X class="h-4 w-4" />
          </DialogClose>
        </div>
        <DialogDescription v-if="description" class="mt-1 text-sm text-muted-foreground">
          {{ description }}
        </DialogDescription>
        <div class="mt-4">
          <slot />
        </div>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>
