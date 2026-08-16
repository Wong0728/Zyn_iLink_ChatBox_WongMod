// SQLite 持久化封装（线程安全）
// 结构化数据：bot 配置、用户 token、消息、媒体元数据、密码哈希、WebDAV 凭证

use crate::config;
use crate::crypto;
use crate::models::{self, MediaMeta, MediaRemote, WebDavConfig};
use anyhow::Context;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 可由 CLI 与管理 Web 修改的系统设置单一白名单。
pub fn is_supported_system_setting(key: &str) -> bool {
    matches!(
        key,
        "site_name"
            | "allow_open_registration"
            | "allow_invite_registration"
            | "terms_version"
            | "terms_text"
            | "terms.url"
            | "docs.url"
            | "default_quota_upload_bytes"
            | "default_quota_download_bytes"
            | "default_quota_media_bytes"
            | "default_quota_msg_per_day"
            | "default_quota_media_count"
            | "default_allow_upload"
            | "default_allow_webdav"
            | "default_allow_custom_webdav"
            | "admin.web_access"
    )
}

pub fn setting_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "on" | "true" | "1" | "yes"
    )
}

pub fn validate_system_setting(key: &str, value: &str) -> anyhow::Result<()> {
    if !is_supported_system_setting(key) {
        anyhow::bail!("不支持的系统设置: {}", key);
    }
    match key {
        "site_name" if value.trim().is_empty() || value.chars().count() > 100 => {
            anyhow::bail!("站点名称须为 1~100 个字符")
        }
        "allow_open_registration"
        | "allow_invite_registration"
        | "default_allow_upload"
        | "default_allow_webdav"
        | "default_allow_custom_webdav"
            if !matches!(
                value.to_ascii_lowercase().as_str(),
                "on" | "off" | "true" | "false" | "1" | "0"
            ) =>
        {
            anyhow::bail!("开关值必须是 on/off、true/false 或 1/0");
        }
        "terms_version" if value.trim().is_empty() || value.len() > 64 => {
            anyhow::bail!("守则版本不能为空且不能超过 64 字节")
        }
        "terms_text" if value.len() > 65_536 => anyhow::bail!("守则正文不能超过 64KB"),
        "terms.url" | "docs.url" if !value.trim().is_empty() => {
            let parsed = url::Url::parse(value)?;
            if !matches!(parsed.scheme(), "http" | "https") {
                anyhow::bail!("文档链接只允许 http/https");
            }
        }
        "default_quota_upload_bytes"
        | "default_quota_download_bytes"
        | "default_quota_media_bytes"
        | "default_quota_msg_per_day"
        | "default_quota_media_count" => {
            let quota = value.parse::<i64>()?;
            if quota < -1 {
                anyhow::bail!("配额只能为 -1（无限制）、0（未设置）或正整数");
            }
        }
        "admin.web_access" if !matches!(value, "off" | "intranet" | "open") => {
            anyhow::bail!("admin.web_access 只能是 off、intranet 或 open")
        }
        _ => {}
    }
    Ok(())
}

/// DPAPI 包装的主密钥文件（审计 M-7，仅 Windows）。
/// 文件格式: b"DPAPI1" + CryptProtectData(原始 32 字节密钥)。
/// 采用机器作用域（CRYPTPROTECT_LOCAL_MACHINE）而非用户作用域：
///   - 解决审计威胁：数据目录整体泄露（备份/打包/误传/磁盘离线挂载）时，
///     无本机 DPAPI 密钥无法解出主密钥，AES-GCM 不再被"密文+密钥同目录"击穿；
///   - 兼容服务/CLI 分体运行（install-service.bat 以管理员跑 `admin init`、
///     服务以 NT SERVICE 虚拟账户运行），用户作用域会导致服务解不开密钥；
///   - 同机其他本地用户仍被文件 ACL（icacls 仅当前用户/服务账户）挡在读文件这层。
#[cfg(windows)]
mod master_key_dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    const MAGIC: &[u8; 6] = b"DPAPI1";
    // CRYPTPROTECT_LOCAL_MACHINE：机器作用域（理由见模块注释）
    const FLAGS: u32 = 0x4;

    pub fn is_wrapped(data: &[u8]) -> bool {
        data.starts_with(MAGIC)
    }

    pub fn wrap(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        unsafe {
            let in_blob = CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            };
            let mut out_blob = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };
            if CryptProtectData(
                &in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                FLAGS,
                &mut out_blob,
            ) == 0
            {
                anyhow::bail!("CryptProtectData 失败");
            }
            let wrapped =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            LocalFree(out_blob.pbData as _);
            let mut file = MAGIC.to_vec();
            file.extend_from_slice(&wrapped);
            Ok(file)
        }
    }

    /// 返回 None 表示未包装或解包失败（如更换运行账户）。
    pub fn unwrap(data: &[u8]) -> Option<Vec<u8>> {
        if !is_wrapped(data) {
            return None;
        }
        let payload = &data[MAGIC.len()..];
        unsafe {
            let in_blob = CRYPT_INTEGER_BLOB {
                cbData: payload.len() as u32,
                pbData: payload.as_ptr() as *mut u8,
            };
            let mut out_blob = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            };
            if CryptUnprotectData(
                &in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                FLAGS,
                &mut out_blob,
            ) == 0
            {
                return None;
            }
            let raw =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            LocalFree(out_blob.pbData as _);
            Some(raw)
        }
    }
}

/// 获取/创建与 db 同目录的主密钥（32 字节）
/// S23: Windows 下 icacls 失败时拒绝以默认权限保存主密钥。
/// 审计 M-7: Windows 下主密钥文件内容经 DPAPI（按当前用户）包装，
///   密文与密钥不再同目录裸奔；旧版明文 32 字节密钥文件仍可读并自动迁移。
/// 审计 L-7: Unix 以 0600 模式原子创建、Windows 先包装后落盘，
///   消除"先写后设权限"的竞态窗口。
///
/// 每次按需读取文件，不做内存缓存。调用方使用后必须调用 `zeroize_key` 清零。
fn get_master_key(db_path: &Path) -> anyhow::Result<Vec<u8>> {
    let key_path = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".ilink_master_key");

    let raw = std::fs::read(&key_path).unwrap_or_default();

    // ① DPAPI 包装格式（仅 Windows）
    #[cfg(windows)]
    if master_key_dpapi::is_wrapped(&raw) {
        let key = master_key_dpapi::unwrap(&raw).filter(|k| k.len() == 32);
        return match key {
            Some(k) => Ok(k),
            None => anyhow::bail!(
                "主密钥文件 DPAPI 解包失败（机器作用域：密钥文件只能在生成它的那台机器上\
                 解开；跨机迁移数据目录需在新机器上删除该文件后按文档重置凭证）。路径: {}",
                key_path.display()
            ),
        };
    }

    // ② 旧版明文 32 字节密钥：兼容读取，Windows 下自动迁移为 DPAPI 包装
    if raw.len() == 32 {
        #[cfg(windows)]
        if let Ok(wrapped) = master_key_dpapi::wrap(&raw) {
            if std::fs::write(&key_path, &wrapped).is_ok() {
                tracing::info!(
                    "[STORAGE] 主密钥已自动迁移为 DPAPI 包装格式: {}",
                    key_path.display()
                );
            }
        }
        return Ok(raw);
    }

    if !raw.is_empty() {
        anyhow::bail!(
            "主密钥文件损坏: {}（长度 {}，期望 32 字节或 DPAPI 包装格式）",
            key_path.display(),
            raw.len()
        );
    }

    // ③ 生成新密钥
    let new_key = crypto::random_bytes(32);
    #[cfg(windows)]
    let file_bytes = match master_key_dpapi::wrap(&new_key) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "[STORAGE] DPAPI 包装主密钥失败，回退明文密钥文件 + icacls 模式: {}",
                e
            );
            new_key.clone()
        }
    };
    #[cfg(not(windows))]
    let file_bytes = new_key.clone();

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // 以 0600 模式创建（审计 L-7），避免先落盘后 chmod 的权限竞态窗口
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&key_path)
            .map_err(|e| anyhow::anyhow!("写入主密钥失败: {}", e))?;
        f.write_all(&file_bytes)
            .map_err(|e| anyhow::anyhow!("写入主密钥失败: {}", e))?;
    }
    #[cfg(windows)]
    {
        std::fs::write(&key_path, &file_bytes)
            .map_err(|e| anyhow::anyhow!("写入主密钥失败: {}", e))?;
        // 用 icacls 限制密钥文件 ACL 仅当前用户完全控制。
        // icacls 失败直接报错（不 fallback），强制运维手动设置 ACL 后才允许保存主密钥。
        // 注：内容已 DPAPI 包装，即使 ACL 设置前的竞态窗口内被读取也无法使用。
        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .map_err(|_| {
                anyhow::anyhow!(
                    "无法获取用户名环境变量 (USERNAME/USER),拒绝以默认权限保存主密钥"
                )
            })?;
        let key_path_str = key_path.to_string_lossy().to_string();
        let grant = format!("{}:F", username);
        let status = std::process::Command::new("icacls")
            .args([&key_path_str, "/inheritance:r", "/grant:r", &grant])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let icacls_ok = matches!(status, Ok(s) if s.success());
        if icacls_ok {
            tracing::info!(
                "[STORAGE] 已通过 icacls 限制主密钥文件 ACL 仅 {} 可访问（内容另经 DPAPI 包装）",
                username
            );
        } else {
            // icacls 失败直接返回 Err，拒绝以默认权限保存主密钥。
            let status_detail = match status {
                Ok(s) => format!("exit_code={}", s.code().unwrap_or(-1)),
                Err(e) => format!("exec_failed: {}", e),
            };
            return Err(anyhow::anyhow!(
                "icacls 设置 ACL 失败 ({}),拒绝以默认权限保存主密钥。\n\
                 请手动执行: icacls \"{}\" /inheritance:r /grant:r {}:F\n\
                 主密钥路径: {}",
                status_detail,
                key_path.display(),
                username,
                key_path.display()
            ));
        }
    }
    tracing::info!("[STORAGE] 已生成 WebDAV 凭证主密钥: {}", key_path.display());
    Ok(new_key)
}

/// 显式清零主密钥字节（volatile write + compiler_fence），防止编译器优化掉清零。
pub fn zeroize_key(key: &mut [u8]) {
    for byte in key.iter_mut() {
        // 使用 volatile write 防止编译器优化掉清零
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    // 编译器屏障，确保清零指令不被重排到 drop 之前
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn encrypt_secret(plaintext: &str, db_path: &Path) -> anyhow::Result<String> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    // 加密失败返回 Err，不再回退明文落库。按需读取主密钥，加密后立即清零。
    let mut key = get_master_key(db_path)?;
    let result = crypto::aes_gcm_encrypt(plaintext, &key)
        .map_err(|e| anyhow::anyhow!("WebDAV 密码加密失败: {}", e));
    zeroize_key(&mut key);
    result
}

fn decrypt_secret(stored: &str, db_path: &Path) -> anyhow::Result<String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    // 明文回退策略统一由 crypto::aes_gcm_decrypt 决策（严格模式默认拒绝）。按需读取主密钥，解密后立即清零。
    let mut key = get_master_key(db_path)?;
    let result = crypto::aes_gcm_decrypt(stored, &key);
    zeroize_key(&mut key);
    result
}

/// 数据库单例管理
static DB_INSTANCES: once_cell::sync::Lazy<Mutex<HashMap<String, Arc<Database>>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

pub struct Database {
    db_path: PathBuf,
    conn: Mutex<Connection>,
}

impl Database {
    /// 获取或创建数据库实例（同路径共享单例）
    ///
    /// DB 初始化失败（文件损坏/磁盘满/权限不足）返回 Err，由调用方统一处理（不再 panic）。
    pub fn new(db_path: &Path) -> anyhow::Result<Arc<Database>> {
        let key = db_path.to_string_lossy().to_string();
        let mut instances = DB_INSTANCES.lock();
        if let Some(inst) = instances.get(&key) {
            return Ok(inst.clone());
        }
        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("无法打开 SQLite 数据库: {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
        )
        .with_context(|| format!("无法设置 SQLite PRAGMA: {}", db_path.display()))?;
        let db = Arc::new(Database {
            db_path: db_path.to_path_buf(),
            conn: Mutex::new(conn),
        });
        db.init_db()?;
        // 在 init_db 释放 conn guard 后执行明文加密迁移（避免死锁）。
        db.run_migrate_plaintext_secrets();
        instances.insert(key, db.clone());
        Ok(db)
    }

    /// 指定 uid 的 user.db 实例（单例，复用 Database::new）
    pub fn new_for_user(uid: i64) -> anyhow::Result<Arc<Database>> {
        Self::new(&config::user_db_file(uid))
    }

