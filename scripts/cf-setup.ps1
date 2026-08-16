# ============================================================================
# cf-setup.ps1 — Cloudflare IPv6 直连 / 302 跳转 一键配置（Windows）
#
# 配套说明：同目录 CF-SETUP.md
#
# 用法：
#   交互式配置： powershell -ExecutionPolicy Bypass -File cf-setup.ps1
#   DDNS 刷新：  powershell -ExecutionPolicy Bypass -File cf-setup.ps1 -Ddns
#                （供计划任务调用，按已保存的 cf-config.json 刷新 DNS/跳转规则）
#
# 可选参数（配合 -Yes 可全自动）：
#   -Mode direct|redirect   direct=DNS AAAA 直连（推荐，数据不经 CF）
#                           redirect=CF 302 跳转到 http(s)://[IPv6]:端口
#   -Token <API Token>      或环境变量 CF_API_TOKEN
#   -Zone <example.com>     跳过交互选择域名
#   -Label <子域前缀>        默认 ilink，最终 FQDN = Label.Zone
#   -Port <端口>             默认 8888（与 ilink-wm Web 端口一致）
#   -Scheme http|https      redirect 目标协议，默认 http
#   -Ip <IPv6>              手动指定地址（跳过自动检测）
#   -Yes                    全部使用默认值，不再提问
# ============================================================================

param(
    [string]$Mode = "",
    [string]$Token = "",
    [string]$Zone = "",
    [string]$Label = "",
    [int]$Port = 0,
    [string]$Scheme = "",
    [string]$Ip = "",
    [switch]$Ddns,
    [switch]$Yes
)

$ErrorActionPreference = "Stop"
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager] -bor [Net.SecurityProtocolType]::Tls12
} catch {}

$Api = "https://api.cloudflare.com/client/v4"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ConfigPath = Join-Path $ScriptDir "cf-config.json"
$RuleMarker = "ilink-cf-ddns"   # 跳转规则标记，DDNS 时按此识别并保留其他规则

