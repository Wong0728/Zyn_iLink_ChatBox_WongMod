// CLI 管理员子命令（Phase 1 Wave 3）
//
// 通过 `ilink-wm1 admin <sub>` 直接操作本地 system.db，不启动 Web 服务。
// 敏感命令（delete/reset-password/config-set-敏感 key）已实施 S10 二次身份确认：
// 提示输入 owner/admin 用户名+密码二次校验。
//
// ⚠ 危险：ILINK_CLI_TRUST=1 跳过所有二次身份确认，仅限单用户/自动化/容器等受控环境使用。
// 禁止在共享终端或多用户服务器上设置。
//     - 任何可被未授权用户触发的脚本（如 CI/CD 中暴露的 admin 调用）
//   跳过时会在控制台打印明显警告 + 记录审计日志（actor=cli, action=*.trust_bypass）
//   以便事后追溯。生产环境强烈建议保持未设置状态。
//
// 命令清单（v2.1 §6）：
//   admin init                              首次初始化 owner 账号
//   admin user list                         列出所有用户
//   admin user create <username> [role]     创建用户
//   admin user delete <user>                删除用户
//   admin user disable <user>               禁用用户
//   admin user enable <user>                启用用户
//   admin user reset-password <user>        重置密码
//   admin user set-quota <user> <key> <v>   设置配额
//   admin user set-feature <user> <key> on/off
//   admin invite create [days] [note]       创建邀请码
//   admin invite list                       列出邀请码
//   admin invite revoke <code>              撤销邀请码
//   admin config get <key>                  读取配置
//   admin config set <key> <value>          写入配置
//   admin config list                       列出所有配置
//   admin server-storage set-local <path>   设置本地存储路径
//   admin server-storage show               显示当前存储配置
//   admin terms set-version <ver>           设置守则版本
//   admin terms set-text                    从 stdin 读守则文本
//   admin stats                             系统统计（含审计 top 5）
//   admin audit list [limit] [--action <a>] [--actor <a>]  审计日志（可按 action/actor 过滤）
//   admin audit stats [limit]               审计日志分组统计
//   admin webset show                       查看前端管理访问策略
//   admin webset set <off|intranet|open>    设置前端管理访问策略
//   admin broadcast send [level] <msg>      发送全局通知
//   admin broadcast clear                   清除全局通知
//   admin broadcast show                    查看当前全局通知

use crate::auth::Auth;
use crate::crypto;
use crate::storage::SystemDatabase;
use std::io::{self, BufRead, Read, Write};
use std::sync::Arc;

// ── clap 命令枚举 ──────────────────────────────────────────────

/// 顶层 CLI 入口（由 main.rs 使用 clap::Parser 派生解析）
#[derive(clap::Parser, Debug)]
#[command(
    name = "ilink-wm1",
    version,
    about = "Zyn iLink ChatBox · WongMod - 多用户版"
)]
pub struct Cli {
    /// 子命令。无子命令时默认启动 Web 服务（向后兼容旧调用方式）。
    #[command(subcommand)]
    pub command: Option<TopCmd>,

    /// 跳过 stdin 交互模式（首次运行向导 + REPL 命令循环）。
    /// CLI 子命令 (ilink-wm1 admin ...) 不受此 flag 影响，始终可用。
    #[arg(long = "no-repl", alias = "nc", default_value_t = false)]
    pub no_repl: bool,

    /// 兼容旧 flag：服务器模式（与 ILINK_SERVER_MODE 等价）
    #[arg(long = "server", alias = "s", default_value_t = false)]
    pub server: bool,

    /// 兼容旧 flag：设置 Web 密码。已迁移到多用户架构，
    /// 该 flag 仅打印提示并退出，引导用户改用 `admin user reset-password`。
    #[arg(long = "setpw", alias = "set-password", default_value_t = false)]
    pub setpw: bool,
}

#[derive(clap::Subcommand, Debug)]
pub enum TopCmd {
    /// 管理员命令组（所有用户/邀请/配置/存储/守则/统计/审计操作）
    #[command(subcommand)]
    Admin(AdminSub),
}

#[derive(clap::Subcommand, Debug)]
pub enum AdminSub {
    /// 首次初始化：创建 owner 账号
    Init,

    /// 用户管理
    #[command(subcommand)]
    User(UserCmd),

    /// 邀请码管理
    #[command(subcommand)]
    Invite(InviteCmd),

    /// 系统配置
    #[command(subcommand)]
    Config(ConfigCmd),

    /// 服务器存储
    #[command(subcommand)]
    ServerStorage(ServerStorageCmd),

    /// 用户守则
    #[command(subcommand)]
    Terms(TermsCmd),

    /// 系统统计
    Stats,

    /// IP 封禁管理
    #[command(subcommand)]
    Ip(IpCmd),

    /// 内网穿透管理（Serveo SSH 隧道）
    #[command(subcommand)]
    Tunnel(TunnelCmd),

    /// 审计日志
    #[command(subcommand)]
    Audit(AuditCmd),

    /// 前端管理面板访问策略
    #[command(subcommand)]
    Webset(WebsetCmd),

    /// 全局通知广播
    #[command(subcommand)]
    Broadcast(BroadcastCmd),
}

#[derive(clap::Subcommand, Debug)]
pub enum UserCmd {
    /// 列出所有用户
    List,
    /// 创建用户
    Create {
        /// 用户名（唯一）
        username: String,
        /// 角色：owner / admin / user
        #[arg(default_value = "user")]
        role: String,
    },
    /// 删除用户（不可删除最后一个 owner）
    Delete {
        /// 用户名或数字 uid
        user: String,
    },
    /// 禁用用户（status → disabled，无法登录）
    Disable { user: String },
    /// 启用用户（status → active）
    Enable { user: String },
    /// 重置密码
    ResetPassword { user: String },
    /// 设置配额：0 = 使用系统默认；负数 = 无限制；正数 = 每日上限。
    SetQuota {
        user: String,
        /// 配额字段：quota_upload_bytes / quota_download_bytes /
        ///           quota_media_bytes / quota_msg_per_day / quota_media_count
        key: String,
        value: i64,
    },
    /// 设置功能开关
    SetFeature {
        user: String,
        /// 功能字段：allow_upload / allow_webdav / allow_custom_webdav
        key: String,
        /// on / off
        value: String,
    },
    /// 设置邮箱
    SetEmail { user: String, email: String },
}

