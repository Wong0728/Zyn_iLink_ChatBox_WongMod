// 衍生/开发 请 标注 原仓库 "https://github.com/zynsync/Zyn-iLink-ChatBox" 与原作者。
// 仓库受到开源证书保护!请合规使用!

use std::path::PathBuf;
use std::sync::OnceLock;

pub const SCRIPT_VERSION: &str = "3.2.4-wm1.0";
pub const SCRIPT_BASED_ON: &str = "3.1.9";

pub const PBKDF2_ITERATIONS: u32 = 600_000;

pub const MAX_CONFIG_SIZE: u64 = 10 * 1024 * 1024; // 10MB

// 运行时不再调用 `std::env::set_var`（Rust 2024 unsafe），改用 OnceLock 全局变量。
//   原因：Rust 2024 中 `std::env::set_var` 被标记为 unsafe（多线程下有 UB 风险）。
//   即使在 edition 2021 下 safe，多线程服务器中 spawn 之后调用 set_var 仍可能
//   导致其他线程读到部分写入的环境变量，引发难以排查的 bug。
//   修复方案：启动时一次性 resolve 到 OnceLock 全局变量，运行时通过 getter 读取。
//
//   BIND_HOST_OVERRIDE：首次运行向导选择的绑定地址覆盖（替代原 set_var("ILINK_HOST", ...)）。
static BIND_HOST_OVERRIDE: OnceLock<String> = OnceLock::new();

/// 设置首次运行向导选择的绑定地址覆盖。
/// 仅在 `first_run_setup` 完成后调用一次（仅在环境变量 ILINK_HOST 未设置时）。
pub fn set_bind_host_override(host: String) {
    let _ = BIND_HOST_OVERRIDE.set(host);
}

/// 归一化绑定地址：去掉 IPv6 字面量的方括号（"[::]" → "::"）并修剪空白。
/// `format!("{}:{}", host, port)` 拼接 ":::8888" 无法解析为 SocketAddr，
/// 统一先去方括号，构造时用 `SocketAddr::new(IpAddr, port)`。
pub fn normalize_bind_host(raw: &str) -> String {
    let h = raw.trim();
    if h.len() > 2 && h.starts_with('[') && h.ends_with(']') {
        h[1..h.len() - 1].to_string()
    } else {
        h.to_string()
    }
}

