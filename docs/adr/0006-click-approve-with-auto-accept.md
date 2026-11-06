# 被控端点击确认 + 10 秒倒计时自动同意

Status: accepted

IT 发起连接时，被控端右下角弹出确认窗口，提供"同意 / 拒绝"两个按钮；窗口内带 10 秒倒计时，倒计时结束无人操作时自动同意本次连接。业务无需值守即可被远程，同时保留拒绝的窗口期。

## Considered Options

- 沿用纯密码直连（ADR 0002）：连接无任何提示，业务侧无感知，IT 支持场景下合规性差。
- 点击确认但不自动同意：业务不在工位时 IT 无法接入，违背远程协助的初衷。
- 点击确认 + 超时自动同意：兼顾知情与可达性，被采用。

## Implementation

- `approve_mode()` 默认值改为 `ApproveMode::Click`（`libs/hbb_common/src/password_security.rs`），登录流程走原生点击审批分支，内置密码不再参与桌面会话放行。
- CM 确认窗口停靠屏幕右下角（`flutter/lib/main.dart` 的 `showCmWindow()` 两处对齐改为 `Alignment.bottomRight`）。
- 10 秒倒计时在被控端 Flutter 侧实现（`flutter/lib/models/server_model.dart` 的 `startApproveCountdown`/`cancelApproveCountdown`），归零自动 `sendLoginResponse(client, true)`；手动同意/拒绝、对端取消、连接关闭均会取消倒计时。
- 接受按钮实时显示剩余秒数（`flutter/lib/desktop/pages/server_page.dart` 的 `buildUnAuthorized`）。
- 审批模式为 password 时不启动倒计时，避免绕过密码。

## Consequences

- 控制端点击连接后需等待对方同意或最多 10 秒，体验从"秒连"变为"弹窗确认"。
- 安全边界变为：IP 白名单（仅内网）+ 10 秒知情窗口；自动同意意味着无人值守机器默认可接入，这是有意取舍。
- ADR 0002 的内置密码保留在二进制中，但在 Click 审批分支前被短路，实际不再校验。
