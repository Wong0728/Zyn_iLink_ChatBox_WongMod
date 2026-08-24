<#!
.SYNOPSIS
  初始化 iLinkWM 家用双入口 HTTPS 部署。

.DESCRIPTION
  同时支持交互输入与参数/配置文件调用。它只写入受保护的本机运行配置、
  Caddyfile、cloudflared 配置并注册看门狗；Cloudflare 登录、Worker/KV 创建
  仍需要操作者在浏览器完成一次授权，脚本不会收集或上传任何凭证。

.EXAMPLE
  .\一键部署.ps1

.EXAMPLE
  .\一键部署.ps1 -NonInteractive -ConfigFile .\home-config.json
#>
[CmdletBinding()]
param(
    [string]$ConfigFile,
    [switch]$NonInteractive,
    [switch]$SkipTaskRegistration
)

$ErrorActionPreference = 'Stop'

function Read-Required([string]$Name, [string]$CurrentValue = '') {
    if ($CurrentValue) { return $CurrentValue }
    do { $value = (Read-Host $Name).Trim() } while (-not $value)
    return $value
}

function Assert-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw '请使用“以管理员身份运行”的 PowerShell 执行本脚本。'
    }
}

function Assert-File([string]$Path, [string]$Label) {
    if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label 不存在：$Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

function Write-Utf8([string]$Path, [string]$Content) {
    $dir = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    [IO.File]::WriteAllText($Path, $Content, (New-Object System.Text.UTF8Encoding($false)))
}
function ConvertTo-PlainText([Security.SecureString]$Value) {
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Value)
    try { return [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr) }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr) }
}

Assert-Admin

$settings = @{}
if ($ConfigFile) {
    if (-not (Test-Path -LiteralPath $ConfigFile)) { throw "找不到配置文件：$ConfigFile" }
    $loadedSettings = Get-Content -LiteralPath $ConfigFile -Raw | ConvertFrom-Json
    foreach ($property in $loadedSettings.PSObject.Properties) {
        $settings[$property.Name] = [string]$property.Value
    }
}
if ($NonInteractive -and -not $ConfigFile) { throw '-NonInteractive 必须同时提供 -ConfigFile。' }

function Get-Setting([string]$Name, [string]$Prompt, [string]$Default = '') {
    $value = if ($settings.ContainsKey($Name)) { [string]$settings[$Name] } else { $Default }
    if ($NonInteractive) {
        if (-not $value) { throw "配置文件缺少必填项：$Name" }
        return $value
    }
    return Read-Required $Prompt $value
}

$installRoot = Get-Setting 'install_root' '安装目录（建议 C:\ProgramData\iLinkWM\hosted）' 'C:\ProgramData\iLinkWM\hosted'
$cfDomain = Get-Setting 'cf_domain' 'Cloudflare 入口域名（例如 chat.example.com）'
$directDomain = Get-Setting 'direct_domain' 'IPv6 直连域名（DNS-only AAAA，例如 direct.example.com）'
$originDomain = Get-Setting 'origin_domain' 'Tunnel 回源域名（无 Worker Route，例如 origin.example.com）'
$email = Get-Setting 'email' '证书通知邮箱'
$ilinkExe = Assert-File (Get-Setting 'ilink_exe' 'ilink-wm1.exe 完整路径') 'ilink-wm1.exe'
$caddyExe = Assert-File (Get-Setting 'caddy_exe' 'caddy.exe 完整路径（DNS-01 模式须含 cloudflare 插件）') 'caddy.exe'
$cloudflaredExe = Assert-File (Get-Setting 'cloudflared_exe' 'cloudflared.exe 完整路径') 'cloudflared.exe'
$tunnelId = Get-Setting 'tunnel_id' 'Cloudflare Tunnel UUID'
$tunnelCredentials = Assert-File (Get-Setting 'tunnel_credentials' 'Tunnel credentials JSON 完整路径') 'Tunnel credentials JSON'
$certificateMode = Get-Setting 'certificate_mode' '证书模式：dns 或 http' 'dns'
if ($certificateMode -notin @('dns', 'http')) { throw 'certificate_mode 只能为 dns 或 http。' }
if ($certificateMode -eq 'dns') {
    $modules = & $caddyExe list-modules 2>$null
    if ($LASTEXITCODE -ne 0 -or $modules -notmatch 'dns\.providers\.cloudflare') {
        throw 'DNS-01 模式需要带 caddy-dns/cloudflare 模块的 Caddy；请下载包含该模块的自定义 Caddy 后重试。'
    }
}
if ($certificateMode -eq 'dns') {
    $modules = & $caddyExe list-modules 2>$null
    if ($LASTEXITCODE -ne 0 -or $modules -notmatch 'dns\.providers\.cloudflare') {
        throw 'DNS-01 模式需要带 caddy-dns/cloudflare 模块的 Caddy；请下载包含该模块的自定义 Caddy 后重试。'
    }
}