function Write-Step([string]$msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Write-Ok([string]$msg)   { Write-Host "  [OK] $msg" -ForegroundColor Green }
function Write-Info([string]$msg) { Write-Host "  ..  $msg" }
function Write-Warn2([string]$msg){ Write-Host "  [!]  $msg" -ForegroundColor Yellow }
function Write-Fail([string]$msg) { Write-Host "  [X]  $msg" -ForegroundColor Red }

function Cf-Request([string]$Method, [string]$Path, $Body, [string]$Tok) {
    $h = @{ Authorization = "Bearer $Tok" }
    try {
        if ($null -ne $Body) {
            $json = ConvertTo-Json -Depth 12 -InputObject $Body
            return Invoke-RestMethod -Method $Method -Uri "$Api$Path" -Headers $h -Body $json -ContentType "application/json"
        } else {
            return Invoke-RestMethod -Method $Method -Uri "$Api$Path" -Headers $h -ContentType "application/json"
        }
    } catch {
        $detail = ""
        if ($_.ErrorDetails -and $_.ErrorDetails.Message) { $detail = $_.ErrorDetails.Message }
        throw "Cloudflare API $Method $Path 失败。$detail"
    }
}

function Read-Default([string]$prompt, [string]$default) {
    if ($Yes) { return $default }
    $v = Read-Host "$prompt [$default]"
    if ([string]::IsNullOrWhiteSpace($v)) { $default } else { $v }
}

# 枚举本机公网 IPv6（排除 loopback / 链路本地 fe80::/10 / ULA fc00::/7 / 已弃用地址，
# 优先非临时地址——Windows 隐私扩展的临时地址(SuffixOrigin=Random)会周期变化）
function Get-PublicIPv6 {
    $cands = @()
    try {
        $cands = @(Get-NetIPAddress -AddressFamily IPv6 -ErrorAction Stop | Where-Object {
            $_.IPAddress -ne "::1" -and
            $_.IPAddress -notlike "fe80*" -and
            $_.IPAddress -notlike "fc*" -and $_.IPAddress -notlike "fd*" -and
            ($_.AddressState -ne "Deprecated")
        } | Sort-Object @{ Expression = { if ($_.SuffixOrigin -eq "Random") { 1 } else { 0 } } } |
           Select-Object -ExpandProperty IPAddress -Unique)
    } catch {}
    return $cands
}

function Select-Ip6([string[]]$Cands) {
    if ($Cands.Count -eq 0) { return "" }
    if ($Cands.Count -eq 1 -or $Yes) { return $Cands[0] }
    Write-Host "  检测到多个公网 IPv6 地址："
    for ($i = 0; $i -lt $Cands.Count; $i++) {
        Write-Host ("  {0}) {1}" -f ($i + 1), $Cands[$i])
    }
    while ($true) {
        $v = Read-Host "选择地址 (1-$($Cands.Count)，默认 1)"
        if ([string]::IsNullOrWhiteSpace($v)) { return $Cands[0] }
        $n = 0
        if ([int]::TryParse($v, [ref]$n) -and $n -ge 1 -and $n -le $Cands.Count) { return $Cands[$n - 1] }
        Write-Host "  无效输入" -ForegroundColor Red
    }
}

# AAAA 记录 upsert。TTL 免费套餐最低 60s；被拒则回退 1（auto）。
function Upsert-Aaaa([string]$Tok, [string]$ZoneId, [string]$Fqdn, [string]$Ip6, [bool]$Proxied) {
    $enc = [Uri]::EscapeDataString($Fqdn)
    $existing = Cf-Request "GET" "/zones/$ZoneId/dns_records?name=$enc&type=AAAA" $null $Tok
    $body = @{
        type = "AAAA"; name = $Fqdn; content = $Ip6
        ttl  = 60; proxied = $Proxied; comment = "ilink-wm cf-setup"
    }
    $recId = ""
    $isUpdate = ($existing.result.Count -gt 0)
    if ($isUpdate) {
        $rec = $existing.result[0]
        if ($rec.content -eq $Ip6 -and ([bool]$rec.proxied -eq $Proxied)) {
            Write-Ok "AAAA 记录已是最新：$Fqdn -> $Ip6"
            return [string]$rec.id
        }
        $recId = [string]$rec.id
    }
    $method = "POST"; $path = "/zones/$ZoneId/dns_records"
    if ($isUpdate) { $method = "PATCH"; $path = "/zones/$ZoneId/dns_records/$recId" }
    try {
        $r = Cf-Request $method $path $body $Tok
        $recId = [string]$r.result.id
    } catch {
        # 某些套餐不允许 TTL=60，回退 auto 后重试一次
        $body.ttl = 1
        $r = Cf-Request $method $path $body $Tok
        $recId = [string]$r.result.id
    }
    if ($isUpdate) { Write-Ok "AAAA 记录已更新：$Fqdn -> $Ip6 (proxied=$Proxied)" }
    else { Write-Ok "AAAA 记录已创建：$Fqdn -> $Ip6 (proxied=$Proxied)" }
    return $recId
}

# 302 跳转规则 upsert（zone 级 http_request_dynamic_redirect 阶段入口规则集）。
# 只动带 $RuleMarker 标记的规则，用户已有的其他跳转规则原样保留。
# Target 形如 http://[2408:8207::1]:8888；规则用表达式字符串字面量绕开
# 静态 URL 校验对 IPv6 字面量的兼容问题。
function Upsert-RedirectRule([string]$Tok, [string]$ZoneId, [string]$Fqdn, [string]$Target) {
    $targetExpr = '"' + $Target + '"'
    $ours = @{
        expression = '(http.host eq "' + $Fqdn + '")'
        action = "redirect"
        action_parameters = @{
            from_value = @{
                status_code = 302
                target_url  = @{ expression = $targetExpr }
                preserve_query_string = $true
            }
        }
        description = $RuleMarker
        enabled = $true
    }
    $rs = $null
    try {
        $rs = (Cf-Request "GET" "/zones/$ZoneId/rulesets/phases/http_request_dynamic_redirect/entrypoint" $null $Tok).result
    } catch { $rs = $null }
    if ($null -eq $rs -or -not $rs.id) {
        $rs = (Cf-Request "POST" "/zones/$ZoneId/rulesets" @{
            name = "ilink redirect"; kind = "zone"; phase = "http_request_dynamic_redirect"
        } $Tok).result
    }
    $others = @()
    if ($rs.rules) { $others = @($rs.rules | Where-Object { $_.description -ne $RuleMarker }) }
    $all = @($others) + @($ours)
    Cf-Request "PUT" "/zones/$ZoneId/rulesets/$($rs.id)/rules" @{ rules = $all } $Tok | Out-Null
    Write-Ok "302 跳转规则已更新：$Fqdn -> $Target"
    return [string]$rs.id
}

function Save-Config([hashtable]$cfg) {
    # token 属敏感信息（审计 M-10）：写入后立即用 icacls 把 ACL 收紧为仅当前用户，
    # 不再依赖脚本目录继承的默认权限；另建议不要把脚本目录放进网盘/共享。
    $cfg | ConvertTo-Json -Depth 6 | Out-File -FilePath $ConfigPath -Encoding ascii -Force
    $user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    icacls $ConfigPath /inheritance:r /grant:r "${user}:F" | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Warn2 "icacls 收紧 $ConfigPath 权限失败（exit=$LASTEXITCODE），请手动执行：icacls `"$ConfigPath`" /inheritance:r /grant:r `"$($user):F`""
    }
    Write-Info "配置已保存：$ConfigPath"
}

