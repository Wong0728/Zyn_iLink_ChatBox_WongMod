@echo off
chcp 65001 >nul
setlocal enabledelayedexpansion

REM ============================================================================
REM  iLink-WM1 Windows 服务安装脚本（NSSM 版）
REM
REM  用途：
REM    1. 检查 NSSM，若未安装则自动下载到 bin\nssm.exe
REM    2. 注册 Windows 服务 ilink-wm1（开机自启 + 崩溃重启）
REM    3. 启动服务
REM    4. 打开浏览器访问 http://localhost:8888
REM
REM  必须以管理员身份运行！
REM    右键 → 以管理员身份运行
REM
REM  卸载：
REM    bin\nssm.exe stop ilink-wm1
REM    bin\nssm.exe remove ilink-wm1 confirm
REM ============================================================================

REM ── 检查管理员权限 ──────────────────────────
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [错误] 此脚本必须以管理员身份运行！
    echo   请右键此 .bat 文件 → 以管理员身份运行
    pause
    exit /b 1
)

cd /d "%~dp0"

set "ROOT_DIR=%~dp0"
set "BIN_DIR=%~dp0bin"
set "NSSM_EXE=%BIN_DIR%\nssm.exe"
set "SERVICE_NAME=ilink-wm1"
set "APP_EXE=%~dp0ilink-wm1.exe"
set "LOG_DIR=%~dp0logs"
set "DEFAULT_PORT=8888"
set "NSSM_URL=https://nssm.cc/release/nssm-2.24.zip"
set "NSSM_ZIP_SHA256=727D1E42275C605E0F04ABA98095C38A8E1E46DEF453CDFFCE42869428AA6743"
set "NSSM_EXE_SHA256=F689EE9AF94B00E9E3F0BB072B34CAAF207F32DCB4F5782FC9CA351DF9A06C97"
set "NSSM_ZIP=%TEMP%\nssm-2.24.zip"
set "NSSM_EXTRACT=%TEMP%\nssm-extract"

echo ========================================
echo   iLink-WM1 Windows 服务安装
echo ========================================
echo.

REM ── 检查二进制 ──────────────────────────────
if not exist "%APP_EXE%" (
    echo [错误] 找不到 ilink-wm1.exe
    echo   期望路径: %APP_EXE%
    pause
    exit /b 1
)

REM ── 检查/下载 NSSM ─────────────────────────
if exist "%NSSM_EXE%" (
    powershell -NoProfile -Command "$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath '%NSSM_EXE%').Hash; if ($actual -ne '%NSSM_EXE_SHA256%') { exit 1 }"
    if !errorlevel! neq 0 (
        echo [警告] 现有 NSSM 未通过固定版本校验，将重新下载可信副本。
        del /q "%NSSM_EXE%" >nul 2>&1
    )
)
if not exist "%NSSM_EXE%" (
    echo [信息] NSSM 未安装，开始下载...
    if not exist "%BIN_DIR%" mkdir "%BIN_DIR%"

    REM 尝试从 nssm.cc 下载（备用：从 github 镜像）
    echo [信息] 下载: %NSSM_URL%
    powershell -Command "try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri '%NSSM_URL%' -OutFile '%NSSM_ZIP%' -UseBasicParsing } catch { exit 1 }"

    if not exist "%NSSM_ZIP%" (
        echo [错误] NSSM 下载失败
        echo   请手动下载 https://nssm.cc/release/nssm-2.24.zip
        echo   解压其中的 win64\nssm.exe 到 %BIN_DIR%\nssm.exe
        echo   然后重新运行此脚本。
        pause
        exit /b 1
    )

    echo [信息] 校验 NSSM SHA-256...
    powershell -NoProfile -Command "$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath '%NSSM_ZIP%').Hash; if ($actual -ne '%NSSM_ZIP_SHA256%') { Write-Error ('SHA-256 不匹配: ' + $actual); exit 1 }"
    if !errorlevel! neq 0 (
        echo [错误] NSSM 完整性校验失败，已停止安装。
        del /q "%NSSM_ZIP%" >nul 2>&1
        pause
        exit /b 1
    )

    echo [信息] 解压 NSSM...
    if exist "%NSSM_EXTRACT%" rmdir /s /q "%NSSM_EXTRACT%"
    powershell -Command "Expand-Archive -Path '%NSSM_ZIP%' -DestinationPath '%NSSM_EXTRACT%' -Force"

    REM 固定复制已校验 ZIP 中的 64 位版本，不接受结构不明的兜底文件。
    if exist "%NSSM_EXTRACT%\nssm-2.24\win64\nssm.exe" (
        copy /y "%NSSM_EXTRACT%\nssm-2.24\win64\nssm.exe" "%NSSM_EXE%" >nul
    ) else (
        echo [错误] NSSM ZIP 结构不符合固定版本预期。
        pause
        exit /b 1
    )

    if not exist "%NSSM_EXE%" (
        echo [错误] NSSM 解压失败，未找到 nssm.exe
        pause
        exit /b 1
    )

    powershell -NoProfile -Command "$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath '%NSSM_EXE%').Hash; if ($actual -ne '%NSSM_EXE_SHA256%') { Write-Error ('nssm.exe SHA-256 不匹配: ' + $actual); exit 1 }"
    if !errorlevel! neq 0 (
        echo [错误] 解压后的 nssm.exe 完整性校验失败。
        del /q "%NSSM_EXE%" >nul 2>&1
        pause
        exit /b 1
    )

    echo [OK] NSSM 已就绪: %NSSM_EXE%
) else (
    echo [OK] NSSM 已存在: %NSSM_EXE%
)

