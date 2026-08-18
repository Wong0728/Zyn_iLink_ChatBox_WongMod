# ============================================================================
#  iLink-WM1 Windows 服务安装脚本（PowerShell 版，NSSM）
#
#  运行方式（推荐）：iLinkWM install-service（自动请求管理员提权）
#  直接运行：右键 →「使用 PowerShell 运行」，或管理员 PowerShell 中执行
#    powershell -ExecutionPolicy Bypass -File install-service.ps1
#
#  功能：
#    1. 检查 NSSM，缺失时下载固定版本并做 SHA-256 校验到 bin\nssm.exe
#    2. 注册 Windows 服务 ilink-wm1（开机自启 + 崩溃自动重启）
#    3. 启动服务并打开浏览器访问 http://localhost:8888
#
#  卸载：iLinkWM uninstall-service
# ============================================================================
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# ── 管理员检查（不足则自我提权重启）──────────
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host '[iLinkWM] 此脚本需要管理员权限，正在请求提权...' -ForegroundColor Yellow
    Start-Process -Verb RunAs -FilePath 'powershell.exe' -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File',"`"$PSCommandPath`""
    exit 0
}

$root        = $PSScriptRoot
$binDir      = Join-Path $root 'bin'
$nssmExe     = Join-Path $binDir 'nssm.exe'
$serviceName = 'ilink-wm1'
$appExe      = Join-Path $root 'ilink-wm1.exe'
$logDir      = Join-Path $root 'logs'
$defaultPort = 8888
$nssmUrl       = 'https://nssm.cc/release/nssm-2.24.zip'
$nssmZipSha256 = '727D1E42275C605E0F04ABA98095C38A8E1E46DEF453CDFFCE42869428AA6743'
$nssmExeSha256 = 'F689EE9AF94B00E9E3F0BB072B34CAAF207F32DCB4F5782FC9CA351DF9A06C97'
$nssmZip     = Join-Path $env:TEMP 'nssm-2.24.zip'
$nssmExtract = Join-Path $env:TEMP 'nssm-extract'

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

Write-Host '========================================'
Write-Host '  iLink-WM1 Windows 服务安装'
Write-Host '========================================'
Write-Host ''

# ── 检查二进制 ──────────────────────────────
if (-not (Test-Path $appExe)) {
    Write-Host '[错误] 找不到 ilink-wm1.exe' -ForegroundColor Red
    Write-Host "  期望路径: $appExe"
    Read-Host '按 Enter 关闭'
    exit 1
}

# ── 检查 / 下载 NSSM ────────────────────────
if (Test-Path $nssmExe) {
    if ((Get-Sha256 $nssmExe) -ne $nssmExeSha256) {
        Write-Host '[警告] 现有 NSSM 未通过固定版本校验，将重新下载可信副本。' -ForegroundColor Yellow
        Remove-Item $nssmExe -Force -ErrorAction SilentlyContinue
    }
}
if (-not (Test-Path $nssmExe)) {
    Write-Host '[信息] NSSM 未安装，开始下载...'
    New-Item -ItemType Directory -Path $binDir -Force | Out-Null

    Write-Host "[信息] 下载: $nssmUrl"
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $nssmUrl -OutFile $nssmZip -UseBasicParsing
    } catch {
        Write-Host "[错误] NSSM 下载失败：$($_.Exception.Message)" -ForegroundColor Red
        Read-Host '按 Enter 关闭'
        exit 1
    }

    Write-Host '[信息] 校验 NSSM SHA-256...'
    $actual = Get-Sha256 $nssmZip
    if ($actual -ne $nssmZipSha256) {
        Write-Host "[错误] NSSM ZIP 完整性校验失败（$actual），已停止安装。" -ForegroundColor Red
        Remove-Item $nssmZip -Force -ErrorAction SilentlyContinue
        Read-Host '按 Enter 关闭'
        exit 1
    }

    Write-Host '[信息] 解压 NSSM...'
    if (Test-Path $nssmExtract) { Remove-Item $nssmExtract -Recurse -Force }
    Expand-Archive -Path $nssmZip -DestinationPath $nssmExtract -Force

    # 仅接受已校验 ZIP 中固定布局的 64 位版本，不做结构不明的兜底。
    $nssmSrc = Join-Path $nssmExtract 'nssm-2.24\win64\nssm.exe'
    if (-not (Test-Path $nssmSrc)) {
        Write-Host '[错误] NSSM ZIP 结构不符合固定版本预期。' -ForegroundColor Red
        Read-Host '按 Enter 关闭'
        exit 1
    }
    Copy-Item $nssmSrc $nssmExe -Force

    if ((Get-Sha256 $nssmExe) -ne $nssmExeSha256) {
        Write-Host '[错误] 解压后的 nssm.exe 完整性校验失败。' -ForegroundColor Red
        Remove-Item $nssmExe -Force -ErrorAction SilentlyContinue
        Read-Host '按 Enter 关闭'
        exit 1
    }
    Write-Host "[OK] NSSM 已就绪: $nssmExe"
} else {
    Write-Host "[OK] NSSM 已存在: $nssmExe"
}

