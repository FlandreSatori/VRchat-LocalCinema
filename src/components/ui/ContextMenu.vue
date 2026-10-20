<script setup lang="ts">
import {
  ContextMenuRoot,
  ContextMenuTrigger,
  ContextMenuPortal,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
} from 'reka-ui'
import type { MenuEntry } from '../../lib/menu'

defineProps<{
  items: MenuEntry[]
}>()
</script>

<template>
  <ContextMenuRoot>
    <ContextMenuTrigger as-child>
      <slot />
    </ContextMenuTrigger>
    <ContextMenuPortal>
      <ContextMenuContent
        class="z-50 min-w-48 rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95"
      >
        <template v-for="(entry, i) in items" :key="i">
          <ContextMenuSeparator v-if="entry.type === 'separator'" class="my-1 h-px bg-border" />
          <ContextMenuItem
            v-else
            :disabled="entry.disabled"
            :title="entry.title"
            @select="entry.onSelect?.()"
            class="flex cursor-pointer select-none items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none transition-colors data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50"
            :class="entry.danger ? 'text-destructive data-[highlighted]:bg-destructive/10' : ''"
          >
            <component :is="entry.icon" v-if="entry.icon" class="h-4 w-4" />
            {{ entry.label }}
          </ContextMenuItem>
        </template>
      </ContextMenuContent>
    </ContextMenuPortal>
  </ContextMenuRoot>
</template>
