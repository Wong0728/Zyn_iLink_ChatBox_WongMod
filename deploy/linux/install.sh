#!/usr/bin/env bash
# ============================================================================
# iLink-WM1 (iLinkWM) Linux / macOS / Termux 一键安装器
#
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/Wong0728/Zyn_iLink_ChatBox_WongMod/main/deploy/linux/install.sh | bash
#
# 可选环境变量：
#   ILINKWM_VERSION=latest        指定版本 tag（如 v3.2.4）
#   ILINKWM_METHOD=auto|binary|source
#
# 行为：
#   1. 优先从 GitHub Release 下载对应架构预编译包（linux_x86_64 / linux_aarch64 / macos_aarch64）
#   2. 无可用预编译包时回退「git clone + cargo build --release」
#      （出于安全考虑不自动安装 Rust 工具链，缺失时给出官方安装指引后退出）
#   3. 安装到 ~/.local/share/iLinkWM，命令入口 ~/.local/bin/iLinkWM
#   4. iLinkWM install-service 可注册 systemd 服务（root=系统级，普通用户=用户级）
# ============================================================================

set -euo pipefail

REPO='Wong0728/Zyn_iLink_ChatBox_WongMod'
BRANCH='main'
RAW_BASE="https://raw.githubusercontent.com/${REPO}/${BRANCH}"
APP_ID='iLinkWM'

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()    { echo -e "${BLUE}[iLinkWM]${NC} $*"; }
success() { echo -e "${GREEN}[iLinkWM]${NC} $*"; }
warn()    { echo -e "${YELLOW}[iLinkWM]${NC} $*"; }
die()     { echo -e "${RED}[iLinkWM]${NC} $*" >&2; exit 1; }

# Termux: /data/data/com.termux 前缀
if [[ -n "${TERMUX_VERSION:-}" || "${PREFIX:-}" == *com.termux* ]]; then
    IS_TERMUX=1
else
    IS_TERMUX=0
fi

if [[ $IS_TERMUX -eq 1 ]]; then
    INSTALL_ROOT="${PREFIX}/share/iLinkWM"
    BIN_DIR="${PREFIX}/bin"
else
    INSTALL_ROOT="${HOME}/.local/share/iLinkWM"
    BIN_DIR="${HOME}/.local/bin"
fi
DATA_DIR="${INSTALL_ROOT}/data"
SHIM="${BIN_DIR}/iLinkWM"

ARCH="$(uname -m)"
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64|Linux-amd64)     ARCH_TAG='linux_x86_64' ;;
    Linux-aarch64|Linux-arm64)    ARCH_TAG='linux_aarch64' ;;
    Darwin-arm64|Darwin-aarch64)  ARCH_TAG='macos_aarch64' ;;
    *)                            ARCH_TAG='' ;;
esac

fetch_json() { curl -fsSL --max-time 30 "$1" 2>/dev/null || true; }

install_from_binary() {
    local version="${1:-latest}" rel_url asset_url asset_name
    if [[ "$version" == "latest" ]]; then
        rel_url="https://api.github.com/repos/${REPO}/releases/latest"
    else
        rel_url="https://api.github.com/repos/${REPO}/releases/tags/${version}"
    fi
    local json
    json="$(fetch_json "$rel_url")"
    [[ -z "$json" ]] && { warn "无法获取 Release 信息（$rel_url）"; return 1; }

    if command -v python3 >/dev/null 2>&1; then
        asset_url="$(printf '%s' "$json" | python3 -c "
import json,sys
try:
    rel=json.load(sys.stdin)
    for a in rel.get('assets',[]):
        n=a['name']
        if '${ARCH_TAG}' and '${ARCH_TAG}' in n and n.endswith('.zip'):
            print(a['browser_download_url']); break
except Exception: pass
" 2>/dev/null || true)"
        asset_name="${asset_url##*/}"
    else
        asset_url=''
    fi
    [[ -z "$asset_url" ]] && { warn "Release 中没有 ${ARCH_TAG} 预编译包"; return 1; }

    local tmp
    tmp="$(mktemp -d)"
    info "下载 ${asset_name}..."
    curl -fsSL --max-time 600 -o "${tmp}/pkg.zip" "$asset_url" || { rm -rf "$tmp"; return 1; }

    if command -v unzip >/dev/null 2>&1; then
        unzip -q "${tmp}/pkg.zip" -d "${tmp}/extract"
    elif command -v bsdtar >/dev/null 2>&1; then
        mkdir -p "${tmp}/extract" && bsdtar -xf "${tmp}/pkg.zip" -C "${tmp}/extract"
    else
        rm -rf "$tmp"; die "未找到 unzip/bsdtar，请先安装（apt install unzip / pkg install unzip）"
    fi

    local src_root="${tmp}/extract"
    [[ -f "${src_root}/ilink-wm1" ]] || src_root="$(find "${tmp}/extract" -maxdepth 2 -type f -name 'ilink-wm1' -exec dirname {} \; | head -1)"
    [[ -f "${src_root}/ilink-wm1" ]] || { rm -rf "$tmp"; warn "包内未找到 ilink-wm1"; return 1; }

    deploy_files "$src_root"
    rm -rf "$tmp"
    success "已安装（Release 预编译包：${asset_name}）"
    return 0
}