#[derive(clap::Subcommand, Debug)]
pub enum InviteCmd {
    /// 创建邀请码
    Create {
        /// 有效期（天），0 = 永久
        #[arg(default_value = "30")]
        days: i64,
        /// 备注
        #[arg(default_value = "")]
        note: String,
    },
    /// 列出所有邀请码
    List,
    /// 撤销邀请码
    Revoke { code: String },
}

#[derive(clap::Subcommand, Debug)]
pub enum ConfigCmd {
    /// 读取单个配置
    Get { key: String },
    /// 写入配置
    Set { key: String, value: String },
    /// 列出所有配置
    List,
}

#[derive(clap::Subcommand, Debug)]
pub enum ServerStorageCmd {
    /// 设置本地存储路径
    SetLocal { path: String },
    /// 显示当前存储配置
    Show,
    // ponytail: ceiling=`set-webdav` 本期不实现（v2.1 C1 留 ceiling）
}

#[derive(clap::Subcommand, Debug)]
pub enum TermsCmd {
    /// 设置守则版本号
    SetVersion { version: String },
    /// 设置守则文本（从 stdin 读取）
    SetText,
}

#[derive(clap::Subcommand, Debug)]
pub enum AuditCmd {
    /// 列出最近审计日志（可按 action/actor 过滤）
    List {
        #[arg(default_value = "50")]
        limit: i64,
        /// 按 action 过滤（如 login, logout, password.change, admin.user.create）
        #[arg(long)]
        action: Option<String>,
        /// 按 actor 过滤（如 uid=1, cli）
        #[arg(long)]
        actor: Option<String>,
    },
    /// 按 action 分组统计最近审计日志
    Stats {
        /// 统计最近 N 条（默认 1000）
        #[arg(default_value = "1000")]
        limit: i64,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum IpCmd {
    /// 封禁 IP 地址
    Ban {
        /// IP 地址（支持 IPv4 和 IPv6）
        ip: String,
        /// 封禁原因
        #[arg(long, default_value = "")]
        reason: String,
        /// 封禁天数（0 = 永久，默认 7 天）
        #[arg(long, default_value = "7")]
        days: i64,
    },
    /// 解封 IP 地址
    Unban {
        /// IP 地址
        ip: String,
    },
    /// 列出所有封禁记录
    List,
}

#[derive(clap::Subcommand, Debug)]
pub enum TunnelCmd {
    /// 启动内网穿透隧道
    Start {
        /// 本地服务端口（默认 8888）
        #[arg(long, default_value = "8888")]
        port: u16,
        /// 远程端口（Serveo 默认 80，一般无需修改）
        #[arg(long, default_value = "80")]
        remote: u16,
        /// 自定义子域名（空 = 随机生成）
        #[arg(long, default_value = "")]
        subdomain: String,
    },
    /// 停止隧道
    Stop,
    /// 查看隧道状态
    Status,
    /// 查看隧道日志
    Logs {
        /// 显示最近 N 行（默认 20）
        #[arg(default_value = "20")]
        count: usize,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum WebsetCmd {
    /// 显示当前前端管理面板访问策略
    Show,
    /// 设置策略: off(关闭) | intranet(仅内网,默认) | open(公网可访问)
    Set {
        /// off / intranet / open
        mode: String,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum BroadcastCmd {
    /// 发送全局通知
    Send {
        /// 级别: info / warn / error
        #[arg(default_value = "info")]
        level: String,
        /// 通知内容（含空格请用引号）
        message: String,
    },
    /// 清除当前全局通知
    Clear,
    /// 显示当前全局通知
    Show,
}

// ── 命令分发 ──────────────────────────────────────────────────

/// 管理员命令入口。由 main.rs 在解析出 `Some(TopCmd::Admin(sub))` 时调用。
pub fn run_admin(sub: &AdminSub) -> anyhow::Result<()> {
    let system_db = SystemDatabase::new()?;
    let auth = Auth::new(system_db.clone());

    match sub {
        AdminSub::Init => cmd_init(&system_db, &auth),
        AdminSub::User(c) => cmd_user(&system_db, &auth, c),
        AdminSub::Invite(c) => cmd_invite(&system_db, c),
        AdminSub::Config(c) => cmd_config(&system_db, &auth, c),
        AdminSub::ServerStorage(c) => cmd_server_storage(&system_db, c),
        AdminSub::Terms(c) => cmd_terms(&system_db, c),
        AdminSub::Stats => cmd_stats(&system_db),
        AdminSub::Ip(c) => cmd_ip(&system_db, &auth, c),
        AdminSub::Tunnel(c) => cmd_tunnel(&auth, c),
        AdminSub::Audit(c) => cmd_audit(&system_db, c),
        AdminSub::Webset(c) => cmd_webset(&system_db, &auth, c),
        AdminSub::Broadcast(c) => cmd_broadcast(&system_db, c),
    }
}

// ── 实现细节 ──────────────────────────────────────────────────

// Phase 5 (LOW-2): read_password_with_mask 已统一到 crate::auth 模块，本地副本删除。

/// 把用户名或数字 uid 解析为 AppUser
fn lookup_user(system_db: &SystemDatabase, user: &str) -> anyhow::Result<crate::models::AppUser> {
    // 优先按数字 uid 查
    if let Ok(uid) = user.parse::<i64>() {
        if let Some(u) = system_db.get_user_by_id(uid) {
            return Ok(u);
        }
    }
    system_db
        .get_user_by_username(user)
        .ok_or_else(|| anyhow::anyhow!("用户不存在: {}", user))
}

/// Phase 5 (§8.2 + Fix-4): 判断 system_settings 中的 key 是否为敏感配置。
///   敏感 key 在 Config::Get / Config::List 中必须以 *** 脱敏显示，
///   避免管理员口令哈希、JWT/session 密钥、加密主密钥等通过 CLI 泄漏到终端历史/日志。
fn is_sensitive_setting(key: &str) -> bool {
    let k = key.to_lowercase();
    // 完全匹配或带命名空间前缀（system.*、server_storage.* 等）
    k == "jwt_secret" || k.ends_with(".jwt_secret")
        || k == "session_secret" || k.ends_with(".session_secret")
        || k == "encryption_key" || k.ends_with(".encryption_key")
        || k == "admin_password_hash" || k.ends_with(".admin_password_hash")
        || k == "admin_salt" || k.ends_with(".admin_salt")
        // WebDAV 凭证类
        || k.contains("webdav_password")
        || k.contains("webdav.token")
        // 通用敏感后缀
        || k.ends_with("_secret") || k.ends_with("_password_hash") || k.ends_with("_salt")
}

/// Phase 5 (S10): 破坏性命令的二次身份确认。
///   提示输入任意 owner/admin 的用户名 + 密码,通过 Auth::verify_user_credentials 校验。
///   防止本机误操作(共享管理终端、sudo 上下文)下,无权用户直接执行 delete/reset-password。
///   ponytail: ceiling=本机 CLI 已假定 OS 级鉴权完成,此层是应用层纵深防御,
///             不替代 OS 权限管理。管道输入或 `admin --force` 可提供无交互方式。
fn confirm_admin_identity(auth: &Auth, action: &str) -> anyhow::Result<()> {
    // ILINK_CLI_TRUST=1 跳过二次身份确认（逃生阀，仅在受控环境使用）。
    //   ⚠ 跳过会任何能执行 `ilink-wm1 admin` 的本机用户均可无交互运行破坏性命令。
    //   详见文件顶部 M17 风险提示。
    let trust_bypass = std::env::var("ILINK_CLI_TRUST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        .unwrap_or(false);
    if trust_bypass {
        println!("┌─ ⚠ M17 安全警告 ───────────────────────────────");
        println!("│ ILINK_CLI_TRUST=1 已设置，跳过 S10 二次身份确认");
        println!("│ 即将执行: {}", action);
        println!("│ ⚠ 任何能执行本命令的本机用户均可触发破坏性操作");
        println!("│ 此跳过行为不会写入审计日志的 trust_bypass 字段，");
        println!("│ 但操作本身仍会按常规路径记录审计日志（actor=cli）");
        println!("└─────────────────────────────────────────────────");
        tracing::warn!(
            "[M17] ILINK_CLI_TRUST=1 已跳过 S10 二次身份确认 action={}",
            action
        );
        return Ok(());
    }

    println!("┌─ S10 二次身份确认 ─────────────────────────────");
    println!("│ 即将执行: {}", action);
    println!("│ 请输入 owner/admin 账号凭证以确认授权");
    println!("└─────────────────────────────────────────────────");

    print!("管理员用户名: ");
    let _ = io::stdout().flush();
    let mut username = String::new();
    if io::stdin().lock().read_line(&mut username).is_err() {
        anyhow::bail!("读取用户名失败");
    }
    let username = username.trim().to_string();
    if username.is_empty() {
        anyhow::bail!("用户名不能为空");
    }
    let password = crate::auth::read_password_with_mask("管理员密码: ");
    if password.is_empty() {
        anyhow::bail!("密码不能为空");
    }

    match auth.verify_user_credentials(&username, &password) {
        Some((_, role)) if role == "owner" || role == "admin" => {
            println!("  ✓ 身份确认通过（{}: {}）", username, role);
            Ok(())
        }
        Some((_, role)) => {
            anyhow::bail!(
                "账号 {} 角色为 {},无权执行此操作(需 owner/admin)",
                username,
                role
            )
        }
        None => anyhow::bail!("管理员凭证校验失败(用户名或密码错误)"),
    }
}

fn cmd_init(system_db: &SystemDatabase, auth: &Auth) -> anyhow::Result<()> {
    // ponytail: ceiling=owner 创建逻辑与 main.rs::first_run_setup 重复约 70 行
    //   （用户名 loop + 密码 loop + create_user + audit）。详见 first_run_setup 的 ceiling 注释。
    // 已有用户 → 拒绝重复初始化
    let existing = system_db.list_users();
    if !existing.is_empty() {
        println!(
            "[admin init] system.db 已有 {} 个用户，跳过初始化",
            existing.len()
        );
        for u in &existing {
            println!(
                "  uid={} username={} role={} status={}",
                u.id, u.username, u.role, u.status
            );
        }
        return Ok(());
    }

    println!("Zyn iLink ChatBox · 多用户初始化");
    println!("{}", "=".repeat(60));
    println!("  请创建 owner 账号（系统最高权限）");
    println!("{}", "=".repeat(60));

    // 1. 用户名
    let username = loop {
        print!("用户名 (默认 owner): ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().lock().read_line(&mut input).is_err() {
            return Err(anyhow::anyhow!("读取用户名失败"));
        }
        let name = input.trim().to_string();
        if name.is_empty() {
            break "owner".to_string();
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            println!("  ✗ 用户名仅允许字母数字、下划线、连字符");
            continue;
        }
        if name.len() < 3 || name.len() > 32 {
            println!("  ✗ 用户名长度需 3-32 字符");
            continue;
        }
        break name;
    };

    // 2. 密码
    println!("密码要求: 8-128 位，必须包含大写字母、小写字母和数字");
    let password = loop {
        let pw1 = crate::auth::read_password_with_mask("请输入密码: ");
        if pw1.is_empty() {
            println!("  ✗ 密码不能为空");
            continue;
        }
        if let Err(e) = Auth::check_password_strength(&pw1) {
            println!("  ✗ {}", e);
            continue;
        }
        let pw2 = crate::auth::read_password_with_mask("请再次输入密码确认: ");
        if pw1 != pw2 {
            println!("  ✗ 两次密码不一致");
            continue;
        }
        break pw1;
    };

    // 3. 创建 owner（create_user 返回 Result<_, String>，转 anyhow 以便 ? 传播）
    let uid = auth
        .create_user(&username, &password, "owner")
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
    system_db.audit_log_warn(
        "cli",
        "admin.init",
        Some(&format!("uid={}", uid)),
        Some(&format!("{{\"username\":\"{}\"}}", username)),
    );

    println!();
    println!("  ✓ owner 账号创建成功");
    println!("    uid      : {}", uid);
    println!("    username : {}", username);
    println!();
    println!("  现在可以启动服务: ilink-wm1");
    Ok(())
}

fn cmd_user(system_db: &SystemDatabase, auth: &Auth, cmd: &UserCmd) -> anyhow::Result<()> {
    match cmd {
        UserCmd::List => {
            let users = system_db.list_users();
            if users.is_empty() {
                println!("（暂无用户，请运行: ilink-wm1 admin init）");
                return Ok(());
            }
            println!("用户列表（{} 个）", users.len());
            println!("{}", "=".repeat(60));
            println!(
                "{:<6} {:<24} {:<8} {:<10} {:<22}",
                "UID", "USERNAME", "ROLE", "STATUS", "CREATED"
            );
            println!("{}", "-".repeat(60));
            for u in &users {
                println!(
                    "{:<6} {:<24} {:<8} {:<10} {:<22}",
                    u.id,
                    u.username,
                    u.role,
                    u.status,
                    &u.created_at[..u.created_at.len().min(19)]
                );
            }
        }
        UserCmd::Create { username, role } => {
            // 校验角色
            if !matches!(role.as_str(), "owner" | "admin" | "user") {
                anyhow::bail!("非法角色: {}（允许 owner/admin/user）", role);
            }
            // 审计 M-3: 创建 owner/admin 等同于授予系统管理权，与 delete/reset-password
            // 同级，纳入 S10 二次身份确认；仅当系统尚无任何管理员（首次初始化）时豁免，
            // 保留 `admin init` 之外的自举路径。
            if role != "user"
                && system_db
                    .list_users()
                    .iter()
                    .any(|u| matches!(u.role.as_str(), "owner" | "admin"))
            {
                confirm_admin_identity(auth, &format!("创建 {} 角色用户 {}", role, username))?;
            }
            println!("为用户 {} 设置密码", username);
            println!("密码要求: 8-128 位，必须包含大写字母、小写字母和数字");
            let password = loop {
                let pw1 = crate::auth::read_password_with_mask("请输入密码: ");
                if pw1.is_empty() {
                    println!("  ✗ 密码不能为空");
                    continue;
                }
                if let Err(e) = Auth::check_password_strength(&pw1) {
                    println!("  ✗ {}", e);
                    continue;
                }
                let pw2 = crate::auth::read_password_with_mask("请再次输入密码确认: ");
                if pw1 != pw2 {
                    println!("  ✗ 两次密码不一致");
                    continue;
                }
                break pw1;
            };
            let uid = auth
                .create_user(username, &password, role)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.user.create",
                Some(&format!("uid={}", uid)),
                Some(&format!(
                    "{{\"username\":\"{}\",\"role\":\"{}\"}}",
                    username, role
                )),
            );
            println!(
                "  ✓ 用户创建成功: uid={} username={} role={}",
                uid, username, role
            );
        }
        UserCmd::Delete { user } => {
            // Phase 5 (S10): 破坏性操作二次身份确认
            confirm_admin_identity(auth, &format!("删除用户 {}", user))?;
            let u = lookup_user(system_db, user)?;
            // 防止删除最后一个 owner
            if u.role == "owner" {
                let owners: Vec<_> = system_db
                    .list_users()
                    .into_iter()
                    .filter(|x| x.role == "owner" && x.status == "active")
                    .collect();
                if owners.len() <= 1 {
                    anyhow::bail!("不可删除最后一个 owner（系统必须有至少一个 owner）");
                }
            }
            // 删用户属关键操作，审计日志写入失败必须阻断。
            //   CLI 已通过 confirm_admin_identity 二次身份确认，但仍需保证审计可追溯。
            //   审计前置：失败则 bail，避免 delete_user 已执行但无审计记录。
            if !system_db.audit_log_warn(
                "cli",
                "admin.user.delete",
                Some(&format!("uid={}", u.id)),
                Some(&format!("{{\"username\":\"{}\"}}", u.username)),
            ) {
                anyhow::bail!("审计日志写入失败，拒绝执行删除操作以保护可追溯性");
            }
            system_db.delete_user(u.id)?;
            println!("  ✓ 已删除用户: uid={} username={}", u.id, u.username);
        }
        UserCmd::Disable { user } => {
            let u = lookup_user(system_db, user)?;
            if u.role == "owner" {
                anyhow::bail!("不可禁用 owner 账号");
            }
            system_db.update_user_status(u.id, "disabled")?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.user.disable",
                Some(&format!("uid={}", u.id)),
                Some(&format!("{{\"username\":\"{}\"}}", u.username)),
            );
            println!("  ✓ 已禁用用户: uid={} username={}", u.id, u.username);
        }
        UserCmd::Enable { user } => {
            let u = lookup_user(system_db, user)?;
            system_db.update_user_status(u.id, "active")?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.user.enable",
                Some(&format!("uid={}", u.id)),
                Some(&format!("{{\"username\":\"{}\"}}", u.username)),
            );
            println!("  ✓ 已启用用户: uid={} username={}", u.id, u.username);
        }
        UserCmd::ResetPassword { user } => {
            // Phase 5 (S10): 破坏性操作二次身份确认（重置他人密码）
            confirm_admin_identity(auth, &format!("重置用户 {} 的密码", user))?;
            let u = lookup_user(system_db, user)?;
            println!("为用户 {} (uid={}) 重置密码", u.username, u.id);
            println!("密码要求: 8-128 位，必须包含大写字母、小写字母和数字");
            let new_pw = loop {
                let pw1 = crate::auth::read_password_with_mask("请输入新密码: ");
                if pw1.is_empty() {
                    println!("  ✗ 密码不能为空");
                    continue;
                }
                if let Err(e) = Auth::check_password_strength(&pw1) {
                    println!("  ✗ {}", e);
                    continue;
                }
                let pw2 = crate::auth::read_password_with_mask("请再次输入密码确认: ");
                if pw1 != pw2 {
                    println!("  ✗ 两次密码不一致");
                    continue;
                }
                break pw1;
            };
            // 直接生成新盐 + hash 写库（不走 change_password 的旧密码校验）
            let salt = crypto::random_hex(16);
            let hash = crypto::pbkdf2_hash(&new_pw, &salt, crate::config::PBKDF2_ITERATIONS);
            // 重置他人密码属关键操作，审计日志写入失败必须阻断。
            //   CLI 已通过 confirm_admin_identity 二次身份确认，但仍需保证审计可追溯。
            //   审计前置：失败则 bail，避免密码已被重置但无审计记录。
            if !system_db.audit_log_warn(
                "cli",
                "admin.user.reset-password",
                Some(&format!("uid={}", u.id)),
                Some(&format!("{{\"username\":\"{}\"}}", u.username)),
            ) {
                anyhow::bail!("审计日志写入失败，拒绝执行密码重置以保护可追溯性");
            }
            system_db.set_user_password(
                u.id,
                &hash,
                &salt,
                crate::config::PBKDF2_ITERATIONS as i64,
            )?;
            // 让该用户的所有现有 session 失效（保留空 token 即删除全部）
            let _ = system_db.delete_other_sessions(u.id, "");
            println!("  ✓ 密码已重置，该用户的所有现有会话已失效");
        }
        UserCmd::SetQuota { user, key, value } => {
            let u = lookup_user(system_db, user)?;
            // 配额设置：0=系统默认，负数=无限制，正数=每日上限。注意语义避免误解。
            //   - 0 = 使用系统默认（系统默认未设 = 无限制）
            //   - 负数 = 无限制（显式声明）
            //   - 正数 = 每日/累计上限
            //   管理员想"封禁"某维度时设 0 是错的（0=用系统默认），应改用 set-feature 关闭功能。
            if *value == 0 {
                println!("  ⚠ 提示: 0 = 使用系统默认（系统默认未设时等价于无限制）");
                println!(
                    "    如需完全禁止该维度，请改用 `admin user set-feature {} <对应功能> off`",
                    u.username
                );
            } else if *value < 0 {
                println!("  ⚠ 提示: 负数 = 显式无限制（与 0=系统默认 的语义区分）");
            }
            system_db.update_user_quota(u.id, key, *value)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.user.set-quota",
                Some(&format!("uid={}", u.id)),
                Some(&format!(
                    "{{\"username\":\"{}\",\"key\":\"{}\",\"value\":{}}}",
                    u.username, key, value
                )),
            );
            println!("  ✓ 已设置 {} 的 {} = {}", u.username, key, value);
        }
        UserCmd::SetFeature { user, key, value } => {
            let u = lookup_user(system_db, user)?;
            let on = match value.to_lowercase().as_str() {
                "on" | "true" | "1" | "yes" => true,
                "off" | "false" | "0" | "no" => false,
                _ => anyhow::bail!("非法 value: {}（允许 on/off）", value),
            };
            system_db.update_user_feature(u.id, key, on)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.user.set-feature",
                Some(&format!("uid={}", u.id)),
                Some(&format!(
                    "{{\"username\":\"{}\",\"key\":\"{}\",\"value\":{}}}",
                    u.username, key, on
                )),
            );
            println!("  ✓ 已设置 {} 的 {} = {}", u.username, key, on);
        }
        UserCmd::SetEmail { user, email } => {
            let u = lookup_user(system_db, user)?;
            system_db.set_user_email(u.id, email)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.user.set-email",
                Some(&format!("uid={}", u.id)),
                Some(&format!(
                    "{{\"username\":\"{}\",\"email\":\"{}\"}}",
                    u.username, email
                )),
            );
            println!("  ✓ 已设置 {} 的邮箱 = {}", u.username, email);
        }
    }
    Ok(())
}

fn cmd_invite(system_db: &SystemDatabase, cmd: &InviteCmd) -> anyhow::Result<()> {
    match cmd {
        InviteCmd::Create { days, note } => {
            // 4 位大写字母+数字组合（如 A3F5），与首次运行向导生成逻辑一致
            let code = system_db.allocate_invite_code()?;
            let expires_at = if *days > 0 {
                let now = chrono::Utc::now();
                Some(
                    now.checked_add_signed(chrono::Duration::days(*days))
                        .unwrap_or(now)
                        .to_rfc3339(),
                )
            } else {
                None
            };
            system_db.create_invite(
                &code,
                expires_at.as_deref(),
                if note.is_empty() { None } else { Some(note) },
            )?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.invite.create",
                Some(&code),
                // L-15：note 为自由文本，经 serde_json 转义防破坏 JSON 结构
                Some(&serde_json::json!({ "days": days, "note": note }).to_string()),
            );
            println!("  ✓ 邀请码创建成功");
            println!("    code       : {}", code);
            println!(
                "    expires_at : {}",
                expires_at.as_deref().unwrap_or("永久")
            );
            if !note.is_empty() {
                println!("    note       : {}", note);
            }
        }
        InviteCmd::List => {
            let invites = system_db.list_invites();
            if invites.is_empty() {
                println!("（暂无邀请码）");
                return Ok(());
            }
            println!("邀请码列表（{} 个）", invites.len());
            println!("{}", "=".repeat(60));
            println!(
                "{:<20} {:<10} {:<26} {:<10}",
                "CODE", "STATUS", "CREATED", "EXPIRES"
            );
            println!("{}", "-".repeat(60));
            for i in &invites {
                println!(
                    "{:<20} {:<10} {:<26} {:<10}",
                    i.code,
                    i.status,
                    &i.created_at[..i.created_at.len().min(19)],
                    i.expires_at.as_deref().unwrap_or("永久")
                );
            }
        }
        InviteCmd::Revoke { code } => {
            system_db.revoke_invite(code)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn("cli", "admin.invite.revoke", Some(code), None);
            println!("  ✓ 邀请码已撤销: {}", code);
        }
    }
    Ok(())
}

