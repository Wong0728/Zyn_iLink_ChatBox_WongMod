// Zyn iLink ChatBox · WongMod - Rust 版
// 衍生/开发 请 标注 原仓库 "https://github.com/zynsync/Zyn-iLink-ChatBox" 与原作者。
// 仓库受到开源证书保护!请合规使用!

mod admin;
mod auth;
mod bot;
mod bot_manager;
mod config;
mod crypto;
mod event_broker;
mod media;
mod migration;
mod models;
mod push;
mod storage;
mod storage_backend;
mod tunnel;
mod web;
mod webdav;
mod webhook;

use admin::{AdminSub, Cli, TopCmd};
use clap::Parser;
use config::SCRIPT_VERSION;
use models::QrLoginState;
use std::sync::Arc;
use std::time::Duration;

fn is_server_mode() -> bool {
    std::env::var("ILINK_SERVER_MODE").is_ok()
}

macro_rules! cprintln {
    ($($arg:tt)*) => {
        if !is_server_mode() {
            println!($($arg)*);
        }
    };
}

/// Phase 5 (LOW-2): read_password_with_mask 已统一到 crate::auth 模块。
fn print_banner() {
    let ver_str = format!("v{}", SCRIPT_VERSION);
    let based_on = format!("Based on v{} · Mod by Mr.Wong", config::SCRIPT_BASED_ON);
    cprintln!("╔{}╗", "═".repeat(58));
    cprintln!("║{:^58}║", "Zyn iLink ChatBox · WongMod");
    cprintln!("║{:^58}║", "-无忧传递-");
    cprintln!("║{:^58}║", ver_str);
    cprintln!("║{:^58}║", based_on);
    cprintln!("╚{}╝", "═".repeat(58));
}

/// 内置默认使用守则文本（首次运行向导写入 system_settings.terms_text）
///
/// 内容覆盖免责声明、合法使用、隐私说明。修改需重编译；
/// 用户可通过 `ilink-wm1 admin terms set --file <path>` 动态覆盖。
const DEFAULT_TERMS_TEXT: &str = r#"# 使用守则

**版本：v1.0**

欢迎使用 Zyn iLink ChatBox（ilink-wm1）。使用本服务前，请认真阅读并同意以下守则。继续使用本服务即视为您已阅读并同意本守则全部内容。

## 一、服务范围

1. 本服务为 iLink 协议的 Web 桥接工具，提供多用户消息收发、历史记录查看、媒体存储与实时事件推送。
2. 本服务不对 iLink 官方服务的可用性、稳定性或政策变更负责。

## 二、账号与密码安全

3. 用户须妥善保管账号与密码，不得共享、出借或转让账号。
4. 因密码泄露、设备丢失或用户自身操作导致的损失，由用户自行承担。
5. 管理员可基于安全原因要求用户重置密码或临时禁用账号。

## 三、合法与合理使用

6. 用户应遵守所在地区法律法规，不得利用本服务从事违法、欺诈、骚扰、诽谤、侵权等活动。
7. 禁止批量发送垃圾信息、滥用接口、爬取数据或进行任何可能影响服务稳定性的行为。
8. 禁止上传、存储或传播病毒、恶意软件、淫秽色情、暴力恐怖或其他违法违规内容。

## 四、内容与数据责任

9. 用户对其发送、接收和存储的内容负全部责任，服务提供者不承担内容审查义务。
10. 重要数据请用户自行定期备份，服务提供者不对非因故意或重大过失导致的数据丢失负责。
11. 服务提供者有权配合监管部门依法提供相关数据。

## 五、隐私与数据存储

12. 用户数据存储在部署本服务的服务器上，敏感字段（如 WebDAV 凭证、设备令牌）采用 AES-256-GCM 加密。
13. 会话信息、审计日志等将按安全策略保留一定时间，用于问题排查与安全防护。

## 六、服务可用性与免责声明

14. 本服务按“现状”提供，不保证 uninterrupted、及时、安全或无错误。
15. 因网络、第三方服务、硬件故障或不可抗力导致的服务中断，服务提供者不承担责任。

## 七、守则更新与违规处理

16. 服务提供者有权根据法律法规或运营需要修改本守则，修改后将在服务内公布。
17. 用户继续使用本服务即视为接受修改后的守则。
18. 对于违反本守则的账号，管理员有权限制、暂停或终止其使用权限。

---

如不同意以上守则，请立即停止使用本服务。"#;

