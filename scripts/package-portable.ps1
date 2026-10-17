# VRchat-LocalCinema portable packaging script
# Produces in dist-portable:
#   VRchat-LocalCinema-v<version>-base.zip   app only, zipped (components can be auto/manual downloaded in-app)
#   VRchat-LocalCinema-v<version>-full\      app + workspace components\ copied as-is, NOT zipped
# Usage:
#   powershell -File scripts\package-portable.ps1
# Note: keep this file ASCII-only; Windows PowerShell 5.1 misparses non-BOM UTF-8.
param(
    [string]$Configuration = "release"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

Push-Location $root
try {
    Write-Host "==> Building frontend..."
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "Frontend build failed" }

    Write-Host "==> Building Tauri app (release)..."
    # Prefer the rustup 1.93 toolchain; an old Rust (e.g. 1.88) on PATH breaks Tauri 2.12
    $toolchainBin = "$env:USERPROFILE\.rustup\toolchains\1.93.0-x86_64-pc-windows-msvc\bin"
    if (Test-Path $toolchainBin) { $env:PATH = "$toolchainBin;$env:PATH" }
    else { $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH" }
    Remove-Item Env:RUSTC -ErrorAction SilentlyContinue
    Push-Location src-tauri
    try {
        cargo build --release --features custom-protocol
        if ($LASTEXITCODE -ne 0) { throw "Rust build failed" }
    } finally {
        Pop-Location
    }

    $exe = Join-Path $root "src-tauri\target\release\vrchat-localcinema.exe"
    if (-not (Test-Path $exe)) { throw "Build output not found: $exe" }

    # ===== Bundled components (copied from the workspace, never re-downloaded here) =====
    $componentsDir = Join-Path $root "components"
    $hasFfmpeg = (Test-Path (Join-Path $componentsDir "ffmpeg\ffmpeg.exe")) -and (Test-Path (Join-Path $componentsDir "ffmpeg\ffprobe.exe"))
    $hasEasytier = (Test-Path (Join-Path $componentsDir "easytier\easytier-core.exe")) -and (Test-Path (Join-Path $componentsDir "easytier\easytier-cli.exe"))
    $fullReady = $hasFfmpeg -and $hasEasytier

    # ===== Staging and packaging =====
    $outRoot = Join-Path $root "dist-portable"
    New-Item -ItemType Directory -Force -Path $outRoot | Out-Null

    function New-Package {
        param(
            [string]$Name,
            [switch]$WithComponents,
            [switch]$SkipZip
        )
        $stage = Join-Path $outRoot $Name
        if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
        New-Item -ItemType Directory -Force -Path $stage | Out-Null

        Write-Host "==> Staging $Name..."
        Copy-Item $exe (Join-Path $stage "VRchat-LocalCinema.exe") -Force
        New-Item -ItemType File -Path (Join-Path $stage "portable.dat") -Force | Out-Null
        Copy-Item (Join-Path $root "README.md") (Join-Path $stage "README.md") -Force

        if ($WithComponents) {
            Copy-Item -Recurse -Force $componentsDir (Join-Path $stage "components")
        }

        if ($SkipZip) {
            Write-Host "==> Done (folder only, not zipped): $stage"
            return
        }

        $zip = Join-Path $outRoot "$Name.zip"
        # Compress the staging dir itself: portable.dat is always included and the zip
        # extracts into a named top-level folder.
        Compress-Archive -Path $stage -DestinationPath $zip -Force
        Write-Host "==> Done: $zip"
    }

    # App version drives the package file names (single source: package.json)
    $appVersion = (Get-Content (Join-Path $root "package.json") -Raw | ConvertFrom-Json).version

    New-Package -Name "VRchat-LocalCinema-v$appVersion-base"
    if ($fullReady) {
        # Full package is staged as a plain folder only (no zip): components\ is copied as-is.
        New-Package -Name "VRchat-LocalCinema-v$appVersion-full" -WithComponents -SkipZip
    } else {
        Write-Warning "components\ incomplete (need ffmpeg\ffmpeg.exe+ffprobe.exe and easytier\easytier-core.exe+easytier-cli.exe) - full folder skipped."
    }
} finally {
    Pop-Location
}
