<script setup lang="ts">
import { MessageCircle, Github, Users, Info } from 'lucide-vue-next'
import Card from '../components/ui/Card.vue'
import { openUrl } from '../lib/api'
import { toast, errMsg } from '../lib/toast'

interface FeedbackChannel {
  name: string
  description: string
  url: string
  icon: any
  color: string
}

const channels: FeedbackChannel[] = [
  {
    name: 'B站私信',
    description: '通过 B 站私信联系作者，适合快速反馈问题或建议',
    url: 'https://space.bilibili.com/2199618',
    icon: MessageCircle,
    color: 'text-pink-600',
  },
  {
    name: 'GitHub Issue',
    description: '在 GitHub 上提交 Issue，适合报告 Bug 或功能建议',
    url: 'https://github.com/FlandreSatori/VRchat-LocalCinema/issues',
    icon: Github,
    color: 'text-gray-700 dark:text-gray-300',
  },
  {
    name: 'QQ 群',
    description: '加入 QQ 群（1128168990）与其他用户交流讨论',
    url: 'https://qm.qq.com/q/l9QaFgvJWo',
    icon: Users,
    color: 'text-blue-600',
  },
]

async function handleOpenUrl(url: string) {
  try {
    await openUrl(url)
  } catch (e) {
    toast.error(errMsg(e))
  }
}
</script>

<template>
  <div class="mx-auto max-w-3xl space-y-4 p-6">
    <h1 class="text-xl font-semibold">反馈与支持</h1>

    <Card class="p-5">
      <h2 class="font-medium">反馈渠道</h2>
      <p class="mt-2 text-sm text-muted-foreground">
        欢迎通过以下渠道反馈问题、提出建议或寻求帮助。
      </p>
      <div class="mt-4 space-y-3">
        <button
          v-for="channel in channels"
          :key="channel.name"
          type="button"
          class="flex w-full items-start gap-4 rounded-lg border border-border bg-card p-4 text-left transition-colors hover:bg-accent"
          @click="handleOpenUrl(channel.url)"
        >
          <component :is="channel.icon" :class="['mt-0.5 h-5 w-5 shrink-0', channel.color]" />
          <div class="flex-1">
            <h3 class="font-medium">{{ channel.name }}</h3>
            <p class="mt-1 text-sm text-muted-foreground">{{ channel.description }}</p>
          </div>
        </button>
      </div>
    </Card>

    <Card class="border-blue-200 bg-blue-50 p-5 dark:border-blue-900 dark:bg-blue-950/30">
      <div class="flex items-start gap-3">
        <Info class="mt-0.5 h-5 w-5 shrink-0 text-blue-600 dark:text-blue-400" />
        <div class="flex-1">
          <h2 class="font-medium text-blue-900 dark:text-blue-100">如何生成样本文件？</h2>
          <div class="mt-2 space-y-2 text-sm text-blue-800 dark:text-blue-200">
            <p>在反馈转码或播放问题时，可能需要提供样本文件。生成方式如下：</p>
            <ol class="ml-4 list-decimal space-y-1">
              <li>打开「媒体库」页面</li>
              <li>找到问题视频，右键点击视频卡片</li>
              <li>在弹出的右键菜单中选择「生成样本（前 10 秒）」</li>
              <li>等待样本生成完成，样本文件会保存在原视频所在目录</li>
              <li>将生成的样本文件发送给开发者以便定位问题</li>
            </ol>
            <p class="mt-2 text-xs">
              💡 提示：样本文件仅包含视频前 10 秒，体积较小，便于上传和分析。
            </p>
          </div>
        </div>
      </div>
    </Card>

    <Card class="p-5">
      <h2 class="font-medium">反馈建议</h2>
      <ul class="mt-2 space-y-1.5 text-sm text-muted-foreground">
        <li>• 报告 Bug 时，请尽量描述复现步骤和错误信息</li>
        <li>• 如遇到转码或播放问题，建议提供样本文件</li>
        <li>• 功能建议请说明使用场景和期望效果</li>
        <li>• 可以在日志目录（关于页面查看）中找到详细日志</li>
      </ul>
    </Card>
  </div>
</template>
