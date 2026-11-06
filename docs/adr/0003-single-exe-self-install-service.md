# 单 exe 自安装服务模式

Status: accepted

沿用 RustDesk 的"绿色 exe 内嵌安装"模式：不维护独立 MSI/NSIS 安装包，单个 exe 首次运行时引导安装为 Windows 服务（SYSTEM 权限、开机自启、托盘入口），并自动写入防火墙放行规则。选择服务模式而非便携版的决定性原因是 UAC Secure Desktop：普通进程截屏在 UAC 提示界面会黑屏失控，SYSTEM 服务才能正常处理。该结论与此前 MeshCentral 调研一致。

## Consequences

- 安装动作需要一次管理员授权，之后完全无人值守。
- exe 内嵌安装逻辑来自上游，裁剪时需保留完整。
