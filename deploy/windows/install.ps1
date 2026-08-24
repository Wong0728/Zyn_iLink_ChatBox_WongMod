<#
.SYNOPSIS
  iLink-WM1 (iLinkWM) Windows 一键安装器
.DESCRIPTION
  通过 GitHub Release 下载预编译包安装；无可用 Release 时回退为
  「git clone + cargo build --release」源码编译。安装后提供 iLinkWM 命令
  （PowerShell 垫片：bin\iLinkWM.ps1 / bin\ilink-wm1.ps1，依赖用户
  PATHEXT 追加 .PS1，不生成任何 .cmd/.bat）。

  用法（PowerShell）：
    irm https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/windows/install.ps1 | iex

  本脚本面向 irm | iex 分发，必须保持 UTF-8 无 BOM：带 BOM 时首个语句会被
  解析成命令名导致 CommandNotFoundException。

  可选环境变量：
    ILINKWM_VERSION  指定版本 tag（如 v3.2.5），默认 v3.2.5；显式设 latest 才跟随浮动版本
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
$DefaultVersion = 'v3.2.5'
$AppId       = 'iLinkWM'
$InstallRoot = Join-Path $env:LOCALAPPDATA "Programs\$AppId"
$BinDir      = Join-Path $InstallRoot 'bin'
$DataDir     = Join-Path $InstallRoot 'data'
$RawBase     = "https://raw.githubusercontent.com/$Repo/$Branch"

function Write-Info  { Write-Host "[iLinkWM] $args" -ForegroundColor Cyan }
function Write-Ok    { Write-Host "[iLinkWM] $args" -ForegroundColor Green }
function Write-Warn2 { Write-Host "[iLinkWM] $args" -ForegroundColor Yellow }

function Find-LocalWindowsPackage {
    param([string]$Version, [switch]$PreferLatest)

    $downloads = Join-Path $env:USERPROFILE 'Downloads'
    if (-not (Test-Path -LiteralPath $downloads -PathType Container)) { return $null }

    $candidates = @()
    foreach ($file in @(Get-ChildItem -LiteralPath $downloads -Filter 'ilink_wm_v*_win_x64.zip' -File -ErrorAction SilentlyContinue)) {
        $match = [regex]::Match($file.Name, '^ilink_wm_(v\d+(?:\.\d+)*(?:-[0-9A-Za-z.-]+)?)_win_x64\.zip$')
        if (-not $match.Success) { continue }
        $tag = $match.Groups[1].Value
        $core = [regex]::Match($tag, '^v(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:\.(\d+))?')
        if (-not $core.Success) { continue }
        $major = [int]$core.Groups[1].Value
        $minor = if ($core.Groups[2].Success) { [int]$core.Groups[2].Value } else { 0 }
        $patch = if ($core.Groups[3].Success) { [int]$core.Groups[3].Value } else { 0 }
        $build = if ($core.Groups[4].Success) { [int]$core.Groups[4].Value } else { 0 }
        $candidates += [pscustomobject]@{
            Path = $file.FullName
            Name = $file.Name
            Version = $tag
            VersionKey = '{0:D8}.{1:D8}.{2:D8}.{3:D8}' -f $major, $minor, $patch, $build
            LastWriteTime = $file.LastWriteTime
        }
    }
    if (-not $candidates) { return $null }

    if ($Version -and $Version -ne 'latest' -and -not $PreferLatest) {
        $candidates = @($candidates | Where-Object { $_.Version -eq $Version })
        if (-not $candidates) { return $null }
    }
    $candidates | Sort-Object VersionKey, LastWriteTime -Descending | Select-Object -First 1
}

function Read-Sha256File {
    param([string]$Path, [string]$AssetName)

    $line = (Get-Content -LiteralPath $Path -Raw).Trim()
    $match = [regex]::Match($line, '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$')
    if (-not $match.Success -or $match.Groups[2].Value.Trim() -ne $AssetName) {
        throw "校验文件格式或文件名不匹配：$([IO.Path]::GetFileName($Path))"
    }
    $match.Groups[1].Value.ToUpperInvariant()
}