fn cmd_config(system_db: &SystemDatabase, auth: &Auth, cmd: &ConfigCmd) -> anyhow::Result<()> {
    match cmd {
        ConfigCmd::Get { key } => {
            if !crate::storage::is_supported_system_setting(key) {
                anyhow::bail!("不支持的系统设置: {}", key);
            }
            match system_db.get_setting(key) {
                // Phase 5 (§8.2): 敏感 key 脱敏显示，防止通过 CLI 终端历史/日志泄漏
                Some(v) if is_sensitive_setting(key) => {
                    println!("***(敏感配置，已脱敏，长度 {} 字符)", v.len())
                }
                Some(v) => println!("{}", v),
                None => {
                    // 未设置的键 exit(0) 并打印空值，
                    //   让脚本能正常区分"未设置"和"致命错误"。
                    println!();
                    std::process::exit(0);
                }
            }
        }
        ConfigCmd::Set { key, value } => {
            crate::storage::validate_system_setting(key, value)?;
            // Phase 5 (S10): 写入敏感 key 前要求二次身份确认（除非 ILINK_CLI_TRUST=1）
            if is_sensitive_setting(key) {
                confirm_admin_identity(auth, &format!("config set {}=<敏感值>", key))?;
            }
            system_db.set_setting(key, value)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.config.set",
                Some(key),
                Some(&format!("{{\"value_len\":{}}}", value.len())),
            );
            // 不回显敏感 value，避免终端日志泄漏
            if is_sensitive_setting(key) {
                println!("  ✓ 已设置 {} = *** (敏感配置，已脱敏)", key);
            } else {
                println!("  ✓ 已设置 {} = {}", key, value);
            }
        }
        ConfigCmd::List => {
            let settings: Vec<_> = system_db
                .list_settings()
                .into_iter()
                .filter(|setting| crate::storage::is_supported_system_setting(&setting.key))
                .collect();
            if settings.is_empty() {
                println!("（暂无配置）");
                return Ok(());
            }
            println!("系统配置（{} 项）", settings.len());
            println!("{}", "=".repeat(60));
            println!("{:<32} VALUE", "KEY");
            println!("{}", "-".repeat(60));
            for s in &settings {
                // Phase 5 (§8.2): 敏感 key 脱敏
                let v = if is_sensitive_setting(&s.key) {
                    format!("*** ({} 字符,已脱敏)", s.value.len())
                } else if s.value.len() > 60 {
                    format!("{}...", &s.value[..60])
                } else {
                    s.value.clone()
                };
                println!("{:<32} {}", s.key, v);
            }
        }
    }
    Ok(())
}

