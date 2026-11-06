# Beacon 环境一键安装（工具链统一装 D:\3.Cache）
# 需要管理员 PowerShell；VS Build Tools 与 Flutter SDK 按提示手动完成
$ErrorActionPreference = "Stop"
$Base = "D:\3.Cache"
New-Item -ItemType Directory -Force -Path $Base | Out-Null

# 1. 环境变量（用户级，安装 rustup 前必须设好）
[Environment]::SetEnvironmentVariable("CARGO_HOME",       "$Base\cargo",        "User")
[Environment]::SetEnvironmentVariable("RUSTUP_HOME",      "$Base\rustup",       "User")
[Environment]::SetEnvironmentVariable("VCPKG_ROOT",       "$Base\vcpkg",        "User")
[Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", "$Base\cargo-target", "User")
[Environment]::SetEnvironmentVariable("FLUTTER_ROOT",     "$Base\flutter",      "User")
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$add = @("$Base\flutter\bin", "$Base\cargo\bin", "$Base\vcpkg", "$Base\LLVM\bin")
foreach ($p in $add) { if ($userPath -notlike "*$p*") { $userPath = "$userPath;$p" } }
[Environment]::SetEnvironmentVariable("Path", $userPath, "User")
Write-Host "环境变量已写入，当前会话也临时生效" -ForegroundColor Green
$env:CARGO_HOME = "$Base\cargo"; $env:RUSTUP_HOME = "$Base\rustup"
$env:VCPKG_ROOT = "$Base\vcpkg"; $env:CARGO_TARGET_DIR = "$Base\cargo-target"
$env:Path = "$env:Path;" + ($add -join ";")

# 2. winget 安装到 D 盘（--location 仅对支持的包生效）
winget install --id LLVM.LLVM -e --location "$Base\LLVM" --accept-package-agreements --accept-source-agreements
winget install --id Kitware.CMake -e --location "$Base\CMake" --accept-package-agreements --accept-source-agreements
winget install --id StrawberryPerl.StrawberryPerl -e --location "$Base\StrawberryPerl" --accept-package-agreements --accept-source-agreements

# 3. Rust（rustup-init 读取 CARGO_HOME / RUSTUP_HOME，自动装到 D 盘）
$rustup = "$Base\rustup-init.exe"
Invoke-WebRequest "https://win.rustup.rs/x86_64" -OutFile $rustup
& $rustup -y --default-toolchain stable

# 4. vcpkg
if (-not (Test-Path "$Base\vcpkg")) { git clone https://github.com/microsoft/vcpkg "$Base\vcpkg" }
& "$Base\vcpkg\bootstrap-vcpkg.bat"

Write-Host ""
Write-Host "===== 还需手动完成两项 =====" -ForegroundColor Yellow
Write-Host "1) VS Build Tools 2022：安装时勾选 C++ 桌面开发负载，安装位置可选 D 盘（SDK 会落 C 盘，正常）" -ForegroundColor Yellow
Write-Host "2) Flutter SDK：下载 zip 解压到 $Base\flutter，然后新开窗口跑 flutter doctor" -ForegroundColor Yellow
