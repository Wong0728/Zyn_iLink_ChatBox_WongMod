// 应用层认证 API（PBKDF2-SHA256 + Session）
// 背靠 SystemDatabase（storage.rs，system.db 绑 uid），不再使用全局单密码表。

use crate::config::PBKDF2_ITERATIONS;
use crate::crypto;
use crate::storage::SystemDatabase;
use std::sync::Arc;

/// 读取密码并显示 `*` 掩码（CLI 通用工具）。
/// 直接使用 windows-sys / libc 操作终端（无外部密码库依赖）。
///   - Windows：GetStdHandle + GetConsoleMode 备份 → SetConsoleMode 清除
///     ENABLE_ECHO_INPUT 与 ENABLE_LINE_INPUT（必须同时清两个，否则
///     ReadConsoleW 仍会等到 Enter 才返回，无法逐字符打印 `*`），保留
///     ENABLE_PROCESSED_INPUT 让 Ctrl+C 仍由系统处理 → ReadConsoleW 每次
///     读 1 个 UTF-16 码元 → 可见字符输出 `*`，BS/DEL 回退擦 `*`，
///     CR 结束并补换行 → Drop 守卫恢复原 console mode。
///   - Unix：tcgetattr 备份 → 清 ECHO + ICANON，VMIN=1 VTIME=0 → read 1
///     字节 → 同规则处理 → Drop 守卫恢复 termios。
///   - 非 tty（stdin 重定向到 pipe/file，例如 CI/测试管道喂入）：GetConsoleMode
///     或 isatty 失败 → 回退到普通 read_line，无掩码（此场景也不需要掩码）。
///
/// panic 安全：所有 syscall 都用 `if x == 0 { return Err(()); }` 显式判断，
/// 不用 `.unwrap()`，因此自身代码无 panic 路径。dev 模式（默认 unwind）
/// Drop 守卫在任何异常返回时都会恢复 mode；release 模式 `panic = "abort"`
/// 不会 panic，正常返回路径 Drop 仍执行。Ctrl+C 走系统 handler 杀进程，
/// mode 不恢复，但 PowerShell/bash 在下次 ReadLine 前会自行 re-init，自愈。
///
/// 依赖：windows-sys（Windows）、libc（Unix），均为 target-specific。
pub fn read_password_with_mask(prompt: &str) -> String {
    use std::io::Write;
    // 先打印 prompt 并 flush（即使非 tty 也要打印，让用户知道在等什么）
    print!("{}", prompt);
    let _ = std::io::stdout().flush();

    match read_password_masked() {
        Ok(s) => s,
        Err(_) => {
            // 回退：非 tty 或 syscall 失败，普通读一行（无掩码）
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            trim_trailing_newlines(&mut s);
            s
        }
    }
}

/// 去掉字符串末尾的 \r 和 \n（跨平台）。
fn trim_trailing_newlines(s: &mut String) {
    while let Some(c) = s.chars().last() {
        if c == '\n' || c == '\r' {
            s.pop();
        } else {
            break;
        }
    }
}

// ── 平台分支 ────────────────────────────────────────────────────

