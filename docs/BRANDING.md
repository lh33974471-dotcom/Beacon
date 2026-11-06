# Beacon 品牌说明

产品定名 **Beacon**（灯塔），v0.3 起生效。

## 名称触点（已改）

| 位置 | 内容 | 影响面 |
|---|---|---|
| `hoptodesk-main/libs/hbb_common/src/config.rs:63` | `APP_NAME = "Beacon"` | 服务名、配置目录（`%APPDATA%\Beacon`）、托盘提示、UI 全文替换 |
| `hoptodesk-main/flutter/windows/CMakeLists.txt` | `BINARY_NAME "Beacon"` | exe 文件名 `Beacon.exe` |
| `hoptodesk-main/flutter/windows/runner/main.cpp` | 窗口标题、单实例 FindWindow 标题、`beacon://` | 标题栏、单实例唤起 |
| `hoptodesk-main/flutter/windows/runner/Runner.rc` | 版本信息 Company/Product/FileDescription | exe 属性页（版权行保留 HopToDesk 归属） |
| `hoptodesk-main/flutter/lib/consts.dart` | `kUniLinksPrefix = "beacon://"` | 深链接 scheme |
| `hoptodesk-main/src/ui.rs`、`src/tray.rs`、`src/auth_2fa.rs`、`src/client.rs`、`src/common.rs` | `beacon://` 解析、通知 id、2FA 签发名、提示文案、注册表策略路径 | 随 APP_NAME 之外的硬编码 |

## 有意保留的内部标识

- `libhoptodesk.dll` / `hoptodesk_core_main_args`：FFI ABI，改名爆炸半径大，不动。
- `Cargo.toml` 的 crate 名 `hoptodesk`：同上。
- `kEnvPortableExecutable = "HOPTODESK_APPNAME"`：便携启动器 ABI。
- `flutter_ffi.rs` 的 `is_custom_client` 判断（`!= "HopToDesk"`）：逻辑上就是"非上游即定制端"，改名后自动为 true。
- Runner.rc 版权行、上游 URL（api/dl.hoptodesk.com，均已短路为不可达死代码）。

## 图片触点（已换，灯塔主题）

- `hoptodesk-main/res/`：`icon.ico`、`tray-icon.ico`、`icon.png`、`128x128.png`、`128x128@2x.png`、`32x32.png`、`logo.svg`（mac-* 未动，不发 mac）。
- `hoptodesk-main/flutter/windows/runner/resources/app_icon.ico`：exe 图标，与 `res/icon.ico` 同图。
- `hoptodesk-main/flutter/assets/`：新增 `icon.png`、`logo.png`（左侧栏灯塔图标+Beacon 文字标）、`logo.svg`。
- 重新生成：`python scripts/make_beacon_icons.py`（主图标）与 `scripts/make_beacon_brand_extras.py`（文字标/SVG），PIL 程序化绘制，可改配色。

## 改名影响

- 配置目录从 `%APPDATA%\HopToDesk` 变为 `%APPDATA%\Beacon`，对端公钥缓存与本机设置会重置（首次需重新确认对端）。
- 服务名变为 `Beacon`，旧 `HopToDesk` 服务需先卸载。
- UI 文案里的 HopToDesk 字样由 `common.dart:1529` 自动替换为 Beacon（isBrandedBuild）。
