<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { AlertTriangle } from 'lucide-vue-next'
import Dialog from './ui/Dialog.vue'
import Button from './ui/Button.vue'
import Separator from './ui/Separator.vue'

interface Props {
  open: boolean
  /**
   * 是否为强制阅读模式（首次启动）
   * 强制模式下无法直接关闭，需要倒计时 10 秒后才能同意
   */
  enforced?: boolean
}

interface Emits {
  'update:open': [boolean]
  agree: []
}

const props = defineProps<Props>()
const emit = defineEmits<Emits>()

const countdown = ref(10)
const countdownTimer = ref<ReturnType<typeof setInterval> | null>(null)

const canAgree = computed(() => !props.enforced || countdown.value <= 0)

watch(
  () => props.open,
  (isOpen) => {
    if (isOpen) {
      countdown.value = 10
      countdownTimer.value = setInterval(() => {
        countdown.value--
        if (countdown.value < 0 && countdownTimer.value) {
          clearInterval(countdownTimer.value)
          countdownTimer.value = null
        }
      }, 1000)
    } else {
      if (countdownTimer.value) {
        clearInterval(countdownTimer.value)
        countdownTimer.value = null
      }
    }
  },
  { immediate: true }
)

function handleAgree() {
  if (canAgree.value) {
    emit('agree')
    emit('update:open', false)
  }
}

function handleOpenChange(open: boolean) {
  // 强制模式下禁止任何关闭尝试
  if (props.enforced && !open) {
    return
  }
  emit('update:open', open)
}

import type { ComponentProps } from 'vue'
type DialogProps = ComponentProps<typeof Dialog>

const dialogProps = computed<DialogProps>(() => ({
  open: props.open,
  onOpenChange: handleOpenChange,
}))
</script>

<template>
  <Dialog v-bind="dialogProps">
    <div class="sm:max-w-xl">
      <div class="flex items-start gap-4">
        <AlertTriangle class="mt-1 h-5 w-5 shrink-0 text-amber-600" />
        <div class="flex-1">
          <h2 class="text-lg font-semibold">免责声明</h2>
        </div>
      </div>

      <Separator class="my-4" />

      <div class="max-h-[60vh] space-y-3 overflow-y-auto pr-4 text-sm leading-relaxed text-muted-foreground">
        <section>
          <h3 class="mb-1.5 font-medium text-foreground">1. 软件声明</h3>
          <p>本软件为独立开源项目，与 VRchat 官方无任何关联。本软件由社区维护，不代表官方立场，请勿将其作为官方渠道使用。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">2. 内容声明</h3>
          <p>本软件仅用于本地媒体分享，与影视资源提供方无任何关联，请勿播放任何违反当地法规的内容。用户需自行确保所分享内容具有合法的使用权，包括但不限于版权授权、许可协议等。用户因分享未授权内容而产生的一切法律后果由用户自行承担。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">3. 知识产权声明</h3>
          <p>本软件使用的图标及其他视觉资源力求尊重知识产权。如您发现任何侵权内容，请及时告知我们，我们将立即下架相关资源。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">4. 开源与赔偿</h3>
          <p>本软件为开源项目，完全免费提供。如果您通过第三方渠道被骗付费购买本软件，或因使用本软件而遭受经济损失，请立即要求退款，并向相关欺诈者举报。本项目开发者不对任何欺诈行为负责。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">5. 网络安全</h3>
          <p>在使用媒体分享功能时，请仅在可信网络环境中使用，避免在不安全的网络（如公共 WiFi）中分享敏感内容。您应当了解网络通信可能存在的安全风险，包括数据拦截、中间人攻击等。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">6. 使用风险</h3>
          <p>本软件依赖第三方组件（如 FFmpeg、EasyTier），这些组件可能存在安全漏洞。用户应定期更新软件和依赖组件，并对使用过程中的任何风险自行评估。本软件开发者对由此产生的任何直接或间接损失不承担责任。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">7. 免责声明</h3>
          <p>本软件按「现状」提供，不提供任何明示或默示的担保。在适用法律允许的最大范围内，开发者对软件中的任何错误、中断、或不适合特定用途不承担任何责任。</p>
        </section>

        <section>
          <h3 class="mb-1.5 font-medium text-foreground">8. 法律适用</h3>
          <p>您使用本软件即表示您已阅读、理解并同意遵守本免责声明及相关法律法规。如有违反，后果由您自行承担。</p>
        </section>
      </div>

      <Separator class="my-4" />

      <div class="flex items-center justify-between gap-2">
        <div class="text-xs text-muted-foreground">
          <span v-if="enforced && countdown > 0" class="font-medium">
            请阅读完整内容后同意（{{ countdown }}s）
          </span>
          <span v-else-if="enforced" class="font-medium text-foreground">
            您可以同意并继续使用
          </span>
        </div>
        <div class="flex gap-2">
          <Button
            v-if="!enforced"
            variant="outline"
            @click="handleOpenChange(false)"
          >
            关闭
          </Button>
          <Button
            :disabled="!canAgree"
            @click="handleAgree"
          >
            {{ enforced ? '我已阅读并同意' : '同意' }}
          </Button>
        </div>
      </div>
    </div>
  </Dialog>
</template>
