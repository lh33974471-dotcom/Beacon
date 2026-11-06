# Beacon 一键构建脚本（Windows x64）
# 用法: scripts\build.ps1 [-Release]
param([switch]$Release = $true)

$ErrorActionPreference = "Stop"
$Root = Split-Path $PSScriptRoot -Parent
$Src = Join-Path $Root "hoptodesk-main"
$Toolchain = "D:\3.Cache"

# 环境（如已全局设置可省略；这里显式保证正确）
$env:CARGO_HOME       = "$Toolchain\cargo"
$env:RUSTUP_HOME      = "$Toolchain\rustup"
$env:VCPKG_ROOT       = "$Toolchain\vcpkg"
$env:LIBCLANG_PATH    = "$Toolchain\LLVM17\bin"   # 必须是 LLVM 17，新版会导致 bindgen 生成空结构体
$env:CARGO_TARGET_DIR = "$Src\target"             # Flutter CMakeLists 依赖该相对位置
Remove-Item Env:RUST_LOG -ErrorAction SilentlyContinue  # 全局 RUST_LOG=warn 会让 codegen 崩溃
$env:Path = "$Toolchain\cargo\bin;$Toolchain\flutter\bin;$Toolchain\CMake\bin;$env:Path"

# 内存保护：本机常驻应用多、提交内存紧张，fat LTO + 并发曾多次把 rustc 编译到 OOM。
# 单并发 + thin LTO + 多 codegen units 实测可稳定通过，产物体积差异可忽略。
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = "1" }
if (-not $env:CARGO_PROFILE_RELEASE_LTO) { $env:CARGO_PROFILE_RELEASE_LTO = "thin" }
if (-not $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS) { $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16" }

foreach ($cmd in "cargo", "flutter", "cmake") {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        throw "缺少命令: $cmd，请先按 docs/BUILDING.md 完成环境搭建"
    }
}

$profile = if ($Release) { "release" } else { "debug" }

# 舰队密码注入：优先环境变量，其次未跟踪的 .secrets\fleet-password.txt。
# 两者都没有则编出占位口令（仅限本地调试，不可分发）。
if (-not $env:BEACON_FLEET_PASSWORD) {
    $secretFile = Join-Path $Root ".secrets\fleet-password.txt"
    if (Test-Path $secretFile) {
        $env:BEACON_FLEET_PASSWORD = (Get-Content $secretFile -Raw).Trim()
    } else {
        Write-Warning "未提供 BEACON_FLEET_PASSWORD（.secrets\fleet-password.txt），将编译占位口令，请勿分发该包"
    }
}

# 桥接代码（src/bridge_generated.rs 等）缺失时重新生成
if (-not (Test-Path (Join-Path $Src "src\bridge_generated.rs"))) {
    Write-Host "==> 生成 flutter_rust_bridge 桥接代码" -ForegroundColor Cyan
    Push-Location $Src
    try {
        flutter_rust_bridge_codegen --rust-input src/flutter_ffi.rs --dart-output flutter/lib/bridge_generated.dart --llvm-path $Toolchain\LLVM17
    } finally { Pop-Location }
}

Write-Host "==> 构建 Rust core ($profile)" -ForegroundColor Cyan
Push-Location $Src
try {
    if ($Release) { cargo build --release --features flutter }
    else { cargo build --features flutter }
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失败 (exit $LASTEXITCODE)" }
} finally { Pop-Location }

Write-Host "==> 构建 Flutter UI ($profile)" -ForegroundColor Cyan
Push-Location (Join-Path $Src "flutter")
try {
    flutter pub get
    if ($LASTEXITCODE -ne 0) { throw "flutter pub get 失败 (exit $LASTEXITCODE)" }
    if ($Release) { flutter build windows --release }
    else { flutter build windows --debug }
    if ($LASTEXITCODE -ne 0) { throw "flutter build 失败 (exit $LASTEXITCODE)" }
} finally { Pop-Location }

$out = Join-Path $Src "flutter\build\windows\x64\runner\$(if ($Release) {'Release'} else {'Debug'})"
Copy-Item (Join-Path $PSScriptRoot "install-beacon.bat") $out -Force
Write-Host "==> 完成，产物目录: $out" -ForegroundColor Green
