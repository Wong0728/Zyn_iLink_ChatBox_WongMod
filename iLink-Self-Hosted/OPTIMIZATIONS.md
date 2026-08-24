# 后续优化

当前包优先保证可部署和正确的安全边界。确认稳定运行后再考虑：

1. 在 Cloudflare 配置 Rate Limiting Rules，保护 `/verify` 与登录 API。
2. 为 `chat` Worker 使用独立 KV namespace，避免多个家庭部署互相覆盖短码。
3. 为 `direct` 入口增加路由器 IPv6 防火墙的源地址白名单，适合固定成员设备。
4. 加入 Caddy、cloudflared 与 iLinkWM 的版本检查及受校验下载器。
5. 将 Worker、Tunnel 创建改为 Cloudflare API 引导；仍保留浏览器 OAuth 确认。

不要把缓存、多个短码或多副本排在 TLS、应用 loopback 监听、可信反代和 IPv6 防火墙之前。
