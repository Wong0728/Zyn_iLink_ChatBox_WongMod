// BotManager: lazily-loaded per-user bot registry + Phase 3 quota/feature/rate-limit.
//
// Owns one WeChatiLinkBot (and its PushHub) per loaded user. Bots are created
// on first request via `get_or_create_bot`, which runs the (blocking, runtime-
// building) `WeChatiLinkBot::new_for_user` inside `spawn_blocking` to avoid the
// "Cannot drop a runtime in a runtime context" panic from reqwest::blocking::Client.
//
// Phase 3 (P1/S2/§7.3): per-uid in-memory QuotaCounters (AtomicU64 × 5) + 5s
// flush to system.db; check_feature (system ∧ user); check_rate_limit (generalized
// from login_attempts, key = "{ip|uid}|{action}").

use crate::bot::WeChatiLinkBot;
use crate::models::{AppUser, SessionState};
use crate::push::PushHub;
use crate::storage::SystemDatabase;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ── Phase 3: 配额维度 / 功能开关 / 错误类型 ────────────────────

/// 配额维度（5 个，与 §7.2 表一致）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaDim {
    UploadBytes,
    DownloadBytes,
    MediaBytes,
    MsgToday,
    MediaCount,
}

impl QuotaDim {
    /// 前端 / API 响应中用的字段名（与 §7.2 表第一列对齐）
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaDim::UploadBytes => "upload_bytes",
            QuotaDim::DownloadBytes => "download_bytes",
            QuotaDim::MediaBytes => "media_bytes",
            QuotaDim::MsgToday => "msg_per_day",
            QuotaDim::MediaCount => "media_count",
        }
    }

    /// SystemDatabase::set_used / inc_used 的白名单字段名
    pub fn db_used_field(self) -> &'static str {
        match self {
            QuotaDim::UploadBytes => "used_upload_bytes",
            QuotaDim::DownloadBytes => "used_download_bytes",
            QuotaDim::MediaBytes => "used_media_bytes",
            QuotaDim::MsgToday => "used_msg_today",
            QuotaDim::MediaCount => "used_media_count",
        }
    }

    /// AppUser 上对应的 quota_* 字段值（i64，0 = 用系统默认）
    pub fn appuser_quota(self, u: &AppUser) -> i64 {
        match self {
            QuotaDim::UploadBytes => u.quota_upload_bytes,
            QuotaDim::DownloadBytes => u.quota_download_bytes,
            QuotaDim::MediaBytes => u.quota_media_bytes,
            QuotaDim::MsgToday => u.quota_msg_per_day,
            QuotaDim::MediaCount => u.quota_media_count,
        }
    }

    /// system_settings 中默认配额的键名
    pub fn system_default_key(self) -> &'static str {
        match self {
            QuotaDim::UploadBytes => "default_quota_upload_bytes",
            QuotaDim::DownloadBytes => "default_quota_download_bytes",
            QuotaDim::MediaBytes => "default_quota_media_bytes",
            QuotaDim::MsgToday => "default_quota_msg_per_day",
            QuotaDim::MediaCount => "default_quota_media_count",
        }
    }

    /// 人类可读单位（用于 U9 错误消息）
    pub fn unit(self) -> &'static str {
        match self {
            QuotaDim::UploadBytes | QuotaDim::DownloadBytes | QuotaDim::MediaBytes => "字节",
            QuotaDim::MsgToday => "条/天",
            QuotaDim::MediaCount => "个",
        }
    }
}

/// 功能开关（§7.3，3 个固定开关）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Upload,
    Webdav,
    CustomWebdav,
}

impl Feature {
    /// AppUser 上对应的 allow_* 字段值（i64，0/1）
    fn appuser_flag(self, u: &AppUser) -> bool {
        match self {
            Feature::Upload => u.allow_upload != 0,
            Feature::Webdav => u.allow_webdav != 0,
            Feature::CustomWebdav => u.allow_custom_webdav != 0,
        }
    }

    /// system_settings 中默认开关的键名
    fn system_default_key(self) -> &'static str {
        match self {
            Feature::Upload => "default_allow_upload",
            Feature::Webdav => "default_allow_webdav",
            Feature::CustomWebdav => "default_allow_custom_webdav",
        }
    }
}