/// Phase 1 多用户：首次运行设置向导
///
/// 与旧版的区别：
///   - 不再设置"全局 web 密码"，改为创建 owner 账号（用户名 + 密码）
///   - 凭据落 system.db.app_users，不再落 bot.db.web_password
///   - 若 system.db 已有用户（含迁移而来的 owner），跳过创建步骤
///
/// 7 步流程：绑定地址 / owner 账号 / 站点名 / 注册策略 / 邀请码生成 / 守则设置 / 运行模式
///
/// 返回 (skip_repl_mode, custom_bind_host)
fn first_run_setup(system_db: &Arc<crate::storage::SystemDatabase>) -> (bool, Option<String>) {
    use std::io::{self, BufRead, Write};
    // ponytail: ceiling=owner 创建逻辑与 admin.rs::cmd_init 重复约 70 行
    //   （用户名 loop + 密码 loop + create_user + audit）。不抽出共享 helper：
    //   两端返回类型 / 提示前缀 / audit actor+action / 是否有绑定地址/REPL 模式向导
    //   都不同，抽 helper 需要 5+ 参数（prompt_prefix/audit_actor/audit_action/
    //   return_type/with_wizard），抽象成本 > 重复成本。升级路径：将来若增加第 3 个
    //   调用点（如 web 安装向导），再考虑抽出 `prompt_owner_credentials(...)`。

    let auth = crate::auth::Auth::new(system_db.clone());
    let has_any_user = !system_db.list_users().is_empty();

    // 检查是否已完成过设置向导（system_settings.setup_complete = "1"）
    let setup_complete = system_db
        .get_setting("setup_complete")
        .map(|v| v == "1")
        .unwrap_or(false);

    if setup_complete {
        println!("\n[zyn] 已检测到现有配置，跳过设置向导。");
        println!("[zyn] 如需重新设置，请运行: ilink-wm1 admin config set setup_complete 0");
        println!("[zyn] 或使用 /set 命令在交互模式下修改设置。");
        return (false, None);
    }

    // 已有用户 → 跳过 owner 创建步骤（仍可继续向导设置绑定地址/CLI 模式）
    let mut skip_owner_creation = has_any_user;

    println!("\n{}", "=".repeat(60));
    println!("  欢迎使用 Zyn iLink ChatBox！（多用户版）");
    if has_any_user {
        println!("  system.db 已有用户账号，跳过 owner 创建步骤");
    } else {
        println!("  这是首次运行向导，请完成以下基本设置。");
    }
    println!("{}", "=".repeat(60));

    let mut skip_repl = false;
    let mut bind_host: Option<String> = None;

    // 1. 询问绑定地址
    println!("\n[1/7] Web 服务绑定地址");
    println!("  1) 仅本机访问 (127.0.0.1) - 推荐，最安全");
    println!("  2) 局域网访问 (0.0.0.0) - 允许同网络 IPv4 设备访问");
    println!("  3) 双栈访问 ([::]) - IPv4+IPv6，支持公网 IPv6 直连（需放行防火墙）");
    println!("  4) 跳过，稍后通过环境变量 ILINK_HOST 设置");
    print!("请选择 [1/2/3/4] (默认 1): ");
    let _ = io::stdout().flush();

    let stdin = io::stdin();
    let mut input = String::new();
    if stdin.lock().read_line(&mut input).is_ok() {
        let choice = input.trim();
        match choice {
            "2" => {
                bind_host = Some("0.0.0.0".to_string());
                println!("  ✓ 已选择局域网访问模式");
            }
            "3" => {
                bind_host = Some("[::]".to_string());
                println!("  ✓ 已选择双栈模式（IPv4 + IPv6）");
                println!(
                    "  ⚠ 公网 IPv6 直连场景请设置 ILINK_ALLOW_INSECURE_PUBLIC=1 跳过公网守卫，"
                );
                println!("    或配置 TLS 反向代理 + ILINK_TRUSTED_PROXIES + ILINK_FORCE_HTTPS=1。");
                println!("    Windows 防火墙放行示例: netsh advfirewall firewall add rule name=\"ilink-wm\" dir=in action=allow protocol=TCP localport=8888");
            }
            "4" => {
                println!("  ✓ 跳过，将使用环境变量 ILINK_HOST");
            }
            _ => {
                println!("  ✓ 已选择仅本机访问（默认）");
            }
        }
    }

    // 2. 创建 owner 账号（若 system.db 无用户）
    if !skip_owner_creation {
        println!("\n[2/7] 创建 owner 账号（系统最高权限，用于 Web 登录）");
        println!("  密码要求: 8-128 位，必须包含大写字母、小写字母和数字");

        // 支持 ILINK_OWNER_USER / ILINK_OWNER_PASSWORD 环境变量
        //   初始化 owner，用于 systemd/Docker 等 stdin 不可用场景。
        //   ⚠ 仅在首次启动（system.db 无用户）时读取；已存在用户后 env var 被忽略。
        //   ⚠ ILINK_OWNER_PASSWORD 明文存于环境变量，需通过 systemd LoadCredential / Docker
        //   secret / .env 文件（chmod 600）等机制保护，勿直接写入 shell history。
        //   ⚠ 读取后立即用，不写入日志、不回显。
        //   L-19：更推荐 ILINK_OWNER_PASSWORD_FILE（配合 systemd LoadCredential=... 落盘
        //   0600 凭据文件），避免明文进进程环境（/proc/<pid>/environ 可读）。
        let env_owner_user = std::env::var("ILINK_OWNER_USER")
            .ok()
            .filter(|s| !s.is_empty());
        let env_owner_pass = std::env::var("ILINK_OWNER_PASSWORD")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                // L-19：优先文件注入路径（读首行、去尾部换行）；文件不可读时明确报错
                std::env::var("ILINK_OWNER_PASSWORD_FILE")
                    .ok()
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        std::fs::read_to_string(&p)
                            .map(|c| c.trim_end_matches(['\r', '\n']).to_string())
                            .map_err(|e| {
                                println!("  ✗ ILINK_OWNER_PASSWORD_FILE 读取失败 ({}): {}", p, e);
                                e
                            })
                            .ok()
                            .filter(|c| !c.is_empty())
                    })
                    .flatten()
            });

        // 用户名
        let username = if let Some(u) = env_owner_user.as_ref() {
            // env var 模式：校验失败直接报错退出（非交互场景无法重试）
            if !u
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                println!("  ✗ ILINK_OWNER_USER 含非法字符（仅允许字母数字、下划线、连字符）");
                skip_owner_creation = true;
                String::new()
            } else if u.len() < 3 || u.len() > 32 {
                println!("  ✗ ILINK_OWNER_USER 长度需 3-32 字符");
                skip_owner_creation = true;
                String::new()
            } else {
                println!("  ✓ 从 ILINK_OWNER_USER 读取用户名: {}", u);
                u.clone()
            }
        } else {
            // 交互式输入
            loop {
                print!("  用户名 (默认 owner): ");
                let _ = io::stdout().flush();
                let mut name_buf = String::new();
                if stdin.lock().read_line(&mut name_buf).is_err() {
                    skip_owner_creation = true;
                    break String::new();
                }
                let name = name_buf.trim().to_string();
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
            }
        };

        if !skip_owner_creation {
            // 密码
            let password = if let Some(p) = env_owner_pass.as_ref() {
                // env var 模式：校验失败直接报错退出
                if p.is_empty() {
                    println!("  ✗ ILINK_OWNER_PASSWORD 为空");
                    skip_owner_creation = true;
                    String::new()
                } else if let Err(e) = crate::auth::Auth::check_password_strength(p) {
                    println!("  ✗ ILINK_OWNER_PASSWORD 强度不足: {}", e);
                    skip_owner_creation = true;
                    String::new()
                } else {
                    println!("  ✓ 从 ILINK_OWNER_PASSWORD 读取密码（已隐藏）");
                    p.clone()
                }
            } else {
                // 交互式输入
                loop {
                    let pw1 = crate::auth::read_password_with_mask("  请输入密码: ");
                    if pw1.is_empty() {
                        println!("  ✗ 密码不能为空");
                        continue;
                    }
                    if let Err(e) = crate::auth::Auth::check_password_strength(&pw1) {
                        println!("  ✗ {}", e);
                        continue;
                    }
                    let pw2 = crate::auth::read_password_with_mask("  请再次输入密码确认: ");
                    if pw1 != pw2 {
                        println!("  ✗ 两次密码不一致");
                        continue;
                    }
                    break pw1;
                }
            };

            if !skip_owner_creation {
                match auth.create_user(&username, &password, "owner") {
                    Ok(uid) => {
                        println!("  ✓ owner 账号创建成功: uid={} username={}", uid, username);
                        // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
                        let source = if env_owner_user.is_some() && env_owner_pass.is_some() {
                            "env"
                        } else {
                            "interactive"
                        };
                        system_db.audit_log_warn(
                            "wizard",
                            "first_run.owner_created",
                            Some(&format!("uid={}", uid)),
                            Some(&format!(
                                "{{\"username\":\"{}\",\"source\":\"{}\"}}",
                                username, source
                            )),
                        );
                        // 创建成功后 skip_owner_creation 已无后续读者，无需再置位
                    }
                    Err(e) => {
                        println!("  ✗ owner 账号创建失败: {}", e);
                        println!("    稍后可通过 `ilink-wm1 admin init` 手动创建");
                    }
                }
            }
        }
    } else {
        println!("\n[2/7] owner 账号 - 已存在 ✓");
    }

    // 3. 站点名称（全局设置，每次向导都问，不论是否已有用户）
    println!("\n[3/7] 站点名称");
    print!("站点名称 (默认 Zyn iLink ChatBox · WongMod): ");
    let _ = io::stdout().flush();
    input.clear();
    if stdin.lock().read_line(&mut input).is_ok() {
        let name = input.trim();
        if !name.is_empty() {
            // 写入失败不阻断向导
            let _ = system_db.set_setting("site_name", name);
        }
    }

    // 4. 注册策略（全局设置，每次向导都问）
    println!("\n[4/7] 注册策略");
    // 4a. 开放注册（默认 N=off，安全优先）
    print!("是否允许开放注册（任何人可直接注册）？ [y/N] ");
    let _ = io::stdout().flush();
    input.clear();
    if stdin.lock().read_line(&mut input).is_ok() {
        let ans = input.trim().to_lowercase();
        // y/N：空回车=否（默认），"y"/"yes"=是，其它=否
        let allow = ans == "y" || ans == "yes";
        let val = if allow { "on" } else { "off" };
        let _ = system_db.set_setting("allow_open_registration", val);
    }
    // 4b. 邀请码注册（默认 Y=on）— allow_invite 供第 5 步判断
    let mut allow_invite = true;
    print!("是否允许邀请码注册？ [Y/n] ");
    let _ = io::stdout().flush();
    input.clear();
    if stdin.lock().read_line(&mut input).is_ok() {
        let ans = input.trim().to_lowercase();
        // Y/n：空回车=是（默认）；非空按 "y"/"yes"=是、其它=否
        allow_invite = if ans.is_empty() {
            true
        } else {
            ans == "y" || ans == "yes"
        };
        let val = if allow_invite { "on" } else { "off" };
        let _ = system_db.set_setting("allow_invite_registration", val);
    }

    // 5. 邀请码生成（仅当允许邀请码注册时询问）
    if allow_invite {
        println!("\n[5/7] 邀请码生成");
        print!("是否立即生成首个邀请码？ [Y/n] ");
        let _ = io::stdout().flush();
        input.clear();
        let mut want_gen = true;
        if stdin.lock().read_line(&mut input).is_ok() {
            let ans = input.trim().to_lowercase();
            want_gen = if ans.is_empty() {
                true
            } else {
                ans == "y" || ans == "yes"
            };
        }
        if want_gen {
            // 有效期天数（默认 30，1-365）
            print!("有效期天数 (1-365，默认 30): ");
            let _ = io::stdout().flush();
            input.clear();
            let mut days: i64 = 30;
            if stdin.lock().read_line(&mut input).is_ok() {
                let t = input.trim();
                if !t.is_empty() {
                    match t.parse::<i64>() {
                        Ok(d) if (1..=365).contains(&d) => days = d,
                        _ => println!("  ✗ 无效天数，使用默认 30 天"),
                    }
                }
            }
            // 备注（可选）
            print!("备注 (可选，回车跳过): ");
            let _ = io::stdout().flush();
            input.clear();
            let mut note = String::new();
            if stdin.lock().read_line(&mut input).is_ok() {
                note = input.trim().to_string();
            }
            // 生成 4 位大写字母+数字邀请码
            match system_db.allocate_invite_code() {
                Ok(code) => {
                    let expires_at = chrono::Utc::now()
                        .checked_add_signed(chrono::Duration::days(days))
                        .unwrap_or_else(chrono::Utc::now)
                        .to_rfc3339();
                    let note_opt: Option<&str> = if note.is_empty() { None } else { Some(&note) };
                    match system_db.create_invite(&code, Some(&expires_at), note_opt) {
                        Ok(_) => {
                            println!("  ✓ 邀请码已生成: {}（有效期 {} 天）", code, days);
                            println!("    可将此邀请码分享给需要注册的用户");
                            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
                            system_db.audit_log_warn(
                                "wizard",
                                "first_run.invite_created",
                                Some(&code),
                                Some(&format!("{{\"days\":{},\"note\":\"{}\"}}", days, note)),
                            );
                        }
                        Err(e) => {
                            println!("  ✗ 邀请码生成失败: {}", e);
                            println!("    可稍后通过 `ilink-wm1 admin invite create` 生成");
                        }
                    }
                }
                Err(e) => {
                    println!("  ✗ 邀请码生成失败: {}", e);
                    println!("    可稍后通过 `ilink-wm1 admin invite create` 生成");
                }
            }
        } else {
            println!("  ✓ 跳过邀请码生成，可稍后通过 `ilink-wm1 admin invite create` 生成");
        }
    } else {
        println!("\n[5/7] 邀请码生成 - 已跳过（邀请码注册未开启）");
    }

    // 6. 使用守则设置
    println!("\n[6/7] 使用守则");
    let current_terms_text = system_db.get_setting("terms_text").unwrap_or_default();
    if current_terms_text.is_empty() {
        // 首次启动：写入默认守则
        if let Err(e) = system_db.set_setting("terms_text", DEFAULT_TERMS_TEXT) {
            println!("  ✗ 写入默认守则失败: {}", e);
        } else {
            let _ = system_db.set_setting("terms_version", "1.0");
            println!("  ✓ 已写入默认守则 v1.0");
        }
    } else {
        println!("  ✓ 已存在守则配置，保留不动");
    }
    // 显示当前守则版本与预览
    let terms_ver = system_db
        .get_setting("terms_version")
        .unwrap_or_else(|| "1.0".to_string());
    println!("  当前守则版本: v{}", terms_ver);
    let display_text = if current_terms_text.is_empty() {
        DEFAULT_TERMS_TEXT.to_string()
    } else {
        current_terms_text.clone()
    };
    let preview: String = display_text.chars().take(80).collect();
    let preview_one_line: String = preview.replace('\n', " ").replace('\r', "");
    println!("  守则文本预览: {}...", preview_one_line);
    print!("是否保留当前守则？ [Y/n] ");
    let _ = io::stdout().flush();
    input.clear();
    if stdin.lock().read_line(&mut input).is_ok() {
        let ans = input.trim().to_lowercase();
        let keep = if ans.is_empty() {
            true
        } else {
            ans == "y" || ans == "yes"
        };
        if !keep {
            println!("  ✓ 可稍后通过 `ilink-wm1 admin terms set --file <path>` 自定义守则文本");
            println!("    当前仍使用默认守则占位");
        }
    }

    // 7. 询问 REPL 模式（运行模式）
    println!("\n[7/7] 运行模式");
    println!("  1) 交互模式 - 终端可输入 /set /webset 等命令并直接发消息");
    println!("  2) 仅 Web 模式 - 终端仅显示状态，管理操作用 ilink-wm1 admin ... 或网页");
    print!("请选择 [1/2] (默认 1): ");
    let _ = io::stdout().flush();

    input.clear();
    if stdin.lock().read_line(&mut input).is_ok() {
        if input.trim() == "2" {
            skip_repl = true;
            println!("  ✓ 已选择仅 Web 模式（REPL 关闭，CLI 子命令仍可用）");
        } else {
            println!("  ✓ 已选择交互模式（默认）");
        }
    }

    // 标记设置向导已完成，下次启动不再重复
    let _ = system_db.set_setting("setup_complete", "1");

    println!("\n{}", "=".repeat(60));
    println!("  设置完成！正在启动服务...");
    println!("  提示: 前端管理面板默认仅内网访问，可用 `ilink-wm1 admin webset` 调整");
    println!("{}", "=".repeat(60));

    (skip_repl, bind_host)
}