#[cfg(windows)]
fn read_password_masked() -> Result<String, ()> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, ReadConsoleW, SetConsoleMode, ENABLE_ECHO_INPUT,
        ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    unsafe {
        // 1. 获取 stdin console handle
        let h_in = GetStdHandle(STD_INPUT_HANDLE);
        if h_in.is_null() || h_in == INVALID_HANDLE_VALUE {
            return Err(());
        }

        // 2. 备份当前 console mode；同时验证 stdin 确为 console
        //    （若 stdin 是 pipe/file，GetConsoleMode 返回 0 → 回退）
        let mut old_mode: u32 = 0;
        if GetConsoleMode(h_in, &mut old_mode) == 0 {
            return Err(());
        }

        // 3. 新 mode：清 ECHO + LINE_INPUT，保留其余位（含 EXTENDED_KEY 等），
        //    显式置位 PROCESSED_INPUT（保证 Ctrl+C 仍由系统处理）
        let new_mode =
            (old_mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT)) | ENABLE_PROCESSED_INPUT;
        if SetConsoleMode(h_in, new_mode) == 0 {
            return Err(());
        }

        // 4. Drop 守卫：无论 return 还是 panic（dev unwind）都恢复 mode
        let _guard = ConsoleModeGuard {
            handle: h_in,
            mode: old_mode,
        };

        // 5. 准备 stdout handle（WriteConsoleW 用；可能为 0，write_console 会自检）
        let h_out = GetStdHandle(STD_OUTPUT_HANDLE);

        let mut buf: Vec<u16> = Vec::new(); // 已输入字符（UTF-16 累积）
        let mut one: [u16; 1] = [0u16; 1]; // 单字符读取缓冲

        loop {
            let mut read: u32 = 0;
            // ReadConsoleW 在 char 模式（无 LINE_INPUT）下：每次返回 1 个 UTF-16
            // 码元。非字符键（方向键/Fn 等）的 UnicodeChar 为 0x0000，会被忽略。
            let ok = ReadConsoleW(
                h_in,
                one.as_mut_ptr() as *mut _,
                1,
                &mut read,
                std::ptr::null_mut(),
            );
            if ok == 0 || read == 0 {
                // 读取失败或 EOF（Ctrl+Z 在 char 模式返回 0x1A，但有些场景 read==0）
                break;
            }
            let c = one[0];
            if c == 0x0D || c == 0x0A {
                // 回车确认：输出 CRLF 换行并结束
                write_console(h_out, &[0x0D, 0x0A]);
                break;
            } else if c == 0x08 || c == 0x7F {
                // Backspace（0x08）或 Delete（0x7F）
                if !buf.is_empty() {
                    buf.pop();
                    // 回退光标、空格覆盖、再回退：擦掉一个 `*`
                    write_console(h_out, &[0x08, b' ' as u16, 0x08]);
                }
            } else if c == 0x1A {
                // Ctrl+Z：当作 EOF，输出换行结束
                write_console(h_out, &[0x0D, 0x0A]);
                break;
            } else if c >= 0x20 {
                // 可见字符（含 BMP 内的中文等）：累积并输出 `*`
                buf.push(c);
                write_console(h_out, &['*' as u16]);
            }
            // 0x00（非字符键）和其他控制字符（0x01..0x1F）：忽略
        }

        // UTF-16 → String。密码几乎都是 BMP 字符，不做 surrogate pair 处理；
        // from_utf16_lossy 对孤立 surrogate 会替换为 U+FFFD，不影响 ASCII 密码。
        Ok(String::from_utf16_lossy(&buf))
    }
}

#[cfg(windows)]
unsafe fn write_console(h: windows_sys::Win32::Foundation::HANDLE, chars: &[u16]) {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::WriteConsoleW;
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return;
    }
    let mut written: u32 = 0;
    let _ = WriteConsoleW(
        h,
        chars.as_ptr() as *const _,
        chars.len() as u32,
        &mut written,
        std::ptr::null(),
    );
}

#[cfg(windows)]
struct ConsoleModeGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
    mode: u32,
}

#[cfg(windows)]
impl Drop for ConsoleModeGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        // 恢复原 mode（忽略错误：进程若在退出，恢复失败也无意义）
        unsafe {
            let _ = SetConsoleMode(self.handle, self.mode);
        }
    }
}

