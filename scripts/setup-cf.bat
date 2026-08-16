@echo off
rem ============================================================
rem  setup-cf.bat — ilink-wm Cloudflare IPv6 直连配置（双击运行）
rem  首次/交互配置：直接双击本文件
rem  DDNS 刷新：     setup-cf.bat --ddns
rem ============================================================
setlocal
rem 管理代码页为 UTF-8，避免中文输出乱码
chcp 65001 >nul
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0cf-setup.ps1" %*
set EC=%ERRORLEVEL%
if "%~1"=="" pause
exit /b %EC%