    /// 暴露内部 sqlite 连接（供 SystemDatabase 等复用 Database 句柄执行 SQL）
    pub fn conn_lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    fn init_db(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS user_tokens (
                user_id TEXT PRIMARY KEY,
                context_token TEXT NOT NULL,
                bot_token TEXT DEFAULT '',
                saved_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id TEXT NOT NULL,
                msg_type TEXT DEFAULT '',
                msg_text TEXT DEFAULT '',
                json_data TEXT NOT NULL,
                time TEXT DEFAULT '',
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_user_id ON messages(user_id);
            -- 复合索引覆盖 (user_id, id)，加速按用户+时间倒序拉取
            -- 旧 idx_messages_user_id 保留不动，避免影响已部署库
            CREATE INDEX IF NOT EXISTS idx_messages_user_id_id ON messages(user_id, id);
            CREATE TABLE IF NOT EXISTS media_meta (
                cache_key TEXT PRIMARY KEY,
                scope TEXT DEFAULT 'global',
                mime TEXT DEFAULT '',
                filename TEXT DEFAULT '',
                size INTEGER DEFAULT 0,
                local_present INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS webdav_config (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                enabled INTEGER NOT NULL DEFAULT 0,
                url TEXT NOT NULL DEFAULT '',
                username TEXT NOT NULL DEFAULT '',
                password TEXT NOT NULL DEFAULT '',
                base_path TEXT NOT NULL DEFAULT '/ilink-media',
                traffic_saver INTEGER NOT NULL DEFAULT 0,
                auto_migrate_on_save INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS media_remote (
                cache_key TEXT PRIMARY KEY,
                remote_path TEXT NOT NULL,
                uploaded_at TEXT NOT NULL,
                user_id TEXT DEFAULT '',
                content_md5 TEXT NOT NULL DEFAULT ''
            );
            -- messages_v2: 持久化优先 + 去重 + 状态机
            -- 解决问题: 旧 messages 表无 message_id 字段，无法去重；崩溃会丢消息；
            --          发送状态机无落库，前端超时即显示失败
            CREATE TABLE IF NOT EXISTS messages_v2 (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                trace_id        TEXT NOT NULL,
                bot_id          TEXT NOT NULL DEFAULT '',       -- 存储 SHA-256(bot_token) hex（不可逆哈希），用作查询/去重索引；原 token 由 user_tokens 表加密保存
                user_id         TEXT NOT NULL,
                direction       TEXT NOT NULL,                -- 'in' | 'out'
                message_id      INTEGER,                      -- iLink 平台消息 ID（入站时拿到）
                client_id       TEXT,                         -- 出站时本地生成
                from_user_id    TEXT,
                to_user_id      TEXT,
                context_token   TEXT,
                item_list_json  TEXT NOT NULL DEFAULT '[]',  -- 完整 item_list（text/image/voice/file/video）
                text            TEXT,
                media_status    TEXT NOT NULL DEFAULT '',    -- '' | 'downloading' | 'ready' | 'failed'
                media_keys_json TEXT,
                send_state      TEXT NOT NULL DEFAULT '',     -- '' | 'pending' | 'sending' | 'sent' | 'delivered' | 'failed' | 'expired'
                send_attempts   INTEGER NOT NULL DEFAULT 0,
                send_last_error TEXT,
                processed       INTEGER NOT NULL DEFAULT 0,  -- MarkProcessed 标志
                created_at_ms   INTEGER NOT NULL,
                updated_at_ms   INTEGER NOT NULL,
                raw_json        TEXT
            );
            -- 入站去重：iLink 偶发重投 (bot_id, message_id) 唯一
            CREATE UNIQUE INDEX IF NOT EXISTS idx_msg_v2_dedup
                ON messages_v2 (bot_id, message_id) WHERE message_id IS NOT NULL;
            -- 出站去重：按 client_id 唯一，便于平台 ack 反查
            CREATE UNIQUE INDEX IF NOT EXISTS idx_msg_v2_client
                ON messages_v2 (bot_id, client_id) WHERE client_id IS NOT NULL;
            -- 按用户+时间倒序拉取（history）
            CREATE INDEX IF NOT EXISTS idx_msg_v2_user_time
                ON messages_v2 (user_id, id);
            -- 未处理消息扫描（启动恢复用）
            CREATE INDEX IF NOT EXISTS idx_msg_v2_unprocessed
                ON messages_v2 (processed, id) WHERE processed = 0;
            -- 发送重试扫描
            CREATE INDEX IF NOT EXISTS idx_msg_v2_outbound_pending
                ON messages_v2 (send_state, id) WHERE direction = 'out' AND send_state IN ('pending','failed');",
        )
        .with_context(|| format!("无法初始化数据库 schema: {}", self.db_path.display()))?;
        // 迁移：media_remote 表添加 content_md5 列
        let _ = conn.execute(
            "ALTER TABLE media_remote ADD COLUMN content_md5 TEXT NOT NULL DEFAULT ''",
            [],
        );
        // 迁移：webdav_config 表添加 auto_migrate_on_save 列
        let _ = conn.execute(
            "ALTER TABLE webdav_config ADD COLUMN auto_migrate_on_save INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE media_meta ADD COLUMN local_present INTEGER NOT NULL DEFAULT 1",
            [],
        );
        // 首次启动自动迁移明文 → 加密态。注意：必须在释放 conn guard 后调用。
        Ok(())
    }

    /// 加密 DB 中明文敏感字段（由 Database::new 在 init_db 后调用）。
    fn run_migrate_plaintext_secrets(&self) {
        if let Err(e) = self.migrate_plaintext_secrets() {
            tracing::warn!(
                "[STORAGE] 明文→加密迁移失败 ({}): {}",
                self.db_path.display(),
                e
            );
            // 迁移失败不阻断启动——decrypt 仍能兼容明文，但会在日志持续告警
        }
    }

    /// 把 DB 中明文敏感字段迁移为 AES-256-GCM 加密态。
    /// 扫描范围（按本 DB schema 实际存放敏感数据的表）：
    ///   - user_tokens.bot_token        —— iLink API 凭证
    ///   - webdav_config.password —— WebDAV 密码（仅 system.db；无该表时会安全忽略）
    ///
    /// 幂等：只重写 `enc:` 前缀缺失的行。空字符串保持空字符串（不加密空值）。
    /// 失败模式：单行加密失败只跳过该行并 warn，不中断整批迁移。
    fn migrate_plaintext_secrets(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        let db_path = &self.db_path;

        // 1. user_tokens.bot_token
        let rows: Vec<(String, String, String)> = {
            let mut stmt =
                match conn.prepare("SELECT user_id, context_token, bot_token FROM user_tokens") {
                    Ok(s) => s,
                    Err(_) => return Ok(()), // 表不存在（极旧库）— init_db 已建表，理论上不会发生
                };
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            mapped.filter_map(|r| r.ok()).collect()
        };
        let mut migrated = 0usize;
        for (user_id, context_token, bot_token) in &rows {
            let need_enc_bot = !bot_token.is_empty() && !bot_token.starts_with("enc:");
            let need_enc_ctx = !context_token.is_empty() && !context_token.starts_with("enc:");
            if !need_enc_bot && !need_enc_ctx {
                continue;
            }
            let enc_bot = if need_enc_bot {
                match encrypt_secret(bot_token, db_path) {
                    Ok(enc) => enc,
                    Err(e) => {
                        tracing::warn!(
                            "[MIGRATE] user_tokens.bot_token 加密失败 (user={}): {}",
                            user_id,
                            e
                        );
                        continue;
                    }
                }
            } else {
                bot_token.clone()
            };
            let enc_ctx = if need_enc_ctx {
                match encrypt_secret(context_token, db_path) {
                    Ok(enc) => enc,
                    Err(e) => {
                        tracing::warn!(
                            "[MIGRATE] user_tokens.context_token 加密失败 (user={}): {}",
                            user_id,
                            e
                        );
                        continue;
                    }
                }
            } else {
                context_token.clone()
            };
            let now = chrono::Local::now().to_rfc3339();
            let _ = conn.execute(
                "UPDATE user_tokens SET context_token=?, bot_token=?, saved_at=? WHERE user_id=?",
                params![enc_ctx, enc_bot, now, user_id],
            );
            migrated += 1;
        }
        if migrated > 0 {
            tracing::info!("[MIGRATE] user_tokens.bot_token 已加密 {} 行", migrated);
        }

        // 2. webdav_config.password（仅 system.db 有此表，user.db 无）
        //   表不存在时 prepare 返回 Err，直接当作空集跳过。
        let webdav_rows: Vec<(i64, String)> =
            match conn.prepare("SELECT id, password FROM webdav_config") {
                Ok(mut stmt) => {
                    let mapped = stmt.query_map([], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    })?;
                    mapped.filter_map(|r| r.ok()).collect()
                }
                Err(_) => Vec::new(),
            };
        let mut migrated_webdav = 0usize;
        for (id, password) in &webdav_rows {
            if password.is_empty() || password.starts_with("enc:") {
                continue;
            }
            match encrypt_secret(password, db_path) {
                Ok(enc) => {
                    let _ = conn.execute(
                        "UPDATE webdav_config SET password=? WHERE id=?",
                        params![enc, id],
                    );
                    migrated_webdav += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        "[MIGRATE] webdav_config.password 加密失败 (id={}): {}",
                        id,
                        e
                    );
                }
            }
        }
        if migrated_webdav > 0 {
            tracing::info!(
                "[MIGRATE] webdav_config.password 已加密 {} 行",
                migrated_webdav
            );
        }
        Ok(())
    }

    // ── config 表 ─────────────────────────────────────────────