function Confirm-LocalPackageWarning {
    param([string[]]$Reasons)

    Write-Warn2 '预下载 ZIP 的本地校验信息与 GitHub Release 不一致：'
    foreach ($reason in $Reasons) { Write-Warn2 "  - $reason" }
    $answer = Read-Host '仍要安装这个本地 ZIP 吗？输入 Y 继续，其他输入将重新下载远端文件'
    if ($answer -notin @('Y', 'y')) {
        Write-Warn2 '已跳过本地 ZIP，将重新下载 GitHub Release 文件。'
        return $false
    }
    Write-Warn2 '已确认继续安装本地 ZIP。'
    return $true
}

function Test-BinaryVersion {
    param([string]$Path)

    $prevEap = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = (& $Path --version 2>&1 | Out-String).Trim()
        $exitCode = $LASTEXITCODE
    } catch {
        $output = $_.Exception.Message
        $exitCode = 1
    } finally {
        $ErrorActionPreference = $prevEap
    }
    if ($exitCode -ne 0) {
        Write-Warn2 "二进制运行验证失败：$output"
        return $false
    }
    Write-Info "二进制运行验证通过：$output"
    return $true
}

function Install-FromBinary {
    param([string]$Version)

    # 未显式指定版本时，预下载包按文件名版本号取最新；显式版本仍只接受精确匹配。
    $preferNewestLocal = -not [bool]$env:ILINKWM_VERSION
    $localPackage = Find-LocalWindowsPackage -Version $Version -PreferLatest:$preferNewestLocal
    if ($localPackage) {
        Write-Info "发现 Downloads 中的预下载包：$($localPackage.Name)"
    }

    $lookupVersion = if ($preferNewestLocal -and $localPackage) { $localPackage.Version } else { $Version }
    $rel = $null
    $releaseUri = if ($lookupVersion -and $lookupVersion -ne 'latest') {
        "https://api.github.com/repos/$Repo/releases/tags/$lookupVersion"
    } else {
        "https://api.github.com/repos/$Repo/releases/latest"
    }
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            $rel = Invoke-RestMethod -Uri $releaseUri -TimeoutSec 30
            break
        } catch {
            Write-Warn2 "Release API 请求失败（第 $attempt/3 次）：$($_.Exception.Message)"
            if ($attempt -lt 3) { Start-Sleep -Seconds 2 }
        }
    }

    $releaseTag = if ($rel) {
        $rel.tag_name
    } elseif ($lookupVersion -and $lookupVersion -ne 'latest') {
        $lookupVersion
    } elseif ($localPackage) {
        $localPackage.Version
    } else {
        $null
    }
    if ($rel -and $releaseTag) {
        $localPackage = Find-LocalWindowsPackage -Version $releaseTag
    }

    $asset = $null
    $hashAsset = $null
    $assetName = $null
    $assetUrl = $null
    $hashUrl = $null
    if ($rel) {
        $asset = $rel.assets | Where-Object { $_.name -match 'win_x64\.zip$' } | Select-Object -First 1
        if ($asset) {
            $assetName = $asset.name
            $assetUrl = $asset.browser_download_url
            $hashAsset = $rel.assets | Where-Object { $_.name -eq "$assetName.sha256" } | Select-Object -First 1
            $hashUrl = if ($hashAsset) { $hashAsset.browser_download_url } else { "$assetUrl.sha256" }
        }
    }

    # 固定版本的资产名可预测；API 限流、网络异常或 JSON 解析失败时直接走 Release 直链。
    if (-not $assetUrl -and $releaseTag) {
        $assetName = "ilink_wm_${releaseTag}_win_x64.zip"
        $assetUrl = "https://github.com/$Repo/releases/download/$releaseTag/$assetName"
        $hashUrl = "$assetUrl.sha256"
        Write-Warn2 "使用备用 Release 下载地址：$assetUrl"
    }
    if (-not $assetUrl -or -not $hashUrl) { return $false }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) "ilinkwm_install_$(Get-Random)"
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    $usingLocalPackage = $null -ne $localPackage
    $zip = if ($usingLocalPackage) { $localPackage.Path } else { Join-Path $tmp 'pkg.zip' }
    $hashFile = Join-Path $tmp "$assetName.sha256"
    $assetSize = if ($asset) { [math]::Round($asset.size/1MB,1) } else { '?' }
    if ($usingLocalPackage) {
        Write-Info "使用预下载包 $($localPackage.Path)（不会重复下载 ZIP）"
    } else {
        Write-Info "下载 $assetName（$assetSize MB）..."
        Invoke-WebRequest -Uri $assetUrl -OutFile $zip -TimeoutSec 600 -UseBasicParsing
    }
    Write-Info "下载并校验 $assetName.sha256..."
    Invoke-WebRequest -Uri $hashUrl -OutFile $hashFile -TimeoutSec 60 -UseBasicParsing
    $expectedHash = Read-Sha256File -Path $hashFile -AssetName $assetName

    if ($usingLocalPackage) {
        $warnings = @()
        $localHashPath = "$zip.sha256"
        $localHash = $null
        if (Test-Path -LiteralPath $localHashPath) {
            try {
                $localHash = Read-Sha256File -Path $localHashPath -AssetName $assetName
            } catch {
                $warnings += "本地 $assetName.sha256 格式无效：$($_.Exception.Message)"
            }
        } else {
            $warnings += "Downloads 中缺少同名 $assetName.sha256"
        }
        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToUpperInvariant()
        if ($localHash -and $localHash -ne $expectedHash) {
            $warnings += "本地 sidecar 的哈希 $localHash 与 GitHub 远端哈希 $expectedHash 不同"
        }
        if ($localHash -and $localHash -ne $actualHash) {
            $warnings += "本地 ZIP 实际哈希 $actualHash 与本地 sidecar 不同"
        }
        if ($actualHash -ne $expectedHash) {
            $warnings += "本地 ZIP 实际哈希 $actualHash 与 GitHub 远端哈希 $expectedHash 不同"
        }
        if ($warnings.Count -gt 0 -and -not (Confirm-LocalPackageWarning -Reasons $warnings)) {
            $usingLocalPackage = $false
            $zip = Join-Path $tmp 'pkg.zip'
            Invoke-WebRequest -Uri $assetUrl -OutFile $zip -TimeoutSec 600 -UseBasicParsing
            $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToUpperInvariant()
        }
    } else {
        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToUpperInvariant()
    }
    if (-not $usingLocalPackage -and $actualHash -ne $expectedHash) {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        throw "$assetName SHA-256 校验失败（实际 $actualHash，期望 $expectedHash），已停止安装。"
    }
    Write-Ok "SHA-256 校验通过（GitHub 远端）：$actualHash"

    $extract = Join-Path $tmp 'extract'
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    # zip 内容在根目录（web/、ilink-wm1.exe、...），若包了一层目录则下钻
    $srcRoot = $extract
    if (-not (Test-Path (Join-Path $srcRoot 'ilink-wm1.exe'))) {
        $nested = Get-ChildItem $extract -Directory | Select-Object -First 1
        if ($nested -and (Test-Path (Join-Path $nested.FullName 'ilink-wm1.exe'))) { $srcRoot = $nested.FullName }
    }
    $portableExe = Join-Path $srcRoot 'ilink-wm1.exe'
    if (-not (Test-Path $portableExe)) { throw "压缩包内未找到 ilink-wm1.exe" }
    if (-not (Test-BinaryVersion -Path $portableExe)) {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        throw '预编译二进制无法在当前 Windows 系统运行，已停止安装。'
    }

    if (Test-Path $InstallRoot) {
        Write-Info "升级：保留 data/ 与 bin/，覆盖其余文件..."
        Get-ChildItem $InstallRoot | Where-Object { $_.Name -ne 'data' -and $_.Name -ne 'bin' } |
            Remove-Item -Recurse -Force
    } else {
        New-Item -ItemType Directory -Path $InstallRoot -Force | Out-Null
    }
    Get-ChildItem $srcRoot | Where-Object { $_.Name -ne 'data' } |
        Copy-Item -Destination $InstallRoot -Recurse -Force
    if (-not (Test-BinaryVersion -Path (Join-Path $InstallRoot 'ilink-wm1.exe'))) {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        throw '已下载的二进制复制后运行验证失败，已停止安装。'
    }
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    Write-Ok "已安装 $releaseTag（Release 预编译包）"
    return $true
}

