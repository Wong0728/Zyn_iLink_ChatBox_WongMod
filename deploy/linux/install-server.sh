#!/usr/bin/env bash
# ============================================================================
# iLink-WM1 Linux 一键部署脚本
#
# 用法：
#   sudo bash install.sh [源码包路径]
#
# 默认源码包路径：/tmp/ilink_wm_v3.2.4-wm1.1_src.zip
#
# 脚本功能：
#   1. 检测系统包管理器（apt / dnf / yum）
#   2. 安装依赖：构建工具、OpenSSL、ffmpeg、OpenSSH client
#   3. 检查预先安装的 Rust stable 工具链
#   4. 创建 /opt/ilink 目录与 ilink 系统用户
#   5. 解压源码到 /opt/ilink/ilink_wm_v3.2.4-wm1.1
#   6. cargo build --release（显示进度）
#   7. 设置目录权限，终端初始化 owner
#   8. 选择安全模式，生成 /etc/ilink/env 与 /etc/systemd/system/ilink.service
#   9. systemctl enable --now ilink
#  10. 防火墙放行 8888 端口
#  11. 输出访问地址与后续步骤
#
# 卸载：
#   sudo systemctl stop ilink
#   sudo systemctl disable ilink
#   sudo rm /etc/systemd/system/ilink.service
#   sudo rm -rf /opt/ilink /etc/ilink
#   sudo userdel ilink
# ============================================================================

set -euo pipefail

# ── 颜色输出 ──────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info()    { echo -e "${BLUE}[INFO]${NC} $*"; }
success() { echo -e "${GREEN}[OK]${NC} $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ── 必须以 root 运行 ──────────────────────────────────────
if [[ $EUID -ne 0 ]]; then
    error "此脚本必须以 root 身份运行（请用 sudo）"
    exit 1
fi

# ── 变量 ──────────────────────────────────────────────────
SRC_ZIP="${1:-/tmp/ilink_wm_v3.2.4-wm1.1_src.zip}"
INSTALL_DIR="/opt/ilink"
APP_DIR="$INSTALL_DIR/ilink_wm_v3.2.4-wm1.1"
DATA_DIR="$INSTALL_DIR/data"
SERVICE_USER="ilink"
SERVICE_NAME="ilink"
DEFAULT_PORT="8888"
ENV_FILE="/etc/ilink/env"
SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

# ── Step 0: 检查源码包 ────────────────────────────────────
info "Step 0/11: 检查源码包..."
SRC_FROM_GIT=0
if [[ ! -f "$SRC_ZIP" ]]; then
    # 无源码包时尝试直接克隆仓库
    if command -v git &>/dev/null; then
        warn "源码包不存在: $SRC_ZIP，改为从 GitHub 克隆..."
        SRC_FROM_GIT=1
    else
        error "源码包不存在: $SRC_ZIP（且未安装 git）"
        echo "请先上传 ilink_wm_v3.2.4-wm1.1_src.zip 到服务器，或通过参数指定路径："
        echo "  sudo bash install.sh /path/to/ilink_wm_v3.2.4-wm1.1_src.zip"
        echo "或安装 git 后由脚本直接克隆：apt install git"
        exit 1
    fi
else
    success "源码包: $SRC_ZIP"
fi

# ── Step 1: 检测包管理器 ──────────────────────────────────
info "Step 1/11: 检测系统包管理器..."
PKG_MANAGER=""
if command -v apt-get &>/dev/null; then
    PKG_MANAGER="apt"
elif command -v dnf &>/dev/null; then
    PKG_MANAGER="dnf"
elif command -v yum &>/dev/null; then
    PKG_MANAGER="yum"
else
    error "不支持的系统：找不到 apt-get / dnf / yum"
    exit 1
fi
success "包管理器: $PKG_MANAGER"

# ── Step 2: 安装系统依赖 ──────────────────────────────────
info "Step 2/11: 安装系统依赖..."
case "$PKG_MANAGER" in
    apt)
        apt-get update -y
        apt-get install -y build-essential pkg-config libssl-dev unzip curl ca-certificates ffmpeg openssh-client
        ;;
    dnf|yum)
        $PKG_MANAGER groupinstall -y "Development Tools"
        $PKG_MANAGER install -y openssl-devel pkg-config unzip curl ca-certificates ffmpeg openssh-clients
        ;;
esac
success "系统依赖已安装"

# ── Step 3: 检查 Rust 工具链 ──────────────────────────────
info "Step 3/11: 检查 Rust 工具链..."
if command -v cargo &>/dev/null; then
    success "Rust 已安装: $(cargo --version)"
else
    error "未找到 cargo。为避免下载后直接执行远程脚本，本安装器不会自动安装 Rust。"
    echo "请按 Rust 官方文档安装 stable 工具链并验证来源，然后重新运行本脚本："
    echo "  https://www.rust-lang.org/tools/install"
    exit 1
