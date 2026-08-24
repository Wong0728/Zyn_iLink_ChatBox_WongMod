# iLink WM API 参考

本文对应当前 Rust 服务路由。除特别说明外，JSON 失败响应为
`{"success":false,"error":"…"}`；登录后的 HTTP 请求依靠 HttpOnly session cookie，
不应把 session token 写入客户端存储。

## 公开接口

| 方法 | 路径 | 请求要点 | 响应要点 |
|---|---|---|---|
| GET | `/healthz` | 无 | `status`、`version`、`uptime_secs`、`db_status`、`active_bots`；数据库异常时 503 |
| GET | `/api/wasm/auth-status` | 无 | `has_any_user`、`multiuser` |
| POST | `/api/wasm/login` | JSON：`username`、`password`、可选设备信息 | `success`、`uid`、`role`，成功时 Set-Cookie |
| POST | `/api/wasm/register` | JSON：`username`、`password`、`confirm_password`、`agreed_terms_ver`、按策略提供 `invite_code` | 新用户 `uid`、`username`、Set-Cookie；格式错误 422、重名 409、注册/邀请码禁止 403 |
| POST | `/api/wasm/auto-login` | 设备令牌 cookie/请求体 | 登录结果与 Set-Cookie |
| GET | `/api/wasm/terms`、`/guide`、`/links`、`/site-info`、`/notification`、`/register-status`、`/refresh-session` | 无 | 各页面需要的公开配置或状态 |

## 登录接口

| 方法 | 路径 | 请求要点 | 响应要点 |
|---|---|---|---|
| GET | `/api/wasm/stats`、`/status`、`/me`、`/session-status`、`/about` | 无 | 当前账号、会话、用量和服务状态 |
| GET | `/api/wasm/qrcode`、`/add-user-status`、`/reauth-poll` | 无 | 微信二维码/添加账号状态 |
| POST | `/api/wasm/add-user-start`、`/reauth-start` | 无 | 操作状态和请求标识 |
| GET | `/api/wasm/users`、`/chat-previews`、`/messages`、`/history` | 查询参数按前端分页/会话筛选 | 会话或消息列表 |
| POST | `/api/wasm/send`、`/typing`、`/switch-user` | JSON：目标会话和操作字段 | `success`/操作结果 |
| POST | `/api/wasm/delete-user`、`/batch-delete`、`/clear-messages`、`/delete-messages` | JSON：目标会话或消息标识 | 删除结果 |
| POST | `/api/wasm/media-presign`、`/download-media`、`/media-stream`、`/send-media`、`/upload-media` | JSON 或媒体 payload；媒体端点可接收较大请求体 | 媒体 key、下载/发送/上传结果 |
| GET | `/api/wasm/media/:cache_key`、`/webdav-proxy/*remote_path` | 路径参数 | 授权后的媒体内容 |
| GET/POST | `/api/wasm/webdav-settings` | GET 读取；POST 提交 WebDAV 配置 | 当前配置或保存结果 |
| POST | `/api/wasm/webdav-test`、`/webdav-traffic-saver`、`/webdav-migrate` | JSON 配置/操作字段 | 测试、节流或迁移结果 |
| GET | `/api/wasm/webdav-migrate-status`、`/webdav-auth` | 无 | 迁移/认证状态 |
| POST | `/api/wasm/export-history`、`/outbound-resend`、`/logout`、`/set-password`、`/set-email`、`/device-token-revoke` | JSON：对应操作字段 | 导出、会话或账号设置结果 |
| GET | `/api/wasm/outbound-pending`、`/email`、`/device-tokens` | 无 | 待恢复消息、邮箱或设备令牌列表 |

## 管理接口

所有 `/api/admin/*` 均需要登录、owner/admin 角色与 `admin.web_access` IP 策略。

| 方法 | 路径组 | 用途 |
|---|---|---|
| GET/POST | `/api/admin/users`、`/user/create`、`/user/disable`、`/user/enable`、`/user/delete`、`/user/quota`、`/user/features` | 用户、配额与功能管理 |
| GET | `/api/admin/bot-status`、`/bot-status-batch` | 已加载 bot 状态 |
| GET/POST | `/api/admin/invites`、`/invite/create`、`/invite/revoke` | 邀请码管理 |
| GET/POST | `/api/admin/settings`、`/setting` | 白名单系统配置 |
| GET | `/api/admin/stats`、`/audit`、`/ip-bans` | 统计、审计与封禁记录 |
| POST | `/api/admin/ip-ban`、`/ip-unban` | IP/CIDR 封禁管理 |
| GET/POST | `/api/admin/tunnel/status`、`/tunnel/start`、`/tunnel/stop` | 隧道管理 |
| POST | `/api/admin/broadcast`、`/broadcast-clear` | 全局通知 |

## WebSocket

`GET /api/ws` 完成升级后校验 session cookie。服务端帧为
`{"type":"事件名","data":{…}}`；当前事件包括 `status`、`message`、`user`、
`notification` 和 `sync_required`。客户端应忽略未知字段，在重连后以 REST 状态接口为准。

| `type` | `data` 的稳定字段 | 推送时机 |
|---|---|---|
| `status` | `logged_in`、`login_done`、`users`、`current_user`、`message` | 连接建立、登录状态或账号列表变化 |
| `message` | `id`/`row_id`、`from`、`to`、`text`、`time`、`type`；媒体消息另含媒体字段 | 收到或发送消息后 |
| `user` | `users`、`current_user` | 添加、移除或切换 iLink 会话后 |
| `notification` | `level`、`message` | 管理员广播通知 |
| `sync_required` | `reason` | 重新认证等需要客户端全量刷新状态的操作后 |
