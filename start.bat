@echo off
chcp 65001 >nul
setlocal enabledelayedexpansion

REM ============================================================================
REM  iLink-WM1 Windows 启动脚本（双击即用版）
REM
REM  用途：
REM    1. 启动 ilink-wm1.exe
REM    2. 等待端口 8888 就绪
REM    3. 自动打开浏览器访问 http://localhost:8888
REM    4. 控制台显示实时日志，关闭窗口即停止服务
REM
REM  数据位置：默认在脚本所在目录的 data\ 子目录
REM            可通过环境变量 ILINK_DATA_DIR 修改
REM ============================================================================

cd /d "%~dp0"

set "BIN_PATH=%~dp0ilink-wm1.exe"
set "WEB_DIR=%~dp0web"
set "DEFAULT_PORT=8888"
set "ILINK_HOST=%ILINK_HOST%"
if "%ILINK_HOST%"=="" set "ILINK_HOST=0.0.0.0"
set "ILINK_PORT=%ILINK_PORT%"
if "%ILINK_PORT%"=="" set "ILINK_PORT=%DEFAULT_PORT%"
set "ILINK_DATA_DIR=%ILINK_DATA_DIR%"
if "%ILINK_DATA_DIR%"=="" set "ILINK_DATA_DIR=%~dp0data"
set "RUST_LOG=%RUST_LOG%"
if "%RUST_LOG%"=="" set "RUST_LOG=ilink_wm1=info"
set "RUST_BACKTRACE=full"

echo ========================================
echo   iLink-WM1 启动中...
echo ========================================
echo.
echo   二进制:    %BIN_PATH%
echo   前端目录:  %WEB_DIR%
echo   监听地址:  %ILINK_HOST%:%ILINK_PORT%
echo   数据目录:  %ILINK_DATA_DIR%
echo   访问地址:  http://localhost:%ILINK_PORT%
echo.

REM ── 检查二进制 ──────────────────────────────
if not exist "%BIN_PATH%" (
    echo [错误] 找不到 ilink-wm1.exe
    echo   期望路径: %BIN_PATH%
    echo   请确认 ZIP 包完整解压。
    pause
    exit /b 1
)

REM ── 检查前端目录 ────────────────────────────
if not exist "%WEB_DIR%" (
    echo [错误] 找不到前端目录 web\
    echo   期望路径: %WEB_DIR%
    echo   请确认 ZIP 包完整解压。
    pause
    exit /b 1
)

REM ── 创建数据目录 ────────────────────────────
if not exist "%ILINK_DATA_DIR%" (
    mkdir "%ILINK_DATA_DIR%"
    echo [信息] 已创建数据目录: %ILINK_DATA_DIR%
)

REM ── 公网监听安全确认 ─────────────────────────
if /i "%ILINK_HOST%"=="0.0.0.0" (
    if not "%ILINK_ALLOW_INSECURE_PUBLIC%"=="1" (
        if "%ILINK_TRUSTED_PROXIES%"=="" (
            echo [安全确认] 当前将监听全部 IPv4 网卡，但尚未配置 HTTPS 反向代理。
            echo   只有在受信任内网中使用时，才可继续明文 HTTP。
            set /p "LAN_CONFIRM=确认这是受信任内网并继续？请输入 YES: "
            if /i not "!LAN_CONFIRM!"=="YES" (
                echo [已取消] 请先配置 HTTPS 反向代理，并设置 ILINK_TRUSTED_PROXIES 与 ILINK_FORCE_HTTPS=1。
                pause
                exit /b 1
            )
            set "ILINK_ALLOW_INSECURE_PUBLIC=1"
        ) else if not "%ILINK_FORCE_HTTPS%"=="1" (
            echo [错误] 已配置可信代理，但 ILINK_FORCE_HTTPS 不是 1。
            echo   仅当上游已经提供真实 HTTPS 时，才可设置 ILINK_FORCE_HTTPS=1。
            pause
            exit /b 1
        )
    )
)

echo [信息] 正在启动服务...
echo [信息] 关闭此窗口或按 Ctrl+C 可停止服务。
echo.

REM ── 启动后台浏览器延迟打开任务 ──────────────
REM 用一个子进程在 3 秒后打开浏览器
start /b cmd /c "timeout /t 3 /nobreak >nul && start """" http://localhost:%ILINK_PORT%"

REM ── 前台运行服务（控制台可见日志）─────────────
"%BIN_PATH%"

REM 如果服务退出（Ctrl+C 或关闭），停在这里
echo.
echo [信息] 服务已停止。
pause