    pub fn save_config(&self, config: &serde_json::Value) {
        let conn = self.conn.lock();
        let raw = serde_json::to_string(config).unwrap_or_default();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO config(key, value) VALUES(?, ?)",
            params!["bot_config", raw],
        );
    }

    pub fn load_config(&self) -> Option<serde_json::Value> {
        let conn = self.conn.lock();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM config WHERE key=?",
                params!["bot_config"],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    // ── user_tokens 表 ────────────────────────────────────────

    pub fn save_user_token(&self, user_id: &str, context_token: &str, bot_token: &str) {
        let conn = self.conn.lock();
        let now = chrono::Local::now().to_rfc3339();
        // bot_token 加密后入库，避免 DB 泄露即丢全部 iLink 凭证。
        let enc_bot_token = match encrypt_secret(bot_token, &self.db_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    "[DB] save_user_token bot_token 加密失败 (user={}): {}",
                    user_id,
                    e
                );
                return;
            }
        };
        // context_token 同样加密存储
        let enc_ctx = match encrypt_secret(context_token, &self.db_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    "[DB] save_user_token context_token 加密失败 (user={}): {}",
                    user_id,
                    e
                );
                return;
            }
        };
        let _ = conn.execute(
            "INSERT OR REPLACE INTO user_tokens(user_id, context_token, bot_token, saved_at) VALUES(?, ?, ?, ?)",
            params![user_id, enc_ctx, enc_bot_token, now],
        );
    }

    pub fn list_user_tokens(&self) -> HashMap<String, (String, String)> {
        let conn = self.conn.lock();
        let mut stmt =
            match conn.prepare("SELECT user_id, context_token, bot_token FROM user_tokens") {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("[DB] list_user_tokens prepare 失败: {}", e);
                    return HashMap::new();
                }
            };
        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[DB] list_user_tokens query_map 失败: {}", e);
                return HashMap::new();
            }
        };
        let mut result = HashMap::new();
        for row in rows.flatten() {
            let bot_token = match decrypt_secret(&row.2, &self.db_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        "[DB] list_user_tokens bot_token 解密失败 (user={}): {}",
                        row.0,
                        e
                    );
                    String::new()
                }
            };
            let ctx_token = match decrypt_secret(&row.1, &self.db_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        "[DB] list_user_tokens context_token 解密失败 (user={}): {}",
                        row.0,
                        e
                    );
                    String::new()
                }
            };
            result.insert(row.0, (ctx_token, bot_token));
        }
        result
    }

    pub fn delete_user_token(&self, user_id: &str) {
        let conn = self.conn.lock();
        let _ = conn.execute("DELETE FROM user_tokens WHERE user_id=?", params![user_id]);
    }

    // ── messages 表 ───────────────────────────────────────────

    pub fn save_user_messages(
        &self,
        user_id: &str,
        messages: &[serde_json::Value],
        max_per_user: usize,
    ) -> Option<i64> {
        // 用 rusqlite transaction() API 替换手动 BEGIN/COMMIT，确保异常时自动回滚。
        //   BEGIN/COMMIT/ROLLBACK。原手动写法在 panic 时连接会留在开放事务中
        //   （下次使用该连接的代码会报 "cannot start a transaction within a transaction"），
        //   transaction() 的 Drop 实现会自动 ROLLBACK 未提交事务，安全且幂等。
        let mut conn = self.conn.lock();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!("[save_user_messages] 开启事务失败: {}", e);
                return None;
            }
        };
        let result: rusqlite::Result<i64> = (|| {
            let now = chrono::Local::now().to_rfc3339();
            let start = if messages.len() > max_per_user {
                messages.len() - max_per_user
            } else {
                0
            };
            let incoming = &messages[start..];

            // Check current count in DB
            let existing_count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE user_id=?",
                    params![user_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let incoming_count = incoming.len() as i64;

            if incoming_count == 0 {
                // No messages to save
                return Ok(0);
            }

            if incoming_count == existing_count {
                // Same count - check if last message matches (no new messages)
                let last_time = incoming
                    .last()
                    .and_then(|m| {
                        m.get("time")
                            .and_then(|v| v.as_str())
                            .or_else(|| m.get("create_time").and_then(|v| v.as_str()))
                    })
                    .unwrap_or("");
                let db_last_time: String = tx
                    .query_row(
                        "SELECT time FROM messages WHERE user_id=? ORDER BY id DESC LIMIT 1",
                        params![user_id],
                        |row| row.get(0),
                    )
                    .unwrap_or_default();
                if !last_time.is_empty() && last_time == db_last_time {
                    // No change, return current last id
                    let last_id: i64 = tx
                        .query_row(
                            "SELECT MAX(id) FROM messages WHERE user_id=?",
                            params![user_id],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);
                    return Ok(last_id);
                }
            }

            if incoming_count > existing_count && existing_count > 0 {
                // New messages appended - only insert the delta
                let delta_start = (incoming_count - existing_count) as usize;
                // Verify the boundary matches (the last existing message should match incoming[delta_start-1])
                let boundary_time = incoming
                    .get(delta_start.saturating_sub(1))
                    .and_then(|m| {
                        m.get("time")
                            .and_then(|v| v.as_str())
                            .or_else(|| m.get("create_time").and_then(|v| v.as_str()))
                    })
                    .unwrap_or("");
                let db_last_time: String = tx
                    .query_row(
                        "SELECT time FROM messages WHERE user_id=? ORDER BY id DESC LIMIT 1",
                        params![user_id],
                        |row| row.get(0),
                    )
                    .unwrap_or_default();

                if !boundary_time.is_empty() && boundary_time == db_last_time {
                    // Boundary matches - incremental insert only new messages
                    let mut last_id: i64 = tx
                        .query_row(
                            "SELECT MAX(id) FROM messages WHERE user_id=?",
                            params![user_id],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);
                    for msg in &incoming[delta_start..] {
                        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        let msg_text = msg
                            .get("text")
                            .and_then(|v| v.as_str())
                            .or_else(|| msg.get("content").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        let msg_time = msg
                            .get("time")
                            .and_then(|v| v.as_str())
                            .or_else(|| msg.get("create_time").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        let json_str = serde_json::to_string(msg).unwrap_or_default();
                        tx.execute(
                            "INSERT INTO messages(user_id, msg_type, msg_text, json_data, time, created_at) VALUES(?, ?, ?, ?, ?, ?)",
                            params![user_id, msg_type, msg_text, json_str, msg_time, now],
                        )?;
                        last_id = tx.last_insert_rowid();
                    }
                    return Ok(last_id);
                }
            }

            // 逐条 DELETE + INSERT 替代全量重写，避免并发读写竞态。
            //   原实现先 `DELETE FROM messages WHERE user_id=?` 再 INSERT 全部 incoming，
            //   崩溃时消息永久丢失；并发轮询期间 SELECT 拿到空集，客户端消息被丢弃后
            //   又被前端去重逻辑屏蔽（用户看到自己发送的消息消失）。
            //   新策略：纯 INSERT + 按 (time, msg_text) 去重 + 单独裁剪超额旧消息，
            //   全程不出现 "DELETE 全部" 窗口，SELECT 永远能看到完整消息集。
            //
            //   注意：旧 messages 表无 UNIQUE 约束，去重靠 SELECT 预加载现有
            //   (time, msg_text) 集合实现；与 messages_v2 表的 (bot_id, message_id)
            //   UNIQUE 幂等去重互不冲突——V2 是权威存储，本表是按用户裁剪的缓存视图。
            let mut existing_keys: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            {
                let mut stmt = tx.prepare("SELECT time, msg_text FROM messages WHERE user_id=?")?;
                let rows = stmt.query_map(params![user_id], |row| {
                    Ok((
                        row.get::<_, String>(0).unwrap_or_default(),
                        row.get::<_, String>(1).unwrap_or_default(),
                    ))
                })?;
                for (t, txt) in rows.flatten() {
                    existing_keys.insert((t, txt));
                }
            }
            let mut last_id: i64 = tx
                .query_row(
                    "SELECT MAX(id) FROM messages WHERE user_id=?",
                    params![user_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            for msg in incoming {
                let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let msg_text = msg
                    .get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| msg.get("content").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let msg_time = msg
                    .get("time")
                    .and_then(|v| v.as_str())
                    .or_else(|| msg.get("create_time").and_then(|v| v.as_str()))
                    .unwrap_or("");
                // 跳过已存在的消息（按 time+text 去重，避免在缓存表里产生重复行）
                if existing_keys.contains(&(msg_time.to_string(), msg_text.to_string())) {
                    continue;
                }
                let json_str = serde_json::to_string(msg).unwrap_or_default();
                tx.execute(
                    "INSERT INTO messages(user_id, msg_type, msg_text, json_data, time, created_at) VALUES(?, ?, ?, ?, ?, ?)",
                    params![user_id, msg_type, msg_text, json_str, msg_time, now],
                )?;
                last_id = tx.last_insert_rowid();
                existing_keys.insert((msg_time.to_string(), msg_text.to_string()));
            }
            // 裁剪超额：保留最新 max_per_user 条，单独 DELETE 旧消息
            //   （只在 INSERT 之后执行，期间 SELECT 仍能看到所有消息，不会出现空集窗口）
            let total: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE user_id=?",
                    params![user_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if total > max_per_user as i64 {
                let excess = total - max_per_user as i64;
                tx.execute(
                    "DELETE FROM messages WHERE id IN (\
                       SELECT id FROM messages WHERE user_id=? ORDER BY id ASC LIMIT ?\
                     )",
                    params![user_id, excess],
                )?;
            }
            Ok(last_id)
        })();
        match result {
            Ok(id) => match tx.commit() {
                Ok(()) => Some(id),
                Err(e) => {
                    tracing::warn!("[save_user_messages] commit 失败: {}", e);
                    None
                }
            },
            Err(e) => {
                // tx Drop 自动 ROLLBACK
                tracing::warn!("[save_user_messages] 事务内失败，已回滚: {}", e);
                None
            }
        }
    }

    pub fn load_user_messages(
        &self,
        user_id: &str,
        limit: Option<usize>,
    ) -> Vec<serde_json::Value> {
        let conn = self.conn.lock();
        // 加 limit 参数避免全量加载。
        let sql = match limit {
            Some(_) => "SELECT json_data FROM messages WHERE user_id=? ORDER BY id ASC LIMIT ?",
            None => "SELECT json_data FROM messages WHERE user_id=? ORDER BY id ASC",
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[DB] load_user_messages prepare 失败: {}", e);
                return Vec::new();
            }
        };
        // 统一构造 params 再调用一次 query_map，避免 match 两支产生不同 closure 类型
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = match limit {
            Some(n) => vec![Box::new(user_id.to_string()), Box::new(n as i64)],
            None => vec![Box::new(user_id.to_string())],
        };
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = match stmt.query_map(param_refs.as_slice(), |row| row.get::<_, String>(0)) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[DB] load_user_messages query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect()
    }

    pub fn load_all_messages(&self) -> Vec<serde_json::Value> {
        // ponytail: 调用方（bot.rs::load_all_messages）未传 limit，保持全量加载。
        //           如需限制，请改用 load_all_messages_with_limit(Some(n))。
        //           bot.rs 改造超出本次可修改文件范围（仅 storage/crypto/storage_backend/media）。
        self.load_all_messages_with_limit(None)
    }

    /// 带 limit 的全量消息加载。limit=None 时不加 LIMIT（保持旧行为）。
    pub fn load_all_messages_with_limit(&self, limit: Option<usize>) -> Vec<serde_json::Value> {
        let conn = self.conn.lock();
        let sql = match limit {
            Some(_) => "SELECT user_id, json_data, id FROM messages ORDER BY id ASC LIMIT ?",
            None => "SELECT user_id, json_data, id FROM messages ORDER BY id ASC",
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[DB] load_all_messages prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = match limit {
            Some(n) => vec![Box::new(n as i64)],
            None => vec![],
        };
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = match stmt.query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[DB] load_all_messages query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok())
            .filter_map(|(user_id, json_str, id)| {
                let mut msg: serde_json::Value = serde_json::from_str(&json_str).ok()?;
                let obj = msg.as_object_mut()?;
                if !obj.contains_key("id") {
                    obj.insert("id".to_string(), serde_json::Value::Number(id.into()));
                }
                if !obj.contains_key("from") {
                    obj.insert("from".to_string(), serde_json::Value::String(user_id));
                }
                if !obj.contains_key("to") {
                    obj.insert("to".to_string(), serde_json::Value::String("".into()));
                }
                Some(msg)
            })
            .collect()
    }

    pub fn delete_user_messages(&self, user_id: &str) {
        let conn = self.conn.lock();
        let _ = conn.execute("DELETE FROM messages WHERE user_id=?", params![user_id]);
    }

    /// 新增 `user_id` 参数，SQL 加 `AND user_id = ?` 过滤，杜绝跨会话删除。
    ///   防止调用方误传 / 故意传入其他 peer 的消息 ID 导致跨会话删除。
    ///   即使 IDOR 漏洞让攻击者拿到其他 peer 的消息 ID，SQL 过滤也会阻断删除。
    pub fn delete_messages_by_ids(&self, ids: &[i64], user_id: &str) -> usize {
        if ids.is_empty() || user_id.is_empty() {
            return 0;
        }
        let conn = self.conn.lock();
        let placeholders: Vec<String> = (0..ids.len()).map(|_| "?".to_string()).collect();
        // P1-8: 加 AND user_id = ? 过滤，确保只删除指定 peer 的消息
        let sql = format!(
            "DELETE FROM messages WHERE id IN ({}) AND user_id = ?",
            placeholders.join(",")
        );
        // params: ids... + user_id
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = ids
            .iter()
            .map(|i| Box::new(*i) as Box<dyn rusqlite::ToSql>)
            .collect();
        params_vec.push(Box::new(user_id.to_string()));
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, params_refs.as_slice()).unwrap_or(0) as usize
    }

    /// 按条件查询消息（替代内存克隆过滤）
    pub fn query_messages(
        &self,
        since: Option<i64>,
        user: Option<&str>,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        // 封顶 1000，避免 usize::MAX 转 i64 为 -1 被 SQLite 解释为无限制致 OOM
        let limit = limit.min(1000);
        let conn = self.conn.lock();
        let sql = match (since, user) {
            (Some(_), Some(_)) => "SELECT json_data, id FROM messages WHERE id > ?1 AND user_id = ?2 ORDER BY id ASC LIMIT ?3",
            (Some(_), None) => "SELECT json_data, id FROM messages WHERE id > ?1 ORDER BY id ASC LIMIT ?3",
            (None, Some(_)) => "SELECT json_data, id FROM messages WHERE user_id = ?2 ORDER BY id ASC LIMIT ?3",
            (None, None) => "SELECT json_data, id FROM messages ORDER BY id ASC LIMIT ?3",
        };
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = match (since, user) {
            (Some(s), Some(u)) => {
                vec![Box::new(s), Box::new(u.to_string()), Box::new(limit as i64)]
            }
            (Some(s), None) => vec![Box::new(s), Box::new(limit as i64)],
            (None, Some(u)) => vec![Box::new(u.to_string()), Box::new(limit as i64)],
            (None, None) => vec![Box::new(limit as i64)],
        };
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(|r| r.ok())
            .filter_map(|(json_str, id)| Self::parse_msg_with_id(json_str, id))
            .collect()
    }

    /// 查询指定用户的历史消息。
    /// 加 before 参数支持游标分页（id < before 取最新 N 条）。
    ///   - before=None：返回最新 N 条（按 id DESC LIMIT N，再反转为 ASC）
    ///   - before=Some(id)：返回 id < before 的最新 N 条
    ///
    /// 原实现 ORDER BY id ASC LIMIT N 取的是最旧 N 条，与“历史加载最新”语义不符。
    /// 返回顺序始终为 ASC（旧→新），前端 appendChild 渲染后新消息在底部。
    pub fn query_history_messages(
        &self,
        user: Option<&str>,
        limit: usize,
        before: Option<i64>,
    ) -> Vec<serde_json::Value> {
        let limit = limit.clamp(1, 1000); // 同 query_messages
        let conn = self.conn.lock();
        // 四种情况组合（user × before），均按 id DESC LIMIT N 取最新 N 条
        let sql = match (user.is_some(), before.is_some()) {
            (true, true)   => "SELECT json_data, id FROM messages WHERE user_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
            (true, false)  => "SELECT json_data, id FROM messages WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2",
            (false, true)  => "SELECT json_data, id FROM messages WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
            (false, false) => "SELECT json_data, id FROM messages ORDER BY id DESC LIMIT ?1",
        };
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = match (user, before) {
            (Some(uid), Some(b)) => vec![
                Box::new(uid.to_string()),
                Box::new(b),
                Box::new(limit as i64),
            ],
            (Some(uid), None) => vec![Box::new(uid.to_string()), Box::new(limit as i64)],
            (None, Some(b)) => vec![Box::new(b), Box::new(limit as i64)],
            (None, None) => vec![Box::new(limit as i64)],
        };
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut result: Vec<serde_json::Value> = rows
            .filter_map(|r| r.ok())
            .filter_map(|(json_str, id)| Self::parse_msg_with_id(json_str, id))
            .collect();
        // DESC 取出后反转为 ASC（旧→新），保持原有契约
        result.reverse();
        result
    }

    /// 查询每个用户的最新一条消息（用于 chat_previews）
    /// S54: 当 user_ids 数量超过 900 时分批查询,避免超过 SQLite 默认 999 参数限制
    pub fn query_latest_message_per_user(
        &self,
        user_ids: &[String],
    ) -> HashMap<String, serde_json::Value> {
        if user_ids.is_empty() {
            return HashMap::new();
        }
        let conn = self.conn.lock();
        let mut result: HashMap<String, serde_json::Value> = HashMap::new();
        // S54: chunks(900) 留余量,SQLite 默认参数上限 999
        for chunk in user_ids.chunks(900) {
            if chunk.is_empty() {
                continue;
            }
            // 单条 SQL：子查询取每个 user_id 的 MAX(id) 对应行，避免 N+1
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT m.user_id, m.json_data, m.id FROM messages m \
                 INNER JOIN (SELECT user_id, MAX(id) AS max_id FROM messages WHERE user_id IN ({}) GROUP BY user_id) t \
                 ON m.id = t.max_id",
                placeholders
            );
            let params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|u| u as &dyn rusqlite::types::ToSql)
                .collect();
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("[DB] query_latest_message_per_user prepare 失败: {}", e);
                    continue;
                }
            };
            let rows = match stmt.query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            }) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for r in rows.filter_map(|r| r.ok()) {
                if let Some(msg) = Self::parse_msg_with_id(r.1, r.2) {
                    result.insert(r.0, msg);
                }
            }
        }
        result
    }

    fn parse_msg_with_id(json_str: String, id: i64) -> Option<serde_json::Value> {
        let mut msg: serde_json::Value = serde_json::from_str(&json_str).ok()?;
        if let Some(obj) = msg.as_object_mut() {
            // S78: 仅对入站消息注入 messages 表自增 id;
            // 出站消息(type='out')保留原 JSON 中的 id 或留空,
            // 避免覆盖前端按 client_id 关联的逻辑
            let is_outgoing = obj.get("type").and_then(|v| v.as_str()) == Some("out");
            if !is_outgoing
                && (!obj.contains_key("id") || obj.get("id").and_then(|v| v.as_i64()) == Some(0))
            {
                obj.insert(
                    "id".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(id)),
                );
            }
        }
        Some(msg)
    }

    pub fn export_user_messages_html(&self, user_id: &str, nickname: &str) -> String {
        // 限制导出最近 5000 条，避免单用户消息过多时 OOM。
        let msgs = self.load_user_messages(user_id, Some(5000));
        let display_name = html_escape::encode_text(nickname.or_if_empty(user_id));
        let mut parts = vec![
            "<!DOCTYPE html><html lang='zh-CN'><head><meta charset='utf-8'>".to_string(),
            format!("<title>聊天记录 - {}</title>", display_name),
            "<meta name='viewport' content='width=device-width, initial-scale=1.0'>".to_string(),
            "<style>".to_string(),
            ":root { --bg: #F2F2F7; --nav-bg: rgba(249,249,249,0.55); --bubble-in: rgba(255,255,255,0.7); --bubble-out: #34C759; --text-primary: #1C1C1E; --text-secondary: #8E8E93; --text-hint: #C7C7CC; --divider: rgba(60,60,67,0.12); --glass-border: rgba(255,255,255,0.35); --accent-light: rgba(52,199,89,0.1); }".to_string(),
            "@media (prefers-color-scheme: dark) { :root { --bg: #000000; --nav-bg: rgba(28,28,30,0.55); --bubble-in: rgba(44,44,46,0.7); --bubble-out: #30D158; --text-primary: #F5F5F7; --text-secondary: #8E8E93; --text-hint: #636366; --divider: rgba(84,84,88,0.36); --glass-border: rgba(255,255,255,0.08); --accent-light: rgba(48,209,88,0.15); } }".to_string(),
            "* { margin: 0; padding: 0; box-sizing: border-box; }".to_string(),
            "body { font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'SF Pro Text', 'Helvetica Neue', Arial, sans-serif; max-width: 720px; margin: 0 auto; background: var(--bg); color: var(--text-primary); line-height: 1.5; transition: background 0.3s, color 0.3s; }".to_string(),
            ".header { position: sticky; top: 8px; z-index: 10; background: var(--nav-bg); backdrop-filter: blur(20px); -webkit-backdrop-filter: blur(20px); padding: 14px 20px; margin: 8px 16px 12px; border-radius: 18px; box-shadow: 0 2px 16px rgba(0,0,0,0.06), inset 0 0 0 0.5px var(--glass-border); text-align: center; }".to_string(),
            ".header h1 { font-size: 17px; font-weight: 600; margin-bottom: 4px; }".to_string(),
            ".header p { font-size: 13px; color: var(--text-secondary); }".to_string(),
            ".messages { padding: 0 16px 24px; display: flex; flex-direction: column; gap: 6px; }".to_string(),
            ".date-divider { text-align: center; margin: 16px 0 8px; font-size: 11px; color: var(--text-hint); font-weight: 500; letter-spacing: 0.5px; position: relative; }".to_string(),
            ".date-divider::before, .date-divider::after { content: ''; position: absolute; top: 50%; width: calc(50% - 50px); height: 0.5px; background: var(--divider); }".to_string(),
            ".date-divider::before { left: 0; } .date-divider::after { right: 0; }".to_string(),
            ".msg { display: flex; align-items: flex-end; gap: 8px; max-width: 80%; animation: bubbleIn 0.3s cubic-bezier(0.16,1,0.3,1); }".to_string(),
            "@keyframes bubbleIn { from { transform: translateY(8px); opacity: 0; } to { transform: translateY(0); opacity: 1; } }".to_string(),
            ".msg-out { margin-left: auto; flex-direction: row-reverse; } .msg-in { margin-right: auto; }".to_string(),
            ".bubble { padding: 10px 14px; border-radius: 20px; font-size: 15px; line-height: 1.45; word-break: break-word; position: relative; }".to_string(),
            ".msg-in .bubble { background: var(--bubble-in); backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px); border-bottom-left-radius: 6px; box-shadow: 0 1px 4px rgba(0,0,0,0.06), inset 0 0 0 0.5px var(--glass-border); }".to_string(),
            ".msg-out .bubble { background: var(--bubble-out); color: #fff; border-bottom-right-radius: 6px; box-shadow: 0 1px 6px rgba(52,199,89,0.2); }".to_string(),
            ".msg-time { font-size: 11px; margin-top: 4px; text-align: right; }".to_string(),
            ".msg-in .msg-time { color: var(--text-hint); } .msg-out .msg-time { color: rgba(255,255,255,0.6); }".to_string(),
            ".msg-text { white-space: pre-wrap; word-break: break-word; }".to_string(),
            ".footer { text-align: center; padding: 20px 0 40px; font-size: 12px; color: var(--text-hint); }".to_string(),
            "@media (max-width: 480px) { .header { margin: 8px 12px 8px; padding: 12px 16px; } .messages { padding: 0 12px 16px; } .bubble { font-size: 14px; padding: 8px 12px; } .msg { max-width: 85%; } }".to_string(),
            "</style></head><body>".to_string(),
            format!("<div class='header'><h1>聊天记录</h1><p>{} · 共 {} 条消息</p></div>", display_name, msgs.len()),
            "<div class='messages'>".to_string(),
        ];
        let mut last_date = String::new();
        for m in &msgs {
            let msg_type = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let msg_text = html_escape::encode_text(
                m.get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.get("content").and_then(|v| v.as_str()))
                    .unwrap_or("(非文本消息)"),
            );
            let msg_time =
                html_escape::encode_text(m.get("time").and_then(|v| v.as_str()).unwrap_or(""));
            let time_str = msg_time.to_string();
            // 字符安全截取前 10 个 Unicode 字符作为日期部分，避免字节索引 panic
            let date_part: String = time_str.chars().take(10).collect();
            if !date_part.is_empty() && date_part != last_date {
                last_date = date_part.clone();
                parts.push(format!(
                    "<div class='date-divider'>{}</div>",
                    html_escape::encode_text(&date_part)
                ));
            }
            let cls = if msg_type == "out" {
                "msg-out"
            } else {
                "msg-in"
            };
            parts.push(format!(
                "<div class='msg {}'><div class='bubble'><div class='msg-text'>{}</div>",
                cls, msg_text
            ));
            if !time_str.is_empty() {
                parts.push(format!("<div class='msg-time'>{}</div>", msg_time));
            }
            parts.push("</div></div>".to_string());
        }
        parts.push("</div>".to_string());
        parts.push("<div class='footer'>导出自 Zyn iLink ChatBox</div>".to_string());
        parts.push("</body></html>".to_string());
        parts.join("\n")
    }

    // ── media_meta 表 ─────────────────────────────────────────

    pub fn save_media_meta(
        &self,
        cache_key: &str,
        mime: &str,
        filename: &str,
        size: i64,
        scope: &str,
    ) {
        let conn = self.conn.lock();
        let now = chrono::Local::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO media_meta(cache_key, scope, mime, filename, size, local_present, created_at)
             VALUES(?, ?, ?, ?, ?, 1, ?)
             ON CONFLICT(cache_key) DO UPDATE SET
                scope=excluded.scope,
                mime=excluded.mime,
                filename=excluded.filename,
                size=excluded.size,
                local_present=1",
            params![cache_key, scope, mime, filename, size, now],
        );
    }

    pub fn list_media_meta_all(&self) -> Vec<MediaMeta> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare("SELECT cache_key, mime, filename, size FROM media_meta")
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[DB] list_media_meta_all prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], |row| {
            Ok(MediaMeta {
                cache_key: row.get::<_, String>(0).unwrap_or_default(),
                mime: row.get::<_, String>(1).unwrap_or_default(),
                filename: row.get::<_, String>(2).unwrap_or_default(),
                size: row.get::<_, i64>(3).unwrap_or(0),
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[DB] list_media_meta_all query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 判断当前用户数据库是否持久化拥有该媒体。
    ///
    /// 同时查询本地元数据和远程元数据，以兼容历史版本曾在迁移后仅保留
    /// `media_remote` 的数据。调用方必须先选定当前认证用户的 user.db。
    pub fn owns_media(&self, cache_key: &str) -> bool {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM media_meta WHERE cache_key=?
                UNION ALL
                SELECT 1 FROM media_remote WHERE cache_key=?
             )",
            params![cache_key, cache_key],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .unwrap_or(false)
    }

    // ── 媒体缓存 LRU 上限 ──────────────────────────

    /// 返回当前用户仍在本地的媒体总字节数（用于 LRU 阈值判断）。
    pub fn media_cache_total_size(&self) -> i64 {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM media_meta WHERE local_present=1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0)
    }

    /// 列出所有 media_meta 条目，按 created_at 升序（最老的最先被 LRU 删除）。
    ///   返回 (cache_key, size, created_at)。
    pub fn list_media_meta_lru(&self) -> Vec<(String, i64, String)> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT cache_key, size, created_at
             FROM media_meta
             WHERE local_present=1
             ORDER BY created_at ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[DB] list_media_meta_lru prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, i64>(1).unwrap_or(0),
                row.get::<_, String>(2).unwrap_or_default(),
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[DB] list_media_meta_lru query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 标记本地副本已删除，同时保留媒体归属和远程定位信息。
    pub fn mark_media_local_absent(&self, cache_key: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE media_meta SET local_present=0 WHERE cache_key=?",
            params![cache_key],
        )?;
        Ok(())
    }

    /// 删除媒体的全部持久化记录，避免留下孤儿 `media_remote` 行。
    /// 返回被删除媒体的字节数；不存在时返回 0。
    pub fn remove_media_records(&self, cache_key: &str) -> anyhow::Result<i64> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let size = tx
            .query_row(
                "SELECT size FROM media_meta WHERE cache_key=?",
                params![cache_key],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        tx.execute(
            "DELETE FROM media_remote WHERE cache_key=?",
            params![cache_key],
        )?;
        tx.execute(
            "DELETE FROM media_meta WHERE cache_key=?",
            params![cache_key],
        )?;
        tx.commit()?;
        Ok(size.max(0))
    }

    /// 当前逻辑媒体用量（本地与远程均计一次）。
    pub fn media_usage(&self) -> (i64, i64) {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT COALESCE(SUM(size), 0), COUNT(*) FROM media_meta",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((0, 0))
    }

    // ── webdav_config 表 ──────────────────────────────────────

    pub fn save_webdav_config(&self, config: &WebDavConfig) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        // 加密失败时直接返回 Err，不写明文落库（保留 DB 中旧配置不动）。
        let stored_pwd = encrypt_secret(&config.password, &self.db_path)?;
        let now = chrono::Local::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO webdav_config(id, enabled, url, username, password, base_path, traffic_saver, auto_migrate_on_save, updated_at) VALUES(1, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                config.enabled as i64,
                config.url,
                config.username,
                stored_pwd,
                if config.base_path.is_empty() { "/ilink-media" } else { &config.base_path },
                config.traffic_saver as i64,
                config.auto_migrate_on_save as i64,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn load_webdav_config(&self) -> Option<WebDavConfig> {
        let conn = self.conn.lock();
        let db_path = self.db_path.clone();
        // decrypt_secret 失败时返回 None，不再把密文当明文填入 password。
        type WebDavConfigRow = (i64, String, String, String, String, i64, i64, String);
        let row_res: rusqlite::Result<WebDavConfigRow> = conn.query_row(
            "SELECT enabled, url, username, password, base_path, traffic_saver, auto_migrate_on_save, updated_at FROM webdav_config WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0).unwrap_or(0),
                    row.get::<_, String>(1).unwrap_or_default(),
                    row.get::<_, String>(2).unwrap_or_default(),
                    row.get::<_, String>(3).unwrap_or_default(),
                    row.get::<_, String>(4).unwrap_or_else(|_| "/ilink-media".into()),
                    row.get::<_, i64>(5).unwrap_or(0),
                    row.get::<_, i64>(6).unwrap_or(0),
                    row.get::<_, String>(7).unwrap_or_default(),
                ))
            },
        );
        let (
            enabled,
            url,
            username,
            pwd_stored,
            base_path,
            traffic_saver,
            auto_migrate,
            updated_at,
        ) = match row_res {
            Ok(v) => v,
            Err(_) => return None,
        };
        // 解密失败 → 返回 None（视为无有效配置），不回退明文
        let password = match decrypt_secret(&pwd_stored, &db_path) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("[STORAGE] WebDAV 密码解密失败，忽略该配置: {}", e);
                return None;
            }
        };
        Some(WebDavConfig {
            enabled: enabled != 0,
            url,
            username,
            password,
            base_path,
            traffic_saver: traffic_saver != 0,
            auto_migrate_on_save: auto_migrate != 0,
            updated_at,
        })
    }

    pub fn update_webdav_traffic_saver(&self, traffic_saver: bool) {
        let conn = self.conn.lock();
        let now = chrono::Local::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE webdav_config SET traffic_saver=?, updated_at=? WHERE id=1",
            params![traffic_saver as i64, now],
        );
    }

    // ── media_remote 表 ───────────────────────────────────────

    pub fn save_media_remote(
        &self,
        cache_key: &str,
        remote_path: &str,
        user_id: &str,
        content_md5: &str,
    ) {
        let conn = self.conn.lock();
        let now = chrono::Local::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO media_remote(cache_key, remote_path, uploaded_at, user_id, content_md5) VALUES(?, ?, ?, ?, ?)",
            params![cache_key, remote_path, now, user_id, content_md5],
        );
    }

    pub fn get_media_remote(&self, cache_key: &str) -> Option<MediaRemote> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT remote_path, uploaded_at, user_id, content_md5 FROM media_remote WHERE cache_key=?",
            params![cache_key],
            |row| {
                Ok(MediaRemote {
                    remote_path: row.get(0)?,
                    uploaded_at: row.get(1)?,
                    user_id: row.get::<_, String>(2).unwrap_or_default(),
                    content_md5: row.get::<_, String>(3).unwrap_or_default(),
                })
            },
        )
        .ok()
    }

    // ── messages_v2 表 (PR1 - 持久化优先 + 去重 + 状态机) ──────────────
    //
    // 设计要点（参考 openilink-hub-main 的 store.MessageStore）：
    //   - upsert_message: 用 (bot_id, message_id) UNIQUE 去重，返回 SaveResult{inserted}
    //   - mark_processed: 投递完成才标 processed=1
    //   - get_unprocessed_messages: 启动恢复用
    //   - insert_outbound: 发送前先入库（client_id 唯一）
    //   - update_outbound_state: ack / 失败回调推进 send_state
    //   - get_outbound_by_client_id: iLink 平台 ack 反查

    /// 入站消息 upsert。
    /// 通过 (bot_id, message_id) UNIQUE 去重；message_id 为 None 时仅做普通插入。
    /// 返回 SaveResult { id, inserted } — inserted=false 表示命中重复消息，调用方应丢弃推送。
    ///
    /// 入库前将明文 bot_token 转为 SHA-256 hex，不可逆存储；token 原文由 user_tokens 表加密保存。
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_inbound_message(
        &self,
        trace_id: &str,
        bot_id: &str,
        message_id: Option<i64>,
        user_id: &str,
        from_user_id: &str,
        to_user_id: &str,
        context_token: &str,
        item_list_json: &str,
        text: &str,
        raw_json: &str,
        created_at_ms: i64,
    ) -> SaveResult {
        let conn = self.conn.lock();
        let bot_id_hash = crypto::sha256_hex(bot_id.as_bytes());

        // 先查重
        if let Some(mid) = message_id {
            let existing: Option<i64> = conn
                .query_row(
                    "SELECT id FROM messages_v2 WHERE bot_id=? AND message_id=?",
                    params![bot_id_hash, mid],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten();
            if let Some(id) = existing {
                return SaveResult {
                    id,
                    inserted: false,
                };
            }
        }

        // 插入（带 message_id 时由 UNIQUE 兜底防并发竞争）
        let res = conn.execute(
            "INSERT OR IGNORE INTO messages_v2 (
                trace_id, bot_id, user_id, direction, message_id,
                from_user_id, to_user_id, context_token, item_list_json,
                text, raw_json, created_at_ms, updated_at_ms
             ) VALUES (?, ?, ?, 'in', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                trace_id,
                bot_id_hash,
                user_id,
                message_id,
                from_user_id,
                to_user_id,
                context_token,
                item_list_json,
                text,
                raw_json,
                created_at_ms,
                created_at_ms,
            ],
        );

        if res.is_err() {
            return SaveResult {
                id: 0,
                inserted: false,
            };
        }

        let id = conn.last_insert_rowid();
        if id == 0 {
            // 兜底：INSERT OR IGNORE 命中了唯一索引但 SELECT 没查到（极小概率）
            if let Some(mid) = message_id {
                if let Ok(found) = conn.query_row(
                    "SELECT id FROM messages_v2 WHERE bot_id=? AND message_id=?",
                    params![bot_id_hash, mid],
                    |row| row.get::<_, i64>(0),
                ) {
                    return SaveResult {
                        id: found,
                        inserted: false,
                    };
                }
            }
        }

        SaveResult { id, inserted: true }
    }

    /// 标记消息已处理（投递完成）
    pub fn mark_processed(&self, id: i64) {
        if id <= 0 {
            return;
        }
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let _ = conn.execute(
            "UPDATE messages_v2 SET processed=1, updated_at_ms=? WHERE id=?",
            params![now_ms, id],
        );
    }

    /// 启动时恢复未处理消息（参考 hub 的 recoverUnprocessed）
    pub fn get_unprocessed_messages(&self, limit: usize) -> Vec<MessageRow> {
        let limit = limit.min(1000) as i64;
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, trace_id, bot_id, user_id, direction, message_id, client_id,
                    from_user_id, to_user_id, context_token, item_list_json, text,
                    media_status, send_state, send_attempts, created_at_ms
             FROM messages_v2 WHERE processed=0 ORDER BY id ASC LIMIT ?",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(params![limit], |row| {
            Ok(MessageRow {
                id: row.get(0)?,
                trace_id: row.get(1)?,
                bot_id: row.get(2)?,
                user_id: row.get(3)?,
                direction: row.get(4)?,
                message_id: row.get(5)?,
                client_id: row.get(6)?,
                from_user_id: row.get(7)?,
                to_user_id: row.get(8)?,
                context_token: row.get(9)?,
                item_list_json: row.get(10)?,
                text: row.get(11)?,
                media_status: row.get(12)?,
                send_state: row.get(13)?,
                send_attempts: row.get(14)?,
                created_at_ms: row.get(15)?,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 更新出站消息状态。
    /// state: 'sending' | 'sent' | 'delivered' | 'failed' | 'expired'
    pub fn update_outbound_state(&self, id: i64, state: &str, error_msg: Option<&str>) {
        if id <= 0 {
            return;
        }
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        // S21c: 成功路径(state='sent'/'delivered')不递增 send_attempts,
        // 仅失败/中间状态(sending/failed/expired)递增
        let _ = conn.execute(
            "UPDATE messages_v2
             SET send_state=?, send_last_error=COALESCE(?, send_last_error),
                 send_attempts=CASE WHEN ? IN ('sent','delivered') THEN send_attempts ELSE send_attempts+1 END,
                 updated_at_ms=?
             WHERE id=?",
            params![state, error_msg, state, now_ms, id],
        );
    }

    /// 批量查询出站消息的最新 send_state（按 client_id 关联）。
    /// 用于 api_history 读取时合并 messages_v2 的最新状态到 messages 表的旧记录
    /// 返回 HashMap<client_id, send_state>
    pub fn get_outbound_states_by_client_ids(
        &self,
        client_ids: &[String],
    ) -> HashMap<String, String> {
        if client_ids.is_empty() {
            return HashMap::new();
        }
        let conn = self.conn.lock();
        let placeholders = client_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT client_id, send_state FROM messages_v2 WHERE client_id IN ({})",
            placeholders
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("[DB] get_outbound_states_by_client_ids prepare 失败: {}", e);
                return HashMap::new();
            }
        };
        let params: Vec<&dyn rusqlite::types::ToSql> = client_ids
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = match stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "[DB] get_outbound_states_by_client_ids query_map 失败: {}",
                    e
                );
                return HashMap::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 重试出站时重置 client_id + req_id(trace_id) + send_state=pending
    /// 保留 row_id 便于前端按 row_id 关联同一逻辑消息
    pub fn update_outbound_resend(&self, id: i64, new_client_id: &str, new_req_id: &str) {
        if id <= 0 {
            return;
        }
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let _ = conn.execute(
            "UPDATE messages_v2
             SET client_id=?, trace_id=?, send_state='pending', send_last_error=NULL,
                 send_attempts=0, updated_at_ms=?
             WHERE id=?",
            params![new_client_id, new_req_id, now_ms, id],
        );
    }

    /// 按 client_id 查找出站消息（iLink 平台 ack 回推时反查）
    ///
    /// 调用方传入明文 bot_token，内部转 SHA-256 hex 后查询。
    pub fn get_outbound_by_client_id(&self, bot_id: &str, client_id: &str) -> Option<MessageRow> {
        let conn = self.conn.lock();
        let bot_id_hash = crypto::sha256_hex(bot_id.as_bytes());
        conn.query_row(
            "SELECT id, trace_id, bot_id, user_id, direction, message_id, client_id,
                    from_user_id, to_user_id, context_token, item_list_json, text,
                    media_status, send_state, send_attempts, created_at_ms
             FROM messages_v2
             WHERE bot_id=? AND client_id=? AND direction='out'",
            params![bot_id_hash, client_id],
            |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    trace_id: row.get(1)?,
                    bot_id: row.get(2)?,
                    user_id: row.get(3)?,
                    direction: row.get(4)?,
                    message_id: row.get(5)?,
                    client_id: row.get(6)?,
                    from_user_id: row.get(7)?,
                    to_user_id: row.get(8)?,
                    context_token: row.get(9)?,
                    item_list_json: row.get(10)?,
                    text: row.get(11)?,
                    media_status: row.get(12)?,
                    send_state: row.get(13)?,
                    send_attempts: row.get(14)?,
                    created_at_ms: row.get(15)?,
                })
            },
        )
        .ok()
    }

    /// 按 trace_id 查消息（前端刷新兜底用）
    #[allow(dead_code)]
    pub fn get_outbound_by_req_id(&self, trace_id: &str) -> Option<MessageRow> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT id, trace_id, bot_id, user_id, direction, message_id, client_id,
                    from_user_id, to_user_id, context_token, item_list_json, text,
                    media_status, send_state, send_attempts, created_at_ms
             FROM messages_v2
             WHERE trace_id=? AND direction='out'
             ORDER BY id DESC LIMIT 1",
            params![trace_id],
            |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    trace_id: row.get(1)?,
                    bot_id: row.get(2)?,
                    user_id: row.get(3)?,
                    direction: row.get(4)?,
                    message_id: row.get(5)?,
                    client_id: row.get(6)?,
                    from_user_id: row.get(7)?,
                    to_user_id: row.get(8)?,
                    context_token: row.get(9)?,
                    item_list_json: row.get(10)?,
                    text: row.get(11)?,
                    media_status: row.get(12)?,
                    send_state: row.get(13)?,
                    send_attempts: row.get(14)?,
                    created_at_ms: row.get(15)?,
                })
            },
        )
        .ok()
    }

    /// 按 user 查未完成的出站（前端刷新后渲染 pending 列表）
    pub fn list_pending_outbound(&self, user_id: &str) -> Vec<MessageRow> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT id, trace_id, bot_id, user_id, direction, message_id, client_id,
                    from_user_id, to_user_id, context_token, item_list_json, text,
                    media_status, send_state, send_attempts, created_at_ms
             FROM messages_v2
             WHERE user_id=? AND direction='out' AND send_state IN ('pending','failed')
             ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map(params![user_id], |row| {
            Ok(MessageRow {
                id: row.get(0)?,
                trace_id: row.get(1)?,
                bot_id: row.get(2).unwrap_or_default(),
                user_id: row.get(3)?,
                direction: row.get(4)?,
                message_id: row.get(5)?,
                client_id: row.get(6)?,
                from_user_id: row.get(7)?,
                to_user_id: row.get(8)?,
                context_token: row.get(9)?,
                item_list_json: row.get(10)?,
                text: row.get(11)?,
                media_status: row.get(12)?,
                send_state: row.get(13)?,
                send_attempts: row.get(14)?,
                created_at_ms: row.get(15)?,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 按 id 查单条
    pub fn get_message_v2(&self, id: i64) -> Option<MessageRow> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT id, trace_id, bot_id, user_id, direction, message_id, client_id,
                    from_user_id, to_user_id, context_token, item_list_json, text,
                    media_status, send_state, send_attempts, created_at_ms
             FROM messages_v2 WHERE id=?",
            params![id],
            |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    trace_id: row.get(1)?,
                    bot_id: row.get(2)?,
                    user_id: row.get(3)?,
                    direction: row.get(4)?,
                    message_id: row.get(5)?,
                    client_id: row.get(6)?,
                    from_user_id: row.get(7)?,
                    to_user_id: row.get(8)?,
                    context_token: row.get(9)?,
                    item_list_json: row.get(10)?,
                    text: row.get(11)?,
                    media_status: row.get(12)?,
                    send_state: row.get(13)?,
                    send_attempts: row.get(14)?,
                    created_at_ms: row.get(15)?,
                })
            },
        )
        .ok()
    }

    /// 更新媒体状态
    #[allow(dead_code)]
    pub fn update_media_status_v2(&self, id: i64, status: &str, keys_json: Option<&str>) {
        if id <= 0 {
            return;
        }
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let _ = conn.execute(
            "UPDATE messages_v2 SET media_status=?, media_keys_json=COALESCE(?, media_keys_json), updated_at_ms=? WHERE id=?",
            params![status, keys_json, now_ms, id],
        );
    }

    /// 删除某用户所有 v2 消息（remove_user 时调用）
    pub fn delete_user_messages_v2(&self, user_id: &str) {
        let conn = self.conn.lock();
        let _ = conn.execute("DELETE FROM messages_v2 WHERE user_id=?", params![user_id]);
    }

    /// 从单例池移除指定 uid 的 Database 句柄，让 Connection/WAL 释放文件句柄。
    pub fn close_for_user(uid: i64) {
        let key = crate::config::user_db_file(uid)
            .to_string_lossy()
            .to_string();
        let mut instances = DB_INSTANCES.lock();
        instances.remove(&key);
    }
}

