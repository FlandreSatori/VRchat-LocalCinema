// 版本号唯一来源：package.json 的 "version"。
// 本脚本把它同步到 src-tauri/Cargo.toml（Cargo 不支持引用外部文件）。
// 其余位置都已改为派生：tauri.conf.json 直接指向 ../package.json；
// 前端由 vite.config.ts 的 define 注入 __APP_VERSION__。
// 因此升级版本只需改 package.json 一处（`npm run build` 会自动同步 Cargo.toml）。
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version

const cargoPath = join(root, 'src-tauri', 'Cargo.toml')
const cargo = readFileSync(cargoPath, 'utf8')
// 只替换 [package] 段顶格的 version 行（依赖项里的 version 都有缩进）
const updated = cargo.replace(/^version = "[^"]*"/m, `version = "${version}"`)
if (updated !== cargo) {
  writeFileSync(cargoPath, updated)
  console.log(`Cargo.toml version -> ${version}`)
}