#[cfg(unix)]
fn read_password_masked() -> Result<String, ()> {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        // 1. 检测 tty
        if libc::isatty(fd) == 0 {
            return Err(());
        }
        // 2. 备份 termios
        let mut term: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut term) != 0 {
            return Err(());
        }
        let old_term = term;
        // 3. 清 ECHO + ICANON，VMIN=1 VTIME=0（保留 ISIG 让 Ctrl+C 仍生效）
        term.c_lflag &= !(libc::ECHO | libc::ICANON);
        term.c_cc[libc::VMIN] = 1;
        term.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &term) != 0 {
            return Err(());
        }
        let _guard = TermiosGuard { fd, term: old_term };

        let mut buf: Vec<u8> = Vec::new();
        let mut one: [u8; 1] = [0u8; 1];
        loop {
            let n = libc::read(fd, one.as_mut_ptr() as *mut _, 1);
            if n <= 0 {
                break;
            }
            let c = one[0];
            match c {
                b'\n' | b'\r' => {
                    // 回车确认：输出换行（终端会自动 \r\n 转换，这里只发 \n）
                    let _ = libc::write(libc::STDOUT_FILENO, b"\n".as_ptr() as *const _, 1);
                    break;
                }
                0x7F | 0x08 => {
                    // DEL（多数 Unix 终端）或 BS
                    if !buf.is_empty() {
                        buf.pop();
                        let _ = libc::write(
                            libc::STDOUT_FILENO,
                            b"\x08 \x08".as_ptr() as *const _,
                            3,
                        );
                    }
                }
                0x04 /* Ctrl+D EOF */ => {
                    let _ = libc::write(libc::STDOUT_FILENO, b"\n".as_ptr() as *const _, 1);
                    break;
                }
                0x20..=0x7E => {
                    // ASCII 可见字符
                    buf.push(c);
                    let _ = libc::write(libc::STDOUT_FILENO, b"*".as_ptr() as *const _, 1);
                }
                _ => {
                    // 非 ASCII 字节（UTF-8 多字节序列的后续字节）：累积但不打印 `*`
                    // 简化处理——密码含非 ASCII 较少，from_utf8_lossy 兜底
                    buf.push(c);
                    let _ = libc::write(libc::STDOUT_FILENO, b"*".as_ptr() as *const _, 1);
                }
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

#[cfg(unix)]
struct TermiosGuard {
    fd: i32,
    term: libc::termios,
}

#[cfg(unix)]
impl Drop for TermiosGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSANOW, &self.term);
        }
    }
}

// 非 Windows/Unix 平台：永远走回退路径（read_line 无掩码）
#[cfg(not(any(windows, unix)))]
fn read_password_masked() -> Result<String, ()> {
    Err(())
}

pub struct Auth {
    system_db: Arc<SystemDatabase>,
}

impl Auth {
    pub fn new(system_db: Arc<SystemDatabase>) -> Self {
        Self { system_db }
    }

    /// 密码强度检查：至少包含大小写字母和数字，长度 8-128 位，不区分大小写的弱口令黑名单。
    /// 一次性返回全部未满足的要求，避免用户多次试错。
    pub fn check_password_strength(password: &str) -> Result<(), String> {
        let mut missing: Vec<&str> = Vec::new();

        if password.len() < 8 {
            return Err("密码长度至少 8 位".to_string());
        }
        if password.len() > 128 {
            return Err("密码长度不能超过 128 位".to_string());
        }

        // 常见弱口令黑名单（不区分大小写，约 40 条）
        const WEAK_PASSWORDS: &[&str] = &[
            "password1",
            "password123",
            "abc12345",
            "qwerty123",
            "12345678",
            "123456789",
            "1234567890",
            "11111111",
            "00000000",
            "iloveyou1",
            "admin123",
            "letmein1",
            "welcome1",
            "monkey123",
            "football1",
            "p@ssw0rd",
            "passw0rd",
            "abcd1234",
            "qwerty1",
            "qwerty12",
            "qwerty1234",
            "1q2w3e4r",
            "1qaz2wsx",
            "qazwsx123",
            "zxcvbnm1",
            "asdfghjkl",
            "qwertyuiop",
            "admin1234",
            "admin@123",
            "administrator1",
            "root123",
            "toor123",
            "test1234",
            "test12345",
            "guest123",
            "user123",
            "user1234",
            "changeme1",
            "welcome123",
            "china123",
            "beijing123",
            "shanghai123",
            "woaini520",
            "woaini1314",
            "5201314a",
            "iloveyou123",
            "sunshine1",
            "princess1",
            "dragon123",
            "master123",
        ];
        let lower = password.to_lowercase();
        if WEAK_PASSWORDS.contains(&lower.as_str()) {
            return Err("密码过于常见,请更换更强的密码".to_string());
        }

        let has_uppercase = password.chars().any(|c| c.is_ascii_uppercase());
        let has_lowercase = password.chars().any(|c| c.is_ascii_lowercase());
        let has_digit = password.chars().any(|c| c.is_ascii_digit());

        if !has_uppercase {
            missing.push("至少一个大写字母");
        }
        if !has_lowercase {
            missing.push("至少一个小写字母");
        }
        if !has_digit {
            missing.push("至少一个数字");
        }

        if !missing.is_empty() {
            return Err(format!(
                "密码需满足以下要求但当前缺失：{}",
                missing.join("、")
            ));
        }

        Ok(())
    }