function Load-Config {
    $cfg = @{}
    $obj = Get-Content $ConfigPath -Raw | ConvertFrom-Json
    $obj.PSObject.Properties | ForEach-Object { $cfg[$_.Name] = $_.Value }
    return $cfg
}

# ── DDNS 模式：按已保存配置刷新，不做任何交互 ─────────────────────────────
if ($Ddns) {
    if (-not (Test-Path $ConfigPath)) { Write-Fail "未找到 cf-config.json，请先运行交互式配置"; exit 1 }
    try {
        $cfg = Load-Config
        $ip6 = $Ip
        if ([string]::IsNullOrWhiteSpace($ip6)) {
            $c = Get-PublicIPv6
            if ($c.Count -eq 0) { Write-Fail "未检测到公网 IPv6（网络变化？）"; exit 1 }
            $ip6 = $c[0]
        }
        $proxied = ($cfg.mode -eq "redirect")
        $null = Upsert-Aaaa $cfg.token $cfg.zone_id $cfg.fqdn $ip6 $proxied
        if ($cfg.mode -eq "redirect") {
            $target = "$($cfg.scheme)://[$ip6]:$($cfg.port)"
            $null = Upsert-RedirectRule $cfg.token $cfg.zone_id $cfg.fqdn $target
        }
        exit 0
    } catch {
        Write-Fail "$_"
        exit 1
    }
}

# ── 交互式配置 ────────────────────────────────────────────────────────────
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host " ilink-wm × Cloudflare IPv6 直连 配置向导" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host @"
两种模式（数据均不经过 Cloudflare）：
  1) direct   （推荐）DNS AAAA 直连——访问 http://子域.你的域名:端口 即直达
              本机 IPv6。地址栏显示域名，无证书告警（http），支持 DDNS 自动更新。
  2) redirect 302 跳转——访问 https://子域.你的域名，CF 返回 302 跳到
              http(s)://[IPv6]:端口。入口域名走 CF 代理（仅一跳 302，无业务数据），
              落地地址栏显示裸 IPv6；若用 https 目标会有证书告警。
"@ 

if (Test-Path $ConfigPath) {
    Write-Warn2 "检测到已有配置 $ConfigPath"
    $re = Read-Default "r=刷新DDNS（按当前配置更新 IP）/ n=重新配置" "r"
    if ($re -match "^[rR]$") {
        # 用外部进程重跑 DDNS 分支，保证退出码可靠传递
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath -Ddns
        exit $LASTEXITCODE
    }
}

# 1. 模式
if ($Mode -notin @("direct", "redirect")) {
    $m = Read-Default "选择模式 1=direct（推荐） / 2=redirect（302）" "1"
    $Mode = if ($m -eq "2") { "redirect" } else { "direct" }
}

