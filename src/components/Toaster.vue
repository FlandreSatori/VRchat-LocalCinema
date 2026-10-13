<script setup lang="ts">
import { CheckCircle2, XCircle, Info } from 'lucide-vue-next'
import { toasts } from '../lib/toast'
</script>

<template>
  <div class="pointer-events-none fixed bottom-4 right-4 z-[100] flex w-80 flex-col gap-2">
    <TransitionGroup name="toast">
      <div
        v-for="t in toasts"
        :key="t.id"
        class="pointer-events-auto flex items-start gap-2 rounded-md border bg-card px-3 py-2.5 text-sm shadow-lg"
        :class="
          t.type === 'error'
            ? 'border-destructive/40'
            : t.type === 'success'
              ? 'border-emerald-500/40'
              : 'border-border'
        "
      >
        <CheckCircle2 v-if="t.type === 'success'" class="mt-0.5 h-4 w-4 shrink-0 text-emerald-500" />
        <XCircle v-else-if="t.type === 'error'" class="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
        <Info v-else class="mt-0.5 h-4 w-4 shrink-0 text-primary" />
        <span class="break-all leading-snug">{{ t.message }}</span>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-enter-active,
.toast-leave-active {
  transition:
    opacity 0.2s ease,
    transform 0.2s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(8px);
}
</style>
