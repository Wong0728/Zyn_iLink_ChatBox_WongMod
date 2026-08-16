# Cloudflare IPv6 直连配置（cf-setup）

> 目标：**用户通过 IPv6 直连访问本机 ilink-wm 服务，业务数据不经过 Cloudflare**。
> Cloudflare 只承担"把好记的域名指向你不断变化的 IPv6 地址"这一件事（DNS / 一跳 302）。
> 前提：本机有**公网 IPv6**（家用宽带普遍下发 /64 前缀；验证见文末排障）。

## 一、两种模式怎么选

| | direct（推荐） | redirect（点名方案） |
|---|---|---|
| 原理 | DNS AAAA 记录直指本机 IPv6（灰云 DNS only） | 访问 `https://子域.域名` → CF 边缘返回 302 → `http(s)://[IPv6]:端口` |
| 浏览器地址栏 | `http://ilink.你的域名:8888`（显示域名） | 先显示域名，跳转后显示 `[2408:...]:8888` |
| 数据是否经 CF | 完全不经 | 不经（CF 只回一跳 302 响应） |
| 证书告警 | 无（http） | 入口无；落地若选 https 目标会有自签/IP 证书告警 |
| IPv6 变地址 | DDNS 自动改 AAAA 记录 | DDNS 自动改 AAAA + 302 目标 |
| 需要 Token 权限 | Zone/Zone Read + Zone/DNS Edit | 上述 + Zone/Rulesets Edit |

日常自用选 **direct**；如果你就是想要"输入 `https://xxx` 直接跳"的体验选 **redirect**，两种可以共存（用不同子域前缀各跑一次脚本）。

## 二、快速开始

### Windows
双击 `scripts\setup-cf.bat`（或在该目录开 PowerShell 执行）：

```powershell
powershell -ExecutionPolicy Bypass -File cf-setup.ps1
```

### Linux / macOS / Termux
依赖 `curl + jq`（Termux: `pkg install curl jq`）：

```bash
cd scripts && chmod +x cf-setup.sh && ./cf-setup.sh
```

### 脚本会依次做这些事
1. 让你在浏览器里创建 Cloudflare API Token（脚本会自动打开创建页面——未登录先登录你的 CF 账号，这就是"浏览器授权登录"步骤）。Token 权限照脚本提示勾选。
2. 列出你账号下的域名（zone）让你选，再输入**子域名前缀**（默认 `ilink`，最终是 `ilink.你的域名`）。
3. 自动检测本机公网 IPv6（排除内网/临时地址；多个地址让你选，也可 `--ip` 手动指定）。
4. 写入 AAAA 记录（direct 模式）或 AAAA + 302 跳转规则（redirect 模式）。
5. 保存 `scripts/cf-config.json`（含 token，已 chmod 600；Windows 下勿放网盘/共享目录）。
6. 安装定时 DDNS：Windows 计划任务 `ilink-cf-ddns`（每小时）/ Unix crontab `@hourly`——IPv6 前缀变化后自动刷新记录与跳转目标。
7. Windows 下提示/代加防火墙入站规则（`netsh advfirewall firewall add rule name="ilink-wm-8888" dir=in action=allow protocol=TCP localport=8888`）。

### 全自动（免交互，例）
```powershell
# PowerShell
.\cf-setup.ps1 -Mode direct -Token <API_TOKEN> -Zone example.com -Label ilink -Port 8888 -Yes
```
```bash
# Bash
CF_API_TOKEN=<t> ./cf-setup.sh --mode direct --zone example.com --label ilink --port 8888 --yes
```

### 手动刷新 DDNS（改完网络/换地址后）
```powershell
powershell -File cf-setup.ps1 -Ddns      # Windows
```
```bash
./cf-setup.sh --ddns                     # Linux/macOS/Termux
```

## 三、ilink-wm 服务端配套（重要）

1. **双栈监听**：启动时向导选 `3) 双栈访问 ([::])`，或设环境变量 `ILINK_HOST=[::]`
   （也接受裸 `::`）。默认 `127.0.0.1` / `0.0.0.0` 只收 IPv4，IPv6 访客进不来。
2. **公网绑定守卫**：绑 `[::]` 会被视为公网暴露，首次需
   `ILINK_ALLOW_INSECURE_PUBLIC=1`（http 直连场景）或配置 TLS 反代 +
   `ILINK_TRUSTED_PROXIES` + `ILINK_FORCE_HTTPS=1`。
3. **光猫/路由器 IPv6 防火墙**：多数设备默认拦截所有 IPv6 入站。需要在光猫
   （如中兴网关的"防火墙→攻击保护/入站过滤"）或路由器里放行入站 TCP 8888，
   或对内网设备做 IPv6 DMZ。这一步脚本无法代替。
4. **临时地址问题**：Windows 隐私扩展会周期换临时地址（脚本优先选稳定地址；
   若你的前缀本身常变，靠每小时 DDNS 兜底即可）。
5. 有公网 IPv6 直连后，serveo 隧道（管理面板"内网穿透"）可以停用——直连更快更稳。

## 四、Token 创建要点（手动版）

打开 <https://dash.cloudflare.com/profile/api-tokens> → Create Token → Create Custom Token：

- Permissions：
  - `Zone / Zone / Read`
  - `Zone / DNS / Edit`
  - `Zone / Rulesets / Edit`（仅 redirect 模式需要）
- Zone Resources：Include → Specific zone → 你的域名
- 创建后**只显示一次**，粘贴给脚本或 `CF_API_TOKEN` 环境变量。

## 五、排障

| 现象 | 原因/处理 |
|---|---|
| 脚本报"未检测到公网 IPv6" | 光猫桥接未开/运营商未下发（打运营商电话开 IPv6）；或网卡只有 fe80 链路本地地址。验证：`ipconfig`（Windows）看是否有 2/3 开头的全局地址 |
| 域名 ping 不通 / 打不开 | ① 光猫 IPv6 防火墙拦截（最常见）② Windows 防火墙未放行端口 ③ 服务没绑 `[::]`。逐层排查：本机 `curl http://[::1]:8888` → 局域网另一台 IPv6 设备访问 → 外网访问 |
| direct 模式打得开首页但登录跳回 | 检查服务是否真的在双栈监听；确认浏览器走的是 IPv6（cmd `ping -6 域名`） |
| redirect 模式跳到 `https://[IPv6]:端口` 报证书错误 | 预期行为：无 CA 为裸 IP/自签发证书。改用 http 目标（`-Scheme http`）或改用 direct 模式 |
| DDNS 任务没跑 | Windows：`schtasks /Run /TN ilink-cf-ddns` 手动触发看报错；Unix：看 `scripts/cf-ddns.log` |
| API 报 authentication error | Token 复制不完整/被撤销，重新创建并重跑脚本 |
| API 报 dns edits forbidden / not authorized | Token 权限缺 Zone/DNS Edit（redirect 另需 Rulesets Edit）或 Zone Resources 没包含目标域名 |

## 六、安全提示

- `cf-config.json` 内含 API Token：不要提交进仓库、不要放进网盘同步目录。仓库 `.gitignore` 已包含 `scripts/cf-config.json` 与 `scripts/cf-ddns.log`（不会被 git 跟踪），但仍请避免把该文件拷贝到仓库外的同步目录（I-3 修正：以 `.gitignore` 实际规则为准）。
- 直连暴露的是 ilink-wm Web 服务本身，务必：owner 强密码、保留登录限流/IP 封禁（本轮已支持 IPv6 与隧道真实 IP）、及时停用不用的 serveo 隧道。