    /// 校验用户凭据（用户名 + 密码）。
    /// S6: 用户名不存在 / 密码错误 / 账号禁用均返回 None，调用方据 None 统一报"用户名或密码错误"。
    /// 如需区分"账号已禁用"，调用方可用 SystemDatabase::get_user_by_username 二次查询 status。
    /// 成功返回 (uid, role)，并异步更新 last_login（忽略错误）。
    /// 验签时序：始终计算一次 PBKDF2（不存在/禁用账号用 dummy hash），
    /// 在常量时间比较完成后再判定账号状态，防止响应时间差异泄露有效用户名。
    pub fn verify_user_credentials(&self, username: &str, password: &str) -> Option<(i64, String)> {
        // 始终尝试取凭据；None 时使用 dummy，保持后续 PBKDF2 + 比较流程一致。
        let cred = self.system_db.get_user_credentials(username);

        // 不存在的用户使用 dummy 凭据：固定 hash/salt + 默认 iterations，
        // 确保 PBKDF2 计算量与真实用户一致，user_exists 延后到比较后判断。
        const DUMMY_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        const DUMMY_SALT: &str = "fedcba9876543210fedcba9876543210";
        let (cred, user_exists) = match cred {
            Some(c) => (c, true),
            None => (
                crate::storage::UserCredentials {
                    uid: 0,
                    role: String::new(),
                    status: String::from("active"), // 假装 active，让流程进入 PBKDF2
                    password_hash: DUMMY_HASH.to_string(),
                    salt: DUMMY_SALT.to_string(),
                    iterations: PBKDF2_ITERATIONS as i64,
                },
                false,
            ),
        };

        // 兼容旧库：iterations <= 0 时回退到默认配置
        let iterations = if cred.iterations > 0 {
            cred.iterations as u32
        } else {
            PBKDF2_ITERATIONS
        };

        // 始终计算 PBKDF2（无论用户是否存在、是否禁用），保证耗时一致
        let digest = crypto::pbkdf2_hash(password, &cred.salt, iterations);

        // 始终进行常量时间比较（dummy hash 必然不匹配，但耗时与真实比较相同）
        let password_ok = crypto::constant_time_compare(&digest, &cred.password_hash);

        // 比较完成后再统一判定：用户必须存在 + 密码必须正确 + 账号必须 active
        if !user_exists || !password_ok || cred.status != "active" {
            return None;
        }

        // 校验通过：更新 last_login（忽略错误，不阻塞登录）
        let _ = self.system_db.update_last_login(cred.uid);

        // 登录时自动升级弱迭代参数：若 iterations < PBKDF2_ITERATIONS（或 ≤0），
        // 用当前参数重新哈希写入 DB，逐步升级存量用户。失败不阻塞登录。
        let need_upgrade = cred.iterations <= 0 || (cred.iterations as u32) < PBKDF2_ITERATIONS;
        if need_upgrade {
            let new_salt = crypto::random_hex(16);
            let new_hash = crypto::pbkdf2_hash(password, &new_salt, PBKDF2_ITERATIONS);
            let old_iter = cred.iterations;
            match self.system_db.set_user_password(
                cred.uid,
                &new_hash,
                &new_salt,
                PBKDF2_ITERATIONS as i64,
            ) {
                Ok(()) => tracing::info!(
                    "[M11] 升级用户 uid={} 密码哈希参数：iterations {} → {}",
                    cred.uid,
                    old_iter,
                    PBKDF2_ITERATIONS
                ),
                Err(e) => tracing::warn!(
                    "[M11] 升级用户 uid={} 密码哈希参数失败（iterations {} → {}）: {}",
                    cred.uid,
                    old_iter,
                    PBKDF2_ITERATIONS,
                    e
                ),
            }
        }

        Some((cred.uid, cred.role))
    }