install_from_source() {
    command -v git >/dev/null 2>&1 || die "未找到 git。源码安装需要 git 与 Rust 工具链（或等待 Release 预编译包）。"
    command -v cargo >/dev/null 2>&1 || [[ -x "$HOME/.cargo/bin/cargo" ]] || die "未找到 cargo。请先安装 Rust stable：
  https://www.rust-lang.org/tools/install
（出于安全考虑，本脚本不会自动执行远程安装命令）"
    local cargo_bin
    cargo_bin="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"

    local tmp
    tmp="$(mktemp -d)"
    info "克隆源码到 ${tmp} ..."
    git clone --depth 1 "https://github.com/${REPO}.git" "$tmp" >/dev/null 2>&1 || { rm -rf "$tmp"; die "git clone 失败"; }

    info "cargo build --release（首次约 3-10 分钟）..."
    (cd "$tmp" && "$cargo_bin" build --release) || { rm -rf "$tmp"; die "编译失败"; }
    [[ -f "$tmp/target/release/ilink-wm1" ]] || { rm -rf "$tmp"; die "编译产物未找到"; }

    local stage="$tmp/stage"
    mkdir -p "$stage"
    cp "$tmp/target/release/ilink-wm1" "$stage/"
    cp -r "$tmp/web" "$stage/"
    for f in LICENSE README.md CHANGELOG.md 用户协议.md 部署指南.md; do
        [[ -f "$tmp/$f" ]] && cp "$tmp/$f" "$stage/"
    done
    deploy_files "$stage"
    rm -rf "$tmp"
    success "已安装（源码编译，分支 ${BRANCH}）"
}

deploy_files() {
    local src_root="$1"
    if [[ -d "$INSTALL_ROOT" ]]; then
        info "升级：保留 data/，覆盖其余文件..."
        find "$INSTALL_ROOT" -mindepth 1 -maxdepth 1 ! -name data -exec rm -rf {} +
    else
        mkdir -p "$INSTALL_ROOT"
    fi
    (cd "$src_root" && tar -cf - .) | (cd "$INSTALL_ROOT" && tar -xf -)
    [[ -x "$INSTALL_ROOT/ilink-wm1" ]] || chmod +x "$INSTALL_ROOT/ilink-wm1"
}

write_shim() {
    mkdir -p "$BIN_DIR"
    cat > "$SHIM" <<EOF
#!/usr/bin/env bash
# iLinkWM - Zyn iLink ChatBox WongMod 统一命令（由安装器生成）
set -euo pipefail
APP_ROOT="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")/.." && pwd)"
BIN="\$APP_ROOT/ilink-wm1"
DATA="\$APP_ROOT/data"

