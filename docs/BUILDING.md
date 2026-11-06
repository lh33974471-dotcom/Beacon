# Beacon 构建环境搭建清单（Windows x64，工具链装 D:\3.Cache）

目标：在 `D:\3.Cache` 下从零搭建可编译 hoptodesk fork（Rust core + Flutter UI）的环境。预计磁盘占用 15GB+，全程需要外网下载（仅搭建阶段；成品运行时零外联）。

## 0. 先设环境变量（安装前，用户级）

```powershell
# D:\3.Cache 下统一安放，脚本见 scripts/setup-env.ps1 自动执行
setx CARGO_HOME        D:\3.Cache\cargo
setx RUSTUP_HOME       D:\3.Cache\rustup
setx VCPKG_ROOT        D:\3.Cache\vcpkg
setx CARGO_TARGET_DIR  D:\3.Cache\cargo-target
setx FLUTTER_ROOT      D:\3.Cache\flutter
# Path 追加: D:\3.Cache\flutter\bin;D:\3.Cache\cargo\bin;D:\3.Cache\vcpkg;D:\3.Cache\LLVM\bin
```

注意：rustup 安装器启动前 `CARGO_HOME`/`RUSTUP_HOME` 必须已生效（新开窗口确认 `echo $env:CARGO_HOME`）。

## 1. 依赖清单与安装位置

| 组件 | 版本要求 | 安装位置 | 安装方式 |
|---|---|---|---|
| Visual Studio Build Tools | 2022（C++ 桌面开发负载、Win10/11 SDK） | 编译器可选 D 盘；**SDK 与共享组件强制 C 盘**（约 3-5GB） | 官网安装器 |
| Rust | stable（rustup） | `D:\3.Cache\rustup` + `D:\3.Cache\cargo` | 官网 rustup-init.exe（先设环境变量再运行） |
| Flutter SDK | **3.32.8**（Dart 3.8；3.35 会破坏 DialogTheme/extended_text，勿用） | `D:\3.Cache\flutter` | 官网 zip 直接解压 |
| vcpkg | latest | `D:\3.Cache\vcpkg` | `git clone` + `bootstrap-vcpkg.bat` |
| LLVM/Clang | latest | `D:\3.Cache\LLVM` | winget `--location` 或手动安装选路径 |
| CMake | latest | `D:\3.Cache\CMake` | winget `--location` |
| Python 3 | 3.10+ | `D:\3.Cache\Python312` | winget `--location`（可选） |
| Strawberry Perl | latest | `D:\3.Cache\StrawberryPerl` | 可选，部分依赖需要 |
| WebView2 Runtime | latest | 系统组件 | 一般系统已带 |

## 2. vcpkg 依赖安装

```powershell
cd D:\3.Cache\vcpkg
.\vcpkg install libvpx:x64-windows-static libyuv:x64-windows-static opus:x64-windows-static aom:x64-windows-static
```

## 3. 验证清单

```powershell
rustc --version        # stable
flutter doctor         # Windows desktop 全绿
vcpkg list             # 四大件就位
scripts\build.ps1      # 一键构建
```

## 4. LLVM 版本：必须 LLVM 17（bindgen 兼容性）

仓库使用 bindgen 0.59（2022 年），与 LLVM 18+ 的 libclang 不兼容，症状是生成的 vpx/opus 结构体只剩 `_address` 字段。必须：

- 安装 LLVM **17.0.6** 到 `D:\3.Cache\LLVM17`
- 设 `LIBCLANG_PATH = D:\3.Cache\LLVM17\bin`

winget 只能装最新版 LLVM，17.0.6 需手动下载安装包：`https://github.com/llvm/llvm-project/releases/download/llvmorg-17.0.6/LLVM-17.0.6-win64.exe`，静默安装 `LLVM-17.0.6-win64.exe /S /D=D:\3.Cache\LLVM17`。

## 5. 桥接代码生成（首次构建前必须执行）

`src/bridge_generated.rs` 等文件不在仓库里，由 flutter_rust_bridge 生成：

```powershell
cargo install flutter_rust_bridge_codegen --version 1.80.1 --locked
cd hoptodesk-main   # 必须在仓库根目录执行（cwd 决定 ffigen 的工作目录）
flutter_rust_bridge_codegen --rust-input src/flutter_ffi.rs --dart-output flutter/lib/bridge_generated.dart --llvm-path D:\3.Cache\LLVM17
```

注意：机器全局若设了 `RUST_LOG=warn`，codegen 会崩溃（它只接受 debug/info），执行前先 `Remove-Item Env:RUST_LOG`。

## 6. 已知坑

- `wireui/Cargo.toml` 锁了 nightly-2023-12-21 工具链，主构建路径走 Flutter，不要碰 wireui。
- `Cargo.lock` 已固定版本，首次构建 cargo 下载数 GB（`CARGO_HOME` 与 `CARGO_TARGET_DIR` 已在 D 盘）。
- Flutter Windows 构建需要 VS 的 "Desktop development with C++" 负载完整，缺了会报 CMake 错误。
- VS Build Tools 安装器里把 "安装位置" 选到 D 盘只能挪编译器本体；Windows SDK 必定落 C 盘，属正常现象，不影响使用。
