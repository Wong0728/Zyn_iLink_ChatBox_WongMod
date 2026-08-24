# iLinkWM Home Hosted

这是随 iLinkWM Release 分发的可选公网部署包。它把家用公网 IPv6 部署为两个 HTTPS 入口，而不是把 `ilink-wm1` 的 HTTP 端口直接暴露到公网。

```text
chat.example.com    → Cloudflare Worker → Tunnel ┐
                                                   ├→ Caddy → ilink-wm1 127.0.0.1:8888
direct.example.com  → DNS-only AAAA → HTTPS ─────┘
```

- `chat.example.com`：Cloudflare 入口，保留密码门和短码。
- `direct.example.com`：不经过 Cloudflare 代理的公网 IPv6 直连，但仍使用浏览器信任的 HTTPS、应用登录与防火墙。
- `origin.example.com`：仅供 Worker 回源的 Tunnel 主机名；**不能**绑定 Worker Route。

短码用于降低入口被随意访问的概率；应用账户登录、TLS 和防火墙才是安全边界。

## 两种启动方式

交互式（推荐给首次部署者）：

```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
.\一键部署.ps1
```

参数化/无人值守：复制 [home-config.example.json](./home-config.example.json) 到本机受保护目录，填入真实值后运行：

```powershell
.\一键部署.ps1 -NonInteractive -ConfigFile C:\Secure\ilink-home.json
```

配置文件中的 Cloudflare Token 是敏感信息，不能提交 Git、不能随 Release 分发。脚本会把运行期密钥、Tunnel 凭证和生成的配置收敛到 `C:\ProgramData\iLinkWM\hosted`，并仅授予 `SYSTEM` 与管理员访问权限。

## 自动化边界

本地部署可一键生成、安装和常驻运行；Cloudflare OAuth、域名所有权与首次创建 Tunnel/Worker 仍必须由账户持有人确认。这样不会把 Cloudflare 凭证硬编码进安装器或发布包。

首次 Cloudflare 配置见 [DEPLOY.md](./DEPLOY.md)。日常维护见 [OPERATIONS.md](./OPERATIONS.md)，问题排查见 [TROUBLESHOOTING.md](./TROUBLESHOOTING.md)。

## 包结构

```text
一键部署.ps1             交互式和配置文件式部署入口
stack-manager.ps1         受保护副本由 SYSTEM 看门狗调用
home-config.example.json  无人值守配置模板（只可本地复制）
cloudflare-worker/         Worker 源码与 wrangler 模板
keepalive/                 旧版脚本，仅保留历史参考，不再作为部署入口
```
