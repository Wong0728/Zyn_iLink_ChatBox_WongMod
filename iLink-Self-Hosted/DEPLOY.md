# 首次部署

## 1. 准备三个域名

以 `example.com` 为例：

| 域名 | Cloudflare 状态 | 用途 |
|---|---|---|
| `chat.example.com` | 橙云 + Worker Route | 密码门、短码和 Tunnel 入口 |
| `direct.example.com` | 灰云 AAAA | 家用公网 IPv6 HTTPS 直连 |
| `origin.example.com` | Tunnel CNAME，无 Worker Route | Worker 专用回源 |

`direct` 指向当前公网 IPv6。公网 IPv6 变化时，SYSTEM 看门狗会每五分钟更新其 AAAA；证书签给域名，不会因 IP 变化重新签发。

## 2. 准备本机程序

需要 Windows 管理员权限、iLinkWM Release 中的 `ilink-wm1.exe`、`cloudflared.exe`，以及 Caddy。

建议用含 `caddy-dns/cloudflare` 模块的 Caddy，使用 DNS-01 自动签证书。它只需家用路由器/Windows 防火墙放行公网 IPv6 TCP 443，不需要开放 80。Cloudflare API Token 最小权限为对应 Zone 的 `DNS Edit`，并需可读取 Zone 信息。

如果不能使用 DNS-01，可将 `certificate_mode` 设为 `http`；此时 Caddy 需要在续期时可从公网访问 80 和 443。

## 3. 创建 Tunnel 与回源 DNS

在拥有 Cloudflare 账户的终端执行一次：

```powershell
cloudflared tunnel login
cloudflared tunnel create ilink-home
cloudflared tunnel route dns ilink-home origin.example.com
```

保存命令输出的 Tunnel UUID 和 credentials JSON 路径。不要把 credentials JSON 放在包目录；一键脚本会复制它到受保护的运行目录，供 SYSTEM 任务使用。

## 4. 配置 Worker

复制 `cloudflare-worker/wrangler.toml` 到一个私有工作目录，填入新建的 KV namespace ID 和三个域名：

```toml
CF_DOMAIN     = "chat.example.com"
DIRECT_URL    = "https://direct.example.com/chat"
TUNNEL_ORIGIN = "https://origin.example.com"
```

`origin.example.com` 不得添加 `chat` Worker Route。随后设置 `PASSWORD`、`REPORT_TOKEN` 和与本机 `origin_token` 一致的 `ORIGIN_TOKEN` secret 并部署 Worker；在 Dashboard 将 `chat.example.com/*` 路由到该 Worker。Caddy 会拒绝未携带该令牌的 Tunnel 请求，避免绕过 Worker 直达源站。

## 5. 一键配置本机

交互式命令会询问程序路径、域名、Tunnel 与证书信息：

```powershell
cd <Release 解压目录>\iLink-Self-Hosted
.\一键部署.ps1
```

无人值守时，使用本地复制的 `home-config.example.json`：

```powershell
.\一键部署.ps1 -NonInteractive -ConfigFile C:\Secure\ilink-home.json
```

脚本完成后会注册 `iLinkWM-Hosted` 计划任务。它以 SYSTEM 启动 iLinkWM、Caddy、cloudflared，并从受保护的运行目录读取配置。

## 6. 家用网络放行与验收

在路由器 IPv6 防火墙中，只允许该设备的 TCP 443 入站；Windows 防火墙同样只放行 Caddy 的 HTTPS 端口。`8888` 永远不对公网开放。

从另一条具有 IPv6 的网络验证：

```powershell
Resolve-DnsName direct.example.com -Type AAAA
curl.exe -6 -I https://direct.example.com/healthz
curl.exe -I https://chat.example.com/
```

直连域名应显示受信任的 HTTPS 证书；CF 域名应显示 Worker 密码页。两个入口各自登录一次是正常行为，Cookie 不跨子域名共享。