function Install-FromSource {
    param([string]$Version)
    $git = Get-Command git -ErrorAction SilentlyContinue
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        $cargoPath = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (Test-Path $cargoPath) { $cargo = $cargoPath }
    }
    if (-not $git)   { throw "未找到 git。源码安装需要 git 与 Rust 工具链，请安装后重试，或等待 Release 预编译包。" }
    if (-not $cargo) { throw "未找到 cargo。请先安装 Rust stable（https://www.rust-lang.org/tools/install），或等待 Release 预编译包。" }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) "ilinkwm_src_$(Get-Random)"
    $sourceRef = if ($Version -and $Version -ne 'latest') { $Version } else { $Branch }
    Write-Info "克隆源码 $sourceRef 到 $tmp ..."
    # PS 5.1 下 `2>&1` 会把 git 的 stderr 进度输出变成 ErrorRecord，
    # 在 $ErrorActionPreference='Stop' 时直接终止脚本（NativeCommandError）；
    # 临时降级后按退出码判断成败。
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & git clone --depth 1 --branch $sourceRef "https://github.com/$Repo.git" $tmp 2>&1 | Out-Null
    $cloneExit = $LASTEXITCODE
    $ErrorActionPreference = $prevEap
    if ($cloneExit -ne 0) { throw "git clone 失败（exit code $cloneExit）" }

    Write-Info "cargo build --release（首次约 3-10 分钟）..."
    Push-Location $tmp
    try {
        & $cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "编译失败" }
    } finally { Pop-Location }

    $exe = Join-Path $tmp 'target\release\ilink-wm1.exe'
    if (-not (Test-Path $exe)) { throw "编译产物未找到：$exe" }
    if (-not (Test-BinaryVersion -Path $exe)) { throw '源码编译产物无法在当前 Windows 系统运行。' }

    if (Test-Path $InstallRoot) {
        Get-ChildItem $InstallRoot | Where-Object { $_.Name -ne 'data' -and $_.Name -ne 'bin' } |
            Remove-Item -Recurse -Force
    } else {
        New-Item -ItemType Directory -Path $InstallRoot -Force | Out-Null
    }
    Copy-Item $exe $InstallRoot -Force
    Copy-Item (Join-Path $tmp 'web') $InstallRoot -Recurse -Force
    $selfHosted = Join-Path $tmp 'iLink-Self-Hosted'
    if (Test-Path $selfHosted) { Copy-Item $selfHosted $InstallRoot -Recurse -Force }
    foreach ($f in 'LICENSE','README.md','CHANGELOG.md','start.ps1','install-service.ps1','用户协议.md','部署指南.md') {
        $p = Join-Path $tmp $f
        if (Test-Path $p) { Copy-Item $p $InstallRoot -Force }
    }
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    Write-Ok "已安装（源码编译，基线 $sourceRef）"
    return $true
}

