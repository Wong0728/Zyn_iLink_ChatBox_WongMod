@echo off
REM ============================================================================
REM  iLink-WM1 Windows service installer (NSSM)
REM
REM  NOTE: this file must stay PURE ASCII. cmd.exe mis-tracks read offsets in
REM  batch files containing multi-byte characters (UTF-8 or GBK alike), which
REM  makes it execute line fragments as commands. Chinese docs: README.md /
REM  deploy guide / `iLinkWM help`.
REM
REM  What it does:
REM    1. check NSSM; download a pinned copy to bin\nssm.exe if missing
REM    2. register the ilink-wm1 Windows service (auto start + crash restart)
REM    3. start the service
REM    4. open http://localhost:8888 in the default browser
REM
REM  Must run as Administrator: right-click -^> Run as administrator
REM  (or simply run:  iLinkWM install-service  which elevates for you)
REM
REM  Uninstall:
REM    bin\nssm.exe stop ilink-wm1
REM    bin\nssm.exe remove ilink-wm1 confirm
REM ============================================================================

setlocal enabledelayedexpansion

REM -- require admin ----------------------------
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [ERROR] This script must be run as Administrator.
    echo   Right-click this .bat file and choose "Run as administrator".
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
echo   iLink-WM1 Windows service setup
echo ========================================
echo.

REM -- check binary -----------------------------
if not exist "%APP_EXE%" (
    echo [ERROR] ilink-wm1.exe not found at:
    echo   %APP_EXE%
    pause
    exit /b 1
)

REM -- check / download NSSM --------------------
if exist "%NSSM_EXE%" (
    powershell -NoProfile -Command "$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath '%NSSM_EXE%').Hash; if ($actual -ne '%NSSM_EXE_SHA256%') { exit 1 }"
    if !errorlevel! neq 0 (
        echo [WARN] existing NSSM failed the pinned-version check; re-downloading a trusted copy.
        del /q "%NSSM_EXE%" >nul 2>&1
    )
)
if not exist "%NSSM_EXE%" (
    echo [INFO] NSSM not found, downloading...
    if not exist "%BIN_DIR%" mkdir "%BIN_DIR%"

    echo [INFO] download: %NSSM_URL%
    powershell -NoProfile -Command "try { $ProgressPreference='SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri '%NSSM_URL%' -OutFile '%NSSM_ZIP%' -UseBasicParsing } catch { exit 1 }"

    if not exist "%NSSM_ZIP%" (
        echo [ERROR] NSSM download failed.
        echo   Please download https://nssm.cc/release/nssm-2.24.zip manually,
        echo   extract win64\nssm.exe into %BIN_DIR%\nssm.exe
        echo   and run this script again.
        pause
        exit /b 1
    )

    echo [INFO] verifying NSSM SHA-256...
    powershell -NoProfile -Command "$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath '%NSSM_ZIP%').Hash; if ($actual -ne '%NSSM_ZIP_SHA256%') { Write-Error ('SHA-256 mismatch: ' + $actual); exit 1 }"
    if !errorlevel! neq 0 (
        echo [ERROR] NSSM integrity check failed. Installation aborted.
        del /q "%NSSM_ZIP%" >nul 2>&1
        pause
        exit /b 1
    )

    echo [INFO] extracting NSSM...
    if exist "%NSSM_EXTRACT%" rmdir /s /q "%NSSM_EXTRACT%"
    powershell -NoProfile -Command "$ProgressPreference='SilentlyContinue'; Expand-Archive -Path '%NSSM_ZIP%' -DestinationPath '%NSSM_EXTRACT%' -Force"

    REM Copy only the verified 64-bit binary from the pinned ZIP layout.
    if exist "%NSSM_EXTRACT%\nssm-2.24\win64\nssm.exe" (
        copy /y "%NSSM_EXTRACT%\nssm-2.24\win64\nssm.exe" "%NSSM_EXE%" >nul
    ) else (
        echo [ERROR] NSSM ZIP layout does not match the pinned-version expectation.
        pause
        exit /b 1
    )

    if not exist "%NSSM_EXE%" (
        echo [ERROR] NSSM extraction failed; nssm.exe not found.
        pause
        exit /b 1
    )

    powershell -NoProfile -Command "$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath '%NSSM_EXE%').Hash; if ($actual -ne '%NSSM_EXE_SHA256%') { Write-Error ('nssm.exe SHA-256 mismatch: ' + $actual); exit 1 }"
    if !errorlevel! neq 0 (
        echo [ERROR] post-extraction nssm.exe integrity check failed.
        del /q "%NSSM_EXE%" >nul 2>&1
        pause
        exit /b 1
    )

    echo [OK] NSSM ready: %NSSM_EXE%
) else (
    echo [OK] NSSM already present: %NSSM_EXE%
)