/// SaveResult 与 openilink-hub-main 对齐：inserted=false 表示去重命中。
#[derive(Debug, Clone, Copy)]
pub struct SaveResult {
    pub id: i64,
    pub inserted: bool,
}

/// messages_v2 行映射
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub trace_id: String,
    pub bot_id: String,
    pub user_id: String,
    pub direction: String,
    pub message_id: Option<i64>,
    pub client_id: Option<String>,
    pub from_user_id: Option<String>,
    pub to_user_id: Option<String>,
    pub context_token: Option<String>,
    pub item_list_json: String,
    pub text: Option<String>,
    pub media_status: String,
    pub send_state: String,
    pub send_attempts: i64,
    pub created_at_ms: i64,
}

// 辅助 trait
trait OrIfEmpty {
    fn or_if_empty<'a>(&'a self, other: &'a str) -> &'a str;
}

impl OrIfEmpty for str {
    fn or_if_empty<'a>(&'a self, other: &'a str) -> &'a str {
        if self.is_empty() {
            other
        } else {
            self
        }
    }
}

// ── SystemDatabase (system.db - 系统库 v2.1) ─────────────────────────────
//
// 与 user.db 的 Database 并存：负责 app_users / sessions / invite_codes /
// system_settings / audit_logs 等系统级表。
// 复用 Database 的单例机制 (DB_INSTANCES) — 同路径共享 sqlite 句柄。