function Write-Shim {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null

    # 清理历史遗留的 cmd 垫片（v3.2.4 早期轮次生成；现已全面 PowerShell 化）
    foreach ($legacy in 'iLinkWM.cmd','ilink-wm1.cmd','iLinkWM-help.txt') {
        $p = Join-Path $BinDir $legacy
        if (Test-Path $p) { Remove-Item $p -Force }
    }

    $cmdPath = Join-Path $BinDir 'iLinkWM.ps1'
    # PowerShell 对 UTF-8（带 BOM）脚本的中文解析没有任何 cmd 那类编码/行定位问题，
    # 垫片直接使用中文提示。BOM 必需：Windows PowerShell 5.1 以 -File 运行无 BOM 的
    # UTF-8 脚本时中文会乱码。
    $shim = @'
# iLinkWM - Zyn iLink ChatBox WongMod 统一命令（由安装器生成，仅 PowerShell 可用）
$ErrorActionPreference = 'Stop'
$appRoot = Split-Path -Parent $PSScriptRoot
$exe     = Join-Path $appRoot 'ilink-wm1.exe'
$nssm    = Join-Path $appRoot 'bin\nssm.exe'
$svcName = 'ilink-wm1'
$rawBase = 'RAWBASE'

$cmd  = if ($args.Count -ge 1) { [string]$args[0] } else { '' }
$rest = if ($args.Count -ge 2) { $args[1..($args.Count - 1)] } else { @() }

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal $id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Start-App {
    param([string[]]$AppArgs = @())
    if (-not (Test-Path $exe)) {
        Write-Host "[iLinkWM] 未找到 ilink-wm1.exe，请重新安装：" -ForegroundColor Yellow
        Write-Host "  irm $rawBase/deploy/windows/install.ps1 | iex"
        exit 1
    }
    Set-Location $appRoot
    if (-not $env:ILINK_DATA_DIR) { $env:ILINK_DATA_DIR = Join-Path $appRoot 'data' }
    & $exe @AppArgs
    exit $LASTEXITCODE
}

function Show-Help {
    Write-Host 'iLinkWM - Zyn iLink ChatBox WongMod 统一命令'
    Write-Host ''
    Write-Host '  iLinkWM                     启动程序（首次运行进入初始化向导）'
    Write-Host '  iLinkWM help                显示本帮助（-h / --help 同效）'
    Write-Host '  iLinkWM install-service     注册 Windows 服务（NSSM，需管理员）'
    Write-Host '  iLinkWM uninstall-service   移除 Windows 服务（需管理员）'
    Write-Host '  iLinkWM service start|stop  启停服务（需管理员）；其余参数查询状态'
        Write-Host '  iLinkWM update              更新到当前正式版本（可用 ILINKWM_VERSION=latest 跟随浮动版本）'
    Write-Host '  iLinkWM uninstall [--keep-data] 卸载；默认删除程序与全部数据，--keep-data 保留数据'
    Write-Host '  iLinkWM admin ...           其余参数原样传给 ilink-wm1'
    Write-Host '  ilink-wm1 ...               二进制直通命令（同在 PATH）：ilink-wm1 --version / admin ...'
    exit 0
}

switch -Regex ($cmd) {
    '^$' { Start-App }

    '^(?i)help$'      { Show-Help }
    '^(?i)-h$'        { Show-Help }
    '^(?i)--help$'    { Show-Help }
    '^(?i)ilinkwm-help$' { Show-Help }

    '^(?i)update$' {
        Write-Host '[iLinkWM] 正在检查并安装当前正式版本...'
        iex (irm "$rawBase/deploy/windows/install.ps1")
    }

    '^(?i)install-service$' {
        $svcPs1 = Join-Path $appRoot 'install-service.ps1'
        if (-not (Test-Path $svcPs1)) {
            Write-Host "[iLinkWM] 未找到 $svcPs1，请重新安装。" -ForegroundColor Yellow
            exit 1
        }
        if (Test-Admin) {
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $svcPs1
            exit $LASTEXITCODE
        }
        Write-Host '[iLinkWM] 注册 Windows 服务需要管理员权限，正在请求提权...'
        Start-Process -Verb RunAs -FilePath 'powershell.exe' -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File',"`"$svcPs1`""
    }

    '^(?i)uninstall-service$' {
        if (-not (Test-Path $nssm)) {
            Write-Host '[iLinkWM] 未找到 bin\nssm.exe，服务未注册过。'
            exit 0
        }
        if (Test-Admin) {
            & $nssm stop $svcName
            & $nssm remove $svcName confirm
        } else {
            Write-Host '[iLinkWM] 移除 Windows 服务需要管理员权限，正在请求提权...'
            Start-Process -Verb RunAs -FilePath 'powershell.exe' -ArgumentList '-NoProfile','-Command',"& '$nssm' stop $svcName; & '$nssm' remove $svcName confirm"
        }
    }

    '^(?i)service$' {
        if (-not (Test-Admin)) {
            Write-Host '[iLinkWM] 服务管理需要管理员权限。用法：Start-Service/Stop-Service ilink-wm1，或 sc.exe query ilink-wm1'
            exit 1
        }
        switch -Regex ("$rest") {
            '^(?i)start$' { & net.exe start $svcName }
            '^(?i)stop$'  { & net.exe stop  $svcName }
            default       { & sc.exe query $svcName }
        }
    }

    '^(?i)uninstall$' {
        $keepData = ($rest.Count -gt 0 -and "$($rest[0])" -eq '--keep-data')
        if ($keepData) {
            $confirm = Read-Host '卸载 iLinkWM？（数据目录将保留；输入 Y 确认）'
        } else {
            $confirm = Read-Host '卸载 iLinkWM 并删除程序与全部数据？输入 Y 确认（保留数据请用 --keep-data）'
        }
        if ($confirm -notin @('Y','y')) { Write-Host '[iLinkWM] 已取消。'; exit 0 }
        Write-Host '[iLinkWM] 正在卸载...'
        if (Test-Path $nssm) {
            & $nssm stop $svcName 2>$null
            & $nssm remove $svcName confirm 2>$null
        }
        $binDir = $PSScriptRoot
        $userPath = [Environment]::GetEnvironmentVariable('Path','User')
        $newPath = ($userPath -split ';' | Where-Object { $_ -and $_ -ne $binDir }) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        if ($keepData) {
            Remove-Item -Recurse -Force (Join-Path $appRoot 'web'), $exe, $binDir, (Join-Path $appRoot 'logs') -ErrorAction SilentlyContinue
            Write-Host "[iLinkWM] 已卸载（数据目录保留在 $(Join-Path $appRoot 'data')，需要时手动删除）。"
        } else {
            Write-Host "[iLinkWM] 已卸载（程序与数据目录 $(Join-Path $appRoot 'data') 将在 2 秒后全部删除）。"
            Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile','-Command',"Start-Sleep -Seconds 2; Remove-Item -LiteralPath '$appRoot' -Recurse -Force -ErrorAction SilentlyContinue"
        }
        Write-Host '[iLinkWM] 请关闭并重开终端使 PATH 变更生效。'
        exit 0
    }

    default { Start-App (@($cmd) + @($rest)) }
}
'@
    $shim = $shim.Replace('RAWBASE', $RawBase)
    # 行尾强制 CRLF（raw 下载的本脚本是 LF），UTF-8 带 BOM 落盘（PS 5.1 中文必需）
    $shim = $shim -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText($cmdPath, $shim, (New-Object System.Text.UTF8Encoding($true)))

    # ilink-wm1 直通命令：任意 PowerShell ilink-wm1 --version / ilink-wm1 admin ...
    $exeShimPath = Join-Path $BinDir 'ilink-wm1.ps1'
    $exeShim = @'
# ilink-wm1 直通命令（由安装器生成）：等价直接运行二进制
$ErrorActionPreference = 'Stop'
$appRoot = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $appRoot 'ilink-wm1.exe'
if (-not (Test-Path $exe)) {
    Write-Host '[ilink-wm1] 未找到 ilink-wm1.exe，请重新安装 iLinkWM。' -ForegroundColor Yellow
    exit 1
}
Set-Location $appRoot
if (-not $env:ILINK_DATA_DIR) { $env:ILINK_DATA_DIR = Join-Path $appRoot 'data' }
& $exe @args
exit $LASTEXITCODE
'@
    $exeShim = $exeShim -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText($exeShimPath, $exeShim, (New-Object System.Text.UTF8Encoding($true)))
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

function Add-UserPathExt {
    # 命令垫片为 .ps1：把 .PS1 追加到用户 PATHEXT，PowerShell 里即可直接输入
    # iLinkWM / ilink-wm1 调用（写入「当前生效完整列表 + .PS1」，追加/覆盖两种
    # 注册表合并语义下均正确）。
    if ($env:PATHEXT -notmatch '(?i)\.PS1') {
        [Environment]::SetEnvironmentVariable('PathExt', "$env:PATHEXT;.PS1", 'User')
        Write-Ok '已将 .PS1 加入用户 PATHEXT（新终端中可直接输入 iLinkWM）'
    } else {
        Write-Ok 'PATHEXT 已包含 .PS1'
    }
}

function Ensure-ExecutionPolicy {
    # 默认 Restricted/AllSigned 会拦截本地 .ps1 垫片；放行当前用户作用域的本地脚本
    if ((Get-ExecutionPolicy) -in @('Restricted','AllSigned')) {
        try {
            Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned -Force
            Write-Ok '已将当前用户执行策略设为 RemoteSigned（允许运行本地脚本）'
        } catch {
            Write-Warn2 "无法设置执行策略（$($_.Exception.Message)）；请手动执行：Set-ExecutionPolicy -Scope CurrentUser RemoteSigned"
        }
    }
}

# ── 主流程 ─────────────────────────────────────────────
Write-Info "iLink-WM1 安装器 · 目标目录 $InstallRoot"
$method  = if ($env:ILINKWM_METHOD)  { $env:ILINKWM_METHOD }  else { 'auto' }
$version = if ($env:ILINKWM_VERSION) { $env:ILINKWM_VERSION } else { $DefaultVersion }
if ($version -eq 'latest') {
    Write-Warn2 '已显式选择浮动 latest；正式部署建议固定 ILINKWM_VERSION=v3.2.5。'
}

$ok = $false
if ($method -eq 'binary') {
    $ok = Install-FromBinary -Version $version
    if (-not $ok) { throw "未找到可用的 Windows 预编译包（$version）" }
} elseif ($method -eq 'source') {
    $ok = Install-FromSource -Version $version
} else {
    try { $ok = Install-FromBinary -Version $version } catch { Write-Warn2 $_ }
    if (-not $ok) {
        Write-Warn2 "回退源码编译模式..."
        $ok = Install-FromSource -Version $version
    }
}

Write-Shim
Add-UserPath -Dir $BinDir
Add-UserPathExt
Ensure-ExecutionPolicy

Write-Host ''
Write-Ok '安装完成！下一步：'
Write-Host '  1. 关闭并重新打开终端（使 PATH/PATHEXT 生效；iLinkWM 为 PowerShell 命令）'
Write-Host '  2. 运行  iLinkWM                # 首次运行进入初始化向导'
Write-Host '  3. 可选  iLinkWM install-service # 注册为 Windows 服务'
Write-Host ''
Write-Host "  安装目录：$InstallRoot"
Write-Host "  数据目录：$DataDir"
Write-Host '  完整文档：README.md / 部署指南.md'
