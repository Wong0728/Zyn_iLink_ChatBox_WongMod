# Cloudflare Worker

Worker 负责 `chat.example.com` 的密码门与短码路径。验证成功后会签发 24 小时、HttpOnly、Secure、SameSite=Strict 且绑定 Cloudflare 客户端 IP 的 gate session；API 和静态资源同样必须携带该 session。它不保存 iLinkWM 账户凭证。

部署前复制 `wrangler.toml` 到私有目录，设置：

- `ILINK_KV`：新建的 KV namespace ID。
- `CF_DOMAIN`：Worker 入口域名，例如 `chat.example.com`。
- `DIRECT_URL`：IPv6 直连 HTTPS 域名，例如 `https://direct.example.com/chat`。
- `TUNNEL_ORIGIN`：独立 Tunnel 回源域名，例如 `https://origin.example.com`。

`TUNNEL_ORIGIN` 不能匹配本 Worker 的 Route；推荐只将 `chat.example.com/*` 绑定 Worker，而让 `origin.example.com` 仅指向 cloudflared Tunnel。

```powershell
npx wrangler kv:namespace create ILINK_KV
npx wrangler secret put PASSWORD
npx wrangler secret put REPORT_TOKEN
npx wrangler secret put ORIGIN_TOKEN
npx wrangler deploy
```

在 Cloudflare Dashboard 创建 `chat.example.com/* → ilink-gate` Route。`origin.example.com` 不创建该 Route。

`ORIGIN_TOKEN` 必须与一键部署配置中的 `origin_token` 完全一致（至少 32 个随机字符）。Caddy 只接受 Worker 反代携带的该请求头，直接访问 Tunnel 回源域名会得到 403。
