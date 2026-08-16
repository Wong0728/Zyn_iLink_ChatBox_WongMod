<#
.SYNOPSIS
  iLink-WM1 (iLinkWM) Windows 一键安装器
.DESCRIPTION
  通过 GitHub Release 下载预编译包安装；无可用 Release 时回退为
  「git clone + cargo build --release」源码编译。安装后提供 iLinkWM 命令。

  用法（PowerShell）：
    irm https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/windows/install.ps1 | iex

  本脚本面向 irm | iex 分发，必须保持 UTF-8 无 BOM：带 BOM 时首个语句会被
  解析成命令名导致 CommandNotFoundException。

  可选环境变量：
    ILINKWM_VERSION  指定版本 tag（如 v3.2.4），默认 latest
    ILINKWM_METHOD   auto | binary | source（默认 auto）
#>

if ($PSVersionTable.PSVersion.Major -lt 5) { throw '[iLinkWM] 需要 Windows PowerShell 5.1 或更高版本。' }
$ErrorActionPreference = 'Stop'
# 静音 Invoke-WebRequest/RestMethod 的进度条（"正在写入请求流..."），
# 同时避免 PS 5.1 进度条渲染拖慢下载
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo        = 'Wong0728/Zyn_iLink_ChatBox_WongMod'
$Branch      = 'main'
$AppId       = 'iLinkWM'
$InstallRoot = Join-Path $env:LOCALAPPDATA "Programs\$AppId"
$BinDir      = Join-Path $InstallRoot 'bin'
$DataDir     = Join-Path $InstallRoot 'data'
$RawBase     = "https://raw.githubusercontent.com/$Repo/$Branch"

function Write-Info  { Write-Host "[iLinkWM] $args" -ForegroundColor Cyan }
function Write-Ok    { Write-Host "[iLinkWM] $args" -ForegroundColor Green }
function Write-Warn2 { Write-Host "[iLinkWM] $args" -ForegroundColor Yellow }

function Install-FromBinary {
    param([string]$Version)

    $rel = $null
    if ($Version -and $Version -ne 'latest') {
        try { $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$Version" -TimeoutSec 30 }
        catch { Write-Warn2 "未找到版本 $Version：$($_.Exception.Message)" }
    } else {
        try { $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -TimeoutSec 30 }
        catch { Write-Warn2 "尚无 Release（$($_.Exception.Message)）" }
    }
    $asset = $null
    if ($rel) { $asset = $rel.assets | Where-Object { $_.name -match 'win_x64\.zip$' } | Select-Object -First 1 }
    if (-not $asset) { return $false }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) "ilinkwm_install_$(Get-Random)"
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    $zip = Join-Path $tmp $asset.name
    Write-Info "下载 $($asset.name)（$([math]::Round($asset.size/1MB,1)) MB）..."
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -TimeoutSec 600 -UseBasicParsing

    $extract = Join-Path $tmp 'extract'
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    # zip 内容在根目录（web/、ilink-wm1.exe、...），若包了一层目录则下钻
    $srcRoot = $extract
    if (-not (Test-Path (Join-Path $srcRoot 'ilink-wm1.exe'))) {
        $nested = Get-ChildItem $extract -Directory | Select-Object -First 1
        if ($nested -and (Test-Path (Join-Path $nested.FullName 'ilink-wm1.exe'))) { $srcRoot = $nested.FullName }
    }
    if (-not (Test-Path (Join-Path $srcRoot 'ilink-wm1.exe'))) { throw "压缩包内未找到 ilink-wm1.exe" }

    if (Test-Path $InstallRoot) {
        Write-Info "升级：保留 data/ 与 bin/，覆盖其余文件..."
        Get-ChildItem $InstallRoot | Where-Object { $_.Name -ne 'data' -and $_.Name -ne 'bin' } |
            Remove-Item -Recurse -Force
    } else {
        New-Item -ItemType Directory -Path $InstallRoot -Force | Out-Null
    }
    Get-ChildItem $srcRoot | Where-Object { $_.Name -ne 'data' } |
        Copy-Item -Destination $InstallRoot -Recurse -Force
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    Write-Ok "已安装 $($rel.tag_name)（Release 预编译包）"
    return $true
}