/// 用户凭证（供 Auth 校验密码用，不在此做 PBKDF2）
#[derive(Debug, Clone)]
pub struct UserCredentials {
    pub uid: i64,
    pub role: String,
    pub status: String,
    pub password_hash: String,
    pub salt: String,
    pub iterations: i64,
}

/// 会话校验结果
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub uid: i64,
    pub role: String,
}

pub struct SystemDatabase {
    db: Arc<Database>,
}

impl SystemDatabase {
    /// 获取或创建 system.db 实例（单例，复用 Database::new）
    ///
    /// 返回 `anyhow::Result`，由调用方顶层统一处理（不再 panic）。
    pub fn new() -> anyhow::Result<Arc<Self>> {
        let db = Database::new(&config::system_db_file())?;
        let sys = Arc::new(Self { db });
        sys.init_schema()?;
        Ok(sys)
    }

    /// 在 system.db 执行建表（v2.1 schema）
    fn init_schema(&self) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();

        // user_sessions / device_tokens 改用 token_hash 列存储 SHA-256 哈希，
        //   SHA-256(token)，避免 DB 泄露时明文 token 可直接重放。
        //   迁移策略：检测旧 schema（token 列存在），DROP + 重建为 token_hash 列。
        //   迁移会清空所有现有 session/device_token，用户需重新登录——这是安全审计
        //   修复的预期行为，避免明文 token 残留。
        //   SQLite 不支持 RENAME COLUMN（3.25+ 才支持，且 bundled 版本可能更老），
        //   且 PRIMARY KEY 列无法直接改名，最干净的方式是 DROP + 重建。
        let need_migrate_sessions = {
            let cols: Vec<String> = conn
                .prepare("PRAGMA table_info(user_sessions)")
                .ok()
                .map(|mut stmt| {
                    stmt.query_map([], |r| r.get::<_, String>(1))
                        .ok()
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            cols.iter().any(|c| c == "token")
        };
        if need_migrate_sessions {
            tracing::info!(
                "[MIGRATE P1-6] user_sessions 旧 schema（token 列），DROP + 重建为 token_hash 列；现有 session 全部失效"
            );
            let _ = conn.execute("DROP TABLE IF EXISTS user_sessions", []);
        }
        let need_migrate_device_tokens = {
            let cols: Vec<String> = conn
                .prepare("PRAGMA table_info(device_tokens)")
                .ok()
                .map(|mut stmt| {
                    stmt.query_map([], |r| r.get::<_, String>(1))
                        .ok()
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            cols.iter().any(|c| c == "token")
        };
        if need_migrate_device_tokens {
            tracing::info!(
                "[MIGRATE P1-6] device_tokens 旧 schema（token 列），DROP + 重建为 token_hash 列；现有 device_token 全部失效"
            );
            let _ = conn.execute("DROP TABLE IF EXISTS device_tokens", []);
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_users (
                id INTEGER PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                salt TEXT NOT NULL,
                iterations INTEGER NOT NULL DEFAULT 600000,
                role TEXT NOT NULL DEFAULT 'user',
                status TEXT NOT NULL DEFAULT 'active',
                quota_upload_bytes INTEGER NOT NULL DEFAULT 0,
                used_upload_bytes INTEGER NOT NULL DEFAULT 0,
                used_upload_date TEXT,
                quota_download_bytes INTEGER NOT NULL DEFAULT 0,
                used_download_bytes INTEGER NOT NULL DEFAULT 0,
                used_download_date TEXT,
                quota_media_bytes INTEGER NOT NULL DEFAULT 0,
                used_media_bytes INTEGER NOT NULL DEFAULT 0,
                quota_msg_per_day INTEGER NOT NULL DEFAULT 0,
                used_msg_today INTEGER NOT NULL DEFAULT 0,
                used_msg_date TEXT,
                quota_media_count INTEGER NOT NULL DEFAULT 0,
                used_media_count INTEGER NOT NULL DEFAULT 0,
                allow_upload INTEGER NOT NULL DEFAULT 1,
                allow_webdav INTEGER NOT NULL DEFAULT 1,
                allow_custom_webdav INTEGER NOT NULL DEFAULT 1,
                email TEXT,
                agreed_terms_ver TEXT,
                agreed_terms_at TEXT,
                created_at TEXT NOT NULL,
                last_login_at TEXT
            );
            CREATE TABLE IF NOT EXISTS system_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS invite_codes (
                code TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                expires_at TEXT,
                used_by INTEGER,
                used_at TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                note TEXT
            );
            -- 多用户会话表命名为 user_sessions，避免与 Database::init_db 的
            -- sessions 表（旧 wechat_bot.db/user.db 单用户 session）同名冲突
            -- token 改为 token_hash 存储 SHA-256(plaintext_token)，不存明文。
            --   DB 泄露时无法直接重放，必须暴力破解 SHA-256 才能拿到原始 token。
            CREATE TABLE IF NOT EXISTS user_sessions (
                token_hash TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                expires_at REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_user_sessions_user ON user_sessions(user_id);
            CREATE INDEX IF NOT EXISTS idx_user_sessions_expires ON user_sessions(expires_at);
            CREATE TABLE IF NOT EXISTS audit_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                actor TEXT NOT NULL,
                action TEXT NOT NULL,
                target TEXT,
                detail_json TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ip_bans (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ip TEXT NOT NULL,
                reason TEXT NOT NULL DEFAULT '',
                banned_by TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                expires_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_ip_bans_ip ON ip_bans(ip);
            -- token 改为 token_hash 存储 SHA-256(plaintext_token)，不存明文。
            --   DB 泄露时无法直接重放。
            CREATE TABLE IF NOT EXISTS device_tokens (
                token_hash TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL,
                device_name TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                last_used_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_device_tokens_user ON device_tokens(user_id);
            -- delete_user 文件系统删除失败的补偿队列。
            --   system.db 已删 app_users 行但 users/<uid>/ 目录残留时记录此表，
            --   后台线程定期重试 remove_dir_all，成功后删除本行。
            CREATE TABLE IF NOT EXISTS pending_user_cleanup (
                uid INTEGER PRIMARY KEY,
                user_dir TEXT NOT NULL,
                failed_at TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );",
        )
        .with_context(|| "无法初始化 system.db schema")?;

        // 迁移：app_users 表加 email 列（幂等）
        let _ = conn.execute("ALTER TABLE app_users ADD COLUMN email TEXT", []);
        let _ = conn.execute("ALTER TABLE app_users ADD COLUMN used_upload_date TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE app_users ADD COLUMN used_download_date TEXT",
            [],
        );

        // 将历史版本中实际未生效的别名迁移到唯一的规范键。
        let setting_aliases = [
            ("terms.version", "terms_version"),
            ("terms.text", "terms_text"),
            ("quota.upload_bytes", "default_quota_upload_bytes"),
            ("quota.download_bytes", "default_quota_download_bytes"),
            ("quota.media_bytes", "default_quota_media_bytes"),
            ("quota.msg_per_day", "default_quota_msg_per_day"),
            ("quota.media_count", "default_quota_media_count"),
            ("feature.upload", "default_allow_upload"),
            ("feature.webdav", "default_allow_webdav"),
            ("feature.custom_webdav", "default_allow_custom_webdav"),
        ];
        let migration_time = chrono::Utc::now().to_rfc3339();
        for (legacy, canonical) in setting_aliases {
            conn.execute(
                "INSERT OR IGNORE INTO system_settings(key, value, updated_at)
                 SELECT ?, value, ? FROM system_settings WHERE key=? LIMIT 1",
                params![canonical, migration_time, legacy],
            )?;
            conn.execute("DELETE FROM system_settings WHERE key=?", params![legacy])?;
        }
        // 这些旧键从未被运行时读取，保留只会让管理员误以为已经生效。
        for stale in [
            "auth_required",
            "server_storage.local_path",
            "server_storage.backend",
        ] {
            conn.execute("DELETE FROM system_settings WHERE key=?", params![stale])?;
        }

        // Phase 4: 注册流程默认设置（INSERT OR IGNORE 幂等，仅首次写入）
        //   - allow_open_registration: 默认 off（安全优先，需管理员显式开启）
        //   - allow_invite_registration: 默认 on（管理员可发邀请码让指定用户注册）
        //   - terms_version / terms_text: 默认守则 v1.0
        //   - site_name: 站点名称
        let now = chrono::Utc::now().to_rfc3339();
        let default_terms = r#"# 使用守则

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
        let defaults: &[(&str, &str)] = &[
            ("allow_open_registration", "off"),
            ("allow_invite_registration", "on"),
            ("terms_version", "1.0"),
            ("terms_text", default_terms),
            ("terms.url", ""),
            ("docs.url", ""),
            ("site_name", "Zyn iLink ChatBox · WongMod"),
            ("default_quota_upload_bytes", "0"),
            ("default_quota_download_bytes", "0"),
            ("default_quota_media_bytes", "0"),
            ("default_quota_msg_per_day", "0"),
            ("default_quota_media_count", "0"),
            ("default_allow_upload", "on"),
            ("default_allow_webdav", "on"),
            ("default_allow_custom_webdav", "on"),
            ("admin.web_access", "intranet"),
        ];
        for (key, value) in defaults {
            let _ = conn.execute(
                "INSERT OR IGNORE INTO system_settings(key, value, updated_at) VALUES(?, ?, ?)",
                params![key, value, now],
            );
        }
        Ok(())
    }

    // ── 用户管理 ─────────────────────────────────────────────

    /// 分配一个 6 位随机 uid（100000..=999999）。
    /// 6 位空间 90 万，远大于本应用的用户数；碰撞概率极低，最多重试 50 次。
    /// 50 次仍冲突则返回 Err（理论上几乎不可能）。
    ///
    /// 随机化重试间隔，避免多线程同时重试导致并发阻塞。
    ///   原实现持锁 50 次重试——并发注册时，一个线程持锁循环 50 次 query，
    ///   其他线程全部阻塞在 conn_lock()，导致整个 system.db 不可用。
    ///   修复策略：
    ///   1) 每次循环单独获取锁、查询、释放锁（不长期持有）
    ///   2) 冲突时 sleep 随机 1-10ms，避免多线程同步重试热点
    ///   3) 保留 50 次重试上限（碰撞概率极低，足够）
    pub fn allocate_random_uid(&self) -> anyhow::Result<i64> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        for attempt in 0..50 {
            let candidate: i64 = rng.gen_range(100000..=999999);
            // 每次循环单独持锁查询，避免长期持有锁阻塞其他 DB 操作。
            let exists: Option<i64> = {
                let conn = self.db.conn_lock();
                conn.query_row(
                    "SELECT 1 FROM app_users WHERE id=?",
                    params![candidate],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten()
            };
            if exists.is_none() {
                return Ok(candidate);
            }
            // 冲突时随机 sleep 1-10ms，避免并发注册线程同步重试导致热点竞争。
            if attempt > 0 {
                let backoff_ms = rng.gen_range(1..=10);
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
            }
        }
        anyhow::bail!("分配随机 uid 失败：50 次尝试均碰撞")
    }

    /// 分配一个未使用的邀请码（4 位大写字母+数字组合，如 `A3F5`）。
    ///
    /// 字符集：A-Z (26) + 0-9 (10) = 36 个字符；4 位组合 36^4 = 1,679,616 种。
    /// 空间对小型部署足够；碰撞由本方法内部重试 50 次解决。
    /// 注意：检查覆盖所有状态（active/used/revoked）的邀请码，因为 code 是 PK
    /// 不能与已删除/已撤销的旧码重复（避免歧义）。
    pub fn allocate_invite_code(&self) -> anyhow::Result<String> {
        use rand::Rng;
        const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let conn = self.db.conn_lock();
        let mut rng = rand::thread_rng();
        for _ in 0..50 {
            let code: String = (0..4)
                .map(|_| {
                    let idx = rng.gen_range(0..CHARSET.len());
                    CHARSET[idx] as char
                })
                .collect();
            let exists: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM invite_codes WHERE code=?",
                    params![&code],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten();
            if exists.is_none() {
                return Ok(code);
            }
        }
        anyhow::bail!("分配邀请码失败：50 次尝试均碰撞")
    }

    /// 创建用户，使用指定的 uid（6 位随机数字，由 allocate_random_uid 生成）。
    /// 返回传入的 uid（不再使用 last_insert_rowid）。
    pub fn create_user(
        &self,
        uid: i64,
        username: &str,
        password_hash: &str,
        salt: &str,
        iterations: i64,
        role: &str,
    ) -> anyhow::Result<i64> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO app_users(id, username, password_hash, salt, iterations, role, status, created_at)
             VALUES(?, ?, ?, ?, ?, ?, 'active', ?)",
            params![uid, username, password_hash, salt, iterations, role, now],
        )?;
        Ok(uid)
    }

    /// 按用户名查用户
    pub fn get_user_by_username(&self, username: &str) -> Option<models::AppUser> {
        let conn = self.db.conn_lock();
        conn.query_row(
            "SELECT id, username, password_hash, salt, iterations, role, status,
                    quota_upload_bytes, used_upload_bytes, used_upload_date,
                    quota_download_bytes, used_download_bytes, used_download_date,
                    quota_media_bytes, used_media_bytes,
                    quota_msg_per_day, used_msg_today, used_msg_date,
                    quota_media_count, used_media_count,
                    allow_upload, allow_webdav, allow_custom_webdav,
                    email,
                    agreed_terms_ver, agreed_terms_at,
                    created_at, last_login_at
             FROM app_users WHERE username=?",
            params![username],
            row_to_app_user,
        )
        .ok()
    }

    /// 按 id 查用户
    pub fn get_user_by_id(&self, uid: i64) -> Option<models::AppUser> {
        let conn = self.db.conn_lock();
        conn.query_row(
            "SELECT id, username, password_hash, salt, iterations, role, status,
                    quota_upload_bytes, used_upload_bytes, used_upload_date,
                    quota_download_bytes, used_download_bytes, used_download_date,
                    quota_media_bytes, used_media_bytes,
                    quota_msg_per_day, used_msg_today, used_msg_date,
                    quota_media_count, used_media_count,
                    allow_upload, allow_webdav, allow_custom_webdav,
                    email,
                    agreed_terms_ver, agreed_terms_at,
                    created_at, last_login_at
             FROM app_users WHERE id=?",
            params![uid],
            row_to_app_user,
        )
        .ok()
    }

    /// 列出所有用户
    pub fn list_users(&self) -> Vec<models::AppUser> {
        let conn = self.db.conn_lock();
        let mut stmt = match conn.prepare(
            "SELECT id, username, password_hash, salt, iterations, role, status,
                    quota_upload_bytes, used_upload_bytes, used_upload_date,
                    quota_download_bytes, used_download_bytes, used_download_date,
                    quota_media_bytes, used_media_bytes,
                    quota_msg_per_day, used_msg_today, used_msg_date,
                    quota_media_count, used_media_count,
                    allow_upload, allow_webdav, allow_custom_webdav,
                    email,
                    agreed_terms_ver, agreed_terms_at,
                    created_at, last_login_at
             FROM app_users ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_users prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], row_to_app_user) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_users query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 更新用户状态（active/disabled）
    pub fn update_user_status(&self, uid: i64, status: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute(
            "UPDATE app_users SET status=? WHERE id=?",
            params![status, uid],
        )?;
        Ok(())
    }

    /// 改造方案 §三：批量更新用户配额（只更新非零值字段）
    pub fn batch_update_user_quota(
        &self,
        uid: i64,
        quota: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        if let Some(v) = quota.get("upload_bytes").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET quota_upload_bytes=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        if let Some(v) = quota.get("download_bytes").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET quota_download_bytes=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        if let Some(v) = quota.get("media_bytes").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET quota_media_bytes=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        if let Some(v) = quota.get("msg_per_day").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET quota_msg_per_day=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        if let Some(v) = quota.get("media_count").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET quota_media_count=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        Ok(())
    }

    /// 改造方案 §三：批量更新用户功能开关
    pub fn batch_update_user_features(
        &self,
        uid: i64,
        features: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        if let Some(v) = features.get("upload").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET allow_upload=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        if let Some(v) = features.get("webdav").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET allow_webdav=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        if let Some(v) = features.get("custom_webdav").and_then(|v| v.as_i64()) {
            conn.execute(
                "UPDATE app_users SET allow_custom_webdav=?1 WHERE id=?2",
                params![v, uid],
            )?;
        }
        Ok(())
    }

    /// 删除用户（顺带清理该用户的 sessions + device_tokens + per-user 数据）
    /// 原 delete_user 仅删 system.db 两表行，不清理 per-user 数据。现在同时清理文件 + 单例 + device_tokens。
    /// 三步 DELETE 用 transaction() 包裹，任一失败自动回滚。
    ///   任一失败自动回滚（避免 app_users 已删但 session/device_token 残留，
    ///   或 session 清了但 app_users 还在的中间态）。
    /// 文件系统删除失败时记录补偿队列（pending_user_cleanup 表），
    ///   后台线程定期重试 remove_dir_all，避免孤儿目录永久残留。
    /// 主密钥不再做内存缓存（每次按需从文件读取），
    ///   删除用户时无需清理缓存；密钥文件随 user_dir 一并被 remove_dir_all 删除。
    pub fn delete_user(&self, uid: i64) -> anyhow::Result<()> {
        {
            let mut conn = self.db.conn_lock();
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM app_users WHERE id=?", params![uid])?;
            tx.execute("DELETE FROM user_sessions WHERE user_id=?", params![uid])?;
            tx.execute("DELETE FROM device_tokens WHERE user_id=?", params![uid])?;
            tx.commit()?;
        } // 释放 system.db 锁
          // 清理 per-user 数据（文件 + 单例）。
          // 1. 先从单例池移除 Arc<Database>，让 Connection/WAL 句柄释放（否则 Windows 上文件被占用无法删）
        Database::close_for_user(uid);
        // 2. 删除 users/<uid>/ 目录（含 user.db、media_cache、user_data、.ilink_master_key）
        let user_db_path = crate::config::user_db_file(uid);
        let user_dir = user_db_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let user_dir_str = user_dir.to_string_lossy().to_string();
        if let Err(e) = std::fs::remove_dir_all(user_dir) {
            tracing::warn!(
                "[delete_user] 清理用户目录失败 uid={} {:?}: {}（已记录补偿队列，后台将重试）",
                uid,
                user_dir,
                e
            );
            // 记录到补偿队列，后台线程定期重试 remove_dir_all。
            //   常见失败原因：Windows 上文件句柄未完全释放、权限问题、网络盘
            //   system.db 已删 app_users 行，残留文件不构成安全风险但占磁盘
            self.record_pending_cleanup(uid, &user_dir_str, &e.to_string());
        }
        Ok(())
    }

    /// 设置用户密码（已 PBKDF2 哈希过）
    pub fn set_user_password(
        &self,
        uid: i64,
        password_hash: &str,
        salt: &str,
        iterations: i64,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute(
            "UPDATE app_users SET password_hash=?, salt=?, iterations=? WHERE id=?",
            params![password_hash, salt, iterations, uid],
        )?;
        Ok(())
    }

    /// 改密码 + 失效全部会话 + 撤销设备令牌，三步原子化（transaction 包裹）。
    ///
    /// 原 auth.change_password 在三个独立 conn_lock() 中分别执行 set_user_password
    /// / delete_all_sessions / revoke_all_device_tokens，中途崩溃会留下中间态：
    ///   - 密码已改但旧 session 仍可用（认证旁路）
    ///   - 密码已改但 device_token 仍可用（"记住我"绕过）
    ///   - session 已清但密码未改（用户被踢但密码强度未升级）
    ///
    /// 用 transaction() 包裹：任一步失败全部回滚，P0-7 安全保证不被破坏。
    pub fn change_user_password_atomic(
        &self,
        uid: i64,
        password_hash: &str,
        salt: &str,
        iterations: i64,
    ) -> anyhow::Result<()> {
        let mut conn = self.db.conn_lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE app_users SET password_hash=?, salt=?, iterations=? WHERE id=?",
            params![password_hash, salt, iterations, uid],
        )?;
        tx.execute("DELETE FROM user_sessions WHERE user_id=?", params![uid])?;
        tx.execute("DELETE FROM device_tokens WHERE user_id=?", params![uid])?;
        tx.commit()?;
        Ok(())
    }

    /// 禁用用户 + 失效全部会话 + 撤销设备令牌，原子化（transaction 包裹）。
    ///
    /// 原 web.rs api_admin_user_disable 三步独立调用 update_user_status(disabled)
    /// / delete_all_sessions / revoke_all_device_tokens，中途崩溃导致禁用不彻底
    /// （status=disabled 但 session 仍可用，或 session 清了但 status 还是 active）。
    /// 用 transaction() 包裹保证禁用操作的原子性。
    pub fn disable_user_atomic(&self, uid: i64) -> anyhow::Result<()> {
        let mut conn = self.db.conn_lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE app_users SET status=? WHERE id=?",
            params!["disabled", uid],
        )?;
        tx.execute("DELETE FROM user_sessions WHERE user_id=?", params![uid])?;
        tx.execute("DELETE FROM device_tokens WHERE user_id=?", params![uid])?;
        tx.commit()?;
        Ok(())
    }

    /// 更新用户配额（key 必须在白名单内防 SQL 注入）
    pub fn update_user_quota(&self, uid: i64, key: &str, value: i64) -> anyhow::Result<()> {
        let col = match key {
            "quota_upload_bytes" => "quota_upload_bytes",
            "quota_download_bytes" => "quota_download_bytes",
            "quota_media_bytes" => "quota_media_bytes",
            "quota_msg_per_day" => "quota_msg_per_day",
            "quota_media_count" => "quota_media_count",
            _ => anyhow::bail!("非法配额字段: {}", key),
        };
        let sql = format!("UPDATE app_users SET {}=? WHERE id=?", col);
        let conn = self.db.conn_lock();
        conn.execute(&sql, params![value, uid])?;
        Ok(())
    }

    /// 更新用户功能开关（feature 必须在白名单内防 SQL 注入）
    pub fn update_user_feature(&self, uid: i64, feature: &str, on: bool) -> anyhow::Result<()> {
        let col = match feature {
            "allow_upload" => "allow_upload",
            "allow_webdav" => "allow_webdav",
            "allow_custom_webdav" => "allow_custom_webdav",
            _ => anyhow::bail!("非法功能字段: {}", feature),
        };
        let sql = format!("UPDATE app_users SET {}=? WHERE id=?", col);
        let conn = self.db.conn_lock();
        conn.execute(&sql, params![on as i64, uid])?;
        Ok(())
    }

    /// 更新最后登录时间
    pub fn update_last_login(&self, uid: i64) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE app_users SET last_login_at=? WHERE id=?",
            params![now, uid],
        )?;
        Ok(())
    }