# ── 创建日志 / 数据目录 ──────────────────────
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $root 'data') -Force | Out-Null

# ── 检测可选运行组件 ─────────────────────────
if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    Write-Host '[警告] 未找到 ffmpeg；语音转换不可用。请从可信来源安装并加入 PATH。' -ForegroundColor Yellow
}
if (-not (Get-Command ssh -ErrorAction SilentlyContinue)) {
    Write-Host '[警告] 未找到 ssh；Serveo 隧道不可用。请安装 Windows OpenSSH Client。' -ForegroundColor Yellow
}

# ── 服务注册前完成 owner 初始化 ──────────────
$env:ILINK_DATA_DIR = Join-Path $root 'data'
Write-Host '[信息] 现在创建或确认 owner 管理员账号...'
Set-Location $root
& $appExe admin init
if ($LASTEXITCODE -ne 0) {
    Write-Host '[错误] owner 初始化失败，未注册服务。' -ForegroundColor Red
    Read-Host '按 Enter 关闭'
    exit 1
}

# ── 选择明确的网络安全模式 ───────────────────
Write-Host ''
Write-Host '请选择部署模式：'
Write-Host '  1. 已有 HTTPS 反向代理（推荐；默认代理地址 127.0.0.1）'
Write-Host '  2. 仅受信任内网明文 HTTP（不会自动获得 TLS）'
$securityMode = Read-Host '输入 1 或 2 [默认 1]'
if (-not $securityMode) { $securityMode = '1' }
if ($securityMode -eq '1') {
    $trustedProxy = Read-Host '可信代理 IP/CIDR [127.0.0.1]'
    if (-not $trustedProxy) { $trustedProxy = '127.0.0.1' }
} elseif ($securityMode -eq '2') {
    $insecureConfirm = Read-Host '确认端口只暴露在受信任内网？请输入 YES'
    if ($insecureConfirm -ne 'YES') {
        Write-Host '[已取消] 未确认明文内网部署。'
        Read-Host '按 Enter 关闭'
        exit 1
    }
} else {
    Write-Host '[错误] 无效选项。' -ForegroundColor Red
    Read-Host '按 Enter 关闭'
    exit 1
}

# ── 若已安装则先停止删除 ─────────────────────
$existing = & sc.exe query $serviceName 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host '[信息] 服务已存在，先停止并删除旧配置...'
    & $nssmExe stop $serviceName 2>$null
    & $nssmExe remove $serviceName confirm 2>$null
    Start-Sleep -Seconds 2
}

# ── 注册服务 ─────────────────────────────────
Write-Host "[信息] 注册服务 $serviceName ..."
& $nssmExe install $serviceName $appExe
& $nssmExe set $serviceName AppDirectory $root

# 审计 M-11：服务改用低权虚拟账户 NT SERVICE\ilink-wm1 运行（不再以
# LocalSystem 运行）；Web 应用被攻破时不再直接获得 SYSTEM 权限。
& $nssmExe set $serviceName ObjectName "NT SERVICE\$serviceName" ''

