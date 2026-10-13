# VRchat-LocalCinema portable packaging script
# Produces two archives in dist-portable:
#   VRchat-Localcinema-base.zip  app only (components can be auto/manual downloaded in-app)
#   VRchat-Localcinema-full.zip  app + bundled components (components\ffmpeg, components\easytier)
# Usage:
#   powershell -File scripts\package-portable.ps1                      # base only unless components\ is ready
#   powershell -File scripts\package-portable.ps1 -DownloadComponents  # fetch latest components first, then build full
# Note: keep this file ASCII-only; Windows PowerShell 5.1 misparses non-BOM UTF-8.
param(
    [string]$Configuration = "release",
    [switch]$DownloadComponents
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

function Get-LatestAsset {
    param(
        [string]$ApiUrl,
        [scriptblock]$Filter
    )
    $headers = @{ "User-Agent" = "VRchat-LocalCinema-packaging" }
    $release = Invoke-RestMethod -Uri $ApiUrl -Headers $headers
    foreach ($asset in $release.assets) {
        if (& $Filter $asset.name) {
            return @{ Name = $asset.name; Url = $asset.browser_download_url; Version = $release.tag_name }
        }
    }
    throw "No suitable Windows asset found at $ApiUrl - please download manually into components\"
}

function Install-Component {
    param(
        [string]$ApiUrl,
        [scriptblock]$Filter,
        [string]$DestDir,
        [string[]]$Required
    )
    New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
    $asset = Get-LatestAsset -ApiUrl $ApiUrl -Filter $Filter
    Write-Host "==> Downloading $($asset.Name) ($($asset.Version))..."
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("vlc-comp-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $zipPath = Join-Path $tmp $asset.Name
        Invoke-WebRequest -Uri $asset.Url -OutFile $zipPath -UseBasicParsing
        $extractDir = Join-Path $tmp "x"
        Expand-Archive -Path $zipPath -DestinationPath $extractDir -Force
        # Same as the in-app installer: keep only exe/dll, flattened into dest dir
        Get-ChildItem -Path $extractDir -Recurse -File |
            Where-Object { $_.Extension -in ".exe", ".dll" } |
            ForEach-Object { Copy-Item $_.FullName (Join-Path $DestDir $_.Name) -Force }
        foreach ($exe in $Required) {
            if (-not (Test-Path (Join-Path $DestDir $exe))) {
                throw "Component archive incomplete, missing $exe - download manually into $DestDir"
            }
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

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

    # ===== Bundled components (used by the full package) =====
    $componentsDir = Join-Path $root "components"
    $ffmpegDir = Join-Path $componentsDir "ffmpeg"
    $easytierDir = Join-Path $componentsDir "easytier"

    if ($DownloadComponents) {
        Install-Component -ApiUrl "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest" `
            -Filter { param($n) $n -match "win64-gpl" -and $n -notmatch "shared" -and $n.EndsWith(".zip") } `
            -DestDir $ffmpegDir -Required @("ffmpeg.exe", "ffprobe.exe")
        Install-Component -ApiUrl "https://api.github.com/repos/EasyTier/EasyTier/releases/latest" `
            -Filter { param($n) $n -match "windows-x86_64" -and $n -notmatch "gui" -and $n.EndsWith(".zip") } `
            -DestDir $easytierDir -Required @("easytier-core.exe", "easytier-cli.exe")
    }

    $hasFfmpeg = (Test-Path (Join-Path $ffmpegDir "ffmpeg.exe")) -and (Test-Path (Join-Path $ffmpegDir "ffprobe.exe"))
    $hasEasytier = (Test-Path (Join-Path $easytierDir "easytier-core.exe")) -and (Test-Path (Join-Path $easytierDir "easytier-cli.exe"))
    $fullReady = $hasFfmpeg -and $hasEasytier

    # ===== Staging and packaging =====
    $outRoot = Join-Path $root "dist-portable"
    New-Item -ItemType Directory -Force -Path $outRoot | Out-Null

    function New-Package {
        param(
            [string]$Name,
            [switch]$WithComponents
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

        $zip = Join-Path $outRoot "$Name.zip"
        # Compress the staging dir itself: portable.dat is always included and the zip
        # extracts into a named top-level folder.
        Compress-Archive -Path $stage -DestinationPath $zip -Force
        Write-Host "==> Done: $zip"
    }

    New-Package -Name "VRchat-Localcinema-base"
    if ($fullReady) {
        New-Package -Name "VRchat-Localcinema-full" -WithComponents
    } else {
        Write-Warning "Components incomplete - full package skipped. Run scripts\package-portable.ps1 -DownloadComponents to fetch them."
    }
} finally {
    Pop-Location
}
