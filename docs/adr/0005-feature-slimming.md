# ADR 0005: 功能裁剪（B 档）——删死代码 + 砍边缘功能入口

## 状态

已接受（2026-11-05，用户确认按推荐方案执行）

## 背景

Beacon 是 HopToDesk 的纯内网裁剪版。上游代码里有大量对本场景无意义的部分：

- 死代码：`mcp_server.rs`（220KB，MCP 管理 API，端口已关）、`ocr_winrt.rs`（只被 MCP 用）、Sciter 旧 UI（`ui.rs` + `src/ui/` 资源，Flutter 构建本就不编译）、`dashboard.rs`（127KB 云管理台，已被 LAN_ONLY 运行时门控）
- 边缘功能入口：文字聊天、语音通话、会话录制、端口转发、摄像头查看——IT 远程支持业务场景用不上

## 决策

采用 B 档精简：

1. 删除 mcp_server / ocr_winrt / Sciter UI 文件
2. dashboard 模块压缩为等价空实现 stub（24 个外部调用 API 保留签名，返回默认值），防止深层外科手术引入回归
3. Flutter UI 移除聊天、语音通话、录制、端口转发、摄像头的入口
4. **保留**：远程桌面、文件传输（IT 发补丁/收日志刚需）

## 理由

- 死代码删除零功能影响；dashboard 用 stub 而非逐点清除，是因为它在 flutter.rs/ipc.rs/client.rs 有 24 个调用点，全删风险大于收益
- 依赖级瘦身（如音频库 magnum-opus）属于 C 档，待版本稳定后另行评估
- 入口级裁剪让业务用户界面更干净，减少误操作

## 后果

- 编译警告（dead_code）显著减少，构建产物体积略降
- 若未来需要恢复某功能，从 `upstream-hoptodesk-baseline` 标签找回对应文件
- 聊天/语音的 Rust 端代码保留（未编译进 UI 入口即不可达），后续 C 档再清理
