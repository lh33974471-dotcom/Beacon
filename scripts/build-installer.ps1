# Beacon 一键安装包构建：完整构建 Release 后调 Inno Setup 打包到 dist/
# 用法: scripts\build-installer.ps1
$ErrorActionPreference = "Stop"
$Root = Split-Path $PSScriptRoot -Parent

& (Join-Path $PSScriptRoot "build.ps1") -Release

$Iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
    "C:\Program Files\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not (Test-Path $Iscc)) {
    throw "未找到 Inno Setup，请先安装: winget install --id JRSoftware.InnoSetup -e"
}

Write-Host "==> 打包安装程序 (Inno Setup, lzma2/ultra64)" -ForegroundColor Cyan
& $Iscc (Join-Path $PSScriptRoot "installer\BeaconSetup.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC 编译失败" }
Write-Host "==> 安装包输出: $Root\dist" -ForegroundColor Green
