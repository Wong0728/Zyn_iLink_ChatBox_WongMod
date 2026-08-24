<#
兼容入口。旧版脚本会把应用暴露到 ::/80，已废弃。
新版本只委派给包根目录的 stack-manager.ps1，并要求由一键部署生成的配置。
#>
[CmdletBinding()]
param(
    [ValidateSet('start', 'stop', 'status', 'watchdog')]
    [string]$Action = 'status',
    [string]$ConfigPath = 'C:\ProgramData\iLinkWM\hosted\runtime\hosted-config.json'
)

$ErrorActionPreference = 'Stop'
$manager = Join-Path (Split-Path -Parent $PSScriptRoot) 'stack-manager.ps1'
if (-not (Test-Path -LiteralPath $ConfigPath)) {
    throw "未找到 Hosted 配置：$ConfigPath。请先运行包根目录的一键部署.ps1。"
}
& $manager -Action $Action -ConfigPath $ConfigPath