function Install-FromSource {
    $git = Get-Command git -ErrorAction SilentlyContinue
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        $cargoPath = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (Test-Path $cargoPath) { $cargo = $cargoPath }
    }
    if (-not $git)   { throw "未找到 git。源码安装需要 git 与 Rust 工具链，请安装后重试，或等待 Release 预编译包。" }
    if (-not $cargo) { throw "未找到 cargo。请先安装 Rust stable（https://www.rust-lang.org/tools/install），或等待 Release 预编译包。" }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) "ilinkwm_src_$(Get-Random)"
    Write-Info "克隆源码到 $tmp ..."
    & git clone --depth 1 "https://github.com/$Repo.git" $tmp 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git clone 失败" }

    Write-Info "cargo build --release（首次约 3-10 分钟）..."
    Push-Location $tmp
    try {
        & $cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "编译失败" }
    } finally { Pop-Location }

    $exe = Join-Path $tmp 'target\release\ilink-wm1.exe'
    if (-not (Test-Path $exe)) { throw "编译产物未找到：$exe" }

    if (Test-Path $InstallRoot) {
        Get-ChildItem $InstallRoot | Where-Object { $_.Name -ne 'data' -and $_.Name -ne 'bin' } |
            Remove-Item -Recurse -Force
    } else {
        New-Item -ItemType Directory -Path $InstallRoot -Force | Out-Null
    }
    Copy-Item $exe $InstallRoot -Force
    Copy-Item (Join-Path $tmp 'web') $InstallRoot -Recurse -Force
    foreach ($f in 'LICENSE','README.md','CHANGELOG.md','start.bat','install-service.bat','用户协议.md','部署指南.md') {
        $p = Join-Path $tmp $f
        if (Test-Path $p) { Copy-Item $p $InstallRoot -Force }
    }
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    Write-Ok "已安装（源码编译，分支 $Branch）"
    return $true
}

function Write-Shim {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    $cmdPath = Join-Path $BinDir 'iLinkWM.cmd'
    # 【重要】cmd.exe 激活 65001 代码页后无法可靠解析含多字节字符的批处理：
    # 行定位按字符数而非字节数计算，会在文件中错位落点，把行片段当命令执行
    # （如 '-service' 不是内部或外部命令）。因此 .cmd 本体必须纯 ASCII，
    # 中文帮助放独立 UTF-8 文本文件由 type 输出（type 输出不经过解析器）。
    $shim = @'
@echo off
setlocal EnableExtensions
set "APP_ROOT=%~dp0.."
set "BIN=%APP_ROOT%\ilink-wm1.exe"

if "%~1"==""                      goto :run
if /i "%~1"=="update"             goto :update
if /i "%~1"=="uninstall"          goto :uninstall
if /i "%~1"=="install-service"    goto :instsvc
if /i "%~1"=="uninstall-service"  goto :uninstsvc
if /i "%~1"=="service"            goto :service
if /i "%~1"=="help"               goto :help
if /i "%~1"=="-h"                 goto :help
if /i "%~1"=="--help"             goto :help
if /i "%~1"=="ilinkwm-help"       goto :help
goto :run

:run
if not exist "%BIN%" (
    echo [iLinkWM] ilink-wm1.exe not found. Please reinstall:
    echo   powershell -NoProfile -ExecutionPolicy Bypass -Command "irm RAWBASE/deploy/windows/install.ps1 | iex"
    exit /b 1
)
cd /d "%APP_ROOT%"
if "%ILINK_DATA_DIR%"=="" set "ILINK_DATA_DIR=%APP_ROOT%\data"
"%BIN%" %*
exit /b %errorlevel%

:update
echo [iLinkWM] Checking and installing the latest version...
powershell -NoProfile -NoLogo -ExecutionPolicy Bypass -Command "irm RAWBASE/deploy/windows/install.ps1 | iex"
exit /b %errorlevel%

:instsvc
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [iLinkWM] Registering the Windows service requires admin. Elevating...
    powershell -NoProfile -NoLogo -Command "Start-Process -Verb RunAs -FilePath '%APP_ROOT%\install-service.bat'"
    exit /b 0
)
call "%APP_ROOT%\install-service.bat"
exit /b %errorlevel%