/// 交互式设置菜单（/set 命令调用）
///
/// 支持通过数字选择跳转到不同的设置项：
///   1) 站点名称   2) 邀请码管理   3) 注册策略
///   4) 使用守则   5) 访问地址/端口  6) 查看环境变量
///
/// 接收外层 `lines` 迭代器，避免内部持有锁时 await。
///   原实现内部多次 `stdin.lock()` 与 REPL 线程外层 `stdin.lock().lines()`
///   争抢同一锁导致死锁（/set 后无法输入）。改为复用调用方传入的 `&mut Lines`，
///   与 /webset 的修复模式一致（见 main.rs L1211-1247）。
fn settings_menu(
    system_db: &Arc<crate::storage::SystemDatabase>,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    stdout: &mut std::io::Stdout,
) {
    use std::io::Write;

    // 从 lines 读取一行（已 trim 末尾换行）；EOF/IO 错误返回 None
    let read_line = |lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
                     stdout: &mut std::io::Stdout,
                     prompt: &str|
     -> Option<String> {
        if !prompt.is_empty() {
            print!("{}", prompt);
            let _ = stdout.flush();
        }
        match lines.next() {
            Some(Ok(s)) => Some(s),
            _ => None,
        }
    };

    loop {
        let site = system_db
            .get_setting("site_name")
            .unwrap_or_else(|| "Zyn iLink ChatBox · WongMod".to_string());
        let open_reg = system_db
            .get_setting("allow_open_registration")
            .map(|value| crate::storage::setting_truthy(&value))
            .unwrap_or(false);
        let invite_reg = system_db
            .get_setting("allow_invite_registration")
            .map(|value| crate::storage::setting_truthy(&value))
            .unwrap_or(true);
        let port = std::env::var("ILINK_PORT").unwrap_or_else(|_| "8888".to_string());
        let host = crate::config::bind_host();
        let terms_ver = system_db
            .get_setting("terms_version")
            .unwrap_or_else(|| "1.0".to_string());

        println!();
        println!("{}", "=".repeat(50));
        println!("  /set 设置菜单");
        println!("{}", "=".repeat(50));
        println!("  1) 站点名称        当前: {}", site);
        println!("  2) 邀请码管理       （生成/查看/撤销邀请码）");
        println!(
            "  3) 注册策略         开放注册: {} | 邀请注册: {}",
            if open_reg { "✓开" } else { "✗关" },
            if invite_reg { "✓开" } else { "✗关" }
        );
        println!("  4) 使用守则         当前版本: v{}", terms_ver);
        println!(
            "  5) 访问地址/端口    {}:{}（修改后需重启生效）",
            host, port
        );
        println!("  6) 环境变量         查看当前环境变量");
        println!("  0) 返回");
        println!("{}", "=".repeat(50));

        let input = match read_line(lines, stdout, "请选择 [0-6]: ") {
            Some(s) => s,
            None => break,
        };
        match input.trim() {
            "0" => break,
            "1" => {
                let name = read_line(lines, stdout, "  新站点名称 (回车跳过): ");
                if let Some(n) = name {
                    let n = n.trim();
                    if !n.is_empty() {
                        let _ = system_db.set_setting("site_name", n);
                        println!("  ✓ 站点名称已更新");
                    }
                }
            }
            "2" => {
                // 邀请码子菜单
                loop {
                    let invites = system_db.list_invites();
                    println!();
                    println!("  -- 邀请码管理 --");
                    if invites.is_empty() {
                        println!("  暂无邀请码");
                    } else {
                        for inv in &invites {
                            let status_icon = match inv.status.as_str() {
                                "active" => "✓",
                                "used" => "已用",
                                "revoked" => "已撤销",
                                _ => "?",
                            };
                            println!(
                                "  {} | {} | 创建: {} | 过期: {}",
                                inv.code,
                                status_icon,
                                &inv.created_at[..10.min(inv.created_at.len())],
                                inv.expires_at.as_deref().unwrap_or("永久")
                            );
                        }
                    }
                    println!("  [c] 创建邀请码  [r 码] 撤销  [0] 返回");
                    let cmd_raw = match read_line(lines, stdout, "  请选择: ") {
                        Some(s) => s,
                        None => break,
                    };
                    let cmd = cmd_raw.trim();
                    if cmd == "0" {
                        break;
                    }
                    if cmd == "c" {
                        let days_raw = read_line(lines, stdout, "  有效期天数 (1-365，默认 30): ");
                        let mut days: i64 = 30;
                        if let Some(d) = days_raw {
                            if let Ok(d) = d.trim().parse::<i64>() {
                                if (1..=365).contains(&d) {
                                    days = d;
                                }
                            }
                        }
                        match system_db.allocate_invite_code() {
                            Ok(code) => {
                                let expires_at = chrono::Utc::now()
                                    .checked_add_signed(chrono::Duration::days(days))
                                    .unwrap_or_else(chrono::Utc::now)
                                    .to_rfc3339();
                                match system_db.create_invite(&code, Some(&expires_at), None) {
                                    Ok(_) => println!("  ✓ 邀请码: {}（{} 天有效）", code, days),
                                    Err(e) => println!("  ✗ 创建失败: {}", e),
                                }
                            }
                            Err(e) => println!("  ✗ 生成失败: {}", e),
                        }
                    } else if let Some(rest) = cmd.strip_prefix("r ") {
                        let code = rest.trim();
                        match system_db.revoke_invite(code) {
                            Ok(_) => println!("  ✓ 邀请码 {} 已撤销", code),
                            Err(e) => println!("  ✗ 撤销失败: {}", e),
                        }
                    }
                }
            }
            "3" => {
                println!();
                println!("  -- 注册策略 --");
                let cur_open = system_db
                    .get_setting("allow_open_registration")
                    .map(|value| crate::storage::setting_truthy(&value))
                    .unwrap_or(false);
                println!("  开放注册当前: {}", if cur_open { "开启" } else { "关闭" });
                let ans = read_line(lines, stdout, "  开启开放注册？[y/N] ");
                if let Some(a) = ans {
                    let val = if a.trim().eq_ignore_ascii_case("y") {
                        "on"
                    } else {
                        "off"
                    };
                    let _ = system_db.set_setting("allow_open_registration", val);
                }
                let cur_invite = system_db
                    .get_setting("allow_invite_registration")
                    .map(|value| crate::storage::setting_truthy(&value))
                    .unwrap_or(true);
                println!(
                    "  邀请注册当前: {}",
                    if cur_invite { "开启" } else { "关闭" }
                );
                let ans = read_line(lines, stdout, "  开启邀请注册？[Y/n] ");
                if let Some(a) = ans {
                    let a = a.trim().to_lowercase();
                    let val = if a.is_empty() || a == "y" {
                        "on"
                    } else {
                        "off"
                    };
                    let _ = system_db.set_setting("allow_invite_registration", val);
                }
                println!("  ✓ 注册策略已更新");
            }
            "4" => {
                let cur_text = system_db.get_setting("terms_text").unwrap_or_default();
                let preview: String = cur_text.chars().take(100).collect();
                println!();
                println!("  -- 使用守则 v{} --", terms_ver);
                println!("  预览: {}...", preview.replace('\n', " "));
                println!("  修改守则请使用: ilink-wm1 admin terms set --file <path>");
                println!("  或使用: ilink-wm1 admin terms set-version <version>");
            }
            "5" => {
                println!();
                println!("  -- 访问地址/端口 --");
                println!(
                    "  当前: {}",
                    crate::config::host_port_display(&host, port.parse().unwrap_or(8888))
                );
                println!("  修改方法:");
                println!("    端口: 设置环境变量 ILINK_PORT=新端口 后重启");
                println!("    地址: 设置环境变量 ILINK_HOST=0.0.0.0（或 [::] 含 IPv6）后重启");
                println!("  ⚠ 修改后需重启服务才能生效！");
            }
            "6" => {
                println!();
                println!("  -- 当前环境变量 --");
                // ILINK_DATA_DIR 在共享终端场景会泄露服务器物理路径，
                //   改为仅显示是否设置（不显示具体值）。其他变量为非敏感配置项保持原样。
                for var in &[
                    "ILINK_PORT",
                    "ILINK_HOST",
                    "ILINK_DATA_DIR",
                    "ILINK_SERVER_MODE",
                    "ILINK_TRUSTED_PROXIES",
                    "ILINK_ALLOWED_ORIGINS",
                    "ILINK_FORCE_HTTPS",
                    "ILINK_CLI_TRUST",
                    "RUST_LOG",
                ] {
                    let val = std::env::var(var).unwrap_or_else(|_| "(未设置)".to_string());
                    let display = if var == &"ILINK_DATA_DIR" && val != "(未设置)" {
                        "(已设置，值已隐藏)".to_string()
                    } else {
                        val
                    };
                    println!("  {}={}", var, display);
                }
            }
            _ => println!("  无效选择，请重试"),
        }
    }
}