    /// 标记用户已同意某版本条款
    pub fn set_user_agreed_terms(&self, uid: i64, ver: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE app_users SET agreed_terms_ver=?, agreed_terms_at=? WHERE id=?",
            params![ver, now, uid],
        )?;
        Ok(())
    }

    // ── 凭证查询（供 Auth 校验密码用，不在此做 PBKDF2）─────────

    /// 按用户名取凭证
    pub fn get_user_credentials(&self, username: &str) -> Option<UserCredentials> {
        let conn = self.db.conn_lock();
        conn.query_row(
            "SELECT id, role, status, password_hash, salt, iterations
             FROM app_users WHERE username=?",
            params![username],
            |row| {
                Ok(UserCredentials {
                    uid: row.get(0)?,
                    role: row.get(1)?,
                    status: row.get(2)?,
                    password_hash: row.get(3)?,
                    salt: row.get(4)?,
                    iterations: row.get(5)?,
                })
            },
        )
        .ok()
    }

    // ── Session（绑 uid，30 天有效期）──────────────────────────

    /// 创建会话，返回 token（random_hex(32) = 64 hex 字符）
    ///
    /// DB 中只存 SHA-256(token)，不存明文。
    ///   明文 token 仅在此函数返回值中存在，由调用方写入 HttpOnly Cookie 返回客户端。
    ///   DB 泄露时攻击者拿到的是 SHA-256 hash，必须暴力破解才能拿到原始 token。
    pub fn create_session(&self, uid: i64) -> anyhow::Result<String> {
        let token = crypto::random_hex(32);
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let now_sec = chrono::Utc::now().timestamp() as f64;
        let expires_at = now_sec + 2_592_000.0; // 30 天
        let created_at = chrono::Utc::now().to_rfc3339();
        let conn = self.db.conn_lock();
        conn.execute(
            "INSERT INTO user_sessions(token_hash, user_id, created_at, expires_at) VALUES(?, ?, ?, ?)",
            params![token_hash, uid, created_at, expires_at],
        )?;
        Ok(token)
    }

    /// 校验会话，返回 SessionInfo（含 role）。
    /// S1 惰性清理：命中后顺带 DELETE 过期 session（限 100 行避免长事务）。
    ///
    /// 输入明文 token，先 SHA-256 哈希再按 token_hash 查询。
    pub fn verify_session(&self, token: &str) -> Option<SessionInfo> {
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let conn = self.db.conn_lock();
        let now_sec = chrono::Utc::now().timestamp() as f64;
        let res: Option<(i64, String)> = conn
            .query_row(
                "SELECT s.user_id, u.role
                 FROM user_sessions s
                 INNER JOIN app_users u ON u.id = s.user_id
                 WHERE s.token_hash=? AND s.expires_at > ?",
                params![token_hash, now_sec],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        // S1 惰性清理：用子查询 LIMIT 100 兼容未开启 SQLITE_ENABLE_UPDATE_DELETE_LIMIT 的构建
        let _ = conn.execute(
            "DELETE FROM user_sessions WHERE rowid IN (
                SELECT rowid FROM user_sessions WHERE expires_at < ? LIMIT 100
             )",
            params![now_sec],
        );
        res.map(|(uid, role)| SessionInfo { uid, role })
    }

    /// 续期会话（expires_at = now + 30 天）
    /// 增加 180 天绝对有效期上限（超过此期限 session 不再续期）。
    ///   原实现每次 renew 都延长 30 天，session 理论上永不过期，stolen session cookie
    ///   可长期有效。现读取 created_at，取 min(now+30天, created_at+180天) 作为新 expires_at。
    ///   超过 180 天的 session 不再续期，verify_session 会因 expires_at < now 自动失效。
    ///
    /// 输入明文 token，先 SHA-256 哈希再按 token_hash 查询/更新。
    pub fn renew_session(&self, token: &str) -> anyhow::Result<()> {
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let conn = self.db.conn_lock();
        let now_sec = chrono::Utc::now().timestamp() as f64;
        let sliding_expiry = now_sec + 2_592_000.0; // now + 30 天

        // 读取 created_at 计算绝对上限
        let created_at_str: Option<String> = conn
            .query_row(
                "SELECT created_at FROM user_sessions WHERE token_hash=?",
                params![token_hash],
                |row| row.get(0),
            )
            .optional()?;

        let new_expiry = if let Some(ref created_str) = created_at_str {
            // 解析 RFC3339 created_at，计算 created_at + 180 天
            let absolute_max = match chrono::DateTime::parse_from_rfc3339(created_str) {
                Ok(dt) => {
                    let created_sec = dt.with_timezone(&chrono::Utc).timestamp() as f64;
                    created_sec + 180.0 * 86400.0 // created_at + 180 天
                }
                Err(e) => {
                    tracing::warn!(
                        "[session] renew_session: 解析 created_at 失败 token_hash={}... val={} err={}，降级为滑动续期",
                        &token_hash[..8.min(token_hash.len())],
                        created_str,
                        e
                    );
                    sliding_expiry
                }
            };
            sliding_expiry.min(absolute_max)
        } else {
            // token 不存在（可能已过期被惰性清理）— 不更新任何行
            return Ok(());
        };

        conn.execute(
            "UPDATE user_sessions SET expires_at=? WHERE token_hash=?",
            params![new_expiry, token_hash],
        )?;
        Ok(())
    }

    /// 删除指定会话
    ///
    /// 输入明文 token，先 SHA-256 哈希再按 token_hash 删除。
    pub fn delete_session(&self, token: &str) -> anyhow::Result<()> {
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let conn = self.db.conn_lock();
        conn.execute(
            "DELETE FROM user_sessions WHERE token_hash=?",
            params![token_hash],
        )?;
        Ok(())
    }

    /// 删除某用户的其他会话（保留 keep_token）
    ///
    /// 输入明文 keep_token，先 SHA-256 哈希再比对 token_hash。
    pub fn delete_other_sessions(&self, uid: i64, keep_token: &str) -> anyhow::Result<()> {
        let keep_hash = crypto::sha256_hex(keep_token.as_bytes());
        let conn = self.db.conn_lock();
        conn.execute(
            "DELETE FROM user_sessions WHERE user_id=? AND token_hash<>?",
            params![uid, keep_hash],
        )?;
        Ok(())
    }

    /// 删除某用户的全部会话（不保留任何 token）。
    ///
    /// 修复旧 token 免密再登录无限续期问题：即使密码已修改，持有有效旧 session token 可绕过密码校验。
    ///   原实现：改密码时只调用 delete_other_sessions(uid, current_token)，
    ///   保留当前 token；若 token 已泄露（例如在改密前被攻击者截获），
    ///   攻击者仍可凭旧 token 持续访问，且每次访问都会触发滑动续期（30 天），
    ///   形成「改密也无法踢出」的漏洞。
    ///   修复：改密码成功后强制作废该用户的全部会话（包括当前 token），
    ///   迫使用户用新密码重新登录，所有旧 token 立即失效。
    ///
    /// 改密/禁用流程已改用 change_user_password_atomic / disable_user_atomic，
    ///   disable_user_atomic 把三步 DELETE 包到 transaction 内，本函数不再被调用。
    ///   保留作为 SystemDatabase 公共 API：未来"踢出全部会话"等独立功能可直接复用，
    ///   无需重新实现。dead_code 警告显式 allow。
    #[allow(dead_code)]
    pub fn delete_all_sessions(&self, uid: i64) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute("DELETE FROM user_sessions WHERE user_id=?", params![uid])?;
        Ok(())
    }

    // ── 邀请码 ───────────────────────────────────────────────

    /// 创建邀请码（code 由调用方用 random_hex 生成）
    pub fn create_invite(
        &self,
        code: &str,
        expires_at: Option<&str>,
        note: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO invite_codes(code, created_at, expires_at, status, note)
             VALUES(?, ?, ?, 'active', ?)",
            params![code, now, expires_at, note],
        )?;
        Ok(())
    }

    /// 列出所有邀请码
    pub fn list_invites(&self) -> Vec<models::InviteCode> {
        let conn = self.db.conn_lock();
        let mut stmt = match conn.prepare(
            "SELECT code, created_at, expires_at, used_by, used_at, status, note
             FROM invite_codes ORDER BY created_at DESC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_invites prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], |row| {
            Ok(models::InviteCode {
                code: row.get(0)?,
                created_at: row.get(1)?,
                expires_at: row.get(2)?,
                used_by: row.get(3)?,
                used_at: row.get(4)?,
                status: row.get(5)?,
                note: row.get(6)?,
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_invites query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 撤销邀请码（UPDATE status='revoked'）
    pub fn revoke_invite(&self, code: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute(
            "UPDATE invite_codes SET status='revoked' WHERE code=?",
            params![code],
        )?;
        Ok(())
    }

    /// 使用邀请码（原子 UPDATE：校验 active 且未过期；命中 0 行则报错并区分原因）
    /// expires_at 为 RFC3339 UTC 字符串，字典序比较等价于时间序
    pub fn use_invite(&self, code: &str, uid: i64) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now();
        let now_str = now.to_rfc3339();

        // 原子 UPDATE：仅校验 status='active'，过期检测在 Rust 层处理
        // 避免 SQL 中 RFC3339 字符串比较因格式精度不同导致误判
        let affected = conn.execute(
            "UPDATE invite_codes
             SET status='used', used_by=?, used_at=?
             WHERE code=? AND status='active'",
            params![uid, now_str, code],
        )?;
        if affected > 0 {
            tracing::info!("[INVITE] 邀请码使用成功 code={} uid={}", code, uid);
            return Ok(());
        }

        // UPDATE 未命中 → 查明原因并记录诊断日志
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT status, expires_at FROM invite_codes WHERE code=?",
                params![code],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            None => anyhow::bail!("邀请码不存在: {}", code),
            Some((status, _)) if status != "active" => {
                tracing::warn!("[INVITE] 邀请码不可用 code={} status={}", code, status);
                anyhow::bail!("邀请码不可用(状态={}): {}", status, code)
            }
            Some((_, Some(ref e))) if !e.is_empty() => {
                // 使用 DateTime 解析 + 数值比较，避免字符串精度不一致问题
                let expired = match chrono::DateTime::parse_from_rfc3339(e) {
                    Ok(exp_time) => exp_time < now,
                    Err(_) => {
                        tracing::warn!(
                            "[INVITE] 解析 expires_at 失败 code={} expires_at={}，降级为字符串比较",
                            code,
                            e
                        );
                        *e < now_str
                    }
                };
                if expired {
                    tracing::warn!(
                        "[INVITE] 邀请码已过期 code={} expires_at={} now={}",
                        code,
                        e,
                        now_str
                    );
                    anyhow::bail!("邀请码已过期: {}", code)
                } else {
                    tracing::warn!("[INVITE] 邀请码状态异常 code={} status=active expires_at={} 但 UPDATE 未命中（可能竞态）", code, e);
                    anyhow::bail!("邀请码使用失败: {}", code)
                }
            }
            _ => anyhow::bail!("邀请码使用失败: {}", code),
        }
    }

    /// 回填邀请码的真实 uid（use_invite 先占位 uid=0，create_user 后再回填）。
    ///   注册流程改为"先 use_invite 占位 uid=0 → create_user → 回填真实 uid"后，
    ///   create_user 成功后需将 used_by 从占位 0 更新为真实 uid。
    pub fn update_invite_uid(&self, code: &str, uid: i64) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let affected = conn.execute(
            "UPDATE invite_codes SET used_by=? WHERE code=? AND status='used' AND used_by=0",
            params![uid, code],
        )?;
        if affected == 0 {
            tracing::warn!(
                "[INVITE] update_invite_uid 未命中 code={} uid={}（可能已被并发覆盖）",
                code,
                uid
            );
        }
        Ok(())
    }

    /// 回滚邀请码到 active 状态（回填失败或用户创建失败时恢复）。
    ///   create_user 失败时调用，避免邀请码被永久消耗。
    ///   仅当 used_by=0（占位状态）时回滚，避免误回滚已成功绑定的邀请码。
    pub fn restore_invite(&self, code: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let affected = conn.execute(
            "UPDATE invite_codes SET status='active', used_by=NULL, used_at=NULL \
             WHERE code=? AND status='used' AND (used_by=0 OR used_by IS NULL)",
            params![code],
        )?;
        if affected == 0 {
            tracing::warn!(
                "[INVITE] restore_invite 未命中 code={}（可能已绑定真实 uid，不回滚）",
                code
            );
        } else {
            tracing::info!("[INVITE] 邀请码已回滚至 active code={}", code);
        }
        Ok(())
    }

    // ── 系统设置 ─────────────────────────────────────────────

    /// 读取单个设置
    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.db.conn_lock();
        conn.query_row(
            "SELECT value FROM system_settings WHERE key=?",
            params![key],
            |row| row.get(0),
        )
        .ok()
    }

    /// UPSERT 设置
    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO system_settings(key, value, updated_at) VALUES(?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![key, value, now],
        )?;
        Ok(())
    }

    /// 列出所有设置
    pub fn list_settings(&self) -> Vec<models::SystemSetting> {
        let conn = self.db.conn_lock();
        let mut stmt = match conn.prepare("SELECT key, value, updated_at FROM system_settings") {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_settings prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], |row| {
            Ok(models::SystemSetting {
                key: row.get(0)?,
                value: row.get(1)?,
                updated_at: row.get(2)?,
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_settings query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    // ── 审计日志 ─────────────────────────────────────────────

    /// 写入审计日志
    pub fn insert_audit(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail_json: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO audit_logs(actor, action, target, detail_json, created_at)
             VALUES(?, ?, ?, ?, ?)",
            params![actor, action, target, detail_json, now],
        )?;
        Ok(())
    }

    /// 写审计日志，失败时 tracing::warn! 告警但不阻断业务。
    ///   调用方按需检查返回值，关键操作失败时应阻断（见 web.rs::audit_log 用法）。
    pub fn audit_log_warn(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail_json: Option<&str>,
    ) -> bool {
        match self.insert_audit(actor, action, target, detail_json) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    "[AUDIT] 审计日志写入失败 actor={} action={} target={:?} detail={:?}: {}",
                    actor,
                    action,
                    target,
                    detail_json,
                    e
                );
                false
            }
        }
    }

    /// 查询最近 audit 日志。
    ///
    /// 上限可配置——通过 `ILINK_AUDIT_LIMIT` 环境变量调整（默认 1000，最大 5000）。
    ///   配合 `purge_old_audit_logs` 按时间清理，避免无界增长。
    pub fn list_audit(&self, limit: i64) -> Vec<models::AuditLog> {
        let conn = self.db.conn_lock();
        // 默认上限 1000，可通过 ILINK_AUDIT_LIMIT 环境变量调高至 10000
        let configured_max = std::env::var("ILINK_AUDIT_LIMIT")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(1000)
            .clamp(1, 10000);
        let limit = limit.min(configured_max);
        let mut stmt = match conn.prepare(
            "SELECT id, actor, action, target, detail_json, created_at
             FROM audit_logs ORDER BY id DESC LIMIT ?",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_audit prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map(params![limit], |row| {
            Ok(models::AuditLog {
                id: row.get(0)?,
                actor: row.get(1)?,
                action: row.get(2)?,
                target: row.get(3)?,
                detail_json: row.get(4)?,
                created_at: row.get(5)?,
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_audit query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 审计日志总数（用于管理面板显示"共 N 条"）。
    ///   原 api_admin_stats 只显示"近 1000 条"数量，管理员无法判断是否被覆盖。
    pub fn audit_log_count(&self) -> i64 {
        let conn = self.db.conn_lock();
        conn.query_row("SELECT COUNT(*) FROM audit_logs", [], |row| row.get(0))
            .unwrap_or(0)
    }

    /// 清理超过指定天数的审计日志（默认 90 天，可通过 ILINK_AUDIT_RETENTION_DAYS 调整）。
    ///   建议 90 天，由 main.rs 启动时 + 后台线程定期调用。
    ///   返回删除的行数。失败仅 warn 不阻断业务——清理是维护任务，非关键路径。
    pub fn purge_old_audit_logs(&self, retention_days: u32) -> usize {
        let cutoff = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(retention_days as i64))
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339();
        let conn = self.db.conn_lock();
        match conn.execute(
            "DELETE FROM audit_logs WHERE created_at < ?",
            params![cutoff],
        ) {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        "[AUDIT] 清理 {} 天前审计日志：删除 {} 条",
                        retention_days,
                        n
                    );
                }
                n
            }
            Err(e) => {
                tracing::warn!(
                    "[AUDIT] 清理审计日志失败 (retention_days={}): {}",
                    retention_days,
                    e
                );
                0
            }
        }
    }

    // ── delete_user 补偿队列 ──────────────────────

    /// 记录文件系统删除失败的用户目录到补偿队列。
    ///   system.db 行已删但 users/<uid>/ 目录残留时调用，后台线程定期重试。
    pub fn record_pending_cleanup(&self, uid: i64, user_dir: &str, error: &str) {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO pending_user_cleanup(uid, user_dir, failed_at, attempts, last_error)
             VALUES(?, ?, ?, COALESCE((SELECT attempts FROM pending_user_cleanup WHERE uid=?), 0) + 1, ?)",
            params![uid, user_dir, now, uid, error],
        );
    }

    /// 列出所有待重试的用户目录清理任务。
    pub fn list_pending_cleanups(&self) -> Vec<(i64, String, i64, Option<String>)> {
        let conn = self.db.conn_lock();
        let mut stmt = match conn
            .prepare("SELECT uid, user_dir, attempts, last_error FROM pending_user_cleanup")
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_pending_cleanups prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_pending_cleanups query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 清理成功后从补偿队列移除。
    pub fn remove_pending_cleanup(&self, uid: i64) {
        let conn = self.db.conn_lock();
        let _ = conn.execute("DELETE FROM pending_user_cleanup WHERE uid=?", params![uid]);
    }

    // ── 配额计数（Phase 3 用，Phase 1 建空壳）──────────────────

    /// Phase 3 (P1): 设置某 used_* 字段的绝对值（供 BotManager 5s flush 用）。
    /// field 必须在白名单内防 SQL 注入。
    pub fn set_used(&self, uid: i64, field: &str, value: i64) -> anyhow::Result<()> {
        const ALLOWED: &[&str] = &[
            "used_upload_bytes",
            "used_download_bytes",
            "used_media_bytes",
            "used_msg_today",
            "used_media_count",
        ];
        if !ALLOWED.contains(&field) {
            anyhow::bail!("非法 used 字段: {}", field);
        }
        // field 已白名单校验，安全拼接
        let sql = format!("UPDATE app_users SET {}=? WHERE id=?", field);
        let conn = self.db.conn_lock();
        conn.execute(&sql, params![value, uid])?;
        Ok(())
    }

    /// 在一个事务中重置已跨日的上传、下载和消息计数。
    /// 返回值依次表示 upload/download/message 是否发生了重置。
    pub fn reset_daily_quotas_if_new_day(&self, uid: i64) -> anyhow::Result<(bool, bool, bool)> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut conn = self.db.conn_lock();
        let tx = conn.transaction()?;
        let upload_changed = tx.execute(
            "UPDATE app_users
             SET used_upload_bytes = 0, used_upload_date = ?
             WHERE id=? AND (used_upload_date IS NULL OR used_upload_date <> ?)",
            params![today, uid, today],
        )?;
        let download_changed = tx.execute(
            "UPDATE app_users
             SET used_download_bytes = 0, used_download_date = ?
             WHERE id=? AND (used_download_date IS NULL OR used_download_date <> ?)",
            params![today, uid, today],
        )?;
        let message_changed = tx.execute(
            "UPDATE app_users
             SET used_msg_today = 0, used_msg_date = ?
             WHERE id=? AND (used_msg_date IS NULL OR used_msg_date <> ?)",
            params![today, uid, today],
        )?;
        tx.commit()?;
        Ok((
            upload_changed > 0,
            download_changed > 0,
            message_changed > 0,
        ))
    }

    // ── IP 封禁 ──────────────────────────────────────────────

    /// 封禁 IP。expires_at 为 None 表示永久封禁。
    pub fn ban_ip(
        &self,
        ip: &str,
        reason: &str,
        banned_by: &str,
        days: Option<i64>,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        let expires_at = days.map(|d| {
            chrono::Utc::now()
                .checked_add_signed(chrono::Duration::days(d))
                .unwrap_or_else(chrono::Utc::now)
                .to_rfc3339()
        });
        conn.execute(
            "INSERT INTO ip_bans(ip, reason, banned_by, created_at, expires_at) VALUES(?, ?, ?, ?, ?)",
            params![ip, reason, banned_by, now, expires_at],
        )?;
        Ok(())
    }

    /// 解封 IP
    pub fn unban_ip(&self, ip: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute("DELETE FROM ip_bans WHERE ip=?", params![ip])?;
        Ok(())
    }

    /// 检查 IP 是否被封禁（含过期自动清理）
    ///
    /// NOTE (M16): M16 改造后 ip_ban_check 中间件改用进程内缓存（IpBanCache），
    ///   不再每请求调用此方法。保留为公共 API 以备未来 CLI 工具或管理脚本使用，
    ///   并提供"惰性清理已过期 ip_bans 记录"的副作用（避免 DB 累积过期记录）。
    #[allow(dead_code)]
    pub fn is_ip_banned(&self, ip: &str) -> bool {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        // 查有效封禁记录
        let banned: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM ip_bans WHERE ip=? AND (expires_at IS NULL OR expires_at > ?)",
                params![ip, now],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        // 惰性清理已过期记录
        let _ = conn.execute(
            "DELETE FROM ip_bans WHERE expires_at IS NOT NULL AND expires_at < ?",
            params![now],
        );
        banned.is_some()
    }

    /// 列出所有 IP 封禁记录
    ///
    /// 列出前主动清理已过期记录，且只返回未过期记录。
    pub fn list_ip_bans(&self) -> Vec<models::IpBan> {
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        // 1. 主动清理已过期记录
        let _ = conn.execute(
            "DELETE FROM ip_bans WHERE expires_at IS NOT NULL AND expires_at < ?",
            params![now],
        );
        // 2. 只返回未过期记录（expires_at IS NULL 表示永久封禁）
        let mut stmt = match conn.prepare(
            "SELECT id, ip, reason, banned_by, created_at, expires_at FROM ip_bans WHERE expires_at IS NULL OR expires_at > ? ORDER BY id DESC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_ip_bans prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map(params![now], |row| {
            Ok(models::IpBan {
                id: row.get(0)?,
                ip: row.get(1)?,
                reason: row.get(2)?,
                banned_by: row.get(3)?,
                created_at: row.get(4)?,
                expires_at: row.get(5)?,
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_ip_bans query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    // ── 设备令牌（浏览器自动登录）────────────────────────────

    /// 创建设备令牌（持久化 refresh token）
    ///
    /// DB 中只存 SHA-256(token)，不存明文。
    ///   明文 token 仅在此函数返回值中存在，由调用方写入 HttpOnly Cookie 返回客户端。
    pub fn create_device_token(
        &self,
        uid: i64,
        device_name: &str,
        days: i64,
    ) -> anyhow::Result<String> {
        let token = crate::crypto::random_hex(32);
        let token_hash = crate::crypto::sha256_hex(token.as_bytes());
        let now = chrono::Utc::now().to_rfc3339();
        let expires_at = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::days(days))
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339();
        let conn = self.db.conn_lock();
        conn.execute(
            "INSERT INTO device_tokens(token_hash, user_id, device_name, created_at, expires_at) VALUES(?, ?, ?, ?, ?)",
            params![token_hash, uid, device_name, now, expires_at],
        )?;
        Ok(token)
    }

    /// 校验设备令牌，成功返回 uid
    ///
    /// 输入明文 token，先 SHA-256 哈希再按 token_hash 查询/更新。
    pub fn verify_device_token(&self, token: &str) -> Option<i64> {
        let token_hash = crate::crypto::sha256_hex(token.as_bytes());
        let conn = self.db.conn_lock();
        let now = chrono::Utc::now().to_rfc3339();
        let res: Option<i64> = conn
            .query_row(
                "SELECT d.user_id FROM device_tokens d
                 INNER JOIN app_users u ON u.id = d.user_id AND u.status = 'active'
                 WHERE d.token_hash=? AND d.expires_at > ?",
                params![token_hash, now],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if res.is_some() {
            // 更新 last_used_at
            let _ = conn.execute(
                "UPDATE device_tokens SET last_used_at=? WHERE token_hash=?",
                params![now, token_hash],
            );
        }
        // 惰性清理过期 token
        let _ = conn.execute(
            "DELETE FROM device_tokens WHERE expires_at < ?",
            params![now],
        );
        res
    }

    /// 列出某用户的所有设备令牌
    ///
    /// SELECT token_hash 列（原 token 列已重命名）。
    ///   models::DeviceToken.token 字段语义改为 hash（前端不应依赖此字段，
    ///   仅用于显示设备列表的元数据：device_name / created_at / expires_at / last_used_at）。
    pub fn list_device_tokens(&self, uid: i64) -> Vec<models::DeviceToken> {
        let conn = self.db.conn_lock();
        let mut stmt = match conn.prepare(
            "SELECT token_hash, user_id, device_name, created_at, expires_at, last_used_at
             FROM device_tokens WHERE user_id=? ORDER BY created_at DESC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[SYSDB] list_device_tokens prepare 失败: {}", e);
                return Vec::new();
            }
        };
        let rows = match stmt.query_map(params![uid], |row| {
            Ok(models::DeviceToken {
                token: row.get(0)?,
                uid: row.get(1)?,
                device_name: row.get(2)?,
                created_at: row.get(3)?,
                expires_at: row.get(4)?,
                last_used_at: row.get(5)?,
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[SYSDB] list_device_tokens query_map 失败: {}", e);
                return Vec::new();
            }
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 撤销（删除）设备令牌
    ///
    /// 输入明文 token，先 SHA-256 哈希再按 token_hash 删除。
    pub fn revoke_device_token(&self, token: &str) -> anyhow::Result<()> {
        let token_hash = crate::crypto::sha256_hex(token.as_bytes());
        let conn = self.db.conn_lock();
        conn.execute(
            "DELETE FROM device_tokens WHERE token_hash=?",
            params![token_hash],
        )?;
        Ok(())
    }

    /// 查询明文 token 对应的 uid（仅查询，不更新 last_used_at）。
    ///   用于 api_revoke_device_token 校验所有权——撤销前确认 token 属于当前用户。
    ///   verify_device_token 会更新 last_used_at 并惰性清理，本方法仅做只读查询。
    pub fn get_device_token_owner(&self, token: &str) -> Option<i64> {
        let token_hash = crate::crypto::sha256_hex(token.as_bytes());
        let conn = self.db.conn_lock();
        conn.query_row(
            "SELECT user_id FROM device_tokens WHERE token_hash=?",
            params![token_hash],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 撤销某用户的所有设备令牌（改密/禁用时调用）
    pub fn revoke_all_device_tokens(&self, uid: i64) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute("DELETE FROM device_tokens WHERE user_id=?", params![uid])?;
        Ok(())
    }

    // ── 邮箱 ──────────────────────────────────────────────────

    /// 设置用户邮箱
    pub fn set_user_email(&self, uid: i64, email: &str) -> anyhow::Result<()> {
        let conn = self.db.conn_lock();
        conn.execute(
            "UPDATE app_users SET email=? WHERE id=?",
            params![email, uid],
        )?;
        Ok(())
    }

    /// 获取用户邮箱
    pub fn get_user_email(&self, uid: i64) -> Option<String> {
        let conn = self.db.conn_lock();
        conn.query_row(
            "SELECT email FROM app_users WHERE id=?",
            params![uid],
            |row| row.get(0),
        )
        .ok()
        .flatten()
        .filter(|s: &String| !s.is_empty())
    }
}

/// 把 rusqlite 行映射为 AppUser（列顺序须与查询 SQL 一致）
fn row_to_app_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<models::AppUser> {
    Ok(models::AppUser {
        id: row.get(0)?,
        username: row.get(1)?,
        password_hash: row.get(2)?,
        salt: row.get(3)?,
        iterations: row.get(4)?,
        role: row.get(5)?,
        status: row.get(6)?,
        quota_upload_bytes: row.get(7)?,
        used_upload_bytes: row.get(8)?,
        used_upload_date: row.get(9)?,
        quota_download_bytes: row.get(10)?,
        used_download_bytes: row.get(11)?,
        used_download_date: row.get(12)?,
        quota_media_bytes: row.get(13)?,
        used_media_bytes: row.get(14)?,
        quota_msg_per_day: row.get(15)?,
        used_msg_today: row.get(16)?,
        used_msg_date: row.get(17)?,
        quota_media_count: row.get(18)?,
        used_media_count: row.get(19)?,
        allow_upload: row.get::<_, i64>(20)?,
        allow_webdav: row.get::<_, i64>(21)?,
        allow_custom_webdav: row.get::<_, i64>(22)?,
        email: row.get(23)?,
        agreed_terms_ver: row.get(24)?,
        agreed_terms_at: row.get(25)?,
        created_at: row.get(26)?,
        last_login_at: row.get(27)?,
    })
}