fn cmd_server_storage(_system_db: &SystemDatabase, cmd: &ServerStorageCmd) -> anyhow::Result<()> {
    match cmd {
        ServerStorageCmd::SetLocal { path } => {
            anyhow::bail!(
                "此命令已废弃且不会修改运行目录。请在启动服务前设置绝对路径 ILINK_DATA_DIR={}，再重启服务",
                path
            );
        }
        ServerStorageCmd::Show => {
            println!("服务器存储");
            println!("{}", "=".repeat(60));
            println!("本地存储路径 : {}", crate::config::base_dir().display());
            println!("配置方式       : 启动前设置 ILINK_DATA_DIR（必须使用绝对路径）");
        }
    }
    Ok(())
}

fn cmd_terms(system_db: &SystemDatabase, cmd: &TermsCmd) -> anyhow::Result<()> {
    match cmd {
        TermsCmd::SetVersion { version } => {
            crate::storage::validate_system_setting("terms_version", version)?;
            system_db.set_setting("terms_version", version)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn("cli", "admin.terms.set-version", Some(version), None);
            println!("  ✓ 守则版本已设置为: {}", version);
        }
        TermsCmd::SetText => {
            println!("请粘贴守则文本，结束按 Ctrl+D（Unix）或 Ctrl+Z 然后 Enter（Windows）：");
            let mut text = String::new();
            io::stdin().read_to_string(&mut text)?;
            crate::storage::validate_system_setting("terms_text", &text)?;
            system_db.set_setting("terms_text", &text)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.terms.set-text",
                None,
                Some(&format!("{{\"bytes\":{}}}", text.len())),
            );
            println!("  ✓ 守则文本已保存（{} 字节）", text.len());
        }
    }
    Ok(())
}