cmd="\${1:-}"
case "\$cmd" in
    "")
        [[ -x "\$BIN" ]] || { echo "[iLinkWM] 未找到 ilink-wm1，请重新安装：curl -fsSL ${RAW_BASE}/deploy/linux/install.sh | bash" >&2; exit 1; }
        cd "\$APP_ROOT"
        export ILINK_DATA_DIR="\${ILINK_DATA_DIR:-\$DATA}"
        exec "\$BIN"
        ;;
    update)
        echo "[iLinkWM] 正在检查并安装最新版本..."
        exec bash -c "curl -fsSL ${RAW_BASE}/deploy/linux/install.sh | bash"
        ;;
    install-service)
        if [[ \$(id -u) -eq 0 ]]; then
            [[ -f "\$DATA/system.db" ]] || { echo "[iLinkWM] 请先运行一次 iLinkWM 完成初始化向导，再注册服务。" >&2; exit 1; }
            mkdir -p /etc/ilink
            cat > /etc/systemd/system/ilink-wm1.service <<UNIT
[Unit]
Description=iLink-WM1 ChatBox Service
After=network.target

[Service]
WorkingDirectory=\$APP_ROOT
Environment=ILINK_DATA_DIR=\$DATA
Environment=ILINK_SERVER_MODE=1
Environment=RUST_LOG=ilink_wm1=info
ExecStart=\$BIN
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT
            systemctl daemon-reload && systemctl enable --now ilink-wm1
            echo "[iLinkWM] 系统服务已启动：systemctl status ilink-wm1"
        else
            [[ -f "\$DATA/system.db" ]] || { echo "[iLinkWM] 请先运行一次 iLinkWM 完成初始化向导，再注册服务。" >&2; exit 1; }
            mkdir -p ~/.config/systemd/user
            cat > ~/.config/systemd/user/ilink-wm1.service <<UNIT
[Unit]
Description=iLink-WM1 ChatBox Service
After=network.target

[Service]
WorkingDirectory=\$APP_ROOT
Environment=ILINK_DATA_DIR=\$DATA
Environment=ILINK_SERVER_MODE=1
Environment=RUST_LOG=ilink_wm1=info
ExecStart=\$BIN
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536

[Install]
WantedBy=default.target
UNIT
            systemctl --user daemon-reload && systemctl --user enable --now ilink-wm1
            loginctl enable-linger "\$USER" 2>/dev/null || echo "[iLinkWM] 提示：enable-linger 失败，注销后用户服务会停止。"
            echo "[iLinkWM] 用户服务已启动：systemctl --user status ilink-wm1"
        fi
        ;;
    uninstall-service)
        if [[ \$(id -u) -eq 0 ]]; then
            systemctl disable --now ilink-wm1 2>/dev/null || true
            rm -f /etc/systemd/system/ilink-wm1.service
            systemctl daemon-reload
        else
            systemctl --user disable --now ilink-wm1 2>/dev/null || true
            rm -f ~/.config/systemd/user/ilink-wm1.service
            systemctl --user daemon-reload 2>/dev/null || true
        fi
        echo "[iLinkWM] 服务已移除。"
        ;;
    service)
        shift
        if [[ \$(id -u) -eq 0 ]]; then systemctl "\$@" ilink-wm1
        else systemctl --user "\$@" ilink-wm1; fi
        ;;
    uninstall)
        mode="\${2:-}"
        if [[ "\$mode" == "--keep-data" ]]; then confirm="Y"
        else
            printf '卸载 iLinkWM 并删除程序与全部数据（%s）？输入 Y 确认（保留数据请用 --keep-data）: ' "\$DATA"
            read -r confirm < /dev/tty
        fi
        [[ "\$confirm" == "Y" || "\$confirm" == "y" ]] || { echo "[iLinkWM] 已取消。"; exit 0; }
        if [[ \$(id -u) -eq 0 ]]; then
            systemctl disable --now ilink-wm1 2>/dev/null || true
            rm -f /etc/systemd/system/ilink-wm1.service /etc/ilink/env 2>/dev/null || true
        else
            systemctl --user disable --now ilink-wm1 2>/dev/null || true
            rm -f ~/.config/systemd/user/ilink-wm1.service 2>/dev/null || true
        fi
        if [[ "\$mode" == "--keep-data" ]]; then
            find "\$APP_ROOT" -mindepth 1 -maxdepth 1 ! -name data -exec rm -rf {} +
            echo "[iLinkWM] 已卸载（数据目录保留在 \$DATA，需要时手动删除）。"
        else
            rm -rf "\$APP_ROOT"
            echo "[iLinkWM] 已卸载（程序与数据已全部删除）。"
        fi
        rm -f "\$SHIM" "\$BIN_DIR/ilink-wm1"
        echo "[iLinkWM] 完成。"
        ;;
    ilinkwm-help|help)
        echo "iLinkWM - Zyn iLink ChatBox WongMod 统一命令"
        echo
        echo "  iLinkWM                    启动程序（首次运行进入初始化向导）"
        echo "  iLinkWM install-service    注册 systemd 服务（root=系统级，否则用户级）"
        echo "  iLinkWM uninstall-service  移除 systemd 服务"
        echo "  iLinkWM service start|stop|restart|status  服务控制"
        echo "  iLinkWM update             更新到最新版本"
        echo "  iLinkWM uninstall [--keep-data] 卸载（默认删除程序与全部数据；--keep-data 保留数据）"
        echo "  iLinkWM admin ...          其余参数原样传给 ilink-wm1"
        echo "  ilink-wm1 ...              二进制直通命令（同在 PATH）：ilink-wm1 --version / admin ..."
        ;;
    *)
        [[ -x "\$BIN" ]] || { echo "[iLinkWM] 未找到 ilink-wm1，请重新安装。" >&2; exit 1; }
        cd "\$APP_ROOT"
        export ILINK_DATA_DIR="\${ILINK_DATA_DIR:-\$DATA}"
        exec "\$BIN" "\$@"
        ;;