:uninstsvc
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [iLinkWM] Removing the Windows service requires admin. Elevating...
    powershell -NoProfile -NoLogo -Command "Start-Process -Verb RunAs -FilePath 'cmd.exe' -ArgumentList '/c \"APPROOTRELSLASH\nssm.exe\" stop ilink-wm1 && \"APPROOTRELSLASH\nssm.exe\" remove ilink-wm1 confirm'"
    exit /b 0
)
if exist "%APP_ROOT%\bin\nssm.exe" (
    "%APP_ROOT%\bin\nssm.exe" stop ilink-wm1
    "%APP_ROOT%\bin\nssm.exe" remove ilink-wm1 confirm
) else (
    echo [iLinkWM] bin\nssm.exe not found. Run manually: nssm stop ilink-wm1 ^&^& nssm remove ilink-wm1 confirm
)
exit /b 0

:service
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [iLinkWM] Service control requires admin. Usage: sc query ilink-wm1 / net stop ilink-wm1 / net start ilink-wm1
    exit /b 1
)
if /i "%~2"=="start" ( net start ilink-wm1 & exit /b 0 )
if /i "%~2"=="stop"  ( net stop  ilink-wm1 & exit /b 0 )
sc query ilink-wm1
exit /b 0

:uninstall
set "MODE=%~2"
set "CONFIRM=N"
if /i "%MODE%"=="--keep-data" (
    set /p CONFIRM=Uninstall iLinkWM but KEEP the data directory? [Y/N]: 
) else (
    set /p CONFIRM=Uninstall iLinkWM and DELETE program + ALL data? [Y/N] ^(--keep-data keeps data^): 
)
if /i not "%CONFIRM%"=="Y" ( echo [iLinkWM] Cancelled. & exit /b 0 )
echo [iLinkWM] Uninstalling...
if exist "%APP_ROOT%\bin\nssm.exe" (
    "%APP_ROOT%\bin\nssm.exe" stop ilink-wm1 >nul 2>&1
    "%APP_ROOT%\bin\nssm.exe" remove ilink-wm1 confirm >nul 2>&1
)
powershell -NoProfile -NoLogo -Command "$b='BINDIR'; $p=[Environment]::GetEnvironmentVariable('Path','User'); $new=($p -split ';' | Where-Object { $_ -and $_ -ne $b }) -join ';'; [Environment]::SetEnvironmentVariable('Path',$new,'User')"
if /i "%MODE%"=="--keep-data" (
    powershell -NoProfile -NoLogo -Command "Remove-Item -Recurse -Force 'INSTALLROOT\web','INSTALLROOT\ilink-wm1.exe','INSTALLROOT\bin','INSTALLROOT\logs' -ErrorAction SilentlyContinue"
    echo [iLinkWM] Uninstalled. Data directory kept at: DATADIR
) else (
    echo [iLinkWM] Uninstalled. Program and data at DATADIR will be deleted in 2 seconds.
    > "%TEMP%\ilinkwm_uninstall.cmd" echo @timeout /t 2 /nobreak ^>nul
    >>"%TEMP%\ilinkwm_uninstall.cmd" echo @rd /s /q "%APP_ROOT%"
    >>"%TEMP%\ilinkwm_uninstall.cmd" echo @del "%%~f0"
    start "" /min "%TEMP%\ilinkwm_uninstall.cmd"
)
echo [iLinkWM] Please close and reopen your terminal to refresh PATH.
exit /b 0

:help
for /f "tokens=2 delims=:" %%c in ('chcp') do set "OLDCP=%%c"
chcp 65001 >nul
type "%~dp0iLinkWM-help.txt"
chcp %OLDCP% >nul
exit /b 0
'@
    # 展开真实路径/URL 占位符（避免正则元字符，用 .Replace）
    $shim = $shim.Replace('RAWBASE', $RawBase)
    $shim = $shim.Replace('BINDIR', $BinDir)
    $shim = $shim.Replace('INSTALLROOT', $InstallRoot)
    $shim = $shim.Replace('DATADIR', $DataDir)
    $shim = $shim.Replace('APPROOTRELSLASH', ($InstallRoot + '\bin'))
    # 行尾强制 CRLF：经 raw.githubusercontent 下载的本脚本是 LF（git 归一化），
    # here-string 会继承 LF；cmd 解析 LF 行尾的批处理不可靠，必须转换。
    # 纯 ASCII 编码落盘：任何控制台代码页下解析都安全。
    $shim = $shim -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText($cmdPath, $shim, [Text.Encoding]::ASCII)

    # 中文帮助文本（UTF-8 无 BOM）：仅由 :help 分支 chcp 65001 后 type 输出，
    # 内容不经过 cmd 解析器，无行错位风险。
    $helpPath = Join-Path $BinDir 'iLinkWM-help.txt'
    $helpText = @'
