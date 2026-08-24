# 故障排查

| 现象 | 首先检查 | 处理 |
|---|---|---|
| 直连域名无证书 | `C:\ProgramData\iLinkWM\hosted\logs\caddy.err.log` | DNS-01：确认 Caddy 含 cloudflare 模块、Token 有 Zone DNS Edit；HTTP-01：确认 80/443 可达。 |
| `direct` 无法访问 | `Resolve-DnsName direct.example.com -Type AAAA` | 检查路由器 IPv6 防火墙与 Windows TCP 443；看 `hosted-manager.log` 的 AAAA 更新结果。 |
| Tunnel 重启后断线 | `cloudflared.err.log` | 确认 credentials 已由一键脚本复制到受保护目录，且 `origin` DNS 指向该 Tunnel。 |
| CF 页面 1101/循环 | Worker `TUNNEL_ORIGIN` | 必须指向 `origin.example.com`，而不是绑定 Worker Route 的 `chat.example.com`。 |
| 登录后请求 403/会话 Cookie 非 Secure | 应用日志 | 确认应用只监听 `127.0.0.1:8888`，且由 Caddy 转发；不要设置 `ILINK_ALLOW_INSECURE_PUBLIC=1`。 |
| 计划任务已运行但服务没启动 | `hosted-manager.log` | 运行 `stack-manager.ps1 -Action status`；确认三份 exe 路径未因升级改变。 |

诊断命令：

```powershell
$root = 'C:\ProgramData\iLinkWM\hosted'
Get-Content "$root\logs\hosted-manager.log" -Tail 100
Get-Content "$root\logs\caddy.err.log" -Tail 100
Get-Content "$root\logs\cloudflared.err.log" -Tail 100
Get-NetTCPConnection -State Listen -LocalPort 443,8080,8888
```
