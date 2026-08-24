@echo off
chcp 65001 >nul
echo [iLinkWM Hosted] 旧入口已迁移到包根目录的一键部署.ps1。
echo 正在启动新的交互式部署入口...
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\一键部署.ps1"
