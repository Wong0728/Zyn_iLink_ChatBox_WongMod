@echo off
REM ============================================================================
REM  iLink-WM1 Windows launcher (double-click to run)
REM
REM  NOTE: this file must stay PURE ASCII. cmd.exe mis-tracks read offsets in
REM  batch files containing multi-byte characters (UTF-8 or GBK alike), which
REM  makes it execute line fragments as commands. Chinese docs: README.md /
REM  deploy guide / `iLinkWM help`.
REM
REM  What it does:
REM    1. start ilink-wm1.exe
REM    2. wait for port 8888 to come up
REM    3. open http://localhost:8888 in the default browser
REM    4. show live logs in this window; close the window to stop the service
REM
REM  Data location: data\ under the script directory by default,
REM  override with the ILINK_DATA_DIR environment variable.
REM ============================================================================

setlocal enabledelayedexpansion

cd /d "%~dp0"

set "BIN_PATH=%~dp0ilink-wm1.exe"
set "WEB_DIR=%~dp0web"
set "DEFAULT_PORT=8888"
if "%ILINK_HOST%"=="" set "ILINK_HOST=0.0.0.0"
if "%ILINK_PORT%"=="" set "ILINK_PORT=%DEFAULT_PORT%"
if "%ILINK_DATA_DIR%"=="" set "ILINK_DATA_DIR=%~dp0data"
if "%RUST_LOG%"=="" set "RUST_LOG=ilink_wm1=info"
set "RUST_BACKTRACE=full"

echo ========================================
echo   iLink-WM1 starting...
echo ========================================
echo.
echo   binary:    %BIN_PATH%
echo   web dir:   %WEB_DIR%
echo   listen:    %ILINK_HOST%:%ILINK_PORT%
echo   data dir:  %ILINK_DATA_DIR%
echo   URL:       http://localhost:%ILINK_PORT%
echo.

REM -- check binary -----------------------------
if not exist "%BIN_PATH%" (
    echo [ERROR] ilink-wm1.exe not found at:
    echo   %BIN_PATH%
    echo   Please make sure the ZIP package was fully extracted.
    pause
    exit /b 1
)

REM -- check web dir ----------------------------
if not exist "%WEB_DIR%" (
    echo [ERROR] web\ directory not found at:
    echo   %WEB_DIR%
    echo   Please make sure the ZIP package was fully extracted.
    pause
    exit /b 1
)

REM -- create data dir --------------------------
if not exist "%ILINK_DATA_DIR%" (
    mkdir "%ILINK_DATA_DIR%"
    echo [INFO] data dir created: %ILINK_DATA_DIR%
)

REM -- public bind safety confirmation ----------
if /i "%ILINK_HOST%"=="0.0.0.0" (
    if not "%ILINK_ALLOW_INSECURE_PUBLIC%"=="1" (
        if "%ILINK_TRUSTED_PROXIES%"=="" (
            echo [SECURITY] About to listen on all IPv4 interfaces without an HTTPS reverse proxy.
            echo   Plain HTTP is only acceptable inside a trusted LAN.
            set /p "LAN_CONFIRM=Trusted LAN - continue with plain HTTP? type YES: "
            if /i not "!LAN_CONFIRM!"=="YES" (
                echo [CANCELLED] Set up an HTTPS reverse proxy first, then set ILINK_TRUSTED_PROXIES and ILINK_FORCE_HTTPS=1.
                pause
                exit /b 1
            )
            set "ILINK_ALLOW_INSECURE_PUBLIC=1"
        ) else if not "%ILINK_FORCE_HTTPS%"=="1" (
            echo [ERROR] Trusted proxies are configured but ILINK_FORCE_HTTPS is not 1.
            echo   Only set ILINK_FORCE_HTTPS=1 when the upstream really provides HTTPS.
            pause
            exit /b 1
        )
    )
)

echo [INFO] starting service...
echo [INFO] close this window or press Ctrl+C to stop.
echo.

REM -- open the browser after 3 seconds ---------
start /b cmd /c "timeout /t 3 /nobreak >nul && start """" http://localhost:%ILINK_PORT%"

REM -- run in foreground (logs visible) ---------
"%BIN_PATH%"

echo.
echo [INFO] service stopped.
pause
