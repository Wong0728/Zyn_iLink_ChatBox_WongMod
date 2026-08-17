# ============================================================================
#  iLink-WM1 Windows 启动脚本（PowerShell 版）
#
#  运行方式：
#    - 命令行：powershell -ExecutionPolicy Bypass -File start.ps1
#    - 资源管理器：右键本文件 →「使用 PowerShell 运行」
#    - 或直接使用全局命令：iLinkWM
#
#  功能：
#    1. 启动 ilink-wm1.exe
#    2. 等待端口就绪后自动打开浏览器访问 http://localhost:8888
#    3. 控制台显示实时日志，Ctrl+C 或关闭窗口即停止服务
#
#  数据位置：默认在脚本所在目录的 data\ 子目录，可用环境变量
#  ILINK_DATA_DIR 修改。
# ============================================================================
$ErrorActionPreference = 'Stop'

$root       = $PSScriptRoot
$binPath    = Join-Path $root 'ilink-wm1.exe'
$webDir     = Join-Path $root 'web'
$defaultPort = 8888
# 默认仅监听本机（与 README §1.4「最安全」一致）。如需公网访问请：
#   1. 显式设 $env:ILINK_HOST='0.0.0.0'（或 LAN IP）
#   2. 配 HTTPS 反代 + ILINK_TRUSTED_PROXIES + ILINK_FORCE_HTTPS=1
#   3. 或在内网测试时在下方安全确认输入 YES
if (-not $env:ILINK_HOST)     { $env:ILINK_HOST = '127.0.0.1' }
if (-not $env:ILINK_PORT)     { $env:ILINK_PORT = $defaultPort }
if (-not $env:ILINK_DATA_DIR) { $env:ILINK_DATA_DIR = Join-Path $root 'data' }
if (-not $env:RUST_LOG)       { $env:RUST_LOG = 'ilink_wm1=info' }
$env:RUST_BACKTRACE = 'full'

Write-Host '========================================'
Write-Host '  iLink-WM1 启动中...'
Write-Host '========================================'
Write-Host ''
Write-Host "  二进制：    $binPath"
Write-Host "  前端目录：  $webDir"
Write-Host "  监听地址：  $($env:ILINK_HOST):$($env:ILINK_PORT)"
Write-Host "  数据目录：  $($env:ILINK_DATA_DIR)"
Write-Host "  访问地址：  http://localhost:$($env:ILINK_PORT)"
Write-Host ''

# ── 检查二进制 ──────────────────────────────
if (-not (Test-Path $binPath)) {
    Write-Host '[错误] 找不到 ilink-wm1.exe' -ForegroundColor Red
    Write-Host "  期望路径: $binPath"
    Write-Host '  请确认 ZIP 包完整解压。'
    Read-Host '按 Enter 关闭'
    exit 1
}

# ── 检查前端目录 ────────────────────────────
if (-not (Test-Path $webDir)) {
    Write-Host '[错误] 找不到前端目录 web\' -ForegroundColor Red
    Write-Host "  期望路径: $webDir"
    Write-Host '  请确认 ZIP 包完整解压。'
    Read-Host '按 Enter 关闭'
    exit 1
}

# ── 创建数据目录 ────────────────────────────
if (-not (Test-Path $env:ILINK_DATA_DIR)) {
    New-Item -ItemType Directory -Path $env:ILINK_DATA_DIR -Force | Out-Null
    Write-Host "[信息] 已创建数据目录: $($env:ILINK_DATA_DIR)"
}

# ── 公网监听安全确认 ─────────────────────────
if ($env:ILINK_HOST -eq '0.0.0.0' -and
    $env:ILINK_ALLOW_INSECURE_PUBLIC -ne '1' -and
    -not $env:ILINK_TRUSTED_PROXIES) {
    Write-Host '[安全确认] 当前将监听全部 IPv4 网卡，但尚未配置 HTTPS 反向代理。' -ForegroundColor Yellow
    Write-Host '  只有在受信任内网中使用时，才可继续明文 HTTP。'
    $lanConfirm = Read-Host '确认这是受信任内网并继续？请输入 YES'
    if ($lanConfirm -ne 'YES') {
        Write-Host '[已取消] 请先配置 HTTPS 反向代理，并设置 ILINK_TRUSTED_PROXIES 与 ILINK_FORCE_HTTPS=1。'
        Read-Host '按 Enter 关闭'
        exit 1
    }
    $env:ILINK_ALLOW_INSECURE_PUBLIC = '1'
}

Write-Host '[信息] 正在启动服务...'
Write-Host '[信息] 关闭此窗口或按 Ctrl+C 可停止服务。'
Write-Host ''

# ── 3 秒后打开浏览器（分离进程，不阻塞）─────
$url = "http://localhost:$($env:ILINK_PORT)"
Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile','-Command',"Start-Sleep -Seconds 3; Start-Process '$url'"

# ── 前台运行服务（控制台可见日志）────────────
Set-Location $root
& $binPath
$code = $LASTEXITCODE

Write-Host ''
Write-Host '[信息] 服务已停止。'
Read-Host '按 Enter 关闭'
exit $code