/// 配额超限错误（§7.2 + U9：携带人类可读 message）
#[derive(Debug)]
pub struct QuotaExceeded {
    pub dim: QuotaDim,
    pub quota: u64,
    pub used: u64,
    pub message: String,
}

impl QuotaExceeded {
    pub fn new(dim: QuotaDim, quota: u64, used: u64) -> Self {
        // U9: 错误响应加 message 字段，前端直接 toast 免再拼文案
        let msg = match dim {
            QuotaDim::MsgToday => format!("今日消息已达上限 ({}/{})", used, quota),
            _ => format!(
                "{} 配额超限：已用 {} / 上限 {} {}",
                dim.as_str(),
                used,
                quota,
                dim.unit()
            ),
        };
        Self {
            dim,
            quota,
            used,
            message: msg,
        }
    }
}

impl std::fmt::Display for QuotaExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QuotaExceeded {}

/// 配额内存计数器（P1：5 个 AtomicU64，5s flush 到 system.db）
///
/// 设计取舍：
/// - 内存值是“权威读”（reserve_quota 在同一把锁中检查并预留）
/// - DB 值是"持久态"（5s flush 覆盖写，crash 丢最近 5s 计数可接受）
/// - 启动时从 DB 加载初始值（首次 get_or_create_quota）
pub struct QuotaCounters {
    pub upload_bytes: AtomicU64,
    pub download_bytes: AtomicU64,
    pub media_bytes: AtomicU64,
    pub msg_today: AtomicU64,
    pub media_count: AtomicU64,
    update_lock: parking_lot::Mutex<()>,
}

impl QuotaCounters {
    fn from_app_user(u: &AppUser) -> Self {
        Self {
            upload_bytes: AtomicU64::new(u.used_upload_bytes.max(0) as u64),
            download_bytes: AtomicU64::new(u.used_download_bytes.max(0) as u64),
            media_bytes: AtomicU64::new(u.used_media_bytes.max(0) as u64),
            msg_today: AtomicU64::new(u.used_msg_today.max(0) as u64),
            media_count: AtomicU64::new(u.used_media_count.max(0) as u64),
            update_lock: parking_lot::Mutex::new(()),
        }
    }

    fn atomic(&self, dim: QuotaDim) -> &AtomicU64 {
        match dim {
            QuotaDim::UploadBytes => &self.upload_bytes,
            QuotaDim::DownloadBytes => &self.download_bytes,
            QuotaDim::MediaBytes => &self.media_bytes,
            QuotaDim::MsgToday => &self.msg_today,
            QuotaDim::MediaCount => &self.media_count,
        }
    }

    pub fn get(&self, dim: QuotaDim) -> u64 {
        self.atomic(dim).load(Ordering::Relaxed)
    }

    /// 原子增减（delta 可负，用于媒体删除回补）
    pub fn inc(&self, dim: QuotaDim, delta: i64) {
        let a = self.atomic(dim);
        if delta >= 0 {
            a.fetch_add(delta as u64, Ordering::Relaxed);
        } else {
            // fetch_sub 不会下溢到负数，但 AtomicU64 是无符号，需手动 clamp
            let _ = a.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(((cur as i64) + delta).max(0) as u64)
            });
        }
    }
}

// ── BotManager ──────────────────────────────────────────────

pub struct BotManager {
    bots: parking_lot::RwLock<HashMap<i64, Arc<WeChatiLinkBot>>>,
    hubs: parking_lot::RwLock<HashMap<i64, Arc<PushHub>>>,
    system_db: Arc<SystemDatabase>,
    creation: tokio::sync::Mutex<()>, // serializes bot creation to prevent duplicates
    web_port: u16,
    // Phase 3 (P1): per-uid in-memory quota counters
    quota_counters: parking_lot::RwLock<HashMap<i64, Arc<QuotaCounters>>>,
    // Phase 3 (S2): general rate-limit, key = "{ip|uid}|{action}"
    rate_limits: parking_lot::Mutex<HashMap<String, Vec<f64>>>,
}

