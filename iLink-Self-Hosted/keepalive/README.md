# 旧版入口

此目录仅保留兼容入口。新部署请运行包根目录的 [一键部署.ps1](../一键部署.ps1)，它会使用 Caddy HTTPS、应用 loopback 监听、独立 Tunnel 回源和受保护的 SYSTEM 看门狗。

请勿使用旧版 `service-keepalive.ps1` 的公网 `::/80` 配置。
