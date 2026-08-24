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
| `iLinkWM update` | 更新到当前正式版本（重新执行安装器，保留数据目录） |
| `iLinkWM uninstall [--keep-data\|--yes]` | 卸载；**默认一条命令删除程序与全部数据目录**，`--keep-data` 仅删程序保留 `data/`，`--yes` 供无 TTY 自动化免确认 |

安装位置：

| 平台 | 程序目录 | 命令入口（均在 PATH） | 数据目录 |
|------|----------|----------|----------|
| Windows | `%LOCALAPPDATA%\Programs\iLinkWM` | `...\iLinkWM\bin\`（`iLinkWM.ps1` + `ilink-wm1.ps1`，自动加入用户 PATH，并把 `.PS1` 追加进用户 PATHEXT） | `...\iLinkWM\data` |
| Linux / macOS | `~/.local/share/iLinkWM` | `~/.local/bin/`（`iLinkWM` + `ilinkwm` + `ilink-wm1`） | `~/.local/share/iLinkWM/data` |
| Termux | `$PREFIX/share/iLinkWM` | `$PREFIX/bin/`（`iLinkWM` + `ilinkwm` + `ilink-wm1`） | `$PREFIX/share/iLinkWM/data` |

> Windows 命令入口为 PowerShell 脚本（**PowerShell 5.1+ / PowerShell 7 均可，cmd.exe 不适用**）。
> 安装器会在需要时把当前用户执行策略设为 `RemoteSigned` 以放行本地脚本。

## 直接使用 Release 便携包

不想运行一键安装器时，从 [Releases](https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod/releases)
下载与系统匹配的完整 ZIP，并解压到一个专用目录（例如 Windows 的
`%LOCALAPPDATA%\iLinkWM-portable\v3.2.5`，Linux/macOS 的
`~/opt/iLinkWM/v3.2.5`）。不要只复制二进制，也不要直接在压缩包内运行。

解压后必须保持二进制与 `web/` 同级：

```text
便携目录/
├── ilink-wm1.exe       # Windows；Linux/macOS 为 ilink-wm1
├── web/
├── start.ps1            # 仅 Windows 便携包
└── install-service.ps1  # 仅 Windows 便携包
```

Windows 推荐在 PowerShell 中运行 `powershell -ExecutionPolicy Bypass -File .\start.ps1`；
它会把数据放到便携目录的 `data\`。Linux/macOS 在目录内运行 `./ilink-wm1`。
如需把数据与程序分离，启动前设置绝对路径，例如
`ILINK_DATA_DIR=/srv/ilink-data ./ilink-wm1` 或
`$env:ILINK_DATA_DIR = 'D:\iLinkWM-data'`。

平台资产目前为：`ilink_wm_v3.2.5_win_x64.zip`、`ilink_wm_v3.2.5_linux_x86_64.zip`、
`ilink_wm_v3.2.5_linux_aarch64.zip`、`ilink_wm_v3.2.5_macos_aarch64.zip`，另有源码包。
每个 ZIP 在 GitHub Release 中都有一个同名 `.zip.sha256` sidecar，例如
`ilink_wm_v3.2.5_win_x64.zip.sha256`；校验文件不放在 ZIP 内，因为它校验的是整个 ZIP。

预先下载的 ZIP 放在 `C:\Users\<用户名>\Downloads` 即可，不需要放进 `%LOCALAPPDATA%\Programs\iLinkWM`。
PowerShell 一键安装器只扫描 `ilink_wm_v<版本>_win_x64.zip` 这种 Release 文件名；有多个版本时按版本号选最新候选，
并把本地 `.zip.sha256` 与 GitHub 远端 sidecar 对照。哈希不一致会 warning 并询问是否继续；用户取消后改为重新下载远端 ZIP。

## 一键安装

**Windows（PowerShell 5.1+）：**

```powershell
irm https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/windows/install.ps1 | iex
```

**Linux / macOS / Termux：**

```bash
curl -fsSL https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/linux/install.sh | bash
```

安装器默认固定正式版 `v3.2.5`，优先下载带同名 SHA-256 sidecar 的 GitHub Release 预编译包；GitHub API 失败时会重试，并对固定版本按可预测资产名直连 Release，下载后执行 `ilink-wm1 --version` 验证二进制可运行。Linux x86_64/aarch64 预编译包使用 musl 静态目标，不受安装机 glibc 版本限制；对应 tag 尚无 Release（或无对应架构）时，自动回退为同名 tag 的
「git clone + cargo build --release」源码编译。**出于安全考虑，安装器不会自动执行
Rust 官方安装脚本之外的任何第三方脚本，也不会在缺少 Rust 时静默安装工具链**——缺失时
打印官方指引后退出。

安装器会把命令目录加载到安装脚本自身的 PATH，并按当前 shell 写入 `~/.profile`、`~/.bashrc`、`~/.zshrc` 或 fish 配置；通过 `curl | bash` 无法修改调用方的父 shell，因此新终端请重新打开或手动 `source ~/.profile`。

可选环境变量：

- `ILINKWM_VERSION`：指定版本 tag（默认 `v3.2.5`；显式设 `latest` 才跟随浮动版本）
- `ILINKWM_METHOD`：`auto` / `binary` / `source`（默认 `auto`）

### NSSM 固定版本升级

Windows 服务脚本当前固定 NSSM 2.24，并在下载 ZIP 与解压 EXE 两层校验 SHA-256。升级 NSSM 时必须同步修改
`install-service.ps1` 中的下载 URL、ZIP 哈希和 EXE 哈希，先用 `Get-FileHash -Algorithm SHA256` 对官方包复核，
再在 Windows 服务安装机上做一次停止/重装/启动验证；缺任一哈希不得改成自动信任远端文件。

## 服务器部署（systemd，root）

面向公网/局域网服务器的完整部署（依赖安装、专用系统用户、沙箱加固 systemd 单元、
防火墙放行、HTTPS 模式选择）：

```bash
sudo bash deploy/linux/install-server.sh            # 无源码包时自动 git clone
sudo bash deploy/linux/install-server.sh /tmp/ilink_wm_v3.2.5_src.zip
```

卸载步骤见脚本头部注释。

## 本地打包

```bash
python deploy/package.py
```

读取 `Cargo.toml` 版本号，产出 `分发/ilink_wm_v<版本>_src.zip` 与
`分发/ilink_wm_v<版本>_win_x64.zip`（Windows 需先 `cargo build --release`），
并只为本次生成的包写入 SHA-256 清单；分发目录中的历史包会保留但不会混入当前清单。

这个脚本不是完整的多平台 Release 构建器。正式发布的四个平台便携包和源码包由
`.github/workflows/release.yml` 在推送 `v*` 标签后构建、校验并上传。

## CI 自动发布

`.github/workflows/release.yml`：推送 `v*` 标签时在 GitHub Actions 上编译
Windows x64 / Linux x64 / Linux arm64 / macOS arm64 便携包，并生成源码包、同名
`.sha256` sidecar 与汇总清单，最后一次性创建 Release，供上述一键脚本下载。
