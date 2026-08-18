# Zyn iLink ChatBox · WongMod

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux%20%7C%20macOS%20%7C%20Termux-lightgrey)](#)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](#)
[![Version](https://img.shields.io/badge/Version-v3.2.4--wm1.1-brightgreen.svg)](CHANGELOG.md)
[![Hero](https://img.shields.io/badge/GitHub%20Pages-Hero%20页面-8A2BE2)](https://wong0728.github.io/Zyn_iLink_ChatBox_WongMod/)

> 微信官方 iLink 协议 · 开源消息管理平台（Zyn iLink ChatBox 的 Rust 重构版）。
> 一个二进制启动，零外部依赖，内置 SQLite 零配置。
> 支持多用户、Web 实时聊天、WebDAV 云存储、消息历史、审计日志与全局通知。

**版本**：v3.2.4-wm1.1 · 基于 Zyn iLink ChatBox v3.1.9 移植，随原版 v3.2.4 对照演进
**原作者**：ZynSync · **修改者**：Mr.Wong（Rust 重构）
**本仓库**：<https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod>
**原仓库**：<https://github.com/zynsync/Zyn-iLink-ChatBox>

### 一键安装（iLinkWM 命令）

**Windows（PowerShell）：**

```powershell
irm https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/windows/install.ps1 | iex
```

**Linux / macOS / Termux：**

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/linux/install.sh | bash
```

安装完成后即可使用 `iLinkWM` 命令：直接运行 `iLinkWM` 启动程序；`iLinkWM install-service` 注册系统服务；`iLinkWM update` 自更新；`iLinkWM uninstall` 卸载（**默认连同数据目录一起删除**，保留数据请用 `--keep-data`，均有确认提示）；`iLinkWM help` 查看全部子命令。详见 [deploy/](deploy/) 与本文档第一章。

> Windows 端命令入口为 PowerShell 脚本（需 PowerShell 5.1+，安装器会自动把 `.PS1` 加入用户 PATHEXT 并在需要时放行本地脚本执行策略）；本项目已全面弃用 cmd 批处理。随包附带的 `start.ps1`（启动）与 `install-service.ps1`（注册服务）同为 PowerShell 脚本。

> 一键脚本默认固定到 `v3.2.4-wm1.1`，从 [Releases](https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod/releases) 下载预编译包并校验 SHA-256；无可用包（或对应架构缺失）时从同名 tag 回退到「克隆源码 + cargo 编译」。只有显式设置 `ILINKWM_VERSION=latest` 才跟随浮动版本。

### 参考项目（致谢）

| 项目 | 说明 |
|------|------|
| [zynsync/Zyn-iLink-ChatBox](https://github.com/zynsync/Zyn-iLink-ChatBox) | 原项目：基于微信官方 iLink 接口的 Python 单文件客户端，本项目由其移植重构 |
| [openilink](https://github.com/openilink) | 开源 iLink 协议生态参考（协议文档与实现思路） |

### 与原版的关键差异

| 对比项 | 原版 Python v3.2.4 | WongMod Rust v3.2.4-wm1.1 |
|--------|--------------------|---------------------------|
| 形态 | 单文件 Python，启动时自动 pip 装依赖 | 单二进制零依赖（Rust 全量重构） |
| 用户 | 全局 Web 密码 / 平级多账号（无角色） | owner-admin-user 三级 + 邀请码 + 配额 |
| 存储 | JSON 文件 | SQLite 分库，自动迁移老库 |
| 消息 | 文本/图片/视频/文件/语音（收） | 全部支持收发（含语音发送）+ 持久化 + HTML 导出 |
| AI | 自动回复 / 识图 / 生图 / 文件识别 | 不内置（预留 Webhook 扩展，见 CHANGELOG） |
| 新增 | — | WebDAV 外置、Webhook、审计日志、限流、WebSocket 推送、CLI/服务化 |
| 移除 | 邮箱验证找回、二维码门户、远程 Gitee 内容、控制台直发 | — |
| 安全 | 基础口令 | PBKDF2 600k、AES-256-GCM、CSRF/SSRF 防护、审计（0 高危） |

> 完整逐项对比（继承强化 / 新增 / 移除 / 行为差异四张表）见 [Hero 页面对比区](https://wong0728.github.io/Zyn_iLink_ChatBox_WongMod/#compare) 与 [CHANGELOG.md](CHANGELOG.md) 差异章节。

---

## 目录

- [一、部署与启动](#一部署与启动)
- [二、第一章 · 用户使用指南](#二第一章--用户使用指南)
- [三、第二章 · 管理员管理指南](#三第二章--管理员管理指南)
- [四、附录](#四附录)

---

## 一、部署与启动

本章面向准备把项目跑起来的任何人（既是管理员的预备课，也是用户能正常使用的前提）。完成本章后，Web 服务已可访问、owner 账号已创建、第一个邀请码已生成。

### 1.1 环境要求

| 项 | 要求 |
|----|------|
| 操作系统 | Windows 10+ / Linux（x86_64、aarch64）/ macOS / Termux（Android） |
| Rust 工具链 | `rustup` + `cargo`（源码编译使用仓库固定的 Rust 1.95.0；使用预编译 Release 时无需） |
| 磁盘 | ≥ 500 MB（编译产物 + 数据库 + 媒体缓存） |
| 内存 | ≥ 256 MB 空闲 |
| 浏览器 | Chrome 90+ / Edge 90+ / Safari 14+ / Firefox 88+，需启用 Cookie 与 JavaScript |
| 网络 | 服务端需能访问微信 iLink 协议域名；客户端浏览器需能访问服务端 |

### 1.2 获取源码

```bash
git clone https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod.git
cd Zyn_iLink_ChatBox_WongMod
```

> 若部署在内网无 git 的服务器，从 Releases 下载 `ilink_wm_v3.2.4-wm1.1_src.zip` 解压即可，源码即部署包。

### 1.3 编译

在项目根目录（即 `Cargo.toml` 所在目录）执行：

```bash
# 调试编译（首次约 3-8 分钟，看机器性能）
cargo build

# 发布编译（性能更好，建议生产用）
cargo build --release
```

- 调试产物：`target/debug/ilink-wm1`（或 Windows 下的 `target\debug\ilink-wm1.exe`）
- 发布产物：`target/release/ilink-wm1`（或 `.exe`）
- 项目附带 `run_test.py`（开发辅助脚本，会自动同步 `web/` 到 target 目录）可一键编译并启动：`python run_test.py [--release] [--build-only]`

> **Windows 路径含中文时 Cargo 可能输出乱码**——把 `CARGO_TARGET_DIR` 指向纯 ASCII 路径即可：
>
> ```powershell
> # PowerShell
> $env:CARGO_TARGET_DIR = "C:\ilink-wm1-target"
> cargo build --release
> ```
>
> ```cmd
> :: cmd
> set CARGO_TARGET_DIR=C:\ilink-wm1-target
> cargo build --release
> ```
>
> 或者直接用 `run_test.py`——它已经把 `CARGO_TARGET_DIR` 指向项目内的 `target/`（仍含中文但能跑通）。

### 1.4 首次启动向导

把编译产物与 `web/` 目录放在同一目录下（保持二进制与 `web/` 同级），然后运行：

```bash
./ilink-wm1
```

向导共 7 步：

1. **Web 服务绑定地址**
   - `1` 仅本机访问（`127.0.0.1`，默认，最安全）
   - `2` 局域网访问（`0.0.0.0`）
   - `3` 跳过，改用环境变量 `ILINK_HOST`
2. **创建 owner 账号**（系统最高权限）
   - 用户名 3-32 位，仅允许字母数字、下划线、连字符
   - 密码 8-128 位，必须包含大写字母、小写字母和数字
   - 容器化场景可改用环境变量 `ILINK_OWNER_USER` + `ILINK_OWNER_PASSWORD` 注入
3. **站点名称**（显示在前端顶部与登录页）
4. **注册策略**
   - 开放注册（默认关，任何人都可直接注册）
   - 邀请码注册（默认开，需邀请码才能注册）
5. **邀请码生成**（仅当上一步允许邀请码注册时询问）
   - 4 位大写字母+数字组合（如 `A3F5`）
   - 可设有效期天数（1-365，默认 30）与备注
6. **使用守则**（默认写入 v1.0 守则文本，后续可用 CLI 覆盖）
7. **运行模式**
   - `1` 交互模式（终端可输入 `/set` `/webset` 等命令）
   - `2` 仅 Web 模式（管理走 CLI 或网页）

完成后控制台会输出：

```
[GUIDE] Web 访问: http://127.0.0.1:8888
[GUIDE] REPL 输入 /help 查看所有命令；/set 打开设置菜单
[GUIDE] CLI 管理命令: ilink-wm1 admin <sub>（如 admin user list）
```

向导只会运行一次（写入 `system.db` 的 `setup_complete=1`），下次启动直接进入服务模式。重置向导：

```bash
ilink-wm1 admin config set setup_complete 0
```

### 1.5 数据目录

默认 **便携模式**：所有数据写在二进制所在目录下。可用 `ILINK_DATA_DIR` 改到其他路径。

目录结构：

```
<base_dir>/
├── ilink-wm1                  # 二进制
├── web/                       # 前端静态文件
├── system.db                  # 多用户/认证/配额/守则/审计/邀请码
├── wechat_bot.db.bak          # 老库迁移后的备份（若有）
└── users/
    └── <uid>/
        ├── user.db            # 该用户的私聊消息/媒体记录/会话
        ├── user_data/         # 该用户的配置
        └── media_cache/       # 该用户的媒体缓存（按 hex 前 2 位分桶）
```

> 默认不写入 `%APPDATA%` 或 `/var/lib`，卸载只需删除整个目录。

> `web/landing.html` 是 Rust 服务运行时的 `/` 首页；`docs/hero/ilinkwm_hero.html` 是 GitHub Pages 宣传页。两者职责不同，前者随安装包分发，后者由 Pages 单独发布。

### 1.6 环境变量速查

| 变量 | 作用 | 默认 | 何时用 |
|------|------|------|--------|
| `ILINK_PORT` | Web 服务端口 | `8888` | 改端口 |
| `ILINK_HOST` | Web 服务绑定地址 | `127.0.0.1` | 改 `0.0.0.0` 公网/局域网 |
| `ILINK_DATA_DIR` | 数据目录 | 二进制所在目录 | 集中存储、迁移数据 |
| `ILINK_SERVER_MODE` | 服务器模式（日志落盘 + 关闭向导） | 未设置 | systemd / Docker |
| `ILINK_OWNER_USER` | 首次启动 owner 用户名 | — | 容器化无 stdin 场景 |
| `ILINK_OWNER_PASSWORD` | 首次启动 owner 密码 | — | 同上 |
| `ILINK_TRUSTED_PROXIES` | 可信反向代理 IP（解析 X-Forwarded-For） | 未设置 | 反代部署 |
| `ILINK_FORCE_HTTPS` | 按 HTTPS 语义设置 Secure Cookie/HSTS | 未设置 | 上游已真实启用 HTTPS 时 |
| `ILINK_WEBDAV_PRIVATE_ALLOWLIST` | 允许访问的私网 WebDAV 主机/IP（逗号分隔） | 未设置 | 自建内网 WebDAV |
| `ILINK_ALLOWED_ORIGINS` | 跨域白名单（逗号分隔） | 自动按端口推导 | 自定义域名 |
| `ILINK_ALLOW_INSECURE_PUBLIC` | 公网绑定无 TLS 时跳过启动守卫 | 未设置 | 仅内网调试 |
| `ILINK_CLI_TRUST` | CLI 跳过 S10 二次身份确认 | 未设置 | 自动化脚本 |
| `ILINK_AUDIT_RETENTION_DAYS` | 审计日志保留天数 | `90` | 合规要求 |
| `ILINK_MEDIA_CACHE_MAX_GB` | 单用户媒体缓存上限（GB） | `5.0` | 调整磁盘占用 |
| `ILINK_MEDIA_CACHE_PURGE_INTERVAL_HOURS` | 媒体缓存清理周期（小时） | `6` | — |
| `ILINK_LOG_FILTER` | 完整 EnvFilter 指令 | `ilink_wm1=info` | 调排障日志 |
| `ILINK_LOG_LEVEL` | 仅设 ilink_wm1 模块级别 | `info` | 简化日志控制 |
| `RUST_LOG` | 标准日志覆盖（非 server_mode 生效） | — | 兼容老式调用 |

### 1.7 启动验证

向导完成后浏览器访问：

```
http://127.0.0.1:8888        # 本机
http://<局域网IP>:8888        # 同一网络其他设备（若选了 0.0.0.0）
```

若见登录页即服务正常。健康检查端点：

```bash
curl http://127.0.0.1:8888/healthz
# 期望返回 200 OK
```

### 1.8 优雅关闭

- 终端按 `Ctrl+C`（或 `kill <pid>`）触发优雅关闭
- 服务会：停止 owner bot → 卸载所有用户的 bot → flush 配额 → web drain（最多 10 秒）
- 切勿 `kill -9` 除非卡死，可能丢失最近未持久化的配额计数

---

## 二、第一章 · 用户使用指南

本章面向已注册的普通用户。读完本章可完成账号注册、登录、扫码绑定微信、收发消息、设置与退出全部流程。

### 2.1 注册账号

注册方式由管理员配置：

- **开放注册**：任何人都可注册，无需邀请码。
- **邀请码注册**（默认）：必须填邀请码才能注册。
- **两者都关**：注册入口显示"注册已关闭，请联系管理员获取账号"。

注册步骤：

1. 在登录页点"注册"或直接访问 `/auth?mode=register`。
2. 阅读使用守则并勾选"我已阅读"。
3. 填写：
   - 用户名：3-32 位，仅字母、数字、下划线、连字符
   - 密码：8-128 位，必须含 **大写字母 + 小写字母 + 数字** 三种；系统内置弱口令黑名单（如 `password123`、`admin123`）
   - 确认密码
   - 邀请码（若开启邀请码注册）
4. 提交后自动登录并跳转到聊天页。

> 同一 IP 5 分钟内最多注册 5 次，超出会提示"请求过于频繁"。

### 2.2 登录

1. 访问 `/auth` 或首页点"登录"。
2. 输入用户名 + 密码。
3. 勾选"记住我"可启用设备令牌，下次自动登录（最长 30 天）。
4. 登录失败统一提示"用户名或密码错误"（不区分用户名是否存在，防探测）。

忘记密码：联系管理员用 CLI 重置：

```bash
ilink-wm1 admin user reset-password <用户名>
```

### 2.3 扫码绑定微信

首次登录后会进入"扫码连接"页面：

1. 页面自动调用 `/api/wasm/qrcode` 获取微信登录二维码。
2. 用微信扫码，在手机上确认。
3. **必须**让微信端发送一条消息给扫码账号，才能完成会话建立。
4. 绑定成功后页面跳转到聊天列表。

> 二维码有效期约 60 秒，过期点"刷新二维码"。
> 一个用户可绑定多个微信会话，在"用户"标签页查看与切换。

### 2.4 聊天界面

聊天界面分 4 区：

| 区域 | 作用 |
|------|------|
| 顶部标题栏 | 显示当前会话名、连接状态、菜单（设置备注名 / 导出聊天记录） |
| 消息区 | 滚动显示历史与新消息，媒体消息点击可查看/下载 |
| 输入区 | 文本输入框 + `+` 按钮（媒体）+ 发送按钮 |
| 底部 Tab | 列表 / 用户 / 设置 |

**消息类型**：

- 文字消息：直接输入发送
- 图片：点 `+` → 相册 / 拍摄
- 视频：点 `+` → 视频
- 文件：点 `+` → 文件

**已读未读**：消息是否送达取决于微信端，本系统不伪造已读。

### 2.5 多会话管理

底部 Tab 切到"用户"：

- 已绑定的微信会话列表（按添加时间排序）
- 点 `+` 添加新会话（再次扫码）
- 长按或点会话右侧菜单可：
  - 设置备注名（仅本地显示，不影响微信）
  - 删除会话（仅清本地记录，不解绑微信）
  - 切换为当前会话（消息收发切换到该会话）

### 2.6 历史消息

- 进入任一会话自动加载最近消息，向上滚动加载更早的（分页）。
- 聊天标题栏菜单 → "导出聊天记录" 可导出为 HTML 文件（含图片/视频/文件链接）。
- 单条消息可删除（仅本地），不影响微信端。

### 2.7 个人设置

底部 Tab → "设置"：

#### 2.7.1 主题

- 切换深色 / 浅色 / 跟随系统
- 自动写入 localStorage，刷新后保留

#### 2.7.2 WebDAV 存储

把媒体文件存到 WebDAV 服务器（如坚果云、Nextcloud、自建 WebDAV），节省服务器磁盘。

安全策略默认拒绝回环、私网、链路本地与其他特殊地址，以防 WebDAV URL 被用于访问服务器内网。若确需连接自建内网 WebDAV，请由部署管理员设置精确白名单，例如：

```bash
ILINK_WEBDAV_PRIVATE_ALLOWLIST=dav.internal.example,192.168.1.20
```

白名单只接受精确主机名或 IP，不接受通配符；仍建议使用 HTTPS 和独立的最小权限账号。

填写：

| 字段 | 说明 |
|------|------|
| 启用 WebDAV | 总开关 |
| 服务地址 | 如 `https://dav.jianguoyun.com/dav/` |
| 用户名 | WebDAV 账号 |
| 密码 | WebDAV 应用密码（坚果云等需单独生成） |
| 远程目录 | 如 `/ilink-media/`，不存在会自动创建 |
| 省流量模式 | 开启后聊天界面不自动加载媒体，需手动点击 |
| 保存时自动迁移 | 保存配置后把本地已有媒体上传到 WebDAV |

操作：

- "测试连接" 验证凭据与目录可访问
- "保存" 写入用户配置（密码加密存储）
- "迁移本地媒体到 WebDAV" 手动触发一次迁移，可看进度条

#### 2.7.3 账号安全

- **修改密码**：填旧密码 + 新密码 + 确认新密码，可选"同时登出其他设备"
- **退出登录**：可选"退出时撤销记住我令牌"（更安全，下次需重输密码）

#### 2.7.4 关于

显示作者、修改者、版本号。

### 2.8 退出登录

设置 → 账号安全 → 退出登录。

- 仅退出当前设备：保留"记住我"令牌，下次自动登录
- 撤销所有设备令牌：所有设备下次都需重输密码（推荐在公共设备上使用）

### 2.9 收到全局通知

管理员可发全局通知（info / warn / error 三级）：

- 通知会显示在聊天列表与聊天页顶部的通知条
- info 级蓝色、warn 级橙色、error 级红色
- 管理员清除通知后，下次刷新页面消失

### 2.10 常见问题

| 现象 | 原因 / 处理 |
|------|------------|
| 二维码不显示 | 检查服务端到微信服务器的网络；F12 看 `/api/wasm/qrcode` 是否 200 |
| 扫码后无反应 | 必须在微信端发一条消息才会建立会话；重新扫码 |
| 消息发不出 | 看顶部连接状态条；网络断开会自动重连，重连后重试 |
| 媒体显示裂图 | 检查 WebDAV 配置；或关省流量模式后重试 |
| 登录后立即被登出 | 管理员重置了你的密码，或你勾选了"登出其他设备" |
| 注册提示"请求过于频繁" | 同 IP 5 分钟内注册超过 5 次，等 5 分钟再试 |

---

## 三、第二章 · 管理员管理指南

本章面向 owner / admin 角色。读完本章可完成用户管理、邀请码、IP 封禁、隧道、审计、备份、公网部署全部运维任务。

### 3.1 角色与权限

| 角色 | 能做什么 |
|------|---------|
| `owner` | 系统最高权限，可执行所有 CLI 与 Web 管理操作；不可被删除或禁用（至少保留 1 个 active owner） |
| `admin` | 可访问 Web 管理面板与 `/api/admin/*`，可管理用户与邀请码；不可删除 owner |
| `user` | 普通用户，仅能用聊天功能 |

Web 管理面板（`/admin`）的访问策略由 `admin.web_access` 决定，三档：

| 值 | 行为 |
|----|------|
| `off` | 完全关闭前端管理面板，只能用 CLI |
| `intranet` | 仅内网 IP 可访问（默认，最安全） |
| `open` | 公网可访问，但仍需登录 session |

切换策略：

```bash
ilink-wm1 admin webset set intranet    # 或 off / open
```

### 3.2 CLI 命令体系（核心）

所有 CLI 命令以 `ilink-wm1 admin <sub>` 形式调用，**不启动 Web 服务**，操作完即退出。需要在二进制所在目录或加入 PATH。

破坏性命令（删除用户 / 重置密码 / 封禁 IP / 写敏感配置 / 改 webset）会触发 **S10 二次身份确认**：要求输入任意 owner/admin 的用户名 + 密码。

> 自动化场景可设 `ILINK_CLI_TRUST=1` 跳过，但 **禁止在共享终端或多用户服务器上设置**，否则任何本机用户均可无交互运行破坏性命令。

#### 3.2.1 初始化

```bash
# 首次初始化 owner 账号（system.db 无用户时可用）
ilink-wm1 admin init
```

#### 3.2.2 用户管理

```bash
ilink-wm1 admin user list                              # 列出所有用户
ilink-wm1 admin user create <username> [role]          # role: owner/admin/user（默认 user）
ilink-wm1 admin user delete <username|uid>             # 删除（不可删最后一个 owner）
ilink-wm1 admin user disable <username|uid>            # 禁用（status → disabled，无法登录）
ilink-wm1 admin user enable <username|uid>             # 启用
ilink-wm1 admin user reset-password <username|uid>     # 重置密码（其他设备会话立即失效）
ilink-wm1 admin user set-quota <user> <key> <value>    # 设配额
ilink-wm1 admin user set-feature <user> <key> on|off   # 设功能开关
ilink-wm1 admin user set-email <user> <email>          # 设邮箱
```

配额 `key`：

| key | 含义 | value 语义 |
|-----|------|-----------|
| `quota_upload_bytes` | 每日上传流量（字节） | `0` = 系统默认；负数 = 无限制；正数 = 每日上限 |
| `quota_download_bytes` | 每日下载流量 | 同上 |
| `quota_media_bytes` | 媒体存储总容量 | 同上 |
| `quota_msg_per_day` | 每日发消息数 | 同上 |
| `quota_media_count` | 媒体文件总数 | 同上 |

> 想完全禁止某维度，**不要**用 `set-quota 0`（0 = 系统默认，系统默认未设时 = 无限制），改用 `set-feature ... off`。

功能 `key`：

| key | 含义 |
|-----|------|
| `allow_upload` | 允许上传媒体 |
| `allow_webdav` | 允许配置 WebDAV |
| `allow_custom_webdav` | 允许自定义 WebDAV（vs 强制用服务器 WebDAV） |

#### 3.2.3 邀请码

```bash
ilink-wm1 admin invite create [days] [note]   # days=0 永久，默认 30
ilink-wm1 admin invite list                   # 列出所有
ilink-wm1 admin invite revoke <code>          # 撤销
```

邀请码 4 位大写字母+数字组合（如 `A3F5`）。

#### 3.2.4 系统配置

```bash
ilink-wm1 admin config get <key>              # 读取（敏感 key 显示 ***）
ilink-wm1 admin config set <key> <value>      # 写入（敏感 key 触发 S10 确认）
ilink-wm1 admin config list                   # 列出所有
```

常用 `key`：

| key | 含义 |
|-----|------|
| `site_name` | 站点名 |
| `allow_open_registration` | 开放注册（`on`/`off`） |
| `allow_invite_registration` | 邀请码注册（`on`/`off`） |
| `terms_version` | 守则版本号 |
| `terms_text` | 守则正文（Markdown） |
| `terms.url` | 守则外链（如飞书文档） |
| `docs.url` | 用户文档外链（首页"文档"按钮指向） |
| `admin.web_access` | 前端管理面板访问策略 |
| `default_quota_upload_bytes` 等 | 系统默认配额（新用户继承） |
| `default_allow_upload` 等 | 系统默认功能开关 |

#### 3.2.5 服务器数据目录

```bash
ILINK_DATA_DIR=/absolute/path ilink-wm1           # 必须在启动前设置绝对路径
ilink-wm1 admin server-storage show               # 查看当前实际目录
```

> `server-storage set-local` 已废弃，因为运行时数据目录必须在打开数据库前确定。每个用户可在设置页配置自己的 WebDAV。

#### 3.2.6 使用守则

```bash
ilink-wm1 admin terms set-version <version>       # 设版本号（如 1.1）
ilink-wm1 admin terms set-text                    # 从 stdin 读守则文本
```

`set-text` 用法：

```bash
cat /path/to/terms.md | ilink-wm1 admin terms set-text
# 或交互式粘贴，按 Ctrl+D（Unix）/ Ctrl+Z+Enter（Windows）结束
```

#### 3.2.7 系统统计

```bash
ilink-wm1 admin stats
```

输出用户总数、active/disabled 数、owner/admin 数、邀请码数、配置项数、审计日志总数与最近活动 Top 5。

#### 3.2.8 IP 封禁

```bash
ilink-wm1 admin ip ban <ip> [--reason <r>] [--days <n>]   # days=0 永久，默认 7
ilink-wm1 admin ip unban <ip>                              # 解封
ilink-wm1 admin ip list                                    # 列出所有
```

危险封禁目标（`0.0.0.0/0`、`::/0`、`127.0.0.0/8`、内网网段）会强制二次确认。CIDR 前缀范围：IPv4 `/0 ~ /32`，IPv6 `/0 ~ /128`。

#### 3.2.9 内网穿透（隧道）

通过 [serveo.net](https://serveo.net) SSH 反向隧道把本地服务暴露到公网。

```bash
ilink-wm1 admin tunnel start [--port <p>] [--remote <p>] [--subdomain <s>]   # 启动
ilink-wm1 admin tunnel stop                                                  # 停止
ilink-wm1 admin tunnel status                                                # 查看状态
ilink-wm1 admin tunnel logs [count]                                          # 查看日志（默认 20 行）
```

默认：本地端口 8888、远程端口 80、子域名随机。

> ⚠ 此功能仅适合测试，不建议长期使用。开启后公网任何人可通过 URL 访问；管理员面板将无法保持内网访问。如需保证安全，请在后端关闭此功能（不启动或 `admin config set feature.tunnel off`）。

#### 3.2.10 审计日志

```bash
ilink-wm1 admin audit list [limit] [--action <a>] [--actor <a>]   # 列出最近 N 条（默认 50）
ilink-wm1 admin audit stats [limit]                                # 分组统计（默认 1000）
```

可按 action（如 `login`、`admin.user.create`）或 actor（如 `cli`、`uid=1`）过滤。

审计日志默认保留 90 天，可用 `ILINK_AUDIT_RETENTION_DAYS` 调整（1-3650）。启动时清理一次，之后每 24 小时清理一次。

#### 3.2.11 前端管理面板访问策略

```bash
ilink-wm1 admin webset show                       # 查看
ilink-wm1 admin webset set <off|intranet|open>    # 设置（即时生效）
```

#### 3.2.12 全局通知广播

```bash
ilink-wm1 admin broadcast send [info|warn|error] <message>   # 发送
ilink-wm1 admin broadcast clear                               # 清除
ilink-wm1 admin broadcast show                                # 查看
```

> CLI 广播不实时推 WebSocket，在线用户下次刷新页面才能看到。要实时推送，用 REPL `/broadcast` 命令。

### 3.3 REPL 交互命令

仅在交互模式（非 `--no-repl`、非 `ILINK_SERVER_MODE`）下可用。启动后终端输入：

| 命令 | 作用 |
|------|------|
| `/help` | 显示所有命令 |
| `/set` | 打开设置菜单（站点名 / 邀请码 / 注册策略 / 守则 / 端口 / 环境变量） |
| `/users` | 列出所有 Web 注册用户 |
| `/notify <用户名> <消息>` | 给指定 Web 用户发系统通知（私信） |
| `/broadcast <info\|warn\|error> <消息>` | 全局广播（实时推 WebSocket） |
| `/web` | 在默认浏览器打开网页聊天界面 |
| `/webset` | 切换前端管理访问策略（off/intranet/open） |
| `/quit` | 退出服务 |

### 3.4 Web 管理面板（`/admin`）

浏览器访问 `/admin`（需 `admin.web_access` 允许当前 IP）。

#### 3.4.1 用户管理

- 创建用户：填用户名 + 密码 + 角色（user / admin）→ 点"创建用户"
- 搜索：按用户名 / 邮箱 / UID 过滤
- 批量操作：勾选多行 → 批量禁用 / 启用 / 删除
- 单行操作：禁用 / 启用 / 删除 / 重置密码 / 设配额 / 设功能 / 设邮箱
- 列：UID、用户名、角色、状态、邮箱、注册时间、上传流量、下载流量、媒体占用、机器人状态、操作

#### 3.4.2 邀请码

- 生成邀请码：可选有效期与备注
- 搜索：按邀请码 / 创建者 / 备注
- 批量撤销：勾选多行 → 批量撤销
- 单行操作：撤销

#### 3.4.3 IP 封禁

- 封禁 IP：填 IP + 原因 + 天数（0 = 永久）
- 搜索：按 IP / 原因
- 批量解封：勾选多行 → 批量解封

#### 3.4.4 系统设置

可视化编辑所有可写 `system_settings` key（与 CLI `admin config` 同源）。每项有中文描述，避免回查文档。

#### 3.4.5 审计日志

- 搜索：按用户 / 操作 / IP
- 日期范围筛选
- 导出 CSV / JSON
- 列：时间、用户、操作、详情、IP

#### 3.4.6 系统统计

显示用户/邀请码/配置/审计概览，以及系统资源（CPU、内存、磁盘、运行时长）。

#### 3.4.7 内网穿透

Web 版隧道管理（与 CLI `admin tunnel` 等价）。⚠ 同 CLI 警告：仅测试用。

#### 3.4.8 全局通知

Web 版广播（与 CLI `admin broadcast` 等价，但通过 WebSocket 实时推送）。

### 3.5 公网部署（生产环境）

**禁止**直接把 `ILINK_HOST=0.0.0.0` 暴露到公网而无 TLS。服务启动时会强制检查：公网绑定 + 无可信反代 + 无强制 HTTPS → 拒绝启动（除非设 `ILINK_ALLOW_INSECURE_PUBLIC=1`，仅限内网调试）。

正确做法：前置反向代理终止 TLS，再设 `ILINK_TRUSTED_PROXIES`。

#### 3.5.1 Nginx 反代示例

```nginx
server {
    listen 443 ssl http2;
    server_name ilink.example.com;

    ssl_certificate     /etc/letsencrypt/live/ilink.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/ilink.example.com/privkey.pem;

    # 关键：保留 Host 与 X-Forwarded-For
    location / {
        proxy_pass http://127.0.0.1:8888;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # WebSocket
    location /api/ws {
        proxy_pass http://127.0.0.1:8888;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 86400;
    }

    # 上传：放宽大小限制到 100MB（与服务端一致）
    client_max_body_size 100M;
}

server {
    listen 80;
    server_name ilink.example.com;
    return 301 https://$host$request_uri;
}
```

#### 3.5.2 Caddy 反代示例（自动 TLS）

```caddy
ilink.example.com {
    reverse_proxy 127.0.0.1:8888
    # Caddy 自动签发 Let's Encrypt 证书
}
```

#### 3.5.3 服务端环境变量

```bash
# systemd 环境文件 /etc/ilink/env
ILINK_HOST=127.0.0.1               # 仅监听本机，让反代转发
ILINK_PORT=8888
ILINK_TRUSTED_PROXIES=127.0.0.1    # 反代 IP（多个用逗号分隔）
ILINK_ALLOWED_ORIGINS=https://ilink.example.com
ILINK_SERVER_MODE=1                # 启用服务器模式（日志落盘 + 关闭向导）
ILINK_DATA_DIR=/var/lib/ilink      # 数据集中存储
```

#### 3.5.4 systemd 服务

```ini
# /etc/systemd/system/ilink.service
[Unit]
Description=Zyn iLink ChatBox · WongMod
After=network.target

[Service]
Type=simple
User=ilink
WorkingDirectory=/opt/ilink
EnvironmentFile=/etc/ilink/env
ExecStart=/opt/ilink/ilink-wm1
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

启用：

```bash
sudo useradd -r -s /usr/sbin/nologin ilink
sudo mkdir -p /opt/ilink /var/lib/ilink /var/log/ilink
sudo chown ilink:ilink /opt/ilink /var/lib/ilink /var/log/ilink
# 把二进制、web/、env 文件放好
sudo systemctl daemon-reload
sudo systemctl enable --now ilink
sudo systemctl status ilink
sudo journalctl -u ilink -f
```

#### 3.5.5 Docker 部署（示例 Dockerfile）

```dockerfile
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/ilink-wm1 /app/
COPY --from=builder /app/web /app/web
ENV ILINK_DATA_DIR=/data
ENV ILINK_SERVER_MODE=1
VOLUME ["/data"]
EXPOSE 8888
ENTRYPOINT ["/app/ilink-wm1"]
```

构建与运行：

```bash
docker build -t ilink-wm1 .
docker run -d --name ilink \
  -p 127.0.0.1:8888:8888 \
  -v ilink-data:/data \
  -e ILINK_OWNER_USER=owner \
  -e ILINK_OWNER_PASSWORD='StrongPass123' \
  ilink-wm1
```

容器内首次启动会自动用 `ILINK_OWNER_USER` / `ILINK_OWNER_PASSWORD` 创建 owner，跳过交互向导。

### 3.6 配额与功能开关

系统级默认（新用户继承）：

```bash
ilink-wm1 admin config set default_quota_upload_bytes 1073741824     # 1GB/天
ilink-wm1 admin config set default_quota_download_bytes 5368709120   # 5GB/天
ilink-wm1 admin config set default_quota_media_bytes 10737418240     # 10GB 总
ilink-wm1 admin config set default_quota_msg_per_day 1000            # 1000 条/天
ilink-wm1 admin config set default_quota_media_count 5000            # 5000 个文件
ilink-wm1 admin config set default_allow_upload on
ilink-wm1 admin config set default_allow_webdav on
ilink-wm1 admin config set default_allow_custom_webdav off           # 强制用服务器 WebDAV
```

单用户覆盖（见 §3.2.2）：

```bash
ilink-wm1 admin user set-quota alice quota_upload_bytes 524288000    # 500MB
ilink-wm1 admin user set-feature alice allow_upload off              # 禁止上传
```

### 3.7 数据备份与迁移

#### 3.7.1 在线备份

服务运行时直接复制 `system.db` 与 `users/` 目录到备份位置。SQLite 在 WAL 模式下支持热备份，但建议先停服务再备份以确保一致性。

#### 3.7.2 sqlite3 命令备份（推荐）

```bash
sqlite3 system.db ".backup /backup/system-$(date +%Y%m%d).db"
# 每个用户库同理
sqlite3 users/1/user.db ".backup /backup/user-1-$(date +%Y%m%d).db"
```

#### 3.7.3 老库迁移

如果是从旧版 `wechat_bot.db`（单用户）升级到多用户版，启动时自动迁移：

- 老库的 `web_password` → owner 账号
- 老库数据 → `users/<owner_uid>/user.db`
- 老库媒体缓存 → `users/<owner_uid>/media_cache/`
- 老库改名 `wechat_bot.db.bak`（保留不删）

迁移幂等，可重复执行。失败时控制台输出 `[MIGRATION] 老库迁移失败: <原因>`，需检查后重试或删除老库。

#### 3.7.4 数据目录迁移

```bash
# 1. 停服务
# 2. 复制整个数据目录到新位置
cp -r /old/path/* /new/path/
# 3. 改启动环境变量
export ILINK_DATA_DIR=/new/path
# 4. 重启
```

### 3.8 故障排查

#### 3.8.1 启动失败

| 现象 | 处理 |
|------|------|
| `[FATAL] system.db 初始化失败` | 检查磁盘空间、文件权限、数据库文件是否损坏 |
| `[安全阻断] 公网绑定但未配置 TLS` | 上游已启用真实 HTTPS 时同时设置 `ILINK_TRUSTED_PROXIES` 与 `ILINK_FORCE_HTTPS=1`；仅受信内网可显式设 `ILINK_ALLOW_INSECURE_PUBLIC=1` |
| `[安全阻断] 公网绑定但未创建任何用户账号` | 先 `ILINK_HOST=127.0.0.1` 启动 → `ilink-wm1 admin init` → 再切公网 |
| 端口被占用 | `lsof -i :8888`（Linux）/ `netstat -ano \| findstr 8888`（Windows），改 `ILINK_PORT` 或杀占用进程 |

#### 3.8.2 数据库损坏

```bash
# 备份后尝试修复
sqlite3 system.db "PRAGMA integrity_check;"
sqlite3 system.db ".recover" > recovered.sql
mv system.db system.db.broken
sqlite3 system.db < recovered.sql
```

#### 3.8.3 日志位置

- 交互模式：标准输出
- 服务器模式（`ILINK_SERVER_MODE=1`）：当前目录 `ilink.log.YYYY-MM-DD`（按天滚动）+ 标准输出

调排障日志：

```bash
# 详细到消息收发链路
ILINK_LOG_LEVEL=debug ./ilink-wm1

# 或完整 EnvFilter
ILINK_LOG_FILTER="ilink_wm1=debug,reqwest=warn" ./ilink-wm1
```

#### 3.8.4 owner 被锁出

场景：忘记 owner 密码，且没设其他 admin。

恢复：

```bash
# 在二进制所在机器上
ilink-wm1 admin user reset-password owner
# S10 二次身份确认会要求输入 owner/admin 凭据；此时无 admin 可用，
# 临时设 ILINK_CLI_TRUST=1 跳过（仅限受控本机！）
ILINK_CLI_TRUST=1 ilink-wm1 admin user reset-password owner
```

> ⚠ 跳过后立即改回（`unset ILINK_CLI_TRUST`），并在审计日志中确认操作记录。

#### 3.8.5 媒体缓存膨胀

每个用户默认上限 5 GB，超出按 LRU 删除最老的。手动调整：

```bash
ilink-wm1 admin config set --help   # 注意：此 key 不是可写白名单内的，需用环境变量
# 重启服务时设
ILINK_MEDIA_CACHE_MAX_GB=2 ./ilink-wm1     # 每用户 2GB
```

启动时执行一次清理，之后每 6 小时执行一次（可用 `ILINK_MEDIA_CACHE_PURGE_INTERVAL_HOURS` 调整）。

### 3.9 安全建议清单

- [ ] 公网部署必走反代 + TLS，禁用 `ILINK_ALLOW_INSECURE_PUBLIC=1`
- [ ] 设强 owner 密码，并定期轮换（CLI 重置）
- [ ] `admin.web_access` 默认 `intranet`，确需公网管理才改 `open`
- [ ] 注册默认走邀请码模式（`allow_open_registration=off`）
- [ ] 审计日志保留天数按合规要求调整（`ILINK_AUDIT_RETENTION_DAYS`）
- [ ] 定期备份 `system.db` 与 `users/` 目录
- [ ] 删除用户后确认 `pending_user_cleanup` 队列已清理（CLI 30 分钟重试一次，最多 100 次）
- [ ] 危险 IP 段封禁前先用 `admin ip list` 复核
- [ ] 不在共享终端设 `ILINK_CLI_TRUST=1`
- [ ] 容器化部署用 `LoadCredential` / Docker secret 保护 `ILINK_OWNER_PASSWORD`，勿写入 shell history

### 3.10 升级与回滚

#### 3.10.1 升级

```bash
# 1. 拉新代码
git pull
# 2. 重新编译
cargo build --release
# 3. 停服务
sudo systemctl stop ilink   # 或 Ctrl+C
# 4. 替换二进制（web/ 目录若有更新也一并替换）
cp target/release/ilink-wm1 /opt/ilink/
cp -r web/* /opt/ilink/web/
# 5. 重启
sudo systemctl start ilink
```

> 启动时自动执行数据库 schema 迁移（幂等），无需手动操作。

#### 3.10.2 回滚

```bash
# 1. 停服务
# 2. 用旧二进制替换
# 3. 从备份恢复 system.db 与 users/
# 4. 重启
```

> 跨大版本回滚可能因 schema 不兼容失败，建议升级前完整备份。

---

## 四、附录

### 4.1 路由总览

| 路径 | 方法 | 鉴权 | 说明 |
|------|------|------|------|
| `/` | GET | 公开 | 首页（landing） |
| `/auth` | GET | 公开 | 登录/注册页 |
| `/chat` | GET | 登录 | 聊天主界面 |
| `/admin` | GET | 登录 + admin + IP 守卫 | 管理面板 |
| `/terms` | GET | 公开 | 使用守则页 |
| `/healthz` | GET | 公开 | 健康检查 |
| `/api/wasm/login` | POST | 公开 | 登录 |
| `/api/wasm/register` | POST | 公开 | 注册 |
| `/api/wasm/terms` | GET | 公开 | 守则文本+版本 |
| `/api/wasm/guide` | GET | 公开 | 使用与管理指南（读部署包内 `部署指南.md`） |
| `/api/wasm/links` | GET | 公开 | `docs_url` / `terms_url` 外链配置 |
| `/api/wasm/site-info` | GET | 公开 | 站点名+版本 |
| `/api/wasm/notification` | GET | 公开 | 当前全局通知 |
| `/api/wasm/auto-login` | POST | 公开 | 设备令牌自动登录 |
| `/api/wasm/qrcode` | GET | 登录 | 获取微信登录二维码 |
| `/api/wasm/messages` | GET | 登录 | 拉取历史消息 |
| `/api/wasm/send` | POST | 登录 | 发送文字消息 |
| `/api/wasm/send-media` | POST | 登录 | 发送媒体消息 |
| `/api/wasm/upload-media` | POST | 登录 | 上传媒体 |
| `/api/wasm/media/:cache_key` | GET | 登录 | 获取媒体 |
| `/api/wasm/users` | GET | 登录 | 会话列表 |
| `/api/wasm/webdav-*` | GET/POST | 登录 | WebDAV 配置/测试/迁移 |
| `/api/wasm/export-history` | POST | 登录 | 导出聊天记录为 HTML |
| `/api/wasm/logout` | POST | 登录 | 登出 |
| `/api/wasm/set-password` | POST | 登录 | 修改自己密码 |
| `/api/wasm/device-tokens` | GET | 登录 | 设备令牌列表 |
| `/api/wasm/device-token-revoke` | POST | 登录 | 撤销设备令牌 |
| `/api/admin/*` | GET/POST | 登录 + admin + IP 守卫 | 管理员 API |
| `/api/ws` | GET | 公开（升级后校验） | WebSocket |
| `/static/*` | GET | 公开 | 前端静态资源（强制 no-cache） |

### 4.2 system_settings 全量 key 参考

> 此表列出常见 key，敏感 key 在 CLI 与 Web 中均脱敏显示。

| key | 类型 | 默认 | 说明 |
|-----|------|------|------|
| `site_name` | str | `Zyn iLink ChatBox · WongMod` | 站点名 |
| `allow_open_registration` | `on`/`off` | `off` | 开放注册 |
| `allow_invite_registration` | `on`/`off` | `on` | 邀请码注册 |
| `admin.web_access` | `off`/`intranet`/`open` | `intranet` | 管理面板访问策略 |
| `terms_version` | str | `1.0` | 守则版本 |
| `terms_text` | str | 内置 v1.0 | 守则正文（Markdown） |
| `terms.url` | str | 空 | 守则外链 |
| `docs.url` | str | 空 | 用户文档外链（首页"文档"按钮） |
| `default_quota_upload_bytes` | int | 空 | 新用户上传配额默认 |
| `default_quota_download_bytes` | int | 空 | 新用户下载配额默认 |
| `default_quota_media_bytes` | int | 空 | 新用户媒体存储默认 |
| `default_quota_msg_per_day` | int | 空 | 新用户消息数默认 |
| `default_quota_media_count` | int | 空 | 新用户媒体数默认 |
| `default_allow_upload` | bool | `on` | 新用户允许上传 |
| `default_allow_webdav` | bool | `on` | 新用户允许 WebDAV |
| `default_allow_custom_webdav` | bool | `on` | 新用户允许自定义 WebDAV |

### 4.3 术语表

| 术语 | 含义 |
|------|------|
| iLink 协议 | 微信官方账号登录与消息收发协议 |
| owner | 系统最高权限角色，不可删除 |
| admin | 管理员角色，可管理用户与配置 |
| user | 普通用户角色，仅用聊天功能 |
| session | 登录会话，HttpOnly Cookie + 服务端记录 |
| 设备令牌 | "记住我"功能的长效令牌，最多 30 天 |
| 邀请码 | 4 位大写字母+数字组合，用于邀请码注册 |
| 内网穿透 | 通过 serveo.net SSH 反向隧道暴露本地服务到公网 |
| S10 二次身份确认 | 破坏性 CLI 命令前要求输入 owner/admin 凭据 |
| 配额 | 用户每日/累计的流量、消息数、媒体数上限 |
| 审计日志 | 所有管理操作的不可篡改记录，默认保留 90 天 |
| LRU 清理 | 媒体缓存按 created_at 升序删除最老的，直到低于阈值 |

### 4.4 反馈与支持

- 🐛 **Bug 报告**：[GitHub Issues › New › Bug Report](https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod/issues/new?template=bug_report.md)
- 💡 **功能建议**：[GitHub Issues › New › Feature Request](https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod/issues/new?template=feature_request.md)
- 🔒 **安全漏洞**：[SECURITY.md](SECURITY.md)（**请勿公开 Issue**；走 GitHub Security Advisories 或维护者主页私密渠道）
- 💬 **使用问题**：先查本文档「故障排查」§3.8 + 搜索 [现有 Issue](https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod/issues?q=is%3Aissue)，仍无解再开新 Issue
- 🤝 **贡献代码**：见 [CONTRIBUTING.md](CONTRIBUTING.md)（含本地构建、commit 规范、PR 流程）
- 📦 **预编译下载**：[Releases](https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod/releases)（各平台 zip 附 `.sha256` 校验）
- 📖 **项目链接**：
  - 本仓库：<https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod>
  - 原仓库（Python 单文件版）：<https://github.com/zynsync/Zyn-iLink-ChatBox>
  - 协议生态参考：<https://github.com/openilink>
- 📐 **代码规范**：见项目根 `代码规范.md`

---

> 本文档由 Mr.Wong 维护，版本随项目主版本同步更新。
> 衍生/开发请标注原仓库 "https://github.com/zynsync/Zyn-iLink-ChatBox" 与原作者。
> 仓库受到开源证书保护！请合规使用！
