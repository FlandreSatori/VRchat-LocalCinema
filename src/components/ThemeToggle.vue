<script setup lang="ts">
import { ref } from 'vue'
import { Sun, Moon } from 'lucide-vue-next'
import Button from './ui/Button.vue'
import { isDark, toggleTheme } from '../lib/theme'

const dark = ref(isDark())

function onToggle() {
  dark.value = toggleTheme()
}
</script>

<template>
  <Button variant="ghost" class="w-full justify-start" @click="onToggle">
    <Sun v-if="dark" class="h-4 w-4" />
    <Moon v-else class="h-4 w-4" />
    {{ dark ? '切换为浅色' : '切换为深色' }}
  </Button>
</template>