fi

# ── Step 4: 创建系统用户与目录 ────────────────────────────
info "Step 4/11: 创建系统用户与目录..."
if id "$SERVICE_USER" &>/dev/null; then
    warn "用户 $SERVICE_USER 已存在，跳过创建"
else
    useradd -r -s /usr/sbin/nologin -d "$INSTALL_DIR" "$SERVICE_USER"
    success "系统用户 $SERVICE_USER 已创建"
fi

mkdir -p "$INSTALL_DIR" "$DATA_DIR"
success "目录已创建: $INSTALL_DIR, $DATA_DIR"

# ── Step 5: 解压源码 ──────────────────────────────────────
info "Step 5/11: 解压源码..."
# 备份旧目录（若存在）
if [[ -d "$APP_DIR" ]]; then
    BACKUP_TS=$(date +%Y%m%d_%H%M%S)
    warn "检测到旧目录 $APP_DIR，备份为 $APP_DIR.bak.$BACKUP_TS"
    mv "$APP_DIR" "$APP_DIR.bak.$BACKUP_TS"
fi
mkdir -p "$APP_DIR"
if [[ "$SRC_FROM_GIT" -eq 1 ]]; then
    rmdir "$APP_DIR"
    git clone --depth 1 "https://github.com/Wong0728/Zyn_iLink_ChatBox_WongMod.git" "$APP_DIR"
else
    unzip -q "$SRC_ZIP" -d "$APP_DIR"
fi
if [[ ! -f "$APP_DIR/Cargo.toml" ]]; then
    error "解压后未找到 $APP_DIR/Cargo.toml，请检查 ZIP 结构"
    exit 1
fi
success "源码已就位于 $APP_DIR"

# ── Step 6: 编译 ──────────────────────────────────────────
info "Step 6/11: 编译 release 版本（约 3-10 分钟，请耐心等待）..."
# 用当前 root 用户的 cargo 编译（避免 ilink 用户无 shell 无法用 rustup）
# 编译产物在 target/release/ilink-wm1
cd "$APP_DIR"
# 设置 CARGO_TARGET_DIR 避免路径含特殊字符问题（本项目路径无中文但保险起见）
export CARGO_TARGET_DIR="$APP_DIR/target"
if ! cargo build --release; then
    error "编译失败，请检查错误输出"
    echo "常见原因："
    echo "  1. 内存不足（OOM）→ 添加 swap：sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile"
    echo "  2. openssl 缺失 → apt install libssl-dev pkg-config"
    exit 1
fi

BINARY="$APP_DIR/target/release/ilink-wm1"
if [[ ! -f "$BINARY" ]]; then
    error "编译产物未找到: $BINARY"
    exit 1
fi
success "编译成功: $BINARY ($(ls -lh $BINARY | awk '{print $5}'))"

# ── Step 7: 设置目录权限 ──────────────────────────────────
info "Step 7/11: 设置目录权限..."
chown -R "$SERVICE_USER":"$SERVICE_USER" "$INSTALL_DIR"
chmod 750 "$INSTALL_DIR"
chmod 750 "$DATA_DIR"
success "权限已设置"

# ── Step 8: 终端初始化 owner ───────────────────────────────
info "Step 8/11: 创建或确认 owner 管理员账号..."
if ! runuser -u "$SERVICE_USER" -- env ILINK_DATA_DIR="$DATA_DIR" "$BINARY" admin init; then
    error "owner 初始化失败，尚未注册或启动 systemd 服务"
    exit 1
fi
success "owner 初始化完成"

# ── Step 9: 选择安全模式并生成服务 ─────────────────────────
info "Step 9/11: 选择网络安全模式..."
echo "  1. 已有 HTTPS 反向代理（推荐）"
echo "  2. 仅受信任内网明文 HTTP"
read -r -p "输入 1 或 2: " SECURITY_MODE
SECURITY_ENV=""
if [[ "$SECURITY_MODE" == "1" ]]; then
    BIND_HOST="127.0.0.1"
    read -r -p "可信代理 IP/CIDR [127.0.0.1]: " TRUSTED_PROXY
    TRUSTED_PROXY="${TRUSTED_PROXY:-127.0.0.1}"
    SECURITY_ENV=$'ILINK_TRUSTED_PROXIES='"$TRUSTED_PROXY"$'\nILINK_FORCE_HTTPS=1'
elif [[ "$SECURITY_MODE" == "2" ]]; then
    BIND_HOST="0.0.0.0"
    read -r -p "确认端口只暴露在受信任内网？请输入 YES: " INSECURE_CONFIRM
    if [[ "$INSECURE_CONFIRM" != "YES" ]]; then
        error "未确认明文内网部署，停止安装"
        exit 1
    fi
    SECURITY_ENV="ILINK_ALLOW_INSECURE_PUBLIC=1"