REM -- create log/data dirs ---------------------
if not exist "%LOG_DIR%" mkdir "%LOG_DIR%"
if not exist "%ROOT_DIR%data" mkdir "%ROOT_DIR%data"

REM -- optional runtime components --------------
where ffmpeg >nul 2>&1
if %errorlevel% neq 0 echo [WARN] ffmpeg not found; voice conversion disabled. Install it from a trusted source into PATH.
where ssh >nul 2>&1
if %errorlevel% neq 0 echo [WARN] ssh not found; Serveo tunnel disabled. Install the Windows OpenSSH Client.

REM -- owner init before service registration --
set "ILINK_DATA_DIR=%ROOT_DIR%data"
echo [INFO] creating / confirming the owner admin account...
"%APP_EXE%" admin init
if %errorlevel% neq 0 (
    echo [ERROR] owner init failed; service not registered.
    pause
    exit /b 1
)

REM -- pick an explicit security mode -----------
echo.
echo Select the deployment mode:
echo   1. HTTPS reverse proxy already in front ^(recommended; default proxy 127.0.0.1^)
echo   2. Trusted-LAN plain HTTP only ^(no TLS added^)
set /p "SECURITY_MODE=Enter 1 or 2 [default 1]: "
if "!SECURITY_MODE!"=="" set "SECURITY_MODE=1"
if "%SECURITY_MODE%"=="1" (
    set /p "TRUSTED_PROXY=Trusted proxy IP/CIDR [127.0.0.1]: "
    if "!TRUSTED_PROXY!"=="" set "TRUSTED_PROXY=127.0.0.1"
) else if "%SECURITY_MODE%"=="2" (
    set /p "INSECURE_CONFIRM=Confirm the port is ONLY exposed on a trusted LAN? type YES: "
    if /i not "!INSECURE_CONFIRM!"=="YES" (
        echo [CANCELLED] plain-LAN deployment not confirmed.
        pause
        exit /b 1
    )
) else (
    echo [ERROR] invalid choice.
    pause
    exit /b 1
)

REM -- stop/remove previous service if any ------
sc query %SERVICE_NAME% >nul 2>&1
if %errorlevel% equ 0 (
    echo [INFO] service exists; stopping and removing the old config first...
    "%NSSM_EXE%" stop %SERVICE_NAME% 2>nul
    "%NSSM_EXE%" remove %SERVICE_NAME% confirm 2>nul
    timeout /t 2 /nobreak >nul
)

REM -- register the service ---------------------
echo [INFO] registering service %SERVICE_NAME% ...
"%NSSM_EXE%" install %SERVICE_NAME% "%APP_EXE%"
"%NSSM_EXE%" set %SERVICE_NAME% AppDirectory "%ROOT_DIR%"

REM Audit M-11: run as the low-privilege virtual account NT SERVICE\ilink-wm1
REM instead of LocalSystem, so a compromised web app does not get SYSTEM.
"%NSSM_EXE%" set %SERVICE_NAME% ObjectName "NT SERVICE\%SERVICE_NAME%" ""

REM Audit M-11: grant that virtual account modify rights on the install dir
REM (data / logs / master key need write). /T applies to existing children too.
icacls "%ROOT_DIR%" /grant "NT SERVICE\%SERVICE_NAME%:(OI)(CI)M" /T >nul
if !errorlevel! neq 0 (
    echo [WARN] icacls grant failed ^(exit=!errorlevel!^); the service may be unable to write data/logs.
    echo   Run manually: icacls "%ROOT_DIR%" /grant "NT SERVICE\%SERVICE_NAME%:(OI)(CI)M" /T
)

