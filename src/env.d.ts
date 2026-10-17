/// <reference types="vite/client" />

/** 由 vite.config.ts 的 define 注入，值来自 package.json 的 version */
declare const __APP_VERSION__: string

declare module '*.vue' {
  import type { DefineComponent } from 'vue'
  const c: DefineComponent<{}, {}, any>
  export default c
}
