# 更新日志（CHANGELOG）

本项目版本号跟随上游原版节奏：`v<原版版本>-wm<重构序号>`。
当前版本：**v3.2.4-wm1.1**（仓库治理与安全加固轮次，无功能新增）。

---

## v3.2.4-wm1.1（2026-08-18）

仓库治理与安全加固轮次，**无功能改动**。重点：消除审查中发现的 17 项风险点。

### 修复

- `start.ps1` 默认监听地址 `0.0.0.0` → `127.0.0.1`（与 README §1.4「最安全」一致）。公网监听仍可手动设 `ILINK_HOST=0.0.0.0` + 通过脚本内安全确认 + `ILINK_ALLOW_INSECURE_PUBLIC=1`。
- `release.yml`：`generate_release_notes: true` 替换为从 `CHANGELOG.md` 自动抽取对应版本段（解决"Full Changelog 链接重复 16 次"问题）。
- README §1.3 给出 `CARGO_TARGET_DIR` 显式命令（之前只说"可设…"）。
- README §4.4 引导用户开 GitHub Issue 反馈（之前只说"如发现 bug…"）。
- `start.ps1` 注释新增"为何默认 127.0.0.1 + 怎么扩到公网"。

### 新增

- `.github/workflows/ci.yml`：PR + push main 触发 `cargo fmt --check` + `cargo clippy` + `cargo build`（多平台）。
- `.github/ISSUE_TEMPLATE/bug_report.md` + `feature_request.md`：规范化反馈入口。
- `.github/PULL_REQUEST_TEMPLATE.md`：PR 提交清单（验证步骤、关联 Issue、兼容性影响）。
- `.github/CODEOWNERS`：默认 owner 为 `@Wong0728`。
- `.github/dependabot.yml`：每周自动检查 `cargo` + `github-actions` 依赖更新。
- `CONTRIBUTING.md`：贡献流程（分支、commit 规范、本地构建、PR 流程）。

### 仓库设置（API 操作）

- main 分支启用 **branch protection**：
  - `allow_force_pushes = false`（防止历史被重写）
  - `required_linear_history = true`（禁止 merge commit）
  - `required_status_checks` 包含 `CI / Lint + Build (ubuntu-latest)` 与 `CI / Lint + Build (windows-latest)`
  - `required_pull_request_reviews: required_approving_review_count = 1`
- 启用 `web_commit_signoff_required`（要求所有贡献者签署 DCO）。
- 关闭 `has_projects` / `has_wiki`（项目早期不使用，减少干扰）。
- 手动编辑 release `v3.2.4` 的 body：清掉 16 次重复的 `Full Changelog` 链接，替换为「请升级到 v3.2.4-wm1.1+」提示。

### 关于历史 force-push 的公开说明

> 首次 release（2026-08-16 17:25 UTC）后，早期 4 个 commit 通过 `git push --force` 被替换：
>
> | 旧 SHA（已弃） | 替换原因 |
> |---|---|
> | `bd3a2efd` security 修复 | 含 2 个错提交文件：`安全审计报告_2026-08-16.md`、`分发/SHA256SUMS.txt` |
> | `a3e01ea0` fix(ci) package.py | 内容被新 `553789d6` 覆盖（内容相同） |
> | `8560b296` fix(installer) CRLF | 内容被新 `11e83c6b` 覆盖（内容相同） |
> | `8604e5a9` docs: 云端发布指南更新 | 整条 commit 丢失，文件调整为内部材料不入仓 |
>
> 替换后 SHA 全变化（`7d5c42a1` / `553789d6` / `11e83c6b` / `431e0a53`），但内容大致相同。Pages 部署历史里仍记录旧 SHA，**跳转会 404**。
>
> 自 v3.2.4-wm1.1 起，main 分支启用 branch protection + `allow_force_pushes=false`，不再发生 force-push。
>
> **审计参考**：详见仓库根 `安全审计报告_2026-08-18_云端仓库审查.md`（该文件被 .gitignore 屏蔽，外部不可访问）。

### 已知遗留

