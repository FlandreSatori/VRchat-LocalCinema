<script setup lang="ts">
import { Separator } from 'reka-ui'
import { cn } from '../../lib/utils'

withDefaults(
  defineProps<{
    class?: string
    orientation?: 'horizontal' | 'vertical'
  }>(),
  {
    orientation: 'horizontal',
  }
)
</script>

<template>
  <Separator
    :orientation="orientation"
    :class="cn('shrink-0 bg-border', orientation === 'vertical' ? 'h-full w-px' : 'h-px w-full', $props.class)"
  />
</template>