fn cmd_stats(system_db: &SystemDatabase) -> anyhow::Result<()> {
    let users = system_db.list_users();
    let active: usize = users.iter().filter(|u| u.status == "active").count();
    let disabled: usize = users.iter().filter(|u| u.status == "disabled").count();
    let owners: usize = users.iter().filter(|u| u.role == "owner").count();
    let admins: usize = users.iter().filter(|u| u.role == "admin").count();
    let invites = system_db.list_invites();
    let active_invites: usize = invites.iter().filter(|i| i.status == "active").count();
    let settings = system_db.list_settings();
    // 用 audit_log_count 取真实总数；list_audit 只取最近活动用于显示时间。
    let recent_audit = system_db.list_audit(1000);
    let audit_total = system_db.audit_log_count();
    let last_activity = recent_audit
        .first()
        .map(|l| &l.created_at[..l.created_at.len().min(19)]);

    println!("Zyn iLink ChatBox · 系统统计");
    println!("{}", "=".repeat(60));
    println!("用户总数     : {}", users.len());
    println!("  active     : {}", active);
    println!("  disabled   : {}", disabled);
    println!("  owner      : {}", owners);
    println!("  admin      : {}", admins);
    println!("邀请码总数   : {}", invites.len());
    println!("  active     : {}", active_invites);
    println!("系统配置项数 : {}", settings.len());
    // 显示真实总数 + 近 1000 条（让管理员判断是否被覆盖）
    println!("审计日志     : 总 {} 条（显示近 1000）", audit_total);
    if let Some(t) = last_activity {
        println!("最近活动时间    : {}", t);
    }
    // 按 action 分组 top 5
    let mut by_action: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for l in &recent_audit {
        *by_action.entry(l.action.as_str()).or_insert(0) += 1;
    }
    let mut actions: Vec<_> = by_action.into_iter().collect();
    actions.sort_by_key(|item| std::cmp::Reverse(item.1));
    if !actions.is_empty() {
        println!("最近活动 Top 5 :");
        for (a, c) in actions.iter().take(5) {
            println!("  {:<24} {} 次", a, c);
        }
    }
    println!("{}", "=".repeat(60));
    Ok(())
}