- `run_test.py`（5.9 KB，仓库根目录）—— 开发辅助脚本，README §1.3 已说明角色，**保留**。
- `web/landing.html`（19 KB）—— Rust 服务的 `/` 路由；与 `docs/hero/ilinkwm_hero.html`（GitHub Pages）功能互补，**保留**。
- `reference/Zyn-iLink-ChatBox-v3.2.4.py`（750 KB）—— 原版 Python 单文件，作对照参考；下次大版本可考虑改用 git submodule 或单独仓库。
- e3b1aa7b commit 含 GBK ↔ UTF-8 字节对位错乱 diff（`install-service.bat +97 -97` 零和）—— 历史可读性问题，无法清理。

---

## v3.2.4-wm1.0（2026-08-16）

相比上一版（v3.2.3 打包轮次），本版本完成了一轮完整安全审计并修复 11 项中危中的 9 项，另有个别功能增强。

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
- **M-1 / M-6**：经业主评估**接受为已知风险**，暂不修改（4 位邀请码熵不足）。

### 新增

- `scripts/`：Cloudflare 隧道/DDNS 一键配置脚本（`cf-setup.ps1` / `cf-setup.sh` / `setup-cf.bat`，说明见 `scripts/CF-SETUP.md`）。
- `deploy/`：各平台一键安装脚本，安装后以 **`iLinkWM`** 命令统一操控（启动、装/卸服务、自更新、卸载）。
- `docs/`：GitHub Pages Hero 宣传页（含与原版对比）。
- `reference/`：收录原版最新 Python 单文件实现（v3.2.4）作对照参考。

### 变更

- 版本号自 `3.1.9-wm1.0` 提升至 `3.2.4-wm1.0`（`Cargo.toml`、`src/config.rs`）。
- 落地页开源协议标注由 MIT 更正为 Apache-2.0（与 LICENSE 文件一致）。

### 第三轮（2026-08-17）：低危清零 + 命令行增强

**安全**：L-1 ~ L-24 全部处理完毕（修复 18、随前轮连带修复 2、协议/架构约束经评估接受 4：L-1 / L-5 / L-6 / L-22）。要点：token 线程标识 SHA-256 化（L-3）、webhook 投递前 DNS 钉扎（L-4）、迁移跳过表落盘报告（L-8）、导出文件名消毒（L-9）、`ILINK_FFMPEG_PATH` 钉死 ffmpeg（L-10）、删除用户失败不再吞错（L-11）、登录/注册 Origin 校验（L-12）、voice 上传修复（L-13）、内部错误不外泄（L-14）、审计 JSON serde 转义（L-15）、CSP 补 object-src（L-16）、外链 scheme 二次校验（L-17）、前端三处防御（L-18）、`ILINK_OWNER_PASSWORD_FILE`（L-19）、消息正文默认不落日志（L-20）、cf Token 密文输入（L-21）、并发撞名友好报错（L-23）、遗留 Python 隔离至 `reference/`（L-24）。

**命令行**：

- `ilink-wm1` 与 `iLinkWM` 一同装入 PATH：任意终端直接 `ilink-wm1 --version`、`ilink-wm1 admin ...`（等价直接调用 EXE）。
- `iLinkWM uninstall` 语义变更：**默认一条命令删除程序与全部数据**（有确认提示）；需要保留数据改用 `iLinkWM uninstall --keep-data`（原 `--purge` 语义并入默认）。

### 第四轮（2026-08-17）：一键安装体验 + 云端仓库治理

**关键修复：cmd.exe 65001 代码页批处理解析 bug**（影响所有含中文的 .cmd/.bat）：

- 根因：cmd.exe 读取批处理时按「已消费字符数」而非字节数回卷文件位置，含多字节字符（UTF-8 或 GBK 均一样）的文件会错位落点，把行片段当命令执行；实测最小 4 行即可复现，纯 ASCII 文件即使切换代码页也 100% 安全。此前 `iLinkWM.cmd` 帮助输出、`start.bat`、`install-service.bat` 均不同程度中招——后者甚至出现过因错位跳过管理员权限检查的情况。
- 修复口径一（安装器生成的 shim）：`iLinkWM.cmd` / `ilink-wm1.cmd` 命令文本全部 ASCII 化（任何代码页下解析安全），中文帮助移入 `bin\iLinkWM-help.txt`（UTF-8）由 `:help` 分支 `chcp 65001` + `type` 输出——`type` 内容不经过解析器，无错位风险；退出前恢复原代码页。
- 修复口径二（随包分发的 .bat）：`start.bat` / `install-service.bat` 整体改写为纯 ASCII（提示信息改英文；中文完整文档见 README / 部署指南 / `iLinkWM help`），并顺带给 NSSM 下载的 PowerShell 子调用补了 `$ProgressPreference` 静音与 `-UseBasicParsing`。无需任何编码转码管线，任意区域设置的 Windows 上解析与显示均正常。

