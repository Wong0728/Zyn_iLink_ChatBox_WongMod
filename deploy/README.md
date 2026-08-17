# deploy · 一键安装与部署

本目录存放各平台的一键安装脚本与发布流水线。安装后所有平台统一使用 **`iLinkWM`** 命令操控程序。

## iLinkWM 命令一览

| 命令 | 说明 |
|------|------|
| `iLinkWM` | 启动程序（首次运行进入初始化向导：绑定地址、创建 owner、站点名、邀请码等） |
| `iLinkWM admin ...` | 其余参数原样传给 `ilink-wm1`（如 `admin user list`、`admin config set`） |
| `ilink-wm1 ...` | 二进制直通命令（与 iLinkWM 同在 PATH）：任意终端 `ilink-wm1 --version`、`ilink-wm1 admin ...` |
| `iLinkWM install-service` | 注册系统服务（Windows: NSSM 服务；Linux: systemd，root 为系统级、普通用户为用户级） |
| `iLinkWM uninstall-service` | 移除系统服务 |
| `iLinkWM service start/stop/restart/status` | 服务启停与状态（Windows 需管理员） |
| `iLinkWM update` | 更新到最新版本（重新执行安装器，保留数据目录） |
| `iLinkWM uninstall [--keep-data]` | 卸载；**默认一条命令删除程序与全部数据目录**（有确认提示），`--keep-data` 仅删程序保留 `data/` |

安装位置：

| 平台 | 程序目录 | 命令入口（均在 PATH） | 数据目录 |
|------|----------|----------|----------|
| Windows | `%LOCALAPPDATA%\Programs\iLinkWM` | `...\iLinkWM\bin\`（`iLinkWM.ps1` + `ilink-wm1.ps1`，自动加入用户 PATH，并把 `.PS1` 追加进用户 PATHEXT） | `...\iLinkWM\data` |
| Linux / macOS | `~/.local/share/iLinkWM` | `~/.local/bin/`（`iLinkWM` + `ilink-wm1`） | `~/.local/share/iLinkWM/data` |
| Termux | `$PREFIX/share/iLinkWM` | `$PREFIX/bin/`（`iLinkWM` + `ilink-wm1`） | `$PREFIX/share/iLinkWM/data` |

> Windows 命令入口为 PowerShell 脚本（**PowerShell 5.1+ / PowerShell 7 均可，cmd.exe 不适用**）。
> 安装器会在需要时把当前用户执行策略设为 `RemoteSigned` 以放行本地脚本。

## 一键安装

**Windows（PowerShell 5.1+）：**

```powershell
irm https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/windows/install.ps1 | iex
```

**Linux / macOS / Termux：**

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/linux/install.sh | bash
```

安装器优先下载 GitHub Release 预编译包；云端尚无 Release（或无对应架构）时自动回退为
「git clone + cargo build --release」源码编译。**出于安全考虑，安装器不会自动执行
Rust 官方安装脚本之外的任何第三方脚本，也不会在缺少 Rust 时静默安装工具链**——缺失时
打印官方指引后退出。

可选环境变量：

- `ILINKWM_VERSION`：指定版本 tag（默认 `latest`）
- `ILINKWM_METHOD`：`auto` / `binary` / `source`（默认 `auto`）

## 服务器部署（systemd，root）

面向公网/局域网服务器的完整部署（依赖安装、专用系统用户、沙箱加固 systemd 单元、
防火墙放行、HTTPS 模式选择）：

```bash
sudo bash deploy/linux/install-server.sh            # 无源码包时自动 git clone
sudo bash deploy/linux/install-server.sh /tmp/ilink_wm_v3.2.4_src.zip
```

卸载步骤见脚本头部注释。

## 本地打包

```bash
python deploy/package.py
```

读取 `Cargo.toml` 版本号，产出 `分发/ilink_wm_v<版本>_src.zip` 与
`分发/ilink_wm_v<版本>_win_x64.zip`（Windows 需先 `cargo build --release`），
并生成 SHA-256 清单。

## CI 自动发布

`.github/workflows/release.yml`：推送 `v*` 标签时在 GitHub Actions 上编译
Windows x64 / Linux x64 / Linux arm64 / macOS 产物，自动打包并创建 Release，
供上述一键脚本下载。
