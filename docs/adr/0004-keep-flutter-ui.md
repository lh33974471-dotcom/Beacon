# 保留 Flutter UI 做精简，不使用 wireui

Status: accepted

不重写界面。在现有 Flutter UI 基础上精简：移除 ID 服务器设置、中继状态、邀请链接等页面，首页改为在线列表 + IP 输入框；品牌图片与名称做成可配置资源，便于后续换肤与定名。仓库中的 wireui（纯 Rust wgpu 引擎，锁定 nightly-2023-12-21 工具链）是实验性支线，踩坑面大，不采用。

## Consequences

- 构建链必须包含 Flutter 3.13+ stable，不能只用纯 Rust 构建。
- 产品命名（Q13）未定，品牌资源需保持集中、可替换。
