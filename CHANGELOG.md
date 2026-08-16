# 更新日志（CHANGELOG）

本项目版本号跟随上游原版节奏：`v<原版版本>-wm<重构序号>`。
当前版本：**v3.2.4-wm1.0**（Rust 重构第 1 轮发布，对应原版 v3.2.4 时代）。

---

## v3.2.4-wm1.0（2026-08-16）

相比上一版（v3.2.3 打包轮次），本版本完成了一轮完整安全审计并修复 11 项中危中的 9 项，另有个别功能增强。全部细节见根目录《安全审计报告_2026-08-16.md》。

### 安全修复（M-1 ~ M-11）

- **M-2（已修复）**：WebDAV 与 webhook 出站客户端禁用 HTTP 重定向跟随（`redirect(Policy::none())`），封堵 302 跳转绕过 SSRF 防护打内网（连带修复 L-4）。
- **M-3（已修复）**：CLI `admin user create` 创建 owner/admin 角色时强制 `confirm_admin_identity` 二次身份确认（系统尚无管理员时保留自举豁免）。
- **M-4（已修复）**：内网穿透新增本地端口白名单（默认仅放行本服务端口，`ILINK_TUNNEL_ALLOW_PORTS` 显式扩展）；CLI `tunnel start` 纳入 S10 二次确认。
- **M-5（已修复）**：webhook payload 改发真实 `bot_id`，不再外发完整 bot_token。
- **M-7（已修复）**：Windows 主密钥文件经 DPAPI 机器作用域包装（`DPAPI1` 格式），旧明文密钥自动迁移；Unix 侧改 0600 原子创建（连带消除 L-7）。
- **M-8（已修复）**：`/static/admin.html`、`/static/zn-admin.js` 收敛到与 `/admin` 相同的 `admin.web_access` 访问策略。
- **M-9（已修复）**：`real_client_ip` 检测「loopback 直连携带 X-Forwarded-For」的危险形态并节流告警，给出 `ILINK_TRUSTED_PROXIES` 配置指引。
- **M-10（已修复）**：`cf-setup.ps1` 写入 cf-config.json 后立即用 `icacls` 收紧 ACL，仅当前用户可读。
- **M-11（已修复）**：Windows 服务改用低权虚拟账户 `NT SERVICE\ilink-wm1` 运行（NSSM），并按需授权数据/日志目录写权限。
- **M-1 / M-6**：经业主评估**接受为已知风险**，暂不修改（4 位邀请码熵不足；详见审计报告）。

### 新增

- `scripts/`：Cloudflare 隧道/DDNS 一键配置脚本（`cf-setup.ps1` / `cf-setup.sh` / `setup-cf.bat`，说明见 `scripts/CF-SETUP.md`）。
- `deploy/`：各平台一键安装脚本，安装后以 **`iLinkWM`** 命令统一操控（启动、装/卸服务、自更新、卸载）。
- `docs/`：GitHub Pages Hero 宣传页（含与原版对比）。
- `reference/`：收录原版最新 Python 单文件实现（v3.2.4）作对照参考。

### 变更

- 版本号自 `3.1.9-wm1.0` 提升至 `3.2.4-wm1.0`（`Cargo.toml`、`src/config.rs`）。
- 落地页开源协议标注由 MIT 更正为 Apache-2.0（与 LICENSE 文件一致）。

### 第三轮（2026-08-17）：低危清零 + 命令行增强

**安全**：L-1 ~ L-24 全部处理完毕（修复 18、随前轮连带修复 2、协议/架构约束经评估接受 4：L-1 / L-5 / L-6 / L-22），逐项明细见《安全审计报告_2026-08-16.md》〇章第三轮速览。要点：token 线程标识 SHA-256 化（L-3）、webhook 投递前 DNS 钉扎（L-4）、迁移跳过表落盘报告（L-8）、导出文件名消毒（L-9）、`ILINK_FFMPEG_PATH` 钉死 ffmpeg（L-10）、删除用户失败不再吞错（L-11）、登录/注册 Origin 校验（L-12）、voice 上传修复（L-13）、内部错误不外泄（L-14）、审计 JSON serde 转义（L-15）、CSP 补 object-src（L-16）、外链 scheme 二次校验（L-17）、前端三处防御（L-18）、`ILINK_OWNER_PASSWORD_FILE`（L-19）、消息正文默认不落日志（L-20）、cf Token 密文输入（L-21）、并发撞名友好报错（L-23）、遗留 Python 隔离至 `reference/`（L-24）。

**命令行**：

- `ilink-wm1` 与 `iLinkWM` 一同装入 PATH：任意终端直接 `ilink-wm1 --version`、`ilink-wm1 admin ...`（等价直接调用 EXE）。
- `iLinkWM uninstall` 语义变更：**默认一条命令删除程序与全部数据**（有确认提示）；需要保留数据改用 `iLinkWM uninstall --keep-data`（原 `--purge` 语义并入默认）。

---

## 与原版（Zyn iLink ChatBox Python 版）的差异

> 原版为 **Python 单文件**（约 750 KB，`reference/Zyn-iLink-ChatBox-v3.2.4.py`），
> WongMod 为 **Rust 全量重构** 的多用户服务端。两者定位不同：原版偏单用户轻量客户端，WongMod 偏可部署的多人消息管理平台。

| 维度 | 原版（Python） | WongMod（Rust） |
|------|----------------|-----------------|
| 运行形态 | 需 Python 3.7+ 与 `pip install qrcode pycryptodomex pilk` | 单二进制 + `web/` 目录，零运行时依赖 |
| 用户体系 | 单用户（Web 密码） | owner / admin / user 三级角色，邀请码注册、设备令牌、会话管理 |
| 存储 | 文件/SQLite 单库 | SQLite（system.db + 用户库），配额、LRU 媒体清理 |
| 消息能力 | 文本/媒体收发、AI 自动回复 | 同等收发能力 + 消息历史、HTML 导出、多会话备注、WebDAV 媒体外置 |
| 安全 | 基础口令 | PBKDF2 600k 迭代、AES-256-GCM 凭证加密、DPAPI 主密钥包装、审计日志（90 天）、S10 破坏性命令二次确认、SSRF 防护、登录限流 |
| 运维 | 手动/启动器热更新 | systemd / NSSM 服务化、Cloudflare 隧道脚本、`iLinkWM` 统一命令行、CLI 管理子命令 |
| 审计状态 | — | 全量静态安全审计：0 高危 / 11 中危（9 已修复、2 风险接受）/ 24 低危，报告随仓库发布 |
| 协议 | Apache-2.0 | Apache-2.0（衍生合规标注原仓库与原作者） |

> 原版的 AI 自动回复依赖外部 AI API，WongMod 当前版本未内置该能力，计划通过 Webhook / 机器人配置在后续版本提供；迁移前请先确认功能覆盖。

---

## v3.2.3 轮次（2026-07-27，历史打包）

首个对外打包轮次：`ilink_wm_v3.2.3_src.zip` / `ilink_wm_v3.2.4_win_x64.zip`（源码与 Win x64 二进制），随附服务器 systemd 一键部署脚本 `install.sh`。