else
    error "无效选项"
    exit 1
fi

info "生成 systemd 服务配置..."
mkdir -p /etc/ilink
cat > "$ENV_FILE" <<EOF
# iLink-WM1 环境变量配置
# 修改后需重启服务：sudo systemctl restart ilink

# 绑定地址（0.0.0.0=公网可访问，127.0.0.1=仅本机）
ILINK_HOST=${BIND_HOST}

# 端口
ILINK_PORT=${DEFAULT_PORT}

# 数据目录
ILINK_DATA_DIR=${DATA_DIR}

# 服务模式禁止后台进程等待交互式输入
ILINK_SERVER_MODE=1

# 日志级别（info=常规，debug=详细）
RUST_LOG=ilink_wm1=info

# panic 时打印完整 backtrace
RUST_BACKTRACE=full

# 安装时已明确选择；ILINK_FORCE_HTTPS 只表示上游已经提供真实 HTTPS，
# 它本身不会为 HTTP 连接增加 TLS。
${SECURITY_ENV}
EOF
chown root:"$SERVICE_USER" "$ENV_FILE"
chmod 640 "$ENV_FILE"

cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=iLink-WM1 ChatBox Service
After=network.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_USER}
WorkingDirectory=${APP_DIR}
EnvironmentFile=${ENV_FILE}
ExecStart=${BINARY}
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536

# 安全沙箱
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=${INSTALL_DIR}

[Install]
WantedBy=multi-user.target
EOF
success "服务配置已生成: $SERVICE_FILE"

# ── Step 10: 启用并启动服务 ───────────────────────────────
info "Step 10/11: 启用并启动服务..."
systemctl daemon-reload
systemctl enable "$SERVICE_NAME"
systemctl start "$SERVICE_NAME"
sleep 2
if systemctl is-active --quiet "$SERVICE_NAME"; then
    success "服务已启动: $SERVICE_NAME"
else
    error "服务启动失败，查看日志：sudo journalctl -u $SERVICE_NAME -n 50"
    exit 1
fi

# ── Step 11: 防火墙 ───────────────────────────────────────
info "Step 11/11: 配置防火墙..."
if command -v ufw &>/dev/null; then
    if [[ "$BIND_HOST" == "0.0.0.0" ]]; then
        ufw allow ${DEFAULT_PORT}/tcp
        success "ufw 已放行 ${DEFAULT_PORT}/tcp（仅适用于已确认的受信内网模式）"
    else
        info "反代模式监听 127.0.0.1，不向防火墙开放应用端口"
    fi
elif command -v firewall-cmd &>/dev/null; then
    if [[ "$BIND_HOST" == "0.0.0.0" ]]; then
        firewall-cmd --permanent --add-port=${DEFAULT_PORT}/tcp
        firewall-cmd --reload
        success "firewalld 已放行 ${DEFAULT_PORT}/tcp（仅适用于已确认的受信内网模式）"
    else
        info "反代模式监听 127.0.0.1，不向防火墙开放应用端口"
    fi
else
    if [[ "$BIND_HOST" == "0.0.0.0" ]]; then
        warn "未检测到防火墙工具（ufw/firewalld），请手动限制并放行 ${DEFAULT_PORT}/tcp"
    else
        info "反代模式仅监听 127.0.0.1，无需对外放行应用端口"
    fi
fi

# ── 获取服务器 IP ────────────────────────────────────────
SERVER_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "服务器IP")
if [[ -z "$SERVER_IP" ]]; then
    SERVER_IP="服务器IP"
fi

# ── 完成 ─────────────────────────────────────────────────
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}  iLink-WM1 部署完成！${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo "服务状态:    sudo systemctl status $SERVICE_NAME"
echo "实时日志:    sudo journalctl -u $SERVICE_NAME -f"
echo "停止服务:    sudo systemctl stop $SERVICE_NAME"
echo "重启服务:    sudo systemctl restart $SERVICE_NAME"
echo "卸载服务:    sudo systemctl stop $SERVICE_NAME && sudo systemctl disable $SERVICE_NAME && sudo rm $SERVICE_FILE && sudo rm -rf $INSTALL_DIR /etc/ilink && sudo userdel $SERVICE_USER"
echo ""
echo -e "${YELLOW}owner 已在安装过程中初始化。下一步请通过已选择的安全入口登录，并扫码绑定 iLink。${NC}"
if [[ "$SECURITY_MODE" == "2" ]]; then
    echo "内网访问地址：http://${SERVER_IP}:${DEFAULT_PORT}"
else
    echo "请使用 HTTPS 反向代理对外提供的地址；不要使用明文 HTTP 登录。"
fi
echo ""
echo "完整使用与管理文档见 $APP_DIR/README.md"
echo "部署指南见 $APP_DIR/部署指南.md"