iLinkWM - Zyn iLink ChatBox WongMod 统一命令

  iLinkWM                     启动程序（首次运行进入初始化向导）
  iLinkWM help                显示本帮助（-h / --help 同效）
  iLinkWM install-service     注册 Windows 服务（NSSM，需管理员）
  iLinkWM uninstall-service   移除 Windows 服务（需管理员）
  iLinkWM service start|stop  启停服务（需管理员）；其余参数查询状态
  iLinkWM update              更新到最新版本
  iLinkWM uninstall [--keep-data] 卸载；默认删除程序与全部数据，--keep-data 保留数据
  iLinkWM admin ...           其余参数原样传给 ilink-wm1
  ilink-wm1 ...               二进制直通命令（同在 PATH）：ilink-wm1 --version / admin ...

安装目录与数据目录默认位于 %LOCALAPPDATA%\Programs\iLinkWM。
'@
    $helpText = $helpText -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText($helpPath, $helpText, (New-Object System.Text.UTF8Encoding($false)))

    # ilink-wm1 直通命令：任意终端 ilink-wm1 --version / ilink-wm1 admin ...
    $exeShimPath = Join-Path $BinDir 'ilink-wm1.cmd'
    $exeShim = @'
@echo off
setlocal EnableExtensions
set "APP_ROOT=%~dp0.."
if not exist "%APP_ROOT%\ilink-wm1.exe" (
    echo [ilink-wm1] ilink-wm1.exe not found. Please reinstall iLinkWM.
    exit /b 1
)
cd /d "%APP_ROOT%"
if "%ILINK_DATA_DIR%"=="" set "ILINK_DATA_DIR=%APP_ROOT%\data"
"%APP_ROOT%\ilink-wm1.exe" %*
exit /b %errorlevel%
'@
    $exeShim = $exeShim -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText($exeShimPath, $exeShim, [Text.Encoding]::ASCII)
    Write-Ok "命令入口：$cmdPath、$exeShimPath"
}

function Add-UserPath {
    param([string]$Dir)
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath) { $userPath = '' }
    $parts = $userPath -split ';' | Where-Object { $_ }
    if ($parts -notcontains $Dir) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $Dir) -join ';'), 'User')
        Write-Ok "已将 $Dir 加入用户 PATH（重开终端后生效）"
    } else {
        Write-Ok "PATH 已包含 $Dir"
    }
}

# ── 主流程 ─────────────────────────────────────────────
Write-Info "iLink-WM1 安装器 · 目标目录 $InstallRoot"
$method  = if ($env:ILINKWM_METHOD)  { $env:ILINKWM_METHOD }  else { 'auto' }
$version = if ($env:ILINKWM_VERSION) { $env:ILINKWM_VERSION } else { 'latest' }

$ok = $false
if ($method -eq 'binary') {
    $ok = Install-FromBinary -Version $version
    if (-not $ok) { throw "未找到可用的 Windows 预编译包（$version）" }
} elseif ($method -eq 'source') {
    $ok = Install-FromSource
} else {
    try { $ok = Install-FromBinary -Version $version } catch { Write-Warn2 $_ }
    if (-not $ok) {
        Write-Warn2 "回退源码编译模式..."
        $ok = Install-FromSource
    }
}

Write-Shim
Add-UserPath -Dir $BinDir

Write-Host ''
Write-Ok '安装完成！下一步：'
Write-Host '  1. 关闭并重新打开终端（使 PATH 生效）'
Write-Host '  2. 运行  iLinkWM                # 首次运行进入初始化向导'
Write-Host '  3. 可选  iLinkWM install-service # 注册为 Windows 服务'
Write-Host ''
Write-Host "  安装目录：$InstallRoot"
Write-Host "  数据目录：$DataDir"
Write-Host '  完整文档：README.md / 部署指南.md'
