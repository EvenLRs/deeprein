# 构建 deeprein
#   -Mode Release    仅编译 release exe（最快，产物在 src-tauri\target\release\deeprein.exe）
#   -Mode Installer  编译并打包 NSIS 安装程序（需要 Node/npm，产物在 src-tauri\target\release\bundle\nsis\）
#   -SkipNpmInstall  打包时跳过 npm install（已安装过 @tauri-apps/cli 时用）
param(
    [ValidateSet('Release', 'Installer')]
    [string]$Mode = 'Release',
    [switch]$SkipNpmInstall
)
$ErrorActionPreference = 'Stop'
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))

function Test-Command([string]$name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

# ---- 前置检查 ----
if (-not (Test-Command cargo)) {
    throw "未找到 cargo。请先安装 Rust：https://rustup.rs （需 MSVC 工具链，见 README）"
}
if (-not (Test-Command link.exe)) {
    Write-Warning "未找到 link.exe（MSVC 链接器）。Tauri 在 Windows 上需要 Visual Studio Build Tools 的 C++ 工作负载。"
}
if ($Mode -eq 'Installer' -and -not (Test-Command node)) {
    throw "打包安装程序需要 Node.js（用于 @tauri-apps/cli）。请先安装 Node.js。"
}

# ---- 编译 ----
Push-Location (Join-Path $root 'src-tauri')
try {
    if ($Mode -eq 'Release') {
        Write-Host "==> cargo build --release"
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo 编译失败（exit $LASTEXITCODE）" }
        $exe = Join-Path (Get-Location) 'target\release\deeprein.exe'
        Write-Host "`n构建成功：$exe" -ForegroundColor Green
    }
    else {
        if (-not $SkipNpmInstall) {
            Push-Location $root
            try { npm install } finally { Pop-Location }
        }
        Write-Host "==> npm run tauri build"
        Push-Location $root
        try { npm run build } finally { Pop-Location }
        $nsis = Join-Path (Get-Location) 'target\release\bundle\nsis'
        if (Test-Path $nsis) {
            Write-Host "`n安装包目录：$nsis" -ForegroundColor Green
            Get-ChildItem $nsis -Filter '*.exe' | Select-Object Name, Length | Format-Table -AutoSize
        }
    }
}
finally {
    Pop-Location
}