/// 绑定地址是否为"仅本机"（非公网暴露）。供启动守卫/提示判断。
/// 覆盖 IPv6：裸 ::1 与带括号 [::1] 均视为本机；其余 IP/主机名视为公网。
pub fn is_private_bind_host(raw: &str) -> bool {
    let h = normalize_bind_host(raw);
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 浏览器可访问的 host:port 展示形式（IPv6 加方括号）。
pub fn host_port_display(raw: &str, port: u16) -> String {
    let h = normalize_bind_host(raw);
    let is_v6 = h
        .parse::<std::net::IpAddr>()
        .map(|ip| matches!(ip, std::net::IpAddr::V6(_)))
        .unwrap_or(false);
    if is_v6 {
        format!("[{}]:{}", h, port)
    } else {
        format!("{}:{}", h, port)
    }
}

/// 返回 Web 服务绑定的 host（已归一化，IPv6 不带方括号）。
/// 优先级：BIND_HOST_OVERRIDE > 环境变量 ILINK_HOST > 默认 "127.0.0.1"。
pub fn bind_host() -> String {
    if let Some(h) = BIND_HOST_OVERRIDE.get() {
        return normalize_bind_host(h);
    }
    std::env::var("ILINK_HOST")
        .map(|v| normalize_bind_host(&v))
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 从环境变量获取数据目录；未配置时固定到可执行文件所在目录。
///
/// 不能使用进程当前工作目录作为默认值：systemd、NSSM、快捷方式和终端从不同
/// 目录启动同一二进制时，会看到不同的数据库，造成数据“消失”的假象。
pub fn base_dir() -> PathBuf {
    std::env::var("ILINK_DATA_DIR")
        .map(|value| {
            let configured = PathBuf::from(value);
            if configured.is_absolute() {
                configured
            } else {
                std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(configured)
            }
        })
        .unwrap_or_else(|_| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."))
        })
}

pub fn config_file() -> PathBuf {
    base_dir().join("wechat_bot_config.json")
}

pub fn media_cache_dir() -> PathBuf {
    base_dir().join("media_cache")
}

pub fn db_file() -> PathBuf {
    base_dir().join("wechat_bot.db")
}

/// 系统库（多用户/认证/配额/守则等）：base_dir/system.db
pub fn system_db_file() -> PathBuf {
    base_dir().join("system.db")
}

/// 用户根目录：base_dir/users
pub fn users_dir() -> PathBuf {
    base_dir().join("users")
}

/// 单用户库（私有消息/媒体记录等）：users/<uid>/user.db
pub fn user_db_file(uid: i64) -> PathBuf {
    users_dir().join(uid.to_string()).join("user.db")
}

/// 单用户根目录：users/<uid>
pub fn user_dir(uid: i64) -> PathBuf {
    users_dir().join(uid.to_string())
}

/// 单用户媒体缓存目录：users/<uid>/media_cache
pub fn user_media_cache_dir(uid: i64) -> PathBuf {
    user_dir(uid).join("media_cache")
}

/// 单用户数据目录：users/<uid>/user_data
pub fn user_data_dir_for_user(uid: i64) -> PathBuf {
    user_dir(uid).join("user_data")
}

/// 安全读取 JSON 文件，超过 max_size 拒绝加载
pub fn load_json_safe(path: &std::path::Path, max_size: u64) -> Option<serde_json::Value> {
    // S58: 先用 metadata 检查文件大小，避免直接 read 超大文件导致 OOM。
    // metadata 失败（特殊文件系统/管道）则回退到直接读取 + 读取后校验。
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > max_size {
            tracing::warn!(
                "[CONFIG] 拒绝加载 {}: 文件 {} 字节超过上限 {} 字节",
                path.display(),
                meta.len(),
                max_size
            );
            return None;
        }
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return None,
    };
    // 二次校验：metadata 可能不准确（特殊文件/流），以实际读取大小为准
    if bytes.len() as u64 > max_size {
        tracing::warn!(
            "[CONFIG] 拒绝加载 {}: 实际读取 {} 字节超过上限 {} 字节",
            path.display(),
            bytes.len(),
            max_size
        );
        return None;
    }
    let content = String::from_utf8(bytes).ok()?;
    serde_json::from_str(&content).ok()
}

/// 检测是否在 Termux 环境中运行
pub fn is_termux() -> bool {
    std::env::consts::OS == "android"
        || std::env::var("PREFIX")
            .map(|p| p.contains("com.termux"))
            .unwrap_or(false)
        || std::path::Path::new("/data/data/com.termux").exists()
}

/// 在 Termux 环境中做兼容性调整
pub fn setup_termux_compat() {
    if !is_termux() {
        return;
    }
    // Termux 默认 /tmp 可能不存在或权限受限，预先创建本地 tmp 目录。
    // 不再调用 `std::env::set_var("TMPDIR", ...)`（Rust 2024 unsafe）。
    //   原因：Rust 2024 中 set_var 为 unsafe；多线程服务器中 spawn 之后调用
    //   可能导致其他线程读到部分写入的环境变量，引发 UB。
    //   本程序代码不直接读 TMPDIR（无 std::env::temp_dir() 调用），第三方库
    //   在 Termux 下通常读系统预设的 TMPDIR；如需覆盖，请在启动前手动设置
    //   环境变量（例如 `export TMPDIR=./tmp`），不要在程序内 set_var。
    if std::env::var("TMPDIR").is_err() {
        let tmp = base_dir().join("tmp");
        let _ = std::fs::create_dir_all(&tmp);
        tracing::info!(
            "[CONFIG] Termux: 已创建本地 tmp 目录 {}（未设置 TMPDIR 环境变量）",
            tmp.display()
        );
    }
    tracing::info!("[CONFIG] 已启用 Termux 兼容模式");
}