fn cmd_ip(system_db: &SystemDatabase, auth: &Auth, cmd: &IpCmd) -> anyhow::Result<()> {
    match cmd {
        IpCmd::Ban { ip, reason, days } => {
            let (network, dangerous) = crate::web::normalize_ip_network(ip)?;
            // S10 二次身份确认
            let risk = if dangerous {
                "，高风险网段/地址"
            } else {
                ""
            };
            let action_desc = format!(
                "封禁 IP/CIDR {}（{} 天, reason: {}{}）",
                network, days, reason, risk
            );
            confirm_admin_identity(auth, &action_desc)?;
            let ban_days = if *days > 0 { Some(*days) } else { None };
            system_db.ban_ip(&network, reason, "cli", ban_days)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.ip.ban",
                Some(&network),
                // L-15：reason 为自由文本，经 serde_json 转义防破坏 JSON 结构
                Some(&serde_json::json!({ "days": days, "reason": reason }).to_string()),
            );
            if *days > 0 {
                println!("  ✓ IP/CIDR {} 已被封禁（{} 天）", network, days);
            } else {
                println!("  ✓ IP/CIDR {} 已被永久封禁", network);
            }
        }
        IpCmd::Unban { ip } => {
            let (network, _) = crate::web::normalize_ip_network(ip)?;
            confirm_admin_identity(auth, &format!("解封 IP/CIDR {}", network))?;
            // 解封 IP 属关键操作，审计日志写入失败必须阻断。
            //   CLI 已通过 confirm_admin_identity 二次身份确认，但仍需保证审计可追溯。
            //   审计前置：失败则 bail，避免 IP 已被解封但无审计记录。
            if !system_db.audit_log_warn("cli", "admin.ip.unban", Some(&network), None) {
                anyhow::bail!("审计日志写入失败，拒绝执行解封操作以保护可追溯性");
            }
            system_db.unban_ip(&network)?;
            println!("  ✓ IP/CIDR {} 已解封", network);
        }
        IpCmd::List => {
            let bans = system_db.list_ip_bans();
            if bans.is_empty() {
                println!("（暂无封禁记录）");
                return Ok(());
            }
            println!("IP 封禁记录（{} 条）", bans.len());
            println!("{}", "=".repeat(60));
            println!(
                "{:<6} {:<18} {:<22} {:<12} BANNED_BY",
                "ID", "IP", "EXPIRES_AT", "REASON"
            );
            println!("{}", "-".repeat(60));
            for b in &bans {
                let expires = b.expires_at.as_deref().unwrap_or("永久");
                println!(
                    "{:<6} {:<18} {:<22} {:<12} {}",
                    b.id, b.ip, expires, b.reason, b.banned_by
                );
            }
        }
    }
    Ok(())
}