# 2. API Token（浏览器内创建，即"浏览器授权登录"）
if ([string]::IsNullOrWhiteSpace($Token)) {
    $Token = $env:CF_API_TOKEN
}
if ([string]::IsNullOrWhiteSpace($Token) -and -not $Yes) {
    Write-Step "创建 Cloudflare API Token（一次性，浏览器操作）"
    Write-Host @"
  1. 脚本即将打开 https://dash.cloudflare.com/profile/api-tokens
     （未登录会先跳登录页，用你的 Cloudflare 账号登录）
  2. 点击 [Create Token] → 找到 [Create Custom Token] 点击 [Get started]
  3. 权限（Permissions）添加三行：
       Zone / Zone / Read
       Zone / DNS / Edit$(if ($Mode -eq "redirect") { "
       Zone / Rulesets / Edit   ← redirect 模式必需" })
  4. Zone Resources 选 Include → Specific zone → 你的域名
  5. Continue → Create Token → 复制生成的 Token 粘贴到下方
"@
    try { Start-Process "https://dash.cloudflare.com/profile/api-tokens" } catch {}
    $Token = Read-Host "粘贴 API Token"
}
if ([string]::IsNullOrWhiteSpace($Token)) { Write-Fail "缺少 API Token（-Token 或环境变量 CF_API_TOKEN）"; exit 1 }

Write-Step "验证 Token"
$v = Cf-Request "GET" "/user/tokens/verify" $null $Token
if (-not $v.success) { Write-Fail "Token 验证失败"; exit 1 }
Write-Ok "Token 有效"

# 3. 选择域名（zone）
Write-Step "获取域名列表"
$zones = (Cf-Request "GET" "/zones?per_page=50" $null $Token).result
if ($zones.Count -eq 0) { Write-Fail "账号下没有域名（zone），请先在 Cloudflare 添加站点"; exit 1 }
$zoneObj = $null
if (-not [string]::IsNullOrWhiteSpace($Zone)) {
    $zoneObj = $zones | Where-Object { $_.name -eq $Zone } | Select-Object -First 1
    if ($null -eq $zoneObj) { Write-Fail "未找到域名 $Zone"; exit 1 }
} elseif ($zones.Count -eq 1 -or $Yes) {
    $zoneObj = $zones[0]
} else {
    Write-Host "  选择要绑定的域名："
    for ($i = 0; $i -lt $zones.Count; $i++) { Write-Host ("  {0}) {1}" -f ($i + 1), $zones[$i].name) }
    while ($true) {
        $v2 = Read-Host "选择 (1-$($zones.Count))"
        $n = 0
        if ([int]::TryParse($v2, [ref]$n) -and $n -ge 1 -and $n -le $zones.Count) { $zoneObj = $zones[$n - 1]; break }
        Write-Host "  无效输入" -ForegroundColor Red
    }
}
$ZoneId = [string]$zoneObj.id
$ZoneName = [string]$zoneObj.name
Write-Ok "域名：$ZoneName"

# 4. 子域名
if ([string]::IsNullOrWhiteSpace($Label)) { $Label = Read-Default "子域名前缀" "ilink" }
if ($Label -notmatch '^[A-Za-z0-9-]+$') { Write-Fail "子域名前缀仅允许字母、数字、连字符"; exit 1 }
$Label = $Label.ToLowerInvariant()
$Fqdn = "$Label.$ZoneName"

# 5. 端口 / 协议
if ($Port -le 0) { $Port = [int](Read-Default "ilink-wm Web 端口" "8888") }
if ($Mode -eq "redirect") {
    if ($Scheme -notin @("http", "https")) {
        $Scheme = (Read-Default "跳转目标协议 http/https（应用无内置 TLS，http 无告警）" "http")
    }
    if ($Scheme -notin @("http", "https")) { $Scheme = "http" }
} else {
    $Scheme = "http"
}

# 6. IPv6 地址
$ip6 = $Ip
if ([string]::IsNullOrWhiteSpace($ip6)) {
    Write-Step "检测本机公网 IPv6"
    $cands = Get-PublicIPv6
    if ($cands.Count -eq 0) {
        Write-Fail "未检测到公网 IPv6 地址。可能原因：运营商未分配 / 光猫桥接未开启 / 无 IPv6 上行。"
        Write-Host "      可用 -Ip <IPv6> 手动指定后重跑。"
        exit 1
    }
    $ip6 = Select-Ip6 $cands
} else {
    Write-Info "使用指定地址：$ip6"
}
Write-Ok "IPv6 地址：$ip6"

# 7. 应用配置
Write-Step "写入 Cloudflare 配置"
$recordId = Upsert-Aaaa $Token $ZoneId $Fqdn $ip6 ($Mode -eq "redirect")
$ruleId = ""
if ($Mode -eq "redirect") {
    $target = "$Scheme`://[$ip6]:$Port"
    $ruleId = Upsert-RedirectRule $Token $ZoneId $Fqdn $target
}

$cfg = @{
    token    = $Token
    zone_id  = $ZoneId
    zone_name= $ZoneName
    fqdn     = $Fqdn
    mode     = $Mode
    port     = $Port
    scheme   = $Scheme
    record_id= $recordId
    rule_id  = $ruleId
}
Save-Config $cfg

# 8. 定时 DDNS（IPv6 前缀变化后自动刷新）
Write-Step "设置定时 DDNS（每小时自动刷新 DNS/跳转规则）"
$wantTask = if ($Yes) { "y" } else { (Read-Default "创建 Windows 计划任务 ilink-cf-ddns? y/n" "y") }
if ($wantTask -match "^[yY]") {
    $taskCmd = "powershell -NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -Ddns"
    $taskDone = $false
    try {
        $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -Ddns"
        $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Hours 1)
        Register-ScheduledTask -TaskName "ilink-cf-ddns" -Action $action -Trigger $trigger -Force | Out-Null
        $taskDone = $true
        Write-Ok "计划任务已创建（每小时执行；删除：Unregister-ScheduledTask ilink-cf-ddns）"
    } catch {
        try {
            schtasks /Create /F /TN "ilink-cf-ddns" /SC HOURLY /TR $taskCmd | Out-Null
            $taskDone = $true
            Write-Ok "计划任务已创建（每小时执行；删除：schtasks /Delete /TN ilink-cf-ddns /F）"
        } catch {
            Write-Warn2 "计划任务创建失败：$_"
        }
    }
    if (-not $taskDone) {
        Write-Warn2 "可手动/定时执行：$taskCmd"
    }
}

# 9. 防火墙（direct 模式必须放行入站端口）
if ($Mode -eq "direct") {
    Write-Step "Windows 防火墙放行入站 TCP $Port"
    $fwCmd = "netsh advfirewall firewall add rule name=""ilink-wm-$Port"" dir=in action=allow protocol=TCP localport=$Port"
    $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if ($isAdmin) {
        $wantFw = if ($Yes) { "y" } else { (Read-Default "立即添加防火墙规则? y/n" "y") }
        if ($wantFw -match "^[yY]") {
            Invoke-Expression $fwCmd | Out-Null
            Write-Ok "防火墙规则已添加"
        }
    } else {
        Write-Warn2 "当前非管理员，请以管理员运行：$fwCmd"
    }
}

# 10. 汇总
Write-Host ""
Write-Host "============================================================" -ForegroundColor Green
Write-Host " 配置完成！" -ForegroundColor Green
if ($Mode -eq "direct") {
    Write-Host " 访问地址： http://$Fqdn`:$Port   （浏览器直达本机 IPv6，数据不经 CF）"
} else {
    Write-Host " 访问地址： https://$Fqdn  → 302 → $Scheme`://[$ip6`]:$Port"
    Write-Host "   （302 由 CF 边缘返回，业务数据直达本机，不经 CF）"
}
Write-Host " 手动刷新 DDNS： powershell -File cf-setup.ps1 -Ddns"
Write-Host ""
Write-Host " ⚠ ilink-wm 侧还需确认：" -ForegroundColor Yellow
Write-Host "   1) 服务以双栈监听：ILINK_HOST=[::]（或向导选 3），否则 IPv6 进不来"
Write-Host "   2) 光猫/路由器 IPv6 防火墙需放行入站 TCP $Port（很多设备默认全拦）"
Write-Host "   3) 有公网 IPv6 直连后，serveo 隧道可以不用了（管理面板可停用）"
Write-Host "============================================================" -ForegroundColor Green