if "%SECURITY_MODE%"=="1" (
    "%NSSM_EXE%" set %SERVICE_NAME% AppEnvironmentExtra "ILINK_HOST=0.0.0.0" "ILINK_PORT=%DEFAULT_PORT%" "ILINK_DATA_DIR=%ROOT_DIR%data" "ILINK_SERVER_MODE=1" "ILINK_TRUSTED_PROXIES=!TRUSTED_PROXY!" "ILINK_FORCE_HTTPS=1" "RUST_LOG=ilink_wm1=info" "RUST_BACKTRACE=full"
) else (
    "%NSSM_EXE%" set %SERVICE_NAME% AppEnvironmentExtra "ILINK_HOST=0.0.0.0" "ILINK_PORT=%DEFAULT_PORT%" "ILINK_DATA_DIR=%ROOT_DIR%data" "ILINK_SERVER_MODE=1" "ILINK_ALLOW_INSECURE_PUBLIC=1" "RUST_LOG=ilink_wm1=info" "RUST_BACKTRACE=full"
)

REM log redirection
"%NSSM_EXE%" set %SERVICE_NAME% AppStdout "%LOG_DIR%\service.log"
"%NSSM_EXE%" set %SERVICE_NAME% AppStderr "%LOG_DIR%\service.log"
"%NSSM_EXE%" set %SERVICE_NAME% AppRotateFiles 1
"%NSSM_EXE%" set %SERVICE_NAME% AppRotateBytes 10485760

REM auto start on boot
"%NSSM_EXE%" set %SERVICE_NAME% Start SERVICE_AUTO_START

REM restart on crash
"%NSSM_EXE%" set %SERVICE_NAME% AppExit Default Restart
"%NSSM_EXE%" set %SERVICE_NAME% AppRestartDelay 5000

echo [OK] service configured.

REM -- start the service ------------------------
echo [INFO] starting service...
"%NSSM_EXE%" start %SERVICE_NAME%
timeout /t 3 /nobreak >nul

sc query %SERVICE_NAME% | findstr /i "RUNNING" >nul
if %errorlevel% equ 0 (
    echo [OK] service is running.
) else (
    echo [ERROR] service failed to start; check the log: %LOG_DIR%\service.log
    pause
    exit /b 1
)

REM -- done -------------------------------------
echo.
echo ========================================
echo   iLink-WM1 service installed!
echo ========================================
echo.
echo   service:     %SERVICE_NAME%
echo   run as:      NT SERVICE\%SERVICE_NAME% ^(low-privilege virtual account^)
echo   binary:      %APP_EXE%
echo   listen:      0.0.0.0:%DEFAULT_PORT%
echo   data dir:    %ROOT_DIR%data
echo   log file:    %LOG_DIR%\service.log
echo.
echo   URL:         http://localhost:%DEFAULT_PORT%
echo.
echo management ^(PowerShell or cmd^):
echo   status:  sc query %SERVICE_NAME%   /  Get-Service %SERVICE_NAME%
echo   start:   sc start %SERVICE_NAME%  /  Start-Service %SERVICE_NAME%
echo   stop:    sc stop %SERVICE_NAME%   /  Stop-Service %SERVICE_NAME%
echo   restart: PowerShell: Restart-Service %SERVICE_NAME%
echo   remove:  "%NSSM_EXE%" stop %SERVICE_NAME% ^&^& "%NSSM_EXE%" remove %SERVICE_NAME% confirm
echo.
echo live logs:
echo   PowerShell: Get-Content "%LOG_DIR%\service.log" -Tail 100 -Wait
echo   cmd:        type "%LOG_DIR%\service.log"
echo.

REM -- open the browser -------------------------
echo [INFO] opening the browser in 3 seconds...
timeout /t 3 /nobreak >nul
start """" http://localhost:%DEFAULT_PORT%

pause