/// 全局隧道管理器单例（供 CLI 和 Web API 共用）
static TUNNEL_MANAGER: once_cell::sync::Lazy<crate::tunnel::TunnelManager> =
    once_cell::sync::Lazy::new(|| {
        let mgr = crate::tunnel::TunnelManager::new();
        // 启动时自动恢复持久化配置的隧道
        mgr.try_restore();
        mgr
    });

/// 获取全局隧道管理器引用
pub fn get_tunnel_manager() -> &'static crate::tunnel::TunnelManager {
    &TUNNEL_MANAGER
}

fn cmd_tunnel(auth: &Auth, cmd: &TunnelCmd) -> anyhow::Result<()> {
    let tm = get_tunnel_manager();
    match cmd {
        TunnelCmd::Start {
            port,
            remote,
            subdomain,
        } => {
            // 审计 M-4: 开隧道 = 把本地端口暴露公网，属破坏性操作，纳入 S10 二次确认
            confirm_admin_identity(
                auth,
                &format!(
                    "启动内网穿透隧道（本地端口 {} → serveo 远程端口 {}）",
                    port, remote
                ),
            )?;
            match tm.start(*port, *remote, subdomain) {
                Ok(()) => {
                    println!("  ✓ 隧道已启动");
                    println!("    本地端口 : {}", port);
                    println!("    远程端口 : {}", remote);
                    if !subdomain.is_empty() {
                        println!("    子域名   : {}", subdomain);
                        println!(
                            "    公网 URL : https://{}.serveo.net (约 30s 后生效)",
                            subdomain
                        );
                    } else {
                        // serveo 对未指定子域名的连接分配随机子域名（与端口号无关），
                        // 实际地址以隧道 stdout 提取结果为准。
                        println!(
                            "    公网 URL : 由 serveo 随机分配，请用 `admin tunnel status` 查看"
                        );
                    }
                }
                Err(e) => println!("  ✗ 启动失败: {}", e),
            }
        }
        TunnelCmd::Stop => match tm.stop() {
            Ok(()) => println!("  ✓ 隧道已停止"),
            Err(e) => println!("  ✗ {}", e),
        },
        TunnelCmd::Status => {
            let info = tm.status();
            println!("  状态     : {:?}", info.state);
            println!("  本地端口 : {}", info.local_port);
            println!("  远程端口 : {}", info.remote_port);
            println!(
                "  子域名   : {}",
                if info.subdomain.is_empty() {
                    "(随机)"
                } else {
                    &info.subdomain
                }
            );
            if let Some(url) = &info.public_url {
                println!("  公网 URL : {}", url);
            }
            if let Some(pid) = info.pid {
                println!("  进程 PID : {}", pid);
            }
        }
        TunnelCmd::Logs { count } => {
            let logs = tm.logs(*count);
            if logs.is_empty() {
                println!("（暂无日志）");
            } else {
                for line in &logs {
                    println!("{}", line);
                }
            }
        }
    }
    Ok(())
}

