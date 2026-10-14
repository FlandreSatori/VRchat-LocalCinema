<script setup lang="ts">
import Dialog from './ui/Dialog.vue'
import Button from './ui/Button.vue'
import { confirmState, acceptConfirm, cancelConfirm } from '../lib/confirm'
</script>

<template>
  <Dialog
    :open="confirmState.open"
    :title="confirmState.title"
    :description="confirmState.description"
    @update:open="cancelConfirm()"
  >
    <div class="flex justify-end gap-2">
      <Button type="button" variant="outline" @click="cancelConfirm()">取消</Button>
      <Button
        type="button"
        :variant="confirmState.danger ? 'destructive' : 'default'"
        @click="acceptConfirm()"
      >
        {{ confirmState.confirmText }}
      </Button>
    </div>
  </Dialog>
</template>