impl BotManager {
    pub fn new(system_db: Arc<SystemDatabase>, web_port: u16) -> Arc<Self> {
        Arc::new(Self {
            bots: parking_lot::RwLock::new(HashMap::new()),
            hubs: parking_lot::RwLock::new(HashMap::new()),
            system_db,
            creation: tokio::sync::Mutex::new(()),
            web_port,
            quota_counters: parking_lot::RwLock::new(HashMap::new()),
            rate_limits: parking_lot::Mutex::new(HashMap::new()),
        })
    }

    pub fn web_port(&self) -> u16 {
        self.web_port
    }

    // 暴露 Webhook 状态供管理面板展示。
    //   每个 WeChatiLinkBot 都有自己的 WebhookDispatcher（都从同一个 env var 创建），
    //   状态相同。这里取任意一个 bot 的 dispatcher 状态即可。
    //   返回 None 表示：无 bot 在线，或所有 bot 都未配置 webhook。
    pub fn webhook_status(&self) -> Option<(Vec<String>, bool, u64)> {
        let bots = self.bots.read();
        for bot in bots.values() {
            let guard = bot.webhook_dispatcher.read();
            if let Some(ref d) = *guard {
                return Some(d.get_status());
            }
        }
        None
    }

    /// 启动 5s 配额 flush 后台任务（P1）。应在 web 启动后调用一次。
    pub fn start_quota_flush_loop(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            // ponytail: 内存计数器 + 5s flush. 上限: crash 丢最近 5s 计数.
            //   后端权威校验仍用内存值拦超限, 不影响安全. 升级路径: WAL + 批量事务.
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            interval.tick().await; // 跳过首次立即触发
            loop {
                interval.tick().await;
                me.flush_all_quota().await;
            }
        });
    }

    /// 启动限速 HashMap 定时清理任务（60s 一次）。
    ///   原实现仅按时间窗口过滤旧时间戳，但 key 本身永不删除，长期运行 + 攻击者
    ///   用海量 IP 触发会导致 HashMap 无限膨胀 → 内存耗尽。
    ///   清理策略：剔除 1 小时内无任何时间戳的 key（已超窗口且无新活动）。
    ///   应在 web 启动后调用一次。
    pub fn start_rate_limit_cleanup_loop(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await; // 跳过首次立即触发
            loop {
                tick.tick().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                // 1 小时窗口：远大于现有最大业务窗口（300s 登录限速 / 300s 改密限速）
                let cutoff = 3600.0_f64;
                let mut m = me.rate_limits.lock();
                m.retain(|_k, v| {
                    v.retain(|t| now - *t < cutoff);
                    !v.is_empty()
                });
            }
        });
    }

    /// 5s flush：遍历所有 quota_counters，把内存值覆盖写回 system.db。
    /// 用 spawn_blocking 包裹因为 SystemDatabase 是同步的。
    pub async fn flush_all_quota(&self) {
        let snapshot: Vec<(i64, Arc<QuotaCounters>)> = {
            let m = self.quota_counters.read();
            m.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        if snapshot.is_empty() {
            return;
        }
        let system_db = self.system_db.clone();
        tokio::task::spawn_blocking(move || {
            for (uid, counters) in &snapshot {
                let _guard = counters.update_lock.lock();
                match system_db.reset_daily_quotas_if_new_day(*uid) {
                    Ok((upload, download, message)) => {
                        if upload {
                            counters.upload_bytes.store(0, Ordering::Relaxed);
                        }
                        if download {
                            counters.download_bytes.store(0, Ordering::Relaxed);
                        }
                        if message {
                            counters.msg_today.store(0, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[QUOTA] uid={} 跨日重置失败: {}", uid, e);
                        continue;
                    }
                }
                for dim in [
                    QuotaDim::UploadBytes,
                    QuotaDim::DownloadBytes,
                    QuotaDim::MediaBytes,
                    QuotaDim::MsgToday,
                    QuotaDim::MediaCount,
                ] {
                    let v = counters.get(dim) as i64;
                    if let Err(e) = system_db.set_used(*uid, dim.db_used_field(), v) {
                        tracing::warn!("[QUOTA] flush uid={} {} 失败: {}", uid, dim.as_str(), e);
                    }
                }
            }
        })
        .await
        .ok();
    }

    /// 取（或首次创建）某 uid 的配额计数器。
    /// 首次创建时从 system.db 加载 used_* 初始值。
    /// 同步方法（parking_lot 锁 + SystemDatabase 同步调用，可从 async 直接调）。
    pub fn get_or_create_quota(&self, uid: i64) -> Arc<QuotaCounters> {
        // fast path
        if let Some(c) = self.quota_counters.read().get(&uid).cloned() {
            return c;
        }
        // miss：从 DB 加载
        let user = self.system_db.get_user_by_id(uid);
        let counters = match user {
            Some(u) => Arc::new(QuotaCounters::from_app_user(&u)),
            None => {
                // 用户不存在（罕见，可能是 delete 后未清理）：返回空计数器，
                // 读取失败时使用 0 上限，让 reserve_quota 安全拒绝写入。
                Arc::new(QuotaCounters {
                    upload_bytes: AtomicU64::new(0),
                    download_bytes: AtomicU64::new(0),
                    media_bytes: AtomicU64::new(0),
                    msg_today: AtomicU64::new(0),
                    media_count: AtomicU64::new(0),
                    update_lock: parking_lot::Mutex::new(()),
                })
            }
        };
        // double-check + insert
        let mut w = self.quota_counters.write();
        if let Some(c) = w.get(&uid).cloned() {
            return c;
        }
        w.insert(uid, counters.clone());
        counters
    }

    /// 解析某维度的生效配额，返回 (quota, unlimited)：
    /// - 用户级 user_q < 0（含 -1）：unlimited=true，无限制
    /// - 用户级 user_q > 0：用用户值
    /// - 用户级 user_q == 0：fallback 系统默认
    ///   - 系统默认 < 0：unlimited=true
    ///   - 系统默认 > 0：用系统默认值
    ///   - 系统默认 == 0：unlimited=true（保持向后兼容：旧数据 0 视为无限制）
    ///
    /// 配额语义清晰化。
    ///   - 正数 = 每日/累计上限
    ///   - 0 = 使用系统默认（系统默认未设 = 无限制，向后兼容）
    ///   - 负数 = 无限制（管理员可显式设 -1 表示"刻意无限制"，与"未设置"区分）
    fn effective_quota(&self, user: &AppUser, dim: QuotaDim) -> (u64, bool) {
        let user_q = dim.appuser_quota(user);
        if user_q < 0 {
            return (0, true);
        }
        if user_q > 0 {
            return (user_q as u64, false);
        }
        // user_q == 0：fallback 系统默认
        let sys = self
            .system_db
            .get_setting(dim.system_default_key())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if sys < 0 {
            return (0, true);
        }
        if sys == 0 {
            // 系统默认未设 = 无限制（向后兼容旧数据）
            return (0, true);
        }
        (sys as u64, false)
    }

    /// 原子预留一个或多个配额维度。
    ///
    /// 同一用户的跨日重置、全部上限检查和全部计数增加由一把锁保护，避免并发请求
    /// 同时通过“先检查后累加”。业务后续失败时调用 `release_quota` 回滚预留。
    pub fn reserve_quota(&self, uid: i64, deltas: &[(QuotaDim, i64)]) -> Result<(), QuotaExceeded> {
        let counters = self.get_or_create_quota(uid);
        let _guard = counters.update_lock.lock();
        let first_dim = deltas
            .first()
            .map(|(dim, _)| *dim)
            .unwrap_or(QuotaDim::MsgToday);
        match self.system_db.reset_daily_quotas_if_new_day(uid) {
            Ok((upload, download, message)) => {
                if upload {
                    counters.upload_bytes.store(0, Ordering::Relaxed);
                }
                if download {
                    counters.download_bytes.store(0, Ordering::Relaxed);
                }
                if message {
                    counters.msg_today.store(0, Ordering::Relaxed);
                }
            }
            Err(e) => {
                tracing::error!("[QUOTA] uid={} 跨日重置失败，拒绝预留: {}", uid, e);
                return Err(QuotaExceeded::new(first_dim, 0, 0));
            }
        }
        let user = match self.system_db.get_user_by_id(uid) {
            Some(u) => u,
            None => return Err(QuotaExceeded::new(first_dim, 0, 0)),
        };
        for (dim, delta) in deltas {
            let (quota, unlimited) = self.effective_quota(&user, *dim);
            if unlimited {
                continue;
            }
            let cur = counters.get(*dim);
            let after = (cur as i64 + *delta).max(0) as u64;
            if after > quota {
                return Err(QuotaExceeded::new(*dim, quota, after));
            }
        }
        for (dim, delta) in deltas {
            counters.inc(*dim, *delta);
        }
        Ok(())
    }

    /// 释放先前的配额预留，或回收已删除媒体的当前存储占用。
    pub fn release_quota(&self, uid: i64, deltas: &[(QuotaDim, i64)]) {
        let counters = self.get_or_create_quota(uid);
        let _guard = counters.update_lock.lock();
        for (dim, delta) in deltas {
            counters.inc(*dim, -*delta);
        }
    }

    /// 以媒体元数据为权威源重算当前存储占用，并同步内存与 system.db。
    pub fn reconcile_media_usage(&self, uid: i64, bytes: i64, count: i64) {
        let counters = self.get_or_create_quota(uid);
        let _guard = counters.update_lock.lock();
        let bytes = bytes.max(0) as u64;
        let count = count.max(0) as u64;
        counters.media_bytes.store(bytes, Ordering::Relaxed);
        counters.media_count.store(count, Ordering::Relaxed);
        if let Err(e) =
            self.system_db
                .set_used(uid, QuotaDim::MediaBytes.db_used_field(), bytes as i64)
        {
            tracing::warn!("[QUOTA] uid={} 重算媒体字节失败: {}", uid, e);
        }
        if let Err(e) =
            self.system_db
                .set_used(uid, QuotaDim::MediaCount.db_used_field(), count as i64)
        {
            tracing::warn!("[QUOTA] uid={} 重算媒体数量失败: {}", uid, e);
        }
    }

    /// 功能开关校验：系统默认 ∧ 用户级（§7.3）。
    /// 系统关 → false（前端隐藏入口 + 后端 403）。
    pub fn check_feature(&self, uid: i64, feature: Feature) -> bool {
        let user = match self.system_db.get_user_by_id(uid) {
            Some(u) => u,
            None => return false,
        };
        let sys_default = self
            .system_db
            .get_setting(feature.system_default_key())
            .map(|value| crate::storage::setting_truthy(&value))
            .unwrap_or(true); // 系统默认未设 = 默认开
        sys_default && feature.appuser_flag(&user)
    }

    /// 通用限速（S2 扩展自 login_attempts）。
    /// key 格式："{ip}|{action}" 或 "{uid}|{action}"
    /// 返回 true = 已超限（应返回 429）；false = 未超限（已记录本次）。
    /// 调用方应在"失败"时调用此函数记录；成功时调 clear_rate_limit 清除。
    /// 也可在"每次请求"时调用，按业务语义选择。
    pub fn check_rate_limit(&self, key: &str, max: usize, window_secs: f64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mut m = self.rate_limits.lock();
        let window = m.entry(key.to_string()).or_default();
        window.retain(|t| now - *t < window_secs);
        if window.len() >= max {
            true
        } else {
            window.push(now);
            false
        }
    }

    /// 仅检查限流状态，不增加计数。
    ///   用于"先检查账号是否锁定，再决定是否消耗一次登录尝试"的场景。
    ///   与 check_rate_limit 的区别：不调用 `window.push(now)`，
    ///   避免每次"检查锁定状态"都增加一次失败计数。
    ///   返回 true = 已超限；false = 未超限。
    pub fn is_rate_limited(&self, key: &str, max: usize, window_secs: f64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mut m = self.rate_limits.lock();
        let window = m.entry(key.to_string()).or_default();
        window.retain(|t| now - *t < window_secs);
        window.len() >= max
    }

    /// 清除限速记录（成功时调用）。
    /// key 是精确键；若要按前缀清（如某 uid 所有 action），用 clear_rate_limit_prefix。
    pub fn clear_rate_limit(&self, key: &str) {
        self.rate_limits.lock().remove(key);
    }

    /// 按前缀清除限速记录（如 "{uid}|" 清该 uid 所有 action）。
    pub fn clear_rate_limit_prefix(&self, prefix: &str) {
        let mut m = self.rate_limits.lock();
        m.retain(|k, _| !k.starts_with(prefix));
    }

    /// Lazily get-or-create the bot for a user. Creation runs in spawn_blocking
    /// (reqwest::blocking::Client cannot be built in async context).
    /// Concurrent first-requests for the same uid are serialized via `creation`
    /// mutex + double-check so only ONE bot is ever created per uid.
    ///
    /// 改返回 `anyhow::Result<Arc<WeChatiLinkBot>>`。
    ///   原 `.expect("bot creation task panicked")` 在用户 DB 初始化失败或 spawn_blocking
    ///   任务 panic 时会再次 panic，配合 `panic = "abort"` 直接拖垮整个服务。
    ///   现在返回 Err 由调用方决定如何降级（HTTP 500 / 跳过该用户），不影响其他用户。
    pub async fn get_or_create_bot(&self, uid: i64) -> anyhow::Result<Arc<WeChatiLinkBot>> {
        // fast path
        if let Some(b) = self.bots.read().get(&uid).cloned() {
            return Ok(b);
        }
        // ponytail: 创作串行化；小部署可接受，扩展 → 每 uid 独立 OnceCell
        let _g = self.creation.lock().await;
        // double-check after acquiring creation lock
        if let Some(b) = self.bots.read().get(&uid).cloned() {
            return Ok(b);
        }
        // create in spawn_blocking (reqwest::blocking::Client builds a runtime)
        let bot = match tokio::task::spawn_blocking(move || WeChatiLinkBot::new_for_user(uid)).await
        {
            Ok(Ok(bot)) => bot,
            Ok(Err(e)) => {
                tracing::error!("[BOT_MANAGER] uid={} bot 创建失败: {:#}", uid, e);
                return Err(e);
            }
            Err(join_err) => {
                tracing::error!("[BOT_MANAGER] uid={} bot 创建任务 panic: {}", uid, join_err);
                return Err(anyhow::anyhow!("bot 创建任务 panic: {}", join_err));
            }
        };
        self.bots.write().insert(uid, bot.clone());
        tracing::info!("[BOT_MANAGER] 已为用户 uid={} 创建 bot 实例", uid);
        Ok(bot)
    }

    /// Get-or-create the PushHub for a user. Sync — caller passes the already-obtained
    /// bot so we can read bot.broker. Hub creation (PushHub::new) is cheap (spawns tokio tasks).
    pub fn get_or_create_hub(&self, uid: i64, bot: &Arc<WeChatiLinkBot>) -> Arc<PushHub> {
        if let Some(h) = self.hubs.read().get(&uid).cloned() {
            return h;
        }
        let hub = PushHub::new(bot.broker.clone());
        self.hubs.write().insert(uid, hub.clone());
        hub
    }

    /// Broadcast an event to all existing user bot brokers (for global notifications).
    /// Only reaches users who have an active bot loaded in memory.
    pub fn broadcast_to_all_bots(&self, event_type: &str, data: serde_json::Value) {
        let bots = self.bots.read();
        for bot in bots.values() {
            bot.broker.publish(event_type, data.clone());
        }
    }

    /// 向指定用户当前已加载的 Bot 推送事件；未加载时由持久化通知兜底。
    pub fn publish_to_loaded_bot(
        &self,
        uid: i64,
        event_type: &str,
        data: serde_json::Value,
    ) -> bool {
        let bots = self.bots.read();
        let Some(bot) = bots.get(&uid) else {
            return false;
        };
        bot.broker.publish(event_type, data);
        true
    }

    /// Remove a user's bot+hub from the maps (admin disable/delete). Best-effort:
    /// call bot.stop() to halt background threads. Do NOT hold locks across bot.stop()
    /// (it joins threads → blocking). Clone the Arc out, drop the map entry, then stop.
    ///
    /// 此同步版本仅供 admin CLI（独立进程，无 tokio runtime）使用。
    ///   HTTP handler / async 上下文请改用 `unload_bot_async`，避免阻塞 tokio worker。
    #[allow(dead_code)]
    pub fn unload_bot(&self, uid: i64) {
        let bot = self.bots.write().remove(&uid);
        self.hubs.write().remove(&uid);
        // 清理该 uid 的限速记录与配额计数器
        self.clear_rate_limit_prefix(&format!("{}|", uid));
        self.quota_counters.write().remove(&uid);
        if let Some(bot) = bot {
            // stop background threads (blocking — caller should be admin CLI, acceptable)
            // ponytail: block_in_place 仅在 multi-thread tokio runtime 上下文合法；
            // 此函数为同步入口（admin CLI），直接同步调用 stop()
            // 以避免在非 runtime 上下文触发 block_in_place panic。stop() 自身会 join 后台
            // 线程，调用方需知其为阻塞操作；如需异步化，请用 unload_bot_async。
            bot.stop();
            tracing::info!("[BOT_MANAGER] 已卸载用户 uid={} 的 bot 实例", uid);
        }
    }

    /// unload_bot 的异步版本。
    ///   bot.stop() 内部含 `thread::join`，在 HTTP handler 中同步调用会阻塞 tokio worker。
    ///   此版本先从 map 移除并清理内存状态（快速），再把 stop() 投递到 spawn_blocking。
    ///   注意：map 移除是同步的，新请求会走 get_or_create_bot 重建 bot；旧 bot 的 stop
    ///   在后台线程中完成，最多阻塞 1-2s（join 后台轮询线程）。
    pub async fn unload_bot_async(&self, uid: i64) {
        let bot = self.bots.write().remove(&uid);
        self.hubs.write().remove(&uid);
        self.clear_rate_limit_prefix(&format!("{}|", uid));
        self.quota_counters.write().remove(&uid);
        if let Some(bot) = bot {
            // 把 thread::join 投递到阻塞线程池，不占 tokio worker
            let _ = tokio::task::spawn_blocking(move || {
                bot.stop();
            })
            .await;
            tracing::info!("[BOT_MANAGER] 已异步卸载用户 uid={} 的 bot 实例", uid);
        }
    }

    /// ponytail HIGH-4: 列出当前已加载的 bot uid（供 main.rs 后台轮询卸载非 active 用户）
    pub fn list_loaded_uids(&self) -> Vec<i64> {
        self.bots.read().keys().copied().collect()
    }

    /// 改造方案 §二：查询指定用户的 Bot 状态（联系人数量 + 会话状态）。
    pub fn bot_status(&self, uid: i64) -> serde_json::Value {
        let bots = self.bots.read();
        if let Some(bot) = bots.get(&uid) {
            let contacts_total = bot.list_users().len();
            let session_state = match &*bot.session_status.read() {
                SessionState::Active => "active",
                SessionState::SessionExpired => "session_expired",
                SessionState::Disconnected => "disconnected",
                SessionState::Reauthing => "reauthing",
            };
            serde_json::json!({
                "has_bot": true,
                "contacts_total": contacts_total,
                "session_state": session_state,
            })
        } else {
            serde_json::json!({
                "has_bot": false,
                "contacts_total": 0,
                "session_state": "disconnected",
            })
        }
    }

    /// 改造方案 §三：更新指定用户的配额。由管理员 API 路由保护。
    pub fn update_user_quota(
        &self,
        username: &str,
        quota: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let user = self
            .system_db
            .get_user_by_username(username)
            .ok_or_else(|| anyhow::anyhow!("用户不存在"))?;
        self.system_db.batch_update_user_quota(user.id, quota)
    }

    /// 改造方案 §三：更新指定用户的功能开关。
    pub fn update_user_features(
        &self,
        username: &str,
        features: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let user = self
            .system_db
            .get_user_by_username(username)
            .ok_or_else(|| anyhow::anyhow!("用户不存在"))?;
        self.system_db.batch_update_user_features(user.id, features)
    }
}
