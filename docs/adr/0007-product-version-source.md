# 产品版本号以安装包脚本为唯一来源

Status: accepted

Beacon 同时存在两个版本号：Rust crate 的 `VERSION`（沿用上游 hoptodesk 的 1.46.x，协议兼容门控在用）和产品对外版本（安装包、关于页）。为避免两套版本号漂移，产品版本号以 `scripts/installer/BeaconSetup.iss` 的 `MyAppVersion` 为唯一来源；`build.ps1` 构建时解析出来，通过 `BEACON_VERSION` 环境变量注入 Rust（`src/lan_only.rs` 的 `BEACON_VERSION` 常量），关于页经 `mainGetVersion()` 显示该值。crate `VERSION` 不再对外展示。

## Consequences

- 发版只需改 `BeaconSetup.iss` 一处的 `MyAppVersion`，安装包文件名、控制面板版本、关于页自动一致。
- 不用 `build.ps1` 直接 cargo 构建时，关于页显示 `0.0.0-dev`，属预期。
- crate `VERSION` 保持上游值，仅用于协议版本门控，不要再拿它做对外展示。
