; Beacon 一键安装包（Inno Setup 6）
; 构建: & "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" scripts\installer\BeaconSetup.iss
#define MyAppVersion "1.3.0"
#define PayloadDir "..\..\hoptodesk-main\flutter\build\windows\x64\runner\Release"

[Setup]
AppId={{8F2C6D4E-3B5A-4E1F-9C7D-2A6B8E0F1D3C}
AppName=Beacon
AppVersion={#MyAppVersion}
AppPublisher=Beacon
DefaultDirName={autopf}\Beacon
DisableDirPage=yes
DisableProgramGroupPage=yes
OutputDir=..\..\dist
OutputBaseFilename=BeaconSetup-{#MyAppVersion}
Compression=lzma2/ultra64
SolidCompression=yes
LZMAUseSeparateProcess=yes
PrivilegesRequired=admin
WizardStyle=modern
SetupIconFile=..\..\hoptodesk-main\res\icon.ico
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force
RestartApplications=no
; 卸载由 Beacon.exe --uninstall 全权负责（服务/防火墙/快捷方式/文件），
; 控制面板条目由 install_me 写入，这里不再生成 Inno 自己的卸载器，避免双条目。
Uninstallable=no
CreateUninstallRegKey=no

[Files]
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "install-beacon.bat"

[Run]
; 原地注册：Windows 服务 + 防火墙规则 + 公共桌面/开始菜单/公共启动项快捷方式
Filename: "{app}\Beacon.exe"; Parameters: "--silent-install --launch-after"; Flags: runhidden waituntilterminated; StatusMsg: "正在配置 Beacon 服务..."