# 审计 M-11：仅授予该虚拟账户对安装目录的修改权限（数据/日志/主密钥文件
# 需要写）；/T 把授权应用到已存在的子目录与文件（升级重装场景）。
& icacls $root /grant "NT SERVICE\${serviceName}:(OI)(CI)M" /T | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Host "[警告] icacls 授权失败（exit=$LASTEXITCODE），服务可能无法写数据/日志目录。" -ForegroundColor Yellow
    Write-Host "  请手动执行: icacls `"$root`" /grant `"NT SERVICE\${serviceName}:(OI)(CI)M`" /T"
}

$dataDir = Join-Path $root 'data'
if ($securityMode -eq '1') {
    & $nssmExe set $serviceName AppEnvironmentExtra `
        "ILINK_HOST=127.0.0.1" "ILINK_PORT=$defaultPort" "ILINK_DATA_DIR=$dataDir" `
        "ILINK_SERVER_MODE=1" "ILINK_TRUSTED_PROXIES=$trustedProxy" "ILINK_FORCE_HTTPS=1" `
        "RUST_LOG=ilink_wm1=info" "RUST_BACKTRACE=full"
} else {
    & $nssmExe set $serviceName AppEnvironmentExtra `
        "ILINK_HOST=0.0.0.0" "ILINK_PORT=$defaultPort" "ILINK_DATA_DIR=$dataDir" `
        "ILINK_SERVER_MODE=1" "ILINK_ALLOW_INSECURE_PUBLIC=1" `
        "RUST_LOG=ilink_wm1=info" "RUST_BACKTRACE=full"
}

# 日志重定向与轮转
& $nssmExe set $serviceName AppStdout (Join-Path $logDir 'service.log')
& $nssmExe set $serviceName AppStderr (Join-Path $logDir 'service.log')
& $nssmExe set $serviceName AppRotateFiles 1
& $nssmExe set $serviceName AppRotateBytes 10485760

# 开机自启 + 崩溃自动重启
& $nssmExe set $serviceName Start SERVICE_AUTO_START
& $nssmExe set $serviceName AppExit Default Restart
& $nssmExe set $serviceName AppRestartDelay 5000

Write-Host '[OK] 服务配置完成'

# ── 启动服务 ─────────────────────────────────
Write-Host '[信息] 启动服务...'
& $nssmExe start $serviceName
Start-Sleep -Seconds 3

$svc = Get-Service $serviceName -ErrorAction SilentlyContinue
if ($svc -and $svc.Status -eq 'Running') {
    Write-Host '[OK] 服务已启动' -ForegroundColor Green
} else {
    Write-Host "[错误] 服务启动失败，请查看日志: $logDir\service.log" -ForegroundColor Red
    Read-Host '按 Enter 关闭'
    exit 1
}

# ── 完成 ─────────────────────────────────────
Write-Host ''
Write-Host '========================================'
Write-Host '  iLink-WM1 服务安装完成！'
Write-Host '========================================'
Write-Host ''
Write-Host "  服务名称：  $serviceName"
Write-Host "  运行账户：  NT SERVICE\$serviceName（低权虚拟账户）"
Write-Host "  二进制：    $appExe"
$listenHost = if ($securityMode -eq '1') { '127.0.0.1' } else { '0.0.0.0' }
Write-Host "  监听地址：  ${listenHost}:$defaultPort"
Write-Host "  数据目录：  $dataDir"
Write-Host "  日志文件：  $logDir\service.log"
Write-Host ''
Write-Host "  访问地址：  http://localhost:$defaultPort"
Write-Host ''
Write-Host '服务管理命令（PowerShell）：'
Write-Host "  查看状态：  Get-Service $serviceName"
Write-Host "  启动：      Start-Service $serviceName"
Write-Host "  停止：      Stop-Service $serviceName"
Write-Host "  重启：      Restart-Service $serviceName"
Write-Host "  卸载：      iLinkWM uninstall-service"
Write-Host ''
Write-Host '实时查看日志：'
Write-Host "  Get-Content `"$logDir\service.log`" -Tail 100 -Wait"
Write-Host ''

# ── 打开浏览器 ───────────────────────────────
Write-Host '[信息] 3 秒后自动打开浏览器...'
Start-Sleep -Seconds 3
Start-Process "http://localhost:$defaultPort"

Read-Host '按 Enter 关闭'