esac
EOF
    chmod +x "$SHIM"

    # ilink-wm1 直通命令：任意终端 ilink-wm1 --version / ilink-wm1 admin ...
    cat > "$BIN_DIR/ilink-wm1" <<'EXESHIM'
#!/usr/bin/env bash
# ilink-wm1 直通命令（由安装器生成）：等价直接运行二进制
set -euo pipefail
APP_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$APP_ROOT/ilink-wm1"
[[ -x "$BIN" ]] || { echo "[ilink-wm1] 未找到 $BIN，请重新安装 iLinkWM。" >&2; exit 1; }
cd "$APP_ROOT"
export ILINK_DATA_DIR="${ILINK_DATA_DIR:-$APP_ROOT/data}"
exec "$BIN" "$@"
EXESHIM
    chmod +x "$BIN_DIR/ilink-wm1"
    success "命令入口：$SHIM、$BIN_DIR/ilink-wm1"
}

ensure_path() {
    case ":${PATH}:" in
        *":${BIN_DIR}:"*) : ;;
        *)
            warn "目录 $BIN_DIR 不在 PATH 中，尝试写入 ~/.profile / ~/.bashrc..."
            {
                echo ''
                echo '# added by iLinkWM installer'
                echo "export PATH=\"\$PATH:${BIN_DIR}\""
            } >> ~/.profile 2>/dev/null || true
            {
                echo ''
                echo '# added by iLinkWM installer'
                echo "export PATH=\"\$PATH:${BIN_DIR}\""
            } >> ~/.bashrc 2>/dev/null || true
            ;;
    esac
}

# ── 主流程 ─────────────────────────────────────────────
info "iLink-WM1 安装器 · 架构 $ARCH · 目标目录 $INSTALL_ROOT"
METHOD="${ILINKWM_METHOD:-auto}"
VERSION="${ILINKWM_VERSION:-latest}"

ok=0
if [[ "$METHOD" == "binary" ]]; then
    install_from_binary "$VERSION" || die "未找到 ${ARCH_TAG} 预编译包（$VERSION）"
    ok=1
elif [[ "$METHOD" == "source" ]]; then
    install_from_source
    ok=1
else
    if install_from_binary "$VERSION"; then
        ok=1
    else
        warn "无可用 Release 预编译包，回退源码编译模式..."
    fi
    [[ $ok -eq 1 ]] || install_from_source
fi

write_shim
ensure_path

echo ''
success '安装完成！下一步：'
echo '  1. 重新打开终端，或执行： source ~/.profile'
echo '  2. 运行  iLinkWM               # 首次运行进入初始化向导'
echo '  3. 可选  iLinkWM install-service  # 注册 systemd 服务'
echo ''
echo "  安装目录：$INSTALL_ROOT"
echo "  数据目录：$DATA_DIR"
echo '  完整文档：README.md / 部署指南.md'
