# 日常运维

运行配置默认位于 `C:\ProgramData\iLinkWM\hosted`，只有管理员和 SYSTEM 可读写。

```powershell
$cfg = 'C:\ProgramData\iLinkWM\hosted\runtime\hosted-config.json'
$mgr = 'C:\ProgramData\iLinkWM\hosted\runtime\stack-manager.ps1'

# 状态、启停
powershell -File $mgr -Action status -ConfigPath $cfg
powershell -File $mgr -Action start  -ConfigPath $cfg
powershell -File $mgr -Action stop   -ConfigPath $cfg

# 开机自启任务
Get-ScheduledTask -TaskName iLinkWM-Hosted

# 日志
Get-Content 'C:\ProgramData\iLinkWM\hosted\logs\hosted-manager.log' -Tail 100 -Wait
```

升级 iLinkWM、Caddy 或 cloudflared 后，先停止任务，替换对应二进制，再运行 `stack-manager.ps1 -Action start`。若改变域名、Tunnel 或证书模式，重新运行 `一键部署.ps1`；它会重新生成受保护配置。

不要手动编辑 `runtime\secrets.ps1`、`tunnel-credentials.json` 或 Caddy 证书目录，也不要把它们备份到公开仓库。