REM ── 创建日志目录 ───────────────────────────
if not exist "%LOG_DIR%" mkdir "%LOG_DIR%"
if not exist "%ROOT_DIR%data" mkdir "%ROOT_DIR%data"

REM ── 检测可选运行组件 ───────────────────────
where ffmpeg >nul 2>&1
if %errorlevel% neq 0 echo [警告] 未找到 ffmpeg；语音转换不可用。请从可信来源安装并加入 PATH。
where ssh >nul 2>&1
if %errorlevel% neq 0 echo [警告] 未找到 ssh；Serveo 隧道不可用。请安装 Windows OpenSSH Client。

REM ── 服务注册前完成 owner 初始化 ─────────────
set "ILINK_DATA_DIR=%ROOT_DIR%data"
echo [信息] 现在创建或确认 owner 管理员账号...
"%APP_EXE%" admin init
if %errorlevel% neq 0 (
    echo [错误] owner 初始化失败，未注册服务。
    pause
    exit /b 1
)

REM ── 选择明确的网络安全模式 ─────────────────
echo.
echo 请选择部署模式：
echo   1. 已有 HTTPS 反向代理（推荐；默认代理地址 127.0.0.1）
echo   2. 仅受信任内网明文 HTTP（不会自动获得 TLS）
set /p "SECURITY_MODE=输入 1 或 2 [默认 1]: "
if "!SECURITY_MODE!"=="" set "SECURITY_MODE=1"
if "%SECURITY_MODE%"=="1" (
    set /p "TRUSTED_PROXY=可信代理 IP/CIDR [127.0.0.1]: "
    if "!TRUSTED_PROXY!"=="" set "TRUSTED_PROXY=127.0.0.1"
) else if "%SECURITY_MODE%"=="2" (
    set /p "INSECURE_CONFIRM=确认端口只暴露在受信任内网？请输入 YES: "
    if /i not "!INSECURE_CONFIRM!"=="YES" (
        echo [已取消] 未确认明文内网部署。
        pause
        exit /b 1
    )
) else (
    echo [错误] 无效选项。
    pause
    exit /b 1
)

REM ── 若已安装则先停止删除 ───────────────────
sc query %SERVICE_NAME% >nul 2>&1
if %errorlevel% equ 0 (
    echo [信息] 服务已存在，先停止并删除旧配置...
    "%NSSM_EXE%" stop %SERVICE_NAME% 2>nul
    "%NSSM_EXE%" remove %SERVICE_NAME% confirm 2>nul
    timeout /t 2 /nobreak >nul
)

REM ── 注册服务 ───────────────────────────────
echo [信息] 注册服务 %SERVICE_NAME% ...
"%NSSM_EXE%" install %SERVICE_NAME% "%APP_EXE%"
"%NSSM_EXE%" set %SERVICE_NAME% AppDirectory "%ROOT_DIR%"

REM 审计 M-11: 服务改用低权虚拟账户 NT SERVICE\ilink-wm1 运行（不再以 LocalSystem 运行），
REM Web 应用被攻破时不再直接获得 SYSTEM 权限；虚拟账户无需密码，仅本服务可用。
"%NSSM_EXE%" set %SERVICE_NAME% ObjectName "NT SERVICE\%SERVICE_NAME%" ""