#[tokio::main]
async fn main() {
    // Termux 兼容
    config::setup_termux_compat();

    // L-2：明文回退开关开启时启动即告警（该开关仅限升级迁移兜底，默认严格拒绝）
    if std::env::var("ILINK_ALLOW_PLAINTEXT_FALLBACK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        eprintln!("[WARN] ILINK_ALLOW_PLAINTEXT_FALLBACK 已开启：敏感数据允许明文兼容回退。");
        eprintln!("[WARN] 请在重新保存相关配置（WebDAV 密码等）后立即移除该环境变量。");
    }

    // 初始化日志
    let server_mode = is_server_mode();
    // 默认日志级别 info，确保 [SEND]/[RECV] 排障日志不被 warn 过滤。
    //   原 REPL 模式默认 ilink_wm1=warn，生产事故时无法看到消息收发链路，定位困难。
    //   现在统一默认 ilink_wm1=info；敏感字段已在 bot.rs 中通过 safe_truncate 脱敏
    //   （消息文本截断 80 字符、bot_token 截断 16 字符、resp 截断 200 字符）。
    //   环境变量覆盖（优先级从高到低）：
    //   - ILINK_LOG_FILTER="ilink_wm1=debug,reqwest=warn"：完整 EnvFilter 指令
    //   - ILINK_LOG_LEVEL=warn|info|debug|trace：仅设置 ilink_wm1 模块级别
    //   - 默认：ilink_wm1=info
    let env_filter = std::env::var("ILINK_LOG_FILTER")
        .ok()
        .or_else(|| {
            std::env::var("ILINK_LOG_LEVEL")
                .ok()
                .map(|lvl| format!("ilink_wm1={}", lvl))
        })
        .unwrap_or_else(|| "ilink_wm1=info".to_string());

    if server_mode {
        let file_appender = tracing_appender::rolling::daily(".", "ilink.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        // 使用 tracing-subscriber 的 Layer 功能将日志输出到控制台和文件
        use tracing_subscriber::prelude::*;
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false);
        let stdout_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(env_filter))
            .with(file_layer)
            .with(stdout_layer)
            .init();

        // 避免 _guard 提前释放导致无法输出
        // S56/ponytail: tracing_appender non_blocking guard 必须 leak，
        // 否则 drop 时会 flush+关闭后台写入线程，导致后续日志全部丢失；
        // 进程退出时由 OS 回收内存，无实际泄漏风险。
        Box::leak(Box::new(_guard));
    } else {
        // 优先尊重 RUST_LOG 环境变量；未设置时回退到默认级别。
        // 默认 ilink_wm1=warn 会屏蔽 info 级的 [SEND]/[RECV] 收发消息日志，
        // 通过 RUST_LOG=ilink_wm1=info 可在 run_test.py 中将其打开。
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(env_filter));
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    // Phase 1 多用户：clap 解析参数。
    //   - 无子命令且无 --setpw → 默认启动服务（向后兼容）
    //   - --setpw → 提示改用 admin user reset-password 并退出
    //   - admin <sub> → 进入 admin CLI，操作完即退出
    let cli = Cli::parse();
    let no_repl = cli.no_repl;
    let _server_mode_flag = cli.server;

    if cli.setpw {
        println!(
            "Zyn iLink ChatBox {} - 多用户版已移除全局 web 密码",
            SCRIPT_VERSION
        );
        println!("如需重置某用户密码，请运行:");
        println!("  ilink-wm1 admin user reset-password <username>");
        println!("如需首次初始化 owner 账号，请运行:");
        println!("  ilink-wm1 admin init");
        return;
    }

    // admin 子命令分发（不启动 Web 服务，操作完即退出）
    if let Some(TopCmd::Admin(sub)) = &cli.command {
        let admin_sub: &AdminSub = sub;
        if let Err(e) = admin::run_admin(admin_sub) {
            eprintln!("[admin] 命令失败: {}", e);
            std::process::exit(1);
        }
        return;
    }

    print_banner();
    if no_repl {
        cprintln!(
            "[REPL] 已启用 --no-repl 模式：跳过终端交互，管理操作请用 ilink-wm1 admin ... 或网页端"
        );
    }

    // Phase 1 多用户：先建 system.db（自动建表），再做老库迁移
    // SystemDatabase::new 返回 Result，
    //   DB 初始化失败（磁盘满/权限/损坏）时打印友好错误并退出，不再 panic=abort 直接杀进程。
    let system_db: Arc<crate::storage::SystemDatabase> = match crate::storage::SystemDatabase::new()
    {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[FATAL] system.db 初始化失败: {:#}", e);
            eprintln!("        请检查磁盘空间、文件权限或数据库文件是否损坏。");
            std::process::exit(1);
        }
    };
    tracing::info!(
        "[MAIN] system.db 已就绪: {}",
        crate::config::system_db_file().display()
    );

    // 老库（wechat_bot.db）→ 多用户架构迁移（幂等）
    //   - 老 web_password → owner 账号
    //   - 老库数据表 → users/<owner_uid>/user.db
    //   - 老媒体缓存 → users/<owner_uid>/media_cache/
    //   - 老库改名 wechat_bot.db.bak
    match migration::migrate_legacy_to_multiuser(&system_db) {
        Ok(Some(owner_uid)) => {
            println!("[MIGRATION] 老库已迁移为 owner 用户 (uid={})", owner_uid);
        }
        Ok(None) => {
            tracing::debug!("[MIGRATION] 无需迁移（system.db 已有用户或无老库）");
        }
        Err(e) => {
            eprintln!("[MIGRATION] 老库迁移失败: {}", e);
            eprintln!("  请检查后重试；如确认无需迁移可删除老库 wechat_bot.db");
            std::process::exit(1);
        }
    }
    // Phase 2 多用户：不再创建全局 bot。
    //   1) 首次运行向导（可能创建 owner 账号）
    //   2) 解析 web_port（与 WeChatiLinkBot::new 内部读 ILINK_PORT 一致）
    //   3) 启动 Web 服务（用 BotManager 按需为每个用户创建独立 bot 实例）
    //   4) 解析 owner_uid，按需通过 BotManager 创建 owner 的 bot 实例
    //   5) 无 owner 时纯服务模式（仅运行 Web，等待 admin init 创建 owner）

    // 首次运行向导（非 --no-repl 模式且非 server_mode）
    let (wizard_skip_repl, wizard_bind_host) = if !no_repl && !is_server_mode() {
        first_run_setup(&system_db)
    } else {
        (false, None)
    };

    // 应用向导设置
    let final_no_repl = no_repl || wizard_skip_repl;
    if let Some(ref host) = wizard_bind_host {
        // 不再调用 `std::env::set_var("ILINK_HOST", ...)`，
        //   改为通过 OnceLock 全局变量记录覆盖值，由 `config::bind_host()` 统一读取。
        //   原因：Rust 2024 中 set_var 为 unsafe；多线程服务器中 spawn 之后调用
        //   可能导致其他线程读到部分写入的环境变量，引发 UB。
        //   优先级：环境变量 ILINK_HOST 已设置 → 不覆盖；否则用向导选择值。
        if std::env::var("ILINK_HOST").is_err() {
            crate::config::set_bind_host_override(host.clone());
        }
    }

    // 解析 web_port（与 WeChatiLinkBot::new 内部读 ILINK_PORT 的逻辑一致）
    let web_port: u16 = std::env::var("ILINK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8888);

    // 启动 Web 服务（让用户可以在网页上扫码）
    // Phase 2 多用户：Web 服务通过 BotManager 按需为每个用户创建独立 bot 实例，
    //   实现每用户数据隔离。媒体授权直接查询当前用户的持久化 user.db。
    let web_system_db = system_db.clone();
    let bot_manager = crate::bot_manager::BotManager::new(web_system_db.clone(), web_port);
    // Phase 3 (P1): 启动 5s 配额 flush 后台任务（内存计数器 → system.db）
    bot_manager.start_quota_flush_loop();
    // 启动限速 HashMap 定时清理任务，防止长期运行 + 海量 IP 触发导致内存膨胀
    bot_manager.start_rate_limit_cleanup_loop();

    // 审计日志 90 天保留策略。
    //   启动时清理一次 + 后台每 24 小时清理一次，避免 audit_logs 表无界增长。
    //   保留天数可通过 ILINK_AUDIT_RETENTION_DAYS 环境变量调整（默认 90）。
    {
        let retention_days: u32 = std::env::var("ILINK_AUDIT_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(90)
            .clamp(1, 3650);
        let purged = system_db.purge_old_audit_logs(retention_days);
        if purged > 0 {
            tracing::info!(
                "[MAIN] 启动时清理 {} 天前审计日志 {} 条",
                retention_days,
                purged
            );
        }
        let sysdb_for_audit = system_db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
            interval.tick().await; // 跳过首次立即触发
            loop {
                interval.tick().await;
                let n = sysdb_for_audit.purge_old_audit_logs(retention_days);
                if n > 0 {
                    tracing::info!(
                        "[AUDIT] 定时清理：删除 {} 天前审计日志 {} 条",
                        retention_days,
                        n
                    );
                }
            }
        });
    }

    // 媒体缓存 LRU 上限。
    //   每 6 小时扫描所有用户的 media_cache，按 created_at 升序删除最老文件，
    //   直到该用户 media_meta 总大小低于阈值。
    //   阈值通过 ILINK_MEDIA_CACHE_MAX_GB 环境变量配置（默认 5GB，每用户）。
    //   实现要点：
    //   - 遍历 list_users() 所有 active 用户的 user.db
    //   - 用 Database::new_for_user(uid) 获取句柄（单例，不重复打开）
    //   - 按 created_at 升序逐条删 media_meta 行 + 本地缓存文件
    //   - 文件路径算法与 LocalFsBackend::file_path 一致（hex 前两位作 bucket）
    {
        let sysdb_for_media = system_db.clone();
        let bot_manager_for_media = bot_manager.clone();
        tokio::task::spawn_blocking(move || {
            let max_gb: f64 = std::env::var("ILINK_MEDIA_CACHE_MAX_GB")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(5.0)
                .clamp(0.1, 1024.0);
            let max_bytes = (max_gb * 1024.0 * 1024.0 * 1024.0) as i64;
            // 启动时执行一次
            purge_media_cache_for_all_users(&sysdb_for_media, &bot_manager_for_media, max_bytes);
            // 后台每 6 小时执行一次
            let interval_secs: u64 = std::env::var("ILINK_MEDIA_CACHE_PURGE_INTERVAL_HOURS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(6)
                .clamp(1, 168)
                * 3600;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval_secs));
                purge_media_cache_for_all_users(
                    &sysdb_for_media,
                    &bot_manager_for_media,
                    max_bytes,
                );
            }
        });
    }

    // delete_user 文件系统删除失败的补偿队列重试。
    //   每 30 分钟扫描 pending_user_cleanup 表，重试 remove_dir_all，
    //   成功后从队列移除。超过 100 次尝试的视为永久失败，仅 warn 不再重试。
    {
        let sysdb_for_cleanup = system_db.clone();
        tokio::task::spawn_blocking(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(30 * 60));
            retry_pending_user_cleanups(&sysdb_for_cleanup);
        });
        // 启动时也执行一次（处理上次进程退出时未完成的清理）
        let sysdb_for_cleanup_init = system_db.clone();
        tokio::task::spawn_blocking(move || {
            retry_pending_user_cleanups(&sysdb_for_cleanup_init);
        });
    }
    // ponytail HIGH-4: 跨进程卸载靠 DB 轮询，小部署可接受；扩展 → Unix socket / HTTP admin 通道
    //   admin CLI user delete/disable 是独立进程，无法直接调 unload_bot；
    //   此处周期（30s）扫 system_db.list_users()，对 status!=active 或不存在的 uid 调 unload_bot。
    {
        let bm = bot_manager.clone();
        let sysdb = system_db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            interval.tick().await; // 跳过首次立即触发
            loop {
                interval.tick().await;
                let users = sysdb.list_users();
                let active_uids: std::collections::HashSet<i64> = users
                    .iter()
                    .filter(|u| u.status == "active")
                    .map(|u| u.id)
                    .collect();
                // unload 不在 active 列表的 bot（best-effort，parking_lot 锁安全）
                // 改用 unload_bot_async 避免阻塞 tokio worker
                for uid in bm.list_loaded_uids() {
                    if !active_uids.contains(&uid) {
                        bm.unload_bot_async(uid).await;
                    }
                }
            }
        });
    }

    // 多用户账号状态（替代旧版"web 密码"提示）
    let has_any_user = !system_db.list_users().is_empty();
    let bind_host = crate::config::bind_host();
    let is_public = !crate::config::is_private_bind_host(&bind_host);

    // 公网绑定启动守卫：公网暴露下若未配置可信反向代理（ILINK_TRUSTED_PROXIES）
    //   或未强制 HTTPS（ILINK_FORCE_HTTPS），拒绝启动。
    //   需显式设置 ILINK_ALLOW_INSECURE_PUBLIC=1 跳过。
    if is_public {
        let has_trusted_proxy = std::env::var("ILINK_TRUSTED_PROXIES")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let force_https = std::env::var("ILINK_FORCE_HTTPS")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        let secure_proxy_configured = has_trusted_proxy && force_https;
        if !secure_proxy_configured {
            let allow_insecure = std::env::var("ILINK_ALLOW_INSECURE_PUBLIC")
                .map(|v| ["1", "true", "yes"].contains(&v.to_lowercase().as_str()))
                .unwrap_or(false);
            if !allow_insecure {
                println!();
                println!("{}", "=".repeat(60));
                println!(
                    "  [安全阻断] 公网绑定 ({}) 但未确认可信 TLS 反向代理！",
                    bind_host
                );
                println!("  当前部署会让登录密码、cookie、消息正文明文暴露在网络中。");
                println!();
                println!("  解决方案（任选其一）：");
                println!("    1) 在前置反向代理（nginx/caddy）终止 TLS，并同时设置：");
                println!("       ILINK_TRUSTED_PROXIES=<反代 IP>");
                println!("       ILINK_FORCE_HTTPS=1");
                println!(
                    "       注意：ILINK_FORCE_HTTPS 只声明上游已有 HTTPS，不会提供 TLS 加密。"
                );
                println!("    2) 仅本机访问：设置 ILINK_HOST=127.0.0.1");
                println!("    3) 临时跳过检查（仅用于受信内网，不应用于公网生产）：");
                println!("       ILINK_ALLOW_INSECURE_PUBLIC=1");
                println!("{}", "=".repeat(60));
                std::process::exit(1);
            } else {
                println!();
                println!("{}", "=".repeat(60));
                println!(
                    "  [安全警告] 公网绑定 ({}) 未配置 TLS / 可信反代！",
                    bind_host
                );
                println!("  ILINK_ALLOW_INSECURE_PUBLIC=1 已跳过启动守卫，仅建议内网调试使用。");
                println!("  生产环境请配置反向代理 + ILINK_TRUSTED_PROXIES。");
                println!("{}", "=".repeat(60));
                println!();
            }
        }
    }

    if has_any_user {
        println!("[AUTH] 多用户登录已启用（访问网页需输入用户名+密码）");
    } else if is_public {
        // 公网绑定且无用户账号时，拒绝启动，防止攻击者在 owner 创建前
        //   访问公开端点（注册、登录页等）窃取信息或滥用资源。
        //   需显式设置 ILINK_ALLOW_INSECURE_PUBLIC=1 才能跳过（仅用于首次部署调试）。
        let allow_insecure = std::env::var("ILINK_ALLOW_INSECURE_PUBLIC")
            .map(|v| ["1", "true", "yes"].contains(&v.to_lowercase().as_str()))
            .unwrap_or(false);
        if allow_insecure {
            println!();
            println!("{}", "=".repeat(60));
            println!(
                "  [安全警告] 当前绑定公网地址 ({}) 但未创建任何用户账号！",
                bind_host
            );
            println!(
                "  已检测到 ILINK_ALLOW_INSECURE_PUBLIC=1，将继续启动（仅建议首次部署调试使用）。"
            );
            println!("  请立即运行：ilink-wm1 admin init 创建 owner 账号");
            println!("{}", "=".repeat(60));
            println!();
        } else {
            println!();
            println!("{}", "=".repeat(60));
            println!(
                "  [安全阻断] 当前绑定公网地址 ({}) 但未创建任何用户账号！",
                bind_host
            );
            println!("  攻击者可能在 owner 创建前访问公开端点，拒绝启动。");
            println!();
            println!("  解决方案（任选其一）：");
            println!("    1) 先以本机模式启动 (ILINK_HOST=127.0.0.1)，运行 ilink-wm1 admin init 创建 owner，");
            println!("       再切换到公网绑定重启");
            println!(
                "    2) 若为首次部署调试，可设置 ILINK_ALLOW_INSECURE_PUBLIC=1 临时跳过此检查"
            );
            println!("{}", "=".repeat(60));
            std::process::exit(1);
        }
    } else {
        println!("[AUTH] 尚无用户账号（本机可直接访问，但需先 ilink-wm1 admin init）");
    }

    // 启动引导：提示 Web 地址和管理方式
    {
        let port = std::env::var("ILINK_PORT").unwrap_or_else(|_| "8888".to_string());
        let host = crate::config::bind_host();
        // 公网绑定且未配置可信反向代理时告警。
        //   trusted_proxies 用于解析真实客户端 IP（X-Forwarded-For / X-Real-IP）。
        //   缺失时 WS Origin 校验仍按 loopback 模式工作，但若反代已部署而未配置此变量，
        //   会导致所有请求的真实 IP 被判为反代自身，可能绕过基于 IP 的限速和审计。
        let is_public_bind = !crate::config::is_private_bind_host(&host);
        let has_trusted_proxy = std::env::var("ILINK_TRUSTED_PROXIES")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if is_public_bind && !has_trusted_proxy {
            println!();
            println!("{}", "=".repeat(58));
            println!(
                "  [BOOT WARN] 公网绑定 ({}) 但未配置 ILINK_TRUSTED_PROXIES",
                host
            );
            println!("  WebSocket Origin 校验将仅放行真正 loopback 客户端；");
            println!("  若已部署反向代理，请设置 ILINK_TRUSTED_PROXIES=<反代 IP>，");
            println!("  否则限速 / 审计 / Origin 校验可能受影响。");
            println!("{}", "=".repeat(58));
        }
        println!();
        println!("{}", "=".repeat(58));
        println!(
            "  [GUIDE] Web 访问: http://{}",
            crate::config::host_port_display(&host, port.parse().unwrap_or(8888))
        );
        println!("  [GUIDE] REPL 输入 /help 查看所有命令；/set 打开设置菜单");
        println!("  [GUIDE] CLI 管理命令: ilink-wm1 admin <sub>（如 admin user list）");
        println!(
            "  [GUIDE] 环境变量: ILINK_PORT={} ILINK_HOST={}",
            port, host
        );
        println!("{}", "=".repeat(58));
        println!();
    }

    // 所有公网绑定、TLS 和首次 owner 安全前置检查完成后，才创建 Router 并绑定端口。
    let mut web_handle = tokio::spawn({
        let bm = bot_manager.clone();
        let sd = web_system_db.clone();
        async move { crate::web::start_server(bm, sd).await }
    });

    // 内网穿透自动恢复：显式触发 TUNNEL_MANAGER 的 Lazy 初始化。
    //   正常启动流程没有任何 tunnel API 调用点，若不在此显式触发，
    //   try_restore 会一直推迟到管理员首次打开隧道页面才执行。
    //   启动失败（Windows 服务先于网络就绪很常见）时有限重试；
    //   连接建立后的断线由 reader 线程的退避重连自愈，无需此处兜底。
    std::thread::Builder::new()
        .name("ilink-tunnel-restore".into())
        .spawn(|| {
            let mgr = crate::admin::get_tunnel_manager(); // 首次访问即执行 try_restore
            for attempt in 1..=4u8 {
                let info = mgr.status();
                let active = matches!(
                    info.state,
                    crate::tunnel::TunnelState::Running | crate::tunnel::TunnelState::Starting
                );
                if active || !crate::tunnel::TunnelManager::load_config_from_file().enabled {
                    break;
                }
                tracing::warn!(
                    "[TUNNEL] 自动恢复未成功（第 {} 次），10s 后重试（网络可能未就绪）",
                    attempt
                );
                std::thread::sleep(std::time::Duration::from_secs(10));
                mgr.try_restore();
            }
        })
        .ok();

    // 解析 owner uid（迁移或向导创建后的 owner 账号）
    let owner_uid: Option<i64> = system_db
        .list_users()
        .into_iter()
        .find(|u| u.role == "owner")
        .map(|u| u.id);

    // 创建 owner 的 bot 实例（若有 owner）。
    // reqwest::blocking::Client 内部会创建自己的 Tokio runtime，
    // BotManager.get_or_create_bot 已用 spawn_blocking 包裹避免 runtime panic。
    // get_or_create_bot 现返回 Result，启动期 owner bot 创建失败
    //   不再 panic 全站，而是降级为纯 Web 服务模式。
    let bot: Option<Arc<bot::WeChatiLinkBot>> = if let Some(uid) = owner_uid {
        match bot_manager.get_or_create_bot(uid).await {
            Ok(b) => Some(b),
            Err(e) => {
                println!("[WARN] owner bot 创建失败，降级为纯 Web 服务模式: {:#}", e);
                println!(
                    "       请检查用户数据库文件 (uid={}) 是否损坏或权限正常。",
                    uid
                );
                None
            }
        }
    } else {
        println!(
            "[MAIN] system.db 无 owner 账号，进入纯服务模式（仅 Web，等待 admin init 创建 owner）"
        );
        None
    };

    // 加载 owner bot 配置并启动轮询（仅 owner 存在时）
    // 不再在终端启动时主动弹出二维码并阻塞等待，绑定流程改为：
    //   用户登录 Web → 访问绑定/二维码页面 → /api/wasm/qrcode 触发 start_login_async。
    let mut bot_timed_out = false;
    if let Some(ref bot) = bot {
        if bot.load_config() {
            println!("[zyn]已获取到连接缓存");
        } else {
            let user_db_path = crate::config::user_db_file(owner_uid.unwrap_or(0));
            let has_db = user_db_path.exists();
            if has_db {
                println!("[zyn]用户数据已存在，但未找到 iLink 连接缓存");
            }

            let qr_state = bot.get_qr_login_state();
            if qr_state.state == QrLoginState::Expired {
                println!("[zyn]会话已过期，请登录 Web 后重新扫码绑定");
                println!("[zyn]历史消息已保留，重新绑定后可继续查看");
            } else if has_db {
                println!(
                    "[zyn]请登录 Web 后扫码连接（数据目录: {}）",
                    user_db_path.display()
                );
            } else {
                println!("[zyn]首次运行，请登录 Web 后扫码连接");
            }
            println!("[zyn]网页地址: http://localhost:{}", bot.web_port);
            // 未从缓存恢复登录，跳过终端轮询；Web 绑定成功后会由 start_login_async 自行启动 polling
            bot_timed_out = true;
        }

        // 启动轮询（仅当 bot 已从缓存恢复登录时）
        if !bot_timed_out {
            bot.start_polling();
            println!("[zyn]后台监听已启动，等待消息...");
            println!("[zyn]网页地址: http://localhost:{}", bot.web_port);

            // 打印用户列表
            let users = bot.list_users();
            if !users.is_empty() {
                cprintln!("\n已保存 {} 个会话", users.len());
                let current = bot.get_current_user();
                for uid in &users {
                    let marker = if Some(uid) == current.as_ref() {
                        "[zyn]"
                    } else {
                        "   "
                    };
                    cprintln!("{}{}", marker, uid);
                }
            } else {
                cprintln!("\n暂未有任何会话");
                println!("[zyn]对方扫完二维码后必须先发送一条消息才能建立联系!");
            }
        } else {
            println!("[zyn]尚未绑定 iLink，登录 Web 后扫码即可开始使用。");
        }
    }

    // stdin REPL 命令循环（仅 owner 存在且非 --no-repl 模式）
    // 重设计 REPL 命令体系：
    //   - 删除默认分支（终端 send 给 iLink 联系人，多用户下语义错位）
    //   - 删除 /switch（切换 iLink 联系人，多用户下无意义）
    //   - /users 改为列出 Web 注册用户（system_db.list_users）
    //   - 新增 /help /notify <username> <msg> /broadcast <level> <msg>
    if let Some(bot_clone) = (!final_no_repl).then_some(bot.as_ref()).flatten().cloned() {
        cprintln!("\n┌{}┐", "─".repeat(58));
        cprintln!("│ /help                                显示所有命令      │");
        cprintln!("│ /set                                 打开设置菜单      │");
        cprintln!("│ /users                               查看所有 Web 用户 │");
        cprintln!("│ /notify <用户名> <消息>              给 Web 用户发通知 │");
        cprintln!("│ /broadcast <info|warn|error> <消息>  全局广播          │");
        cprintln!("│ /web                                 打开网页聊天界面  │");
        cprintln!("│ /webset                              切换前端管理策略  │");
        cprintln!("│ /quit                                退出              │");
        cprintln!("└{}┘\n", "─".repeat(58));

        let sysdb_clone = system_db.clone();
        let bot_manager_repl = bot_manager.clone();
        std::thread::spawn(move || {
            use std::io::{self, BufRead, Write};
            let stdin = io::stdin();
            let mut stdout = io::stdout();
            // 改为显式迭代器，使 /webset / /set 分支可复用
            //   同一迭代器读下一行。
            let mut lines = stdin.lock().lines();
            while let Some(line) = lines.next() {
                match line {
                    Ok(input) => {
                        let input = input.trim().to_string();
                        if input.is_empty() {
                            continue;
                        }
                        match input.as_str() {
                            "/quit" => {
                                bot_clone
                                    .running
                                    .store(false, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                            "/help" => {
                                println!();
                                println!("{}", "=".repeat(58));
                                println!("  REPL 命令帮助");
                                println!("{}", "=".repeat(58));
                                println!("  /help                                 显示本帮助");
                                println!("  /set                                  打开设置菜单（站点名/邀请码/注册策略/守则/端口/环境变量）");
                                println!(
                                    "  /users                                列出所有 Web 注册用户"
                                );
                                println!("  /notify <用户名> <消息>               给 Web 注册用户发系统通知（私信）");
                                println!("  /broadcast <info|warn|error> <消息>   全局广播（所有用户可见）");
                                println!(
                                    "  /web                                  打开网页聊天界面"
                                );
                                println!("  /webset                               切换前端管理访问策略（off/intranet/open）");
                                println!("  /quit                                 退出服务");
                                println!("{}", "=".repeat(58));
                                println!("  CLI 管理命令（在系统终端运行，非 REPL 内）：");
                                println!("    ilink-wm1 admin user list                          查看所有用户");
                                println!("    ilink-wm1 admin user create <username> [role]     创建用户");
                                println!("    ilink-wm1 admin user reset-password <user>        重置密码");
                                println!("    ilink-wm1 admin invite create [days] [note]       创建邀请码");
                                println!("    ilink-wm1 admin invite list                       列出邀请码");
                                println!("    ilink-wm1 admin invite revoke <code>              撤销邀请码");
                                println!("    ilink-wm1 admin config get <key>                  读取配置");
                                println!("    ilink-wm1 admin config set <key> <value>          写入配置");
                                println!("    ilink-wm1 admin config list                       列出所有配置");
                                println!("    ilink-wm1 admin terms set-version <ver>           设置守则版本");
                                println!("    ilink-wm1 admin terms set-text                    从 stdin 读守则文本");
                                println!("    ilink-wm1 admin stats                             系统统计");
                                println!("    ilink-wm1 admin audit list [limit] [--action a]   审计日志");
                                println!("    ilink-wm1 admin audit stats [limit]               审计统计");
                                println!("    ilink-wm1 admin webset show                       查看前端管理策略");
                                println!("    ilink-wm1 admin webset set <off|intranet|open>    设置前端管理策略");
                                println!("    ilink-wm1 admin broadcast send [level] <msg>      发送全局通知");
                                println!("    ilink-wm1 admin broadcast clear                   清除全局通知");
                                println!("    ilink-wm1 admin ip ban <ip> [--reason r] [--days n]  封禁 IP");
                                println!(
                                    "    ilink-wm1 admin ip unban <ip>                     解封 IP"
                                );
                                println!("    ilink-wm1 admin ip list                           列出封禁记录");
                                println!("    ilink-wm1 admin tunnel start [--port p] [--subdomain s]  启动内网穿透");
                                println!("    ilink-wm1 admin tunnel stop                       停止隧道");
                                println!("    ilink-wm1 admin tunnel status                     查看隧道状态");
                                println!("{}", "=".repeat(58));
                            }
                            "/set" => {
                                settings_menu(&sysdb_clone, &mut lines, &mut stdout);
                            }
                            "/users" => {
                                let users = sysdb_clone.list_users();
                                if users.is_empty() {
                                    println!("[zyn] 暂无 Web 注册用户（运行 ilink-wm1 admin init 创建 owner）");
                                } else {
                                    println!("[zyn] Web 注册用户列表（{} 个）:", users.len());
                                    println!(
                                        "{:<6} {:<24} {:<8} {:<10}",
                                        "UID", "USERNAME", "ROLE", "STATUS"
                                    );
                                    println!("{}", "-".repeat(58));
                                    for u in &users {
                                        println!(
                                            "{:<6} {:<24} {:<8} {:<10}",
                                            u.id, u.username, u.role, u.status
                                        );
                                    }
                                }
                            }
                            s if s.starts_with("/notify ") => {
                                let rest = &s[8..];
                                // 解析 <username> <message>：username 是第一个空格前的部分
                                let (username, message) = match rest.split_once(char::is_whitespace)
                                {
                                    Some((u, m))
                                        if !u.trim().is_empty() && !m.trim().is_empty() =>
                                    {
                                        (u.trim(), m.trim())
                                    }
                                    _ => {
                                        println!("  ✗ 用法: /notify <用户名> <消息>");
                                        println!("    例: /notify wong 你好");
                                        let users = sysdb_clone.list_users();
                                        if !users.is_empty() {
                                            println!("  已注册用户:");
                                            for u in &users {
                                                println!(
                                                    "    uid={} username={} role={}",
                                                    u.id, u.username, u.role
                                                );
                                            }
                                        }
                                        continue;
                                    }
                                };
                                let target = sysdb_clone.get_user_by_username(username);
                                let target = match target {
                                    Some(u) => u,
                                    None => {
                                        println!("  ✗ 用户不存在: {}", username);
                                        let users = sysdb_clone.list_users();
                                        if !users.is_empty() {
                                            println!("  已注册用户:");
                                            for u in &users {
                                                println!(
                                                    "    uid={} username={} role={}",
                                                    u.id, u.username, u.role
                                                );
                                            }
                                        }
                                        continue;
                                    }
                                };
                                // 写入系统设置：user_notification.<uid> = JSON
                                // 前端轮询/登录时读取，并按 target_uid 过滤展示
                                let notification = serde_json::json!({
                                    "target_uid": target.id,
                                    "target_username": target.username,
                                    "message": message,
                                    "level": "info",
                                    "time": chrono::Local::now().to_rfc3339(),
                                    "author": "cli-repl",
                                });
                                let key = format!("user_notification.{}", target.id);
                                let val = notification.to_string();
                                let _ = sysdb_clone.set_setting(&key, &val);
                                sysdb_clone.audit_log_warn(
                                    "cli-repl",
                                    "admin.notify.send",
                                    Some(&format!("uid={}", target.id)),
                                    Some(&val),
                                );
                                bot_manager_repl.publish_to_loaded_bot(
                                    target.id,
                                    "notification",
                                    notification,
                                );
                                println!(
                                    "  ✓ 已向用户 {} (uid={}) 发送通知: {}",
                                    target.username, target.id, message
                                );
                            }
                            s if s.starts_with("/broadcast ") => {
                                let rest = &s[11..];
                                let (level, message) = match rest.split_once(char::is_whitespace) {
                                    Some((lv, msg))
                                        if !lv.trim().is_empty() && !msg.trim().is_empty() =>
                                    {
                                        (lv.trim(), msg.trim())
                                    }
                                    _ => {
                                        println!("  ✗ 用法: /broadcast <info|warn|error> <消息>");
                                        println!("    例: /broadcast warn 系统将于今晚维护");
                                        continue;
                                    }
                                };
                                let lv = match level {
                                    "info" | "warn" | "error" => level,
                                    _ => {
                                        println!(
                                            "  ✗ 非法 level: {}（允许 info/warn/error）",
                                            level
                                        );
                                        continue;
                                    }
                                };
                                let notification = serde_json::json!({
                                    "message": message,
                                    "level": lv,
                                    "time": chrono::Local::now().to_rfc3339(),
                                    "author": "cli-repl",
                                });
                                let _ = sysdb_clone
                                    .set_setting("global_notification", &notification.to_string());
                                sysdb_clone.audit_log_warn(
                                    "cli-repl",
                                    "admin.broadcast.send",
                                    Some("global_notification"),
                                    Some(&notification.to_string()),
                                );
                                bot_manager_repl
                                    .broadcast_to_all_bots("notification", notification);
                                println!("  ✓ 已广播 [{}] 通知: {}", lv, message);
                            }
                            "/web" => {
                                bot_clone.open_browser();
                            }
                            "/webset" => {
                                let cur = sysdb_clone
                                    .get_setting("admin.web_access")
                                    .unwrap_or_else(|| "intranet".to_string());
                                println!("[webset] 当前策略: {}", cur);
                                println!("  off=关闭  intranet=仅内网  open=公网");
                                println!("  （输入 q 或 0 或回车 返回主菜单）");
                                // 改为循环重问，非法值不再直接回到主循环
                                loop {
                                    print!("  新策略: ");
                                    let _ = stdout.flush();
                                    // 复用外层 lines 迭代器，不再自己 lock stdin
                                    let m = match lines.next() {
                                        Some(Ok(s)) => s.trim().to_string(),
                                        _ => String::new(),
                                    };
                                    if m.is_empty() || m == "0" || m.eq_ignore_ascii_case("q") {
                                        println!("  ↩ 已返回主菜单");
                                        break;
                                    }
                                    match m.as_str() {
                                        "off" | "intranet" | "open" => {
                                            let _ = sysdb_clone.set_setting("admin.web_access", &m);
                                            // 审计日志写入失败时 warn 告警，不阻断业务（非关键操作）。
                                            sysdb_clone.audit_log_warn(
                                                "cli-repl",
                                                "admin.webset.set",
                                                Some("admin.web_access"),
                                                Some(&format!("{{\"value\":\"{}\"}}", m)),
                                            );
                                            println!("  ✓ 已设置为 {}（即时生效）", m);
                                            break;
                                        }
                                        _ => println!("  ✗ 非法值，允许 off/intranet/open（或 q/0/回车 返回）"),
                                    }
                                }
                            }
                            other => {
                                println!("[zyn] 未知命令: {}（输入 /help 查看所有命令）", other);
                            }
                        }
                        print!("repl>");
                        let _ = stdout.flush();
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // 主循环：保持运行
    //   - 有 owner bot 且正常运行 → 等 bot.running 退出 或 信号触发
    //   - bot 超时/失败 → 降级为纯服务模式，保持 Web 运行
    //   - 无 owner 时纯服务模式一直运行（仅由 Ctrl+C / kill 退出）
    //
    // 监听 SIGINT/SIGTERM，触发后：
    //   1) stop owner bot — join 后台线程、flush 持久化
    //   2) unload 所有 BotManager 管理的 bot — 触发 quota flush
    //   3) 等 web_handle 完成（axum graceful drain 已由 web.rs shutdown_signal 触发）
    let bot_manager_clone = bot_manager.clone();
    let shutdown_signal = async move {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        #[cfg(unix)]
        let terminate = async {
            if let Ok(mut s) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                s.recv().await;
            }
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    };

    if bot_timed_out || bot.is_none() {
        // 降级/纯服务模式：等待 web 服务 或 信号
        println!("[MAIN] 进入纯 Web 服务模式，按 Ctrl+C 退出。");
        tokio::select! {
            _ = &mut web_handle => {
                tracing::info!("[MAIN] Web 服务已退出");
            }
            _ = shutdown_signal => {
                println!("[MAIN] 收到退出信号，正在优雅关闭...");
                // 触发 web graceful shutdown（web.rs 的 shutdown_signal 也会被同一信号唤醒）
                // 等待 web_handle 完成 drain（最多 10s）
                // 使用 &mut 借用，避免 web_handle 被 select 消费后无法在
                //   shutdown_signal 分支体中再次使用。
                let _ = tokio::time::timeout(Duration::from_secs(10), &mut web_handle).await;
                // 多用户模式：卸载所有 bot，触发 quota flush
                for uid in bot_manager_clone.list_loaded_uids() {
                                    // 使用异步版本，避免阻塞 tokio 运行时
                    bot_manager_clone.unload_bot_async(uid).await;
                }
                println!("[zyn]已退出");
            }
        }
    } else if let Some(ref bot) = bot {
        tokio::select! {
            // bot 自行退出（登录超时 / 协议错误）
            _ = async {
                while bot.running.load(std::sync::atomic::Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            } => {
                // bot 已停止，无需再 stop
                // 多用户模式：卸载其它用户 bot
                for uid in bot_manager_clone.list_loaded_uids() {
                                    // 使用异步版本，避免阻塞 tokio 运行时
                    bot_manager_clone.unload_bot_async(uid).await;
                }
                println!("[zyn]bot 已退出");
                // web 服务保持运行（用户可继续访问历史消息）
                // 这里不主动 abort web，等用户 Ctrl+C 时再退出
                let _ = web_handle.await;
            }
            // 收到信号：优雅关闭
            _ = shutdown_signal => {
                println!("[MAIN] 收到退出信号，正在优雅关闭...");
                // 1. 停止 owner bot（block_in_place 因 bot.stop() 内含 thread::join）
                tokio::task::block_in_place(|| bot.stop());
                // 2. 卸载所有其它用户的 bot，触发 quota flush
                for uid in bot_manager_clone.list_loaded_uids() {
                                    // 使用异步版本，避免阻塞 tokio 运行时
                    bot_manager_clone.unload_bot_async(uid).await;
                }
                println!("[zyn]已退出");
                // 3. 等 web drain 完成（最多 10s）
                let _ = tokio::time::timeout(Duration::from_secs(10), web_handle).await;
            }
        }
    }
}

/// 遍历所有 active 用户，对每个用户的 media_cache 执行 LRU 清理。
///
/// 策略：若用户 media_meta 总大小 > max_bytes，按 created_at 升序逐条删除
///   删除本地副本，直到总大小 <= max_bytes。远程副本存在时保留归属与远程元数据；
///   没有远程副本时删除全部记录并根据实际元数据重算媒体配额。
///
/// 实现要点：
///   - 文件路径算法与 `LocalFsBackend::file_path` 一致：hex 前两位作 bucket 子目录
///   - 删除文件失败仅 warn 不阻断（文件可能已被手动删除/被占用）
///   - 删除 media_meta 行后继续，确保即使文件丢失也能清理元数据
///   - 不清理非 active 用户的缓存（他们由 delete_user 负责）
fn purge_media_cache_for_all_users(
    system_db: &Arc<crate::storage::SystemDatabase>,
    bot_manager: &Arc<crate::bot_manager::BotManager>,
    max_bytes: i64,
) {
    let users = system_db.list_users();
    let mut total_purged_bytes: i64 = 0;
    let mut total_purged_count: usize = 0;
    for user in &users {
        if user.status != "active" {
            continue;
        }
        let uid = user.id;
        let db = match crate::storage::Database::new_for_user(uid) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[M2] 用户 uid={} user.db 打开失败，跳过: {}", uid, e);
                continue;
            }
        };
        let total = db.media_cache_total_size();
        if total <= max_bytes {
            continue;
        }
        let media_dir = crate::config::user_media_cache_dir(uid);
        let entries = db.list_media_meta_lru();
        let mut current_size = total;
        let mut user_purged_count: usize = 0;
        let mut user_purged_bytes: i64 = 0;
        for (cache_key, size, _created_at) in entries {
            if current_size <= max_bytes {
                break;
            }
            // 计算本地文件路径（与 LocalFsBackend::file_path 一致）
            let file_path =
                if cache_key.is_empty() || !cache_key.bytes().all(|c| c.is_ascii_hexdigit()) {
                    let hash = crate::crypto::md5_hex(cache_key.as_bytes());
                    media_dir.join(&hash[..2]).join(&hash)
                } else if cache_key.len() >= 2 {
                    let bucket = &cache_key[..2];
                    media_dir.join(bucket).join(&cache_key)
                } else {
                    media_dir.join("00").join(&cache_key)
                };
            // 删本地文件；删除失败时保留 local_present，避免元数据与磁盘状态漂移。
            if file_path.exists() {
                if let Err(e) = std::fs::remove_file(&file_path) {
                    tracing::warn!(
                        "[M2] 删除媒体文件失败 uid={} key={} path={:?}: {}",
                        uid,
                        cache_key,
                        file_path,
                        e
                    );
                    continue;
                }
            }
            let metadata_result = if db.get_media_remote(&cache_key).is_some() {
                db.mark_media_local_absent(&cache_key)
            } else {
                db.remove_media_records(&cache_key).map(|_| ())
            };
            if let Err(e) = metadata_result {
                tracing::warn!(
                    "[M2] 更新媒体元数据失败 uid={} key={}: {}",
                    uid,
                    cache_key,
                    e
                );
                continue;
            }
            current_size -= size;
            user_purged_count += 1;
            user_purged_bytes += size;
        }
        if user_purged_count > 0 {
            let (media_bytes, media_count) = db.media_usage();
            bot_manager.reconcile_media_usage(uid, media_bytes, media_count);
            tracing::info!(
                "[M2] LRU 清理 uid={} username={}：删除 {} 个文件，释放 {:.2} MB",
                uid,
                user.username,
                user_purged_count,
                user_purged_bytes as f64 / 1024.0 / 1024.0
            );
            total_purged_count += user_purged_count;
            total_purged_bytes += user_purged_bytes;
        }
    }
    if total_purged_count > 0 {
        tracing::info!(
            "[M2] LRU 清理完成：共删除 {} 个文件，释放 {:.2} MB（阈值 {} bytes）",
            total_purged_count,
            total_purged_bytes as f64 / 1024.0 / 1024.0,
            max_bytes
        );
    }
}

/// 重试 pending_user_cleanup 队列中的孤儿目录清理。
///
/// 策略：遍历所有待清理条目，尝试 remove_dir_all：
///   - 成功 → 从队列移除
///   - 失败 → attempts++，attempts >= 100 时仅 warn 不再重试（视为永久失败）
fn retry_pending_user_cleanups(system_db: &Arc<crate::storage::SystemDatabase>) {
    let pending = system_db.list_pending_cleanups();
    if pending.is_empty() {
        return;
    }
    let mut succeeded = 0;
    let mut failed = 0;
    for (uid, user_dir_str, attempts, last_error) in pending {
        let user_dir = std::path::Path::new(&user_dir_str);
        if !user_dir.exists() {
            // 目录已不存在（可能被手动清理），从队列移除
            system_db.remove_pending_cleanup(uid);
            succeeded += 1;
            continue;
        }
        match std::fs::remove_dir_all(user_dir) {
            Ok(()) => {
                tracing::info!(
                    "[M8] 补偿清理成功 uid={} dir={}（之前失败 {} 次）",
                    uid,
                    user_dir_str,
                    attempts
                );
                system_db.remove_pending_cleanup(uid);
                succeeded += 1;
            }
            Err(e) => {
                failed += 1;
                system_db.record_pending_cleanup(uid, &user_dir_str, &e.to_string());
                if attempts >= 100 {
                    tracing::warn!(
                        "[M8] uid={} 目录清理已失败 {} 次，视为永久失败：{} (last_error={:?})",
                        uid,
                        attempts,
                        user_dir_str,
                        last_error
                    );
                    // 仍保留在队列中（但不增加重试频率），运维可手动清理 + 通过 CLI 移除
                } else if attempts % 10 == 0 {
                    tracing::warn!(
                        "[M8] uid={} 目录清理失败第 {} 次：{} (error={})",
                        uid,
                        attempts,
                        user_dir_str,
                        e
                    );
                }
            }
        }
    }
    if succeeded > 0 || failed > 0 {
        tracing::info!(
            "[M8] 补偿清理本次：成功 {} 失败 {}（共 {} 条待处理）",
            succeeded,
            failed,
            system_db.list_pending_cleanups().len()
        );
    }
}
