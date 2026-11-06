# Beacon 部署手册

## 产物

- `dist\BeaconSetup-<版本>.exe`：单文件安装包（Inno Setup，lzma2/ultra64 深度压缩，约 16 MB）
- 构建：`scripts\build-installer.ps1`（先 `build.ps1 -Release` 再调 ISCC）

## 业务机（被控端）安装

安装全程无需用户操作，固定安装到 `C:\Program Files\Beacon`（不让选路径）：

- 手工：管理员双击 `BeaconSetup-x.x.x.exe`，下一步到底即可
- 自动化/批量（MDT、SCCM、域 GPO、封装镜像）：

```cmd
BeaconSetup-1.0.0.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
```

安装自动完成：

- 复制完整运行文件到 `C:\Program Files\Beacon`
- 注册并启动 Windows 服务 `Beacon`（SYSTEM，免 UAC 远程控制）
- 添加防火墙规则 `Beacon Service`（入站+出站，不再弹防火墙询问窗）
- 公共桌面 `C:\Users\Public\Desktop\Beacon.lnk`（任何用户登录都能看到）
- 公共开始菜单 + 公共启动项托盘（任何用户登录自动驻留托盘）
- 控制面板单一卸载条目

## IT 机（控制端）

无需安装，直接双击 Release 目录里的 `Beacon.exe` 即可（只发起到业务机的连接，不需要入站规则）。

## 卸载

控制面板卸载，或管理员运行：

```cmd
"C:\Program Files\Beacon\Beacon.exe" --uninstall
```

清理范围：服务、防火墙规则、公共桌面/开始菜单/启动项快捷方式、安装目录、ProgramData 配置。

## 注意

- **Win11 智能应用控制（SAC）拦截**：未签名 exe 在新版 Win11 上可能被 SAC 直接拦死（无"仍要运行"按钮）。内网受管设备直接关掉即可（该功能面向个人用户，微软也建议企业设备关闭）：
  - 单台：设置 → 隐私和安全性 → Windows 安全中心 → 应用和浏览器控制 → 智能应用控制设置 → 关闭
  - 批量（管理员 CMD，重启生效）：`reg add "HKLM\SYSTEM\CurrentControlSet\Control\CI\Policy" /v VerifiedAndReputablePolicyState /t REG_DWORD /d 0 /f`
  - 关闭后不可再开（除非重装系统）；Win10 无此机制，SmartScreen 点"详细信息→仍要运行"即可
  - 要彻底免拦截需购买 EV 代码签名证书（约 ¥2000-4000/年），纯内网部署不推荐
- 升级：直接跑新版安装包覆盖安装即可
- 安装包内嵌的舰队密码在构建时由环境变量 `BEACON_FLEET_PASSWORD`（或本地未跟踪的 `.secrets\fleet-password.txt`）注入；源码仅含占位符 `CHANGE-ME-BEFORE-DEPLOY`，发正式包前必须注入真实值，且该文件/变量不得随仓库公开
- 若某台机器曾手动点过防火墙弹窗的"取消"，先管理员 CMD 执行
  `netsh advfirewall firewall delete rule name=all program="C:\Program Files\Beacon\Beacon.exe"`
  再安装
