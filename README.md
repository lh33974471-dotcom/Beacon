# Beacon（灯塔）

纯局域网 IP 直连的 Windows 远程协助工具：控制端（IT）输入被控端 IP 即可连接，无账号、无中继、零外网依赖。
感谢KIMI K3的大力支持

基于 [HopToDesk](https://github.com/hoptodesk/hoptodesk)（其本身基于 [RustDesk](https://github.com/rustdesk/rustdesk)）二次开发，许可证沿用 **AGPL-3.0**（见 `hoptodesk-main/LICENSE`）。

## 与上游的区别

- 彻底剥离公网：无 ID/中继服务器、无云端 API、无账号体系，仅 LAN IP 直连
- 编译期内置舰队密码，连接免确认（IT → 业务机的支持场景）
- Windows 服务模式运行，免 UAC 黑屏；托盘常驻
- 单文件安装包（Inno Setup）：自动装服务、加防火墙规则、公共桌面快捷方式
- 界面精简：移除聊天/语音/录制/端口转发/摄像头等入口

## 构建

见 `docs/BUILDING.md`；一键：`scripts\build.ps1 -Release`。

安装包：`scripts\build-installer.ps1`（需 Inno Setup 6，产物在 `dist\`）。

## 部署

见 `docs/DEPLOYMENT.md`。业务机静默安装：

```cmd
BeaconSetup-x.x.x.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
```

## 安全说明

- 舰队密码在**构建时**通过环境变量 `BEACON_FLEET_PASSWORD` 或未跟踪文件 `.secrets\fleet-password.txt` 注入，源码中只有占位符 `CHANGE-ME-BEFORE-DEPLOY`。自行编译分发前必须设置，否则任何同源码构建的包都可互连。
- 该密码是"门槛"而非保险柜（可从二进制提取），请配合内网边界与 IP 白名单使用。
- 仅接受 RFC1918 私网/链路本地地址的入站连接。

## 文档

- `docs/adr/`：架构决策记录
- `docs/BRANDING.md`：品牌资源触点
- `docs/USAGE.md`：使用说明（含中文输入注意事项）