    /// 为指定 uid 创建会话，返回新 token；底层失败返回 None。
    pub fn create_session(&self, uid: i64) -> Option<String> {
        self.system_db.create_session(uid).ok()
    }

    /// 校验会话 token。返回 SessionInfo（含 uid/role）；
    /// None 表示无效或过期（SystemDatabase 内部已做 S1 惰性清理）。
    pub fn verify_session(&self, token: &str) -> Option<crate::storage::SessionInfo> {
        self.system_db.verify_session(token)
    }

    /// 续期会话（30 天滑动窗口）。错误忽略。
    pub fn renew_session(&self, token: &str) {
        let _ = self.system_db.renew_session(token);
    }

    /// 删除指定会话（登出）。错误忽略。
    pub fn delete_session(&self, token: &str) {
        let _ = self.system_db.delete_session(token);
    }

    /// 修改密码：先校验旧密码，再写入新盐 + 新 hash。
    /// 成功后调用方应自行 delete_other_sessions(uid, current_token) 让旧会话失效。
    /// 改密码后强制作废该用户全部会话（含当前 token），迫使用户用新密码重登。
    pub fn change_password(
        &self,
        uid: i64,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), String> {
        // 新密码强度检查
        Self::check_password_strength(new_password)?;

        // 取用户当前凭据
        let user = self
            .system_db
            .get_user_by_id(uid)
            .ok_or_else(|| "用户不存在".to_string())?;

        // 校验旧密码
        let iterations = if user.iterations > 0 {
            user.iterations as u32
        } else {
            PBKDF2_ITERATIONS
        };
        let old_digest = crypto::pbkdf2_hash(old_password, &user.salt, iterations);
        if !crypto::constant_time_compare(&old_digest, &user.password_hash) {
            return Err("旧密码错误".to_string());
        }

        // 生成新盐 + 新 hash
        let new_salt = crypto::random_hex(16);
        let new_hash = crypto::pbkdf2_hash(new_password, &new_salt, PBKDF2_ITERATIONS);

        // 改密 + 失效全部会话 + 撤销设备令牌三步原子化。
        //   改用 SystemDatabase::change_user_password_atomic 用 transaction() 包裹，
        //   任一步失败全部回滚。同时 P0-7 的"改密后旧 token 立即失效"保证不被破坏。
        //   atomic 函数返回 Err 即三步全失败，返回 Ok 即三步全成功。
        // map_err 不再返回 e.to_string()（可能泄露 SQL 错误），
        //   改为通用消息 + warn 日志。
        self.system_db
            .change_user_password_atomic(uid, &new_hash, &new_salt, PBKDF2_ITERATIONS as i64)
            .map_err(|e| {
                tracing::warn!("[M14] change_user_password_atomic 失败 uid={}: {}", uid, e);
                "修改密码失败，请稍后重试".to_string()
            })?;

        Ok(())
    }

    /// 创建新用户。返回新 uid（6 位随机数字）。
    /// 用户名重复等底层错误经 anyhow 转为 String。
    pub fn create_user(&self, username: &str, password: &str, role: &str) -> Result<i64, String> {
        Self::check_password_strength(password)?;

        // 用户名重复显式检查，返回业务消息（不依赖 SQL UNIQUE 错误）。
        if self.system_db.get_user_by_username(username).is_some() {
            return Err("用户名已存在".to_string());
        }

        let salt = crypto::random_hex(16);
        let hash = crypto::pbkdf2_hash(password, &salt, PBKDF2_ITERATIONS);

        // 先分配 6 位随机 uid（100000..=999999），再插入。
        // 6 位空间 90 万，远大于本应用的用户数；碰撞由 allocate_random_uid 内部重试 50 次。
        let uid = self.system_db.allocate_random_uid().map_err(|e| {
            tracing::warn!("[M14] allocate_random_uid 失败: {}", e);
            "创建用户失败，请稍后重试".to_string()
        })?;

        self.system_db
            .create_user(uid, username, &hash, &salt, PBKDF2_ITERATIONS as i64, role)
            .map_err(|e| {
                tracing::warn!("[M14] create_user 失败 username={}: {}", username, e);
                "创建用户失败，请稍后重试".to_string()
            })?;

        Ok(uid)
    }
}