$dnsToken = if ($settings.ContainsKey('cloudflare_dns_api_token')) { [string]$settings['cloudflare_dns_api_token'] } else { '' }
$zoneId = if ($settings.ContainsKey('cloudflare_zone_id')) { [string]$settings['cloudflare_zone_id'] } else { '' }
$recordId = if ($settings.ContainsKey('direct_aaaa_record_id')) { [string]$settings['direct_aaaa_record_id'] } else { '' }
$originToken = if ($settings.ContainsKey('origin_token')) { [string]$settings['origin_token'] } else { '' }
if (-not $NonInteractive) {
    if (-not $dnsToken) { $dnsToken = ConvertTo-PlainText (Read-Host 'Cloudflare DNS API Token（仅 Zone DNS Edit）' -AsSecureString) }
    if (-not $zoneId) { $zoneId = Read-Required 'Cloudflare Zone ID' }
    if (-not $recordId) { $recordId = Read-Required 'direct 域名 AAAA Record ID' }
    if (-not $originToken) { $originToken = ConvertTo-PlainText (Read-Host 'Worker 回源令牌（随机长字符串）' -AsSecureString) }
}
if (-not $dnsToken -or -not $zoneId -or -not $recordId -or $originToken.Length -lt 32) {
    throw '需要 cloudflare_dns_api_token、cloudflare_zone_id、direct_aaaa_record_id，以及至少 32 字符的 origin_token。'
}

$installRoot = [IO.Path]::GetFullPath($installRoot)
$runtime = Join-Path $installRoot 'runtime'
$logs = Join-Path $installRoot 'logs'
$data = Join-Path $installRoot 'data'
$caddyfile = Join-Path $runtime 'Caddyfile'
$cloudflaredConfig = Join-Path $runtime 'cloudflared.yml'
$configPath = Join-Path $runtime 'hosted-config.json'
$secretsPath = Join-Path $runtime 'secrets.ps1'
$managedManager = Join-Path $runtime 'stack-manager.ps1'
$managedCredentials = Join-Path $runtime 'tunnel-credentials.json'
New-Item -ItemType Directory -Force -Path $runtime, $logs, $data | Out-Null
Copy-Item -LiteralPath $tunnelCredentials -Destination $managedCredentials -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'stack-manager.ps1') -Destination $managedManager -Force

$tlsBlock = if ($certificateMode -eq 'dns') {
@"
    tls {
        dns cloudflare {env.CLOUDFLARE_DNS_API_TOKEN}
    }
"@
} else { '' }

Write-Utf8 $caddyfile @"
{
    email $email
}

$directDomain {
$tlsBlock    reverse_proxy 127.0.0.1:8888
}

# cloudflared 只连本机；显式 https 头让应用正确设置 Secure Cookie。
http://127.0.0.1:8080 {
    @worker header X-ILink-Origin-Token $originToken
    handle @worker {
      reverse_proxy 127.0.0.1:8888 {
        header_up X-Forwarded-Proto https
      }
    }
    respond "forbidden" 403
}
"@

Write-Utf8 $cloudflaredConfig @"
tunnel: $tunnelId
credentials-file: $managedCredentials
ingress:
  - hostname: $originDomain
    service: http://127.0.0.1:8080
  - service: http_status:404
"@

$config = [ordered]@{
    install_root = $installRoot
    ilink_exe = $ilinkExe
    caddy_exe = $caddyExe
    cloudflared_exe = $cloudflaredExe
    caddyfile = $caddyfile
    cloudflared_config = $cloudflaredConfig
    data_dir = $data
    logs_dir = $logs
    cf_domain = $cfDomain
    direct_domain = $directDomain
    origin_domain = $originDomain
    origin_token = $originToken
    cloudflare_zone_id = $zoneId
    direct_aaaa_record_id = $recordId
}
Write-Utf8 $configPath ($config | ConvertTo-Json -Depth 4)

# Token 同时用于 DNS-01（如选择）和 IPv6 AAAA 自动更新；其 ACL 在下方立即收紧。
$escapedDnsToken = $dnsToken.Replace("'", "''")
Write-Utf8 $secretsPath "`$env:CLOUDFLARE_DNS_API_TOKEN = '$escapedDnsToken'`r`n"

# 配置、Token、Tunnel 凭证均由高权限任务使用；禁止普通用户写入。
& icacls.exe $installRoot /inheritance:r /grant:r 'SYSTEM:(OI)(CI)F' 'Administrators:(OI)(CI)F' | Out-Null

if (-not $SkipTaskRegistration) {
    $manager = $managedManager
    Assert-File $manager 'stack-manager.ps1' | Out-Null
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -File "{0}" -Action watchdog -ConfigPath "{1}"' -f $manager, $configPath)
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName 'iLinkWM-Hosted' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
}

Write-Host "[OK] 本机 Hosted 配置已写入：$installRoot" -ForegroundColor Green
Write-Host "[下一步] 将 $originDomain 路由到 Tunnel；Worker 的 TUNNEL_ORIGIN 设置为 https://$originDomain。"
Write-Host "[验证] 从 IPv6 网络访问 https://$directDomain/healthz；CF 入口访问 https://$cfDomain/。"