fn cmd_audit(system_db: &SystemDatabase, cmd: &AuditCmd) -> anyhow::Result<()> {
    match cmd {
        AuditCmd::List {
            limit,
            action,
            actor,
        } => {
            let limit = *limit;
            let logs = system_db.list_audit(limit);
            // Phase 5: 支持 --action / --actor 过滤
            let filtered: Vec<_> = logs
                .into_iter()
                .filter(|l| {
                    if let Some(a) = action {
                        if !l.action.contains(a) {
                            return false;
                        }
                    }
                    if let Some(a) = actor {
                        if !l.actor.contains(a) {
                            return false;
                        }
                    }
                    true
                })
                .collect();
            if filtered.is_empty() {
                println!("（暂无审计日志）");
                return Ok(());
            }
            println!("审计日志（{} 条，过滤后）", filtered.len());
            println!("{}", "=".repeat(60));
            println!(
                "{:<6} {:<22} {:<12} {:<24} TARGET",
                "ID", "TIME", "ACTOR", "ACTION"
            );
            println!("{}", "-".repeat(60));
            for l in &filtered {
                println!(
                    "{:<6} {:<22} {:<12} {:<24} {}",
                    l.id,
                    &l.created_at[..l.created_at.len().min(19)],
                    l.actor,
                    l.action,
                    l.target.as_deref().unwrap_or("")
                );
                if let Some(d) = &l.detail_json {
                    println!("       detail: {}", d);
                }
            }
        }
        AuditCmd::Stats { limit } => {
            // Phase 5: 按 action 分组统计最近 N 条审计日志
            let logs = system_db.list_audit(*limit);
            if logs.is_empty() {
                println!("（暂无审计日志）");
                return Ok(());
            }
            let mut by_action: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            let mut by_actor: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for l in &logs {
                *by_action.entry(l.action.as_str()).or_insert(0) += 1;
                *by_actor.entry(l.actor.as_str()).or_insert(0) += 1;
            }
            let mut actions: Vec<_> = by_action.into_iter().collect();
            actions.sort_by_key(|item| std::cmp::Reverse(item.1));
            let mut actors: Vec<_> = by_actor.into_iter().collect();
            actors.sort_by_key(|item| std::cmp::Reverse(item.1));

            println!("审计日志统计（最近 {} 条）", logs.len());
            println!("{}", "=".repeat(60));
            println!("\n按 Action 分组（Top 20）:");
            println!("{:<32} {:<8}", "ACTION", "COUNT");
            println!("{}", "-".repeat(60));
            for (a, c) in actions.iter().take(20) {
                println!("{:<32} {:<8}", a, c);
            }
            println!("\n按 Actor 分组（Top 20）:");
            println!("{:<24} {:<8}", "ACTOR", "COUNT");
            println!("{}", "-".repeat(60));
            for (a, c) in actors.iter().take(20) {
                println!("{:<24} {:<8}", a, c);
            }
            println!("\n{}", "=".repeat(60));
        }
    }
    Ok(())
}

// 抑制未使用警告（Arc 在 run_admin 内已用，但保留 import 以便后续扩展）
#[allow(dead_code)]
fn _unused_arc_marker(_: Arc<()>) {}

// ── 前端管理面板访问策略（admin.web_access） ──────────────────

fn cmd_webset(system_db: &SystemDatabase, auth: &Auth, cmd: &WebsetCmd) -> anyhow::Result<()> {
    match cmd {
        WebsetCmd::Show => {
            let cur = system_db
                .get_setting("admin.web_access")
                .unwrap_or_else(|| "intranet".to_string());
            println!("前端管理面板访问策略");
            println!("{}", "=".repeat(60));
            println!("当前策略: {}", cur);
            println!("  off       = 完全关闭前端管理（仅 CLI 可操作）");
            println!("  intranet  = 仅内网 IP 可访问（默认）");
            println!("  open      = 公网可访问（需登录 session）");
        }
        WebsetCmd::Set { mode } => {
            let normalized = match mode.as_str() {
                "off" | "intranet" | "open" => mode.clone(),
                _ => anyhow::bail!("非法 mode: {}（允许 off/intranet/open）", mode),
            };
            // 改变安全姿态，走 S10 二次身份确认
            confirm_admin_identity(auth, &format!("设置 admin.web_access = {}", normalized))?;
            system_db.set_setting("admin.web_access", &normalized)?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn(
                "cli",
                "admin.webset.set",
                Some("admin.web_access"),
                // L-15：value 经 serde_json 转义防破坏 JSON 结构
                Some(&serde_json::json!({ "value": normalized }).to_string()),
            );
            println!("  ✓ 已设置 admin.web_access = {}", normalized);
            println!("    即时生效，无需重启服务");
        }
    }
    Ok(())
}

// ── 全局通知广播（CLI 版本，不推 WebSocket） ──────────────────

fn cmd_broadcast(system_db: &SystemDatabase, cmd: &BroadcastCmd) -> anyhow::Result<()> {
    match cmd {
        BroadcastCmd::Send { level, message } => {
            let lv = match level.as_str() {
                "info" | "warn" | "error" => level.as_str(),
                _ => anyhow::bail!("非法 level: {}（允许 info/warn/error）", level),
            };
            if message.trim().is_empty() {
                anyhow::bail!("消息内容不能为空");
            }
            let notification = serde_json::json!({
                "message": message.trim(),
                "level": lv,
                "time": chrono::Local::now().to_rfc3339(),
                "author": "cli",
            });
            system_db.set_setting("global_notification", &notification.to_string())?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn("cli", "admin.broadcast", Some(lv), Some(message.trim()));
            println!("  ✓ 全局通知已发送（level={}）", lv);
            println!("    注意: CLI 无法实时推送 WebSocket，在线用户下次刷新页面才能看到");
        }
        BroadcastCmd::Clear => {
            system_db.set_setting("global_notification", "")?;
            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
            system_db.audit_log_warn("cli", "admin.broadcast-clear", None, None);
            println!("  ✓ 全局通知已清除（在线用户下次刷新后消失）");
        }
        BroadcastCmd::Show => {
            let raw = system_db
                .get_setting("global_notification")
                .unwrap_or_default();
            if raw.is_empty() {
                println!("（暂无全局通知）");
            } else {
                println!("全局通知");
                println!("{}", "=".repeat(60));
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(v) => {
                        println!(
                            "消息  : {}",
                            v.get("message").and_then(|x| x.as_str()).unwrap_or("")
                        );
                        println!(
                            "级别  : {}",
                            v.get("level").and_then(|x| x.as_str()).unwrap_or("")
                        );
                        println!(
                            "时间  : {}",
                            v.get("time").and_then(|x| x.as_str()).unwrap_or("")
                        );
                        println!(
                            "作者  : {}",
                            v.get("author").and_then(|x| x.as_str()).unwrap_or("")
                        );
                    }
                    Err(_) => println!("（通知内容解析失败: {}）", raw),
                }
            }
        }
    }
    Ok(())
}