REM 审计 M-11: 仅授予该虚拟账户对安装目录的修改权限（数据/日志/主密钥文件需要写），
REM /T 把授权应用到已存在的子目录与文件（升级重装场景）。
icacls "%ROOT_DIR%" /grant "NT SERVICE\%SERVICE_NAME%:(OI)(CI)M" /T >nul
if !errorlevel! neq 0 (
    echo [警告] icacls 授权失败（exit=!errorlevel!），服务可能无法写数据/日志目录。
    echo   请手动执行: icacls "%ROOT_DIR%" /grant "NT SERVICE\%SERVICE_NAME%:(OI)(CI)M" /T
)

if "%SECURITY_MODE%"=="1" (
    "%NSSM_EXE%" set %SERVICE_NAME% AppEnvironmentExtra "ILINK_HOST=0.0.0.0" "ILINK_PORT=%DEFAULT_PORT%" "ILINK_DATA_DIR=%ROOT_DIR%data" "ILINK_SERVER_MODE=1" "ILINK_TRUSTED_PROXIES=!TRUSTED_PROXY!" "ILINK_FORCE_HTTPS=1" "RUST_LOG=ilink_wm1=info" "RUST_BACKTRACE=full"
) else (
    "%NSSM_EXE%" set %SERVICE_NAME% AppEnvironmentExtra "ILINK_HOST=0.0.0.0" "ILINK_PORT=%DEFAULT_PORT%" "ILINK_DATA_DIR=%ROOT_DIR%data" "ILINK_SERVER_MODE=1" "ILINK_ALLOW_INSECURE_PUBLIC=1" "RUST_LOG=ilink_wm1=info" "RUST_BACKTRACE=full"
)

REM 日志重定向
"%NSSM_EXE%" set %SERVICE_NAME% AppStdout "%LOG_DIR%\service.log"
"%NSSM_EXE%" set %SERVICE_NAME% AppStderr "%LOG_DIR%\service.log"
"%NSSM_EXE%" set %SERVICE_NAME% AppRotateFiles 1
"%NSSM_EXE%" set %SERVICE_NAME% AppRotateBytes 10485760

REM 启动模式：开机自启
"%NSSM_EXE%" set %SERVICE_NAME% Start SERVICE_AUTO_START

REM 崩溃自动重启
"%NSSM_EXE%" set %SERVICE_NAME% AppExit Default Restart
"%NSSM_EXE%" set %SERVICE_NAME% AppRestartDelay 5000

echo [OK] 服务配置完成

REM ── 启动服务 ───────────────────────────────
echo [信息] 启动服务...
"%NSSM_EXE%" start %SERVICE_NAME%
timeout /t 3 /nobreak >nul

sc query %SERVICE_NAME% | findstr /i "RUNNING" >nul
if %errorlevel% equ 0 (
    echo [OK] 服务已启动
) else (
    echo [错误] 服务启动失败，请查看日志: %LOG_DIR%\service.log
    pause
    exit /b 1
)

REM ── 完成 ───────────────────────────────────
echo.
echo ========================================
echo   iLink-WM1 服务安装完成！
echo ========================================
echo.
echo   服务名称:  %SERVICE_NAME%
echo   运行账户:  NT SERVICE\%SERVICE_NAME%（低权虚拟账户）
echo   二进制:    %APP_EXE%
echo   监听地址:  0.0.0.0:%DEFAULT_PORT%
echo   数据目录:  %ROOT_DIR%data
echo   日志文件:  %LOG_DIR%\service.log
echo.
echo   访问地址:  http://localhost:%DEFAULT_PORT%
echo.
echo 服务管理命令（PowerShell 或 cmd）：
echo   查看状态:  sc query %SERVICE_NAME%
echo             或 Get-Service %SERVICE_NAME%
echo   启动:      sc start %SERVICE_NAME%
echo             或 Start-Service %SERVICE_NAME%
echo   停止:      sc stop %SERVICE_NAME%
echo             或 Stop-Service %SERVICE_NAME%
echo   重启:      PowerShell: Restart-Service %SERVICE_NAME%
echo   卸载:      "%NSSM_EXE%" stop %SERVICE_NAME% ^&^& "%NSSM_EXE%" remove %SERVICE_NAME% confirm
echo.
echo 实时查看日志：
echo   PowerShell: Get-Content "%LOG_DIR%\service.log" -Tail 100 -Wait
echo   cmd:        tail -f "%LOG_DIR%\service.log"
echo.

REM ── 打开浏览器 ─────────────────────────────
echo [信息] 3 秒后自动打开浏览器...
timeout /t 3 /nobreak >nul
start """" http://localhost:%DEFAULT_PORT%

pause