**安装器（Windows）**：

- 修复 `irm | iex` 首行报 `CommandNotFoundException`：脚本改存 UTF-8 无 BOM，并移除仅对脚本文件生效的 `#Requires` 指令（改为运行时版本检查）。
- `$ProgressPreference = 'SilentlyContinue'`：消除「正在写入 Web 请求 / 正在写入请求流」进度条刷屏，同时避免 PS 5.1 进度条拖慢下载。
- `Invoke-WebRequest` 加 `-UseBasicParsing`，规避 IE 引擎未初始化机器上的解析失败。
- 生成的 shim 内所有子 PowerShell 调用加 `-NoLogo`，不再打印「Windows PowerShell / Copyright」横幅。
- `iLinkWM` 支持 `help` / `-h` / `--help`（与 Linux 端对齐）；shim 内提示信息改英文以保证任何代码页下可读。

**安装器（Linux / macOS）**：

- 修复 auto 模式逻辑 bug：Release 预编译包安装成功后仍会继续走一遍源码编译。
- macOS（Apple Silicon）现在直接使用 Release 的 `macos_aarch64` 预编译包（原先误映射到 Linux 资产导致总回退源码编译）。
- `install-server.sh` 步骤编号与头部说明统一为 11 步。

**云端仓库治理**：

- 内部文档（安全审计报告明细、云端发布操作指南、本地分发校验清单）移出公开仓库，历史一并清理；对外安全披露渠道改由 [SECURITY.md](SECURITY.md) 承担。
- README / Hero 页同步修正：过时的「Release 尚未发布」提示、`/api/wasm/guide` 实际读取 `部署指南.md`、补全 `iLinkWM uninstall` 数据删除语义与 Releases 入口。
- Pages 页面补 favicon。

### 第五轮（2026-08-17）：全面 PowerShell 化，弃用 cmd

- Windows 命令垫片改为 `bin\iLinkWM.ps1` / `bin\ilink-wm1.ps1`（中文提示回归，UTF-8 带 BOM）。安装器把 `.PS1` 追加进用户 PATHEXT，PowerShell 中可直接裸敲 `iLinkWM` / `ilink-wm1`；需要时把当前用户执行策略设为 `RemoteSigned` 以放行本地脚本。升级安装会自动清理旧轮次生成的 `.cmd` 垫片。
- `start.bat` / `install-service.bat` 移除，等价移植为 `start.ps1` / `install-service.ps1`：NSSM 固定版本下载 + ZIP/EXE 双重 SHA-256 校验、`NT SERVICE` 低权虚拟账户、icacls 目录授权、HTTPS 反代 / 受信内网安全模式选择等逻辑保持一致；中文提示回归；`install-service.ps1` 非管理员运行时自动请求 UAC 提权。
- `scripts/setup-cf.bat` 移除（统一用 `powershell -ExecutionPolicy Bypass -File cf-setup.ps1`）。
- 卸载 / 服务管理等子命令全部原生 PowerShell 实现，不再派生 cmd.exe；cmd.exe 不再是受支持的外壳（PowerShell 5.1+ / PowerShell 7 均可）。
- `deploy/package.py` 打包前校验 `.ps1` 均带 UTF-8 BOM，防止未来编辑误删导致 PS 5.1 中文乱码。

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
| 审计状态 | — | 全量静态安全审计：0 高危 / 11 中危（9 已修复、2 风险接受）/ 24 低危（已清零或评估接受） |
| 协议 | Apache-2.0 | Apache-2.0（衍生合规标注原仓库与原作者） |

> 原版的 AI 自动回复依赖外部 AI API，WongMod 当前版本未内置该能力，计划通过 Webhook / 机器人配置在后续版本提供；迁移前请先确认功能覆盖。

---

## v3.2.3 轮次（2026-07-27，历史打包）

首个对外打包轮次：`ilink_wm_v3.2.3_src.zip` / `ilink_wm_v3.2.4_win_x64.zip`（源码与 Win x64 二进制），随附服务器 systemd 一键部署脚本 `install.sh`。
