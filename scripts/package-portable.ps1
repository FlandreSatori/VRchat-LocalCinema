# VRchat-LocalCinema 绿色便携版打包脚本
# 用法: 在项目根目录执行  powershell -File scripts\package-portable.ps1
param(
    [string]$Configuration = "release"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Write-Host "==> 构建前端与 Tauri 应用..."
Push-Location $root
try {
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "前端构建失败" }

    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    Push-Location src-tauri
    try {
        cargo build --release --features custom-protocol
        if ($LASTEXITCODE -ne 0) { throw "Rust 构建失败" }
    } finally {
        Pop-Location
    }

    $exe = Join-Path $root "src-tauri\target\release\vrchat-localcinema.exe"
    if (-not (Test-Path $exe)) { throw "未找到构建产物 $exe" }

    $outDir = Join-Path $root "dist-portable\VRchat-LocalCinema"
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    $portableExe = Join-Path $outDir "VRchat-LocalCinema.exe"
    Copy-Item $exe $portableExe -Force
    if ((Get-FileHash $exe -Algorithm SHA256).Hash -ne (Get-FileHash $portableExe -Algorithm SHA256).Hash) {
        throw "便携版 EXE 校验失败"
    }

    # 保留已有的便携标记、说明和用户数据
    $marker = Join-Path $outDir "portable.dat"
    if (-not (Test-Path $marker)) { New-Item -ItemType File -Path $marker | Out-Null }
    $readme = Join-Path $outDir "README.md"
    if (-not (Test-Path $readme)) { Copy-Item (Join-Path $root "README.md") $readme }

    $zip = Join-Path $root "dist-portable\VRchat-LocalCinema-portable.zip"
    Compress-Archive -Path $portableExe, $marker, $readme -DestinationPath $zip -Force
    Write-Host "==> 完成: $zip"
} finally {
    Pop-Location
}
