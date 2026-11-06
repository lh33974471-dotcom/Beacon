@echo off
rem Beacon 一键安装（被控端用）：自提权后静默安装服务 + 防火墙规则 + 开机自启。
rem 把本脚本放到 Beacon.exe 同目录，双击运行，UAC 点"是"一次即可。
net session >nul 2>&1
if errorlevel 1 (
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
"%~dp0Beacon.exe" --silent-install
echo.
echo Beacon installed. You can close this window.
timeout /t 5 >nul
