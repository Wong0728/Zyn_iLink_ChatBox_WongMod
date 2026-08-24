<#!
.SYNOPSIS
  由 iLinkWM-Hosted 计划任务调用的本机进程守护。
#>
[CmdletBinding()]
param(
    [ValidateSet('start', 'stop', 'status', 'watchdog')]
    [string]$Action = 'status',
    [Parameter(Mandatory = $true)]
    [string]$ConfigPath
)

$ErrorActionPreference = 'Stop'
$config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
$secrets = Join-Path $config.install_root 'runtime\secrets.ps1'
if (Test-Path -LiteralPath $secrets) { . $secrets }

function Write-Log([string]$Message) {
    $file = Join-Path $config.logs_dir 'hosted-manager.log'
    "[$(Get-Date -Format o)] $Message" | Out-File -LiteralPath $file -Append -Encoding utf8
}
function Get-OwnedProcess([string]$Exe) {
    $full = [IO.Path]::GetFullPath($Exe)
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
        $_.ExecutablePath -and [IO.Path]::GetFullPath($_.ExecutablePath) -eq $full
    }
}
function Start-Owned([string]$Exe, [string[]]$Args, [string]$Name) {
    if (Get-OwnedProcess $Exe) { return }
    $stdout = Join-Path $config.logs_dir ("$Name.log")
    $stderr = Join-Path $config.logs_dir ("$Name.err.log")
    Start-Process -FilePath $Exe -ArgumentList $Args -WorkingDirectory (Split-Path -Parent $Exe) -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden | Out-Null
    Write-Log "started $Name"
}
function Stop-Owned([string]$Exe) {
    Get-OwnedProcess $Exe | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}
function Get-PublicIpv6 {
    Get-NetIPAddress -AddressFamily IPv6 -ErrorAction SilentlyContinue |
        Where-Object {
            $_.AddressState -eq 'Preferred' -and $_.SuffixOrigin -ne 'Random' -and
            $_.IPAddress -notmatch '^(::1|fe[89ab][0-9a-f]?|fc|fd)'
        } | Select-Object -First 1 -ExpandProperty IPAddress
}
function Update-DirectDns {
    if (-not $config.cloudflare_zone_id -or -not $config.direct_aaaa_record_id -or -not $env:CLOUDFLARE_DNS_API_TOKEN) { return }
    $ipv6 = Get-PublicIpv6
    if (-not $ipv6) { Write-Log 'no stable public IPv6 found; DNS unchanged'; return }
    $uri = "https://api.cloudflare.com/client/v4/zones/$($config.cloudflare_zone_id)/dns_records/$($config.direct_aaaa_record_id)"
    $headers = @{ Authorization = "Bearer $env:CLOUDFLARE_DNS_API_TOKEN"; 'Content-Type' = 'application/json' }
    $body = @{ type = 'AAAA'; name = $config.direct_domain; content = $ipv6; ttl = 1; proxied = $false } | ConvertTo-Json -Compress
    try {
        $result = Invoke-RestMethod -Method Put -Uri $uri -Headers $headers -Body $body -TimeoutSec 15
        if (-not $result.success) { throw 'Cloudflare API returned success=false' }
        Write-Log "updated direct AAAA to $ipv6"
    } catch { Write-Log "direct AAAA update failed: $($_.Exception.Message)" }
}
function Start-Stack {
    $env:ILINK_HOST = '127.0.0.1'
    $env:ILINK_PORT = '8888'
    $env:ILINK_DATA_DIR = $config.data_dir
    $env:ILINK_TRUSTED_PROXIES = '127.0.0.1,::1'
    $env:ILINK_ALLOWED_ORIGINS = "https://$($config.cf_domain),https://$($config.direct_domain)"
    Remove-Item Env:ILINK_ALLOW_INSECURE_PUBLIC -ErrorAction SilentlyContinue
    Start-Owned $config.ilink_exe @() 'ilink-wm1'
    Start-Owned $config.caddy_exe @('run', '--config', $config.caddyfile, '--adapter', 'caddyfile') 'caddy'
    Start-Owned $config.cloudflared_exe @('--config', $config.cloudflared_config, 'tunnel', 'run') 'cloudflared'
}
function Show-Status {
    foreach ($pair in @(@('ilink-wm1', $config.ilink_exe), @('caddy', $config.caddy_exe), @('cloudflared', $config.cloudflared_exe))) {
        $p = Get-OwnedProcess $pair[1]
        Write-Host (if ($p) { "[OK] $($pair[0]) PID=$($p[0].ProcessId)" } else { "[--] $($pair[0]) 未运行" })
    }
}

switch ($Action) {
    'start' { Start-Stack; Show-Status }
    'stop' { Stop-Owned $config.cloudflared_exe; Stop-Owned $config.caddy_exe; Stop-Owned $config.ilink_exe; Show-Status }
    'status' { Show-Status }
    'watchdog' {
        $tick = 19
        while ($true) {
            try { Start-Stack } catch { Write-Log "watchdog error: $($_.Exception.Message)" }
            $tick++
            if ($tick -ge 20) { $tick = 0; Update-DirectDns }
            Start-Sleep -Seconds 15
        }
    }
}
