<script setup lang="ts">
import {
  DropdownMenuRoot,
  DropdownMenuTrigger,
  DropdownMenuPortal,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from 'reka-ui'
import type { MenuEntry } from '../../lib/menu'

withDefaults(
  defineProps<{
    items: MenuEntry[]
    align?: 'start' | 'center' | 'end'
  }>(),
  {
    align: 'start',
  }
)
</script>

<template>
  <DropdownMenuRoot>
    <DropdownMenuTrigger as-child>
      <slot />
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent
        :align="align"
        :side-offset="4"
        class="z-50 min-w-44 rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95"
      >
        <template v-for="(entry, i) in items" :key="i">
          <DropdownMenuSeparator v-if="entry.type === 'separator'" class="my-1 h-px bg-border" />
          <DropdownMenuItem
            v-else
            :disabled="entry.disabled"
            @select="entry.onSelect?.()"
            class="flex cursor-pointer select-none items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-none transition-colors data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50"
            :class="entry.danger ? 'text-destructive data-[highlighted]:bg-destructive/10' : ''"
          >
            <component :is="entry.icon" v-if="entry.icon" class="h-4 w-4" />
            {{ entry.label }}
          </DropdownMenuItem>
        </template>
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
