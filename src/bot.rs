// iLink 微信机器人核心逻辑
// 衍生/开发 请 标注 原仓库 "https://github.com/zynsync/Zyn-iLink-ChatBox" 与原作者。

use crate::config::*;
use crate::crypto;
use crate::event_broker::EventBroker;
use crate::media;
use crate::models::*;
use crate::storage::Database;
use crate::storage_backend::{LocalFsBackend, TieredStorage, WebDavStorageBackend};
use crate::webdav::WebDavClient;
use crate::webhook::WebhookDispatcher;

use base64::Engine;
use parking_lot::{Mutex, RwLock};
use reqwest::blocking::Client as HttpClient;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

const ILINK_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CDN_BASE: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
const MAX_DECRYPT_SIZE: usize = 100 * 1024 * 1024; // 100MB

/// 校验 cache_key 是否合法：仅允许 hex 字符，长度 1-128，杜绝 . .. / \ 等路径遍历
pub(crate) fn is_valid_cache_key(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// UTF-8 安全的字节截断：若 max_bytes 落在多字节字符中间则向前回退到字符边界
pub(crate) fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// S32: 简单的计数信号量（基于 std::sync::Mutex + Condvar）
///   用于限制 spawn_retry_send 并发数，避免 outbound-scan 批量恢复时
///   瞬间 spawn 大量长时间（最长 380s+）的发送线程导致线程爆炸
struct CountingSemaphore {
    limit: usize,
    count: std::sync::Mutex<usize>,
    cvar: std::sync::Condvar,
}

impl CountingSemaphore {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            count: std::sync::Mutex::new(0),
            cvar: std::sync::Condvar::new(),
        }
    }
    /// 阻塞直到获得许可；返回的 RetryPermit 在 drop 时自动释放许可
    fn acquire(&self) -> RetryPermit<'_> {
        let mut c = self.count.lock().unwrap();
        while *c >= self.limit {
            c = self.cvar.wait(c).unwrap();
        }
        *c += 1;
        RetryPermit { sem: self }
    }
}

/// RAII 许可：drop 时递减计数并唤醒一个等待者
struct RetryPermit<'a> {
    sem: &'a CountingSemaphore,
}

impl Drop for RetryPermit<'_> {
    fn drop(&mut self) {
        let mut c = self.sem.count.lock().unwrap();
        *c -= 1;
        self.sem.cvar.notify_one();
    }
}

/// SSRF 防护：禁止 IP 字面量（含 IPv6）与 localhost / 内网域名后缀
/// 限制 scheme 只允许 http/https；对域名做 DNS 解析，拒绝解析到内网 IP 的域名（DNS rebinding 基础防护）
/// 注意：DNS 解析与实际请求之间仍存在 TOCTOU 窗口，但比无防护强
pub(crate) fn is_ssrf_safe_url(raw: &str) -> bool {
    ssrf_safe_resolve(raw).is_some()
}

fn webdav_private_allowlist() -> Vec<String> {
    std::env::var("ILINK_WEBDAV_PRIVATE_ALLOWLIST")
        .unwrap_or_default()
        .split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn webdav_host_or_ip_allowed(host: &str, ip: Option<std::net::IpAddr>) -> bool {
    let allowlist = webdav_private_allowlist();
    let host = host.to_ascii_lowercase();
    allowlist.iter().any(|entry| {
        entry == &host
            || ip
                .map(|address| entry == &address.to_string().to_ascii_lowercase())
                .unwrap_or(false)
    })
}

/// SSRF DNS 重绑定防护：解析 URL + DNS 解析 + 内网 IP 校验，返回校验通过的公网 IP，
/// 供调用方通过 reqwest resolve() 固定 host→IP 映射，消除 TOCTOU 窗口。
///
/// 返回 (host, port, safe_ip)：
///   - host: URL 中的 host，用于 resolve() 的 domain 参数
///   - port: URL 端口（默认 80/443）
///   - safe_ip: 校验通过的公网 IP（首次 DNS 解析结果）
///
/// 校验规则与 is_ssrf_safe_url 完全一致（任一解析结果为内网 IP 即拒绝），
/// 但额外返回第一个公网 IP 供调用方固定。
pub(crate) fn ssrf_safe_resolve(raw: &str) -> Option<(String, u16, std::net::IpAddr)> {
    let url = url::Url::parse(raw).ok()?;
    // 限制 scheme 只允许 http/https
    match url.scheme() {
        "http" | "https" => {}
        _ => return None,
    }
    let host = url.host_str()?.to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    // IP 字面量仅允许部署 owner 通过环境变量显式加入私网白名单。
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return webdav_host_or_ip_allowed(&host, Some(ip)).then_some((host, port, ip));
    }
    // 禁止 localhost / 内网域名后缀
    let lower = host.to_lowercase();
    let private_name = lower == "localhost"
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower.ends_with(".lan")
        || lower.ends_with(".home")
        || lower.ends_with(".corp")
        || lower.ends_with(".intranet")
        || lower.ends_with(".private");
    if private_name && !webdav_host_or_ip_allowed(&host, None) {
        return None;
    }
    // DNS rebinding 防护：解析域名，若任一解析结果为内网 IP 则拒绝
    use std::net::ToSocketAddrs;
    let socket_addrs = format!("{}:{}", host, port);
    let addrs = socket_addrs.to_socket_addrs().ok()?;
    // 取第一个公网 IP（resolve() 只接受单个 SocketAddr），
    // 同时保持原 is_ssrf_safe_url 语义：任一解析结果为内网 IP 即拒绝
    let mut first_public: Option<std::net::IpAddr> = None;
    for addr in addrs {
        if !is_public_ip(&addr.ip()) && !webdav_host_or_ip_allowed(&host, Some(addr.ip())) {
            return None;
        }
        if first_public.is_none() {
            first_public = Some(addr.ip());
        }
    }
    first_public.map(|ip| (host, port, ip))
}

/// 判断 IP 是否为公网 IP（非内网/环回/链路本地/组播/IPv4-mapped 内网等）
fn is_public_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            if v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
            {
                return false;
            }
            // 额外拦截 0.0.0.0
            if v4.octets() == [0, 0, 0, 0] {
                return false;
            }
            true
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return false;
            }
            // 拦截 IPv4-mapped IPv6 (::ffff:0.0.0.0 到 ::ffff:255.255.255.255) 中的内网地址
            if let Some(v4) = v6.to_ipv4_mapped() {
                if !is_public_ip(&std::net::IpAddr::V4(v4)) {
                    return false;
                }
            }
            // ULA (fc00::/7) 等内网 IPv6
            let segs = v6.segments();
            if (segs[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            true
        }
    }
}

/// S30: 流式读取响应体并在读取过程中校验大小，避免无 Content-Length 时 OOM
/// 使用 copy_to + 自定义 Write，边写边累计，超限立即返回错误
fn read_response_with_size_limit(
    resp: reqwest::blocking::Response,
    limit: usize,
) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Write;
    struct SizeLimitedCollector {
        buf: Vec<u8>,
        limit: usize,
    }
    impl Write for SizeLimitedCollector {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<usize> {
            let new_len = self.buf.len().saturating_add(chunk.len());
            if new_len > self.limit {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("response size {} exceeds limit {}", new_len, self.limit),
                ));
            }
            self.buf.extend_from_slice(chunk);
            Ok(chunk.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut collector = SizeLimitedCollector {
        buf: Vec::new(),
        limit,
    };
    let mut r = resp;
    r.copy_to(&mut collector).map_err(std::io::Error::other)?;
    Ok(collector.buf)
}

struct SaveTask {
    user_id: String,
    messages: Vec<serde_json::Value>,
    max_per_user: usize,
    db: Arc<Database>,
}

struct PrefetchTask {
    bot: Arc<WeChatiLinkBot>,
    cache_key: String,
    cdn_info: serde_json::Value,
    filename: String,
    user_id: String,
}

impl PrefetchTask {
    fn run(self) {
        let bot = &self.bot;
        if bot.get_cached_media(&self.cache_key).is_some() {
            bot.broker.publish(
                crate::event_broker::EVENT_MEDIA_CACHE_UPDATE,
                serde_json::json!({
                    "cache_key": self.cache_key,
                    "status": "cached",
                    "user_id": self.user_id,
                }),
            );
            return;
        }
        let result = bot.download_media(&self.cdn_info, &self.filename, &self.user_id);
        let status = if result.is_some() { "ready" } else { "failed" };
        let mut data = serde_json::json!({
            "cache_key": self.cache_key,
            "status": status,
            "user_id": self.user_id,
        });
        if let Some(url) = bot.webdav_url_for_cache_key(
            &self.cache_key,
            media::derive_ext("", &self.filename).as_deref(),
        ) {
            if let Some(obj) = data.as_object_mut() {
                obj.insert("webdav_url".into(), serde_json::Value::String(url));
            }
        }
        bot.broker
            .publish(crate::event_broker::EVENT_MEDIA_CACHE_UPDATE, data);
    }
}

pub struct WeChatiLinkBot {
    // 核心凭证
    pub token: RwLock<Option<String>>,
    pub bot_id: RwLock<Option<String>>,
    pub user_id: RwLock<Option<String>>,
    cursor: RwLock<String>,

    // 会话管理
    pub context_tokens: RwLock<HashMap<String, String>>,
    pub current_user: RwLock<Option<String>>,
    pub user_token_map: RwLock<HashMap<String, String>>,
    pub bot_accounts: RwLock<HashMap<String, serde_json::Value>>,

    // 消息
    pub messages: RwLock<Vec<serde_json::Value>>,

    // 运行状态
    pub running: std::sync::atomic::AtomicBool,
    pub login_done: std::sync::atomic::AtomicBool,
    pub web_port: u16,

    // QR 登录
    qr_login_started: std::sync::atomic::AtomicBool, // 防止并发启动多个 QR 轮询线程
    qr_login_state: RwLock<QrLoginState>,
    qr_login_message: RwLock<String>,
    qrcode_matrix: RwLock<Option<Vec<Vec<String>>>>,
    qrcode_key: RwLock<Option<String>>,

    // Session 管理
    pub session_status: RwLock<SessionState>,

    // 媒体
    media_cache_dir: PathBuf,
    user_data_dir: PathBuf,
    disable_media_cache: bool,

    // 添加用户
    pub pending_qrcode: RwLock<Option<serde_json::Value>>,

    // 轮询
    poll_started_tokens: RwLock<HashSet<String>>,
    pub poll_health: RwLock<HashMap<String, PollHealth>>,
    // S45: 每个 poll 线程的 cancel flag（key = 完整 bot_token）
    //   reauth 时设置旧 token 的 flag=true，旧 poll 线程循环顶部检测后退出，
    //   避免旧 poll 在 reauth 成功后短暂覆盖 session_status
    poll_cancels: RwLock<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,

    // S32: 限制 spawn_retry_send 并发数（8），避免批量重试时线程爆炸
    retry_send_sem: Arc<CountingSemaphore>,

    // typing 去重：同一用户 25s 内只 spawn 一次 stop 线程
    last_typing_time: Mutex<HashMap<String, f64>>,

    // WebDAV
    pub webdav_client: RwLock<Option<Arc<WebDavClient>>>,
    pub webdav_config: RwLock<WebDavConfig>,
    pub webdav_migrate_state: RwLock<WebDavMigrateState>,

    // Webhook 出站推送
    pub webhook_dispatcher: RwLock<Option<WebhookDispatcher>>,

    // 媒体多级存储
    pub tiered_storage: RwLock<TieredStorage>,

    // 内部组件
    pub db: Arc<Database>,
    pub broker: Arc<EventBroker>,
    http_client: HttpClient,
    send_client: HttpClient,
    cdn_client: HttpClient,

    // 发送锁
    send_locks: RwLock<HashMap<String, Arc<Mutex<()>>>>,

    // 后台保存队列
    save_tx: Mutex<Option<Sender<SaveTask>>>,
    save_handle: Mutex<Option<std::thread::JoinHandle<()>>>,

    // 媒体预取线程池
    prefetch_tx: Mutex<Option<crossbeam_channel::Sender<PrefetchTask>>>,
    prefetch_handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl WeChatiLinkBot {
    /// DB 初始化或 HTTP 客户端构建失败返回 Err，由 BotManager 降级处理（不再 panic）。
    pub fn new_for_user(uid: i64) -> anyhow::Result<Arc<Self>> {
        let db = Database::new_for_user(uid)?;
        let broker = Arc::new(EventBroker::new());

        let media_cache = crate::config::user_media_cache_dir(uid);
        let user_data = crate::config::user_data_dir_for_user(uid);
        let _ = std::fs::create_dir_all(&media_cache);
        let _ = std::fs::create_dir_all(&user_data);

        // 预构建 tiered_storage（需要 media_cache 的 clone）
        let mut tiered_storage = TieredStorage::new();
        tiered_storage.add_backend(Box::new(LocalFsBackend::new(media_cache.clone())));

        // HTTP 客户端构建失败返回 Err（不 panic）
        let http_client = HttpClient::builder()
            .timeout(Duration::from_secs(35))
            .build()
            .map_err(|e| anyhow::anyhow!("无法创建 HTTP 客户端: {}", e))?;

        let send_client = HttpClient::builder()
            .timeout(Duration::from_secs(25))
            .build()
            .map_err(|e| anyhow::anyhow!("无法创建发送 HTTP 客户端: {}", e))?;

        let cdn_client = HttpClient::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| anyhow::anyhow!("无法创建 CDN HTTP 客户端: {}", e))?;

        let disable_media_cache = std::env::var("ILINK_DISABLE_MEDIA_CACHE")
            .map(|v| ["1", "true", "yes"].contains(&v.to_lowercase().as_str()))
            .unwrap_or(false);

        let web_port: u16 = std::env::var("ILINK_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8888);

        let bot = Arc::new(Self {
            token: RwLock::new(None),
            bot_id: RwLock::new(None),
            user_id: RwLock::new(None),
            cursor: RwLock::new(String::new()),
            context_tokens: RwLock::new(HashMap::new()),
            current_user: RwLock::new(None),
            user_token_map: RwLock::new(HashMap::new()),
            bot_accounts: RwLock::new(HashMap::new()),
            messages: RwLock::new(Vec::new()),
            running: std::sync::atomic::AtomicBool::new(true),
            login_done: std::sync::atomic::AtomicBool::new(false),
            web_port,
            qr_login_started: std::sync::atomic::AtomicBool::new(false),
            qr_login_state: RwLock::new(QrLoginState::Idle),
            qr_login_message: RwLock::new(String::new()),
            qrcode_matrix: RwLock::new(None),
            qrcode_key: RwLock::new(None),
            session_status: RwLock::new(SessionState::Disconnected),
            media_cache_dir: media_cache,
            user_data_dir: user_data,
            disable_media_cache,
            pending_qrcode: RwLock::new(None),
            poll_started_tokens: RwLock::new(HashSet::new()),
            poll_health: RwLock::new(HashMap::new()),
            poll_cancels: RwLock::new(HashMap::new()),
            retry_send_sem: Arc::new(CountingSemaphore::new(8)),
            last_typing_time: Mutex::new(HashMap::new()),
            webdav_client: RwLock::new(None),
            webdav_config: RwLock::new(WebDavConfig::default()),
            webdav_migrate_state: RwLock::new(WebDavMigrateState::default()),
            webhook_dispatcher: RwLock::new(WebhookDispatcher::from_env()),
            tiered_storage: RwLock::new(tiered_storage),
            db,
            broker,
            http_client,
            send_client,
            cdn_client,
            send_locks: RwLock::new(HashMap::new()),
            save_tx: Mutex::new(None),
            save_handle: Mutex::new(None),
            prefetch_tx: Mutex::new(None),
            prefetch_handles: Mutex::new(Vec::new()),
        });

        // 启动后台保存队列
        WeChatiLinkBot::start_save_worker(&bot);
        // 启动媒体预取线程池
        WeChatiLinkBot::start_prefetch_pool(&bot);
        // 启动媒体缓存清理线程
        WeChatiLinkBot::start_media_cleanup_loop(&bot);

        // 尝试加载 WebDAV 配置
        {
            let mut cfg = bot.webdav_config.write();
            if let Some(stored) = bot.db.load_webdav_config() {
                *cfg = stored;
            }
        }
        let _ = bot.reload_webdav_client();

        // 启动时恢复未处理消息（持久化优先架构）
        // 上次崩溃 / 重启后, processed=0 的入站行需要重投 SSE
        bot.recover_unprocessed_messages();

        // 启动时恢复未完成的出站消息
        bot.recover_pending_outbound();

        Ok(bot)
    }

    fn start_save_worker(bot: &Arc<Self>) {
        let (tx, rx) = channel::<SaveTask>();
        let handle = std::thread::Builder::new()
            .name("ilink-save".into())
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    let _ = task.db.save_user_messages(
                        &task.user_id,
                        &task.messages,
                        task.max_per_user,
                    );
                }
                // 队列已关闭，排空剩余任务
                while let Ok(task) = rx.try_recv() {
                    let _ = task.db.save_user_messages(
                        &task.user_id,
                        &task.messages,
                        task.max_per_user,
                    );
                }
            })
            .ok();
        *bot.save_tx.lock() = Some(tx);
        *bot.save_handle.lock() = handle;
    }

    fn start_prefetch_pool(bot: &Arc<Self>) {
        // crossbeam-channel: Receiver 可 Clone + Sync，无需持锁即可 recv，3 个 worker 真并行
        let (tx, rx) = crossbeam_channel::bounded::<PrefetchTask>(256);
        let mut handles = Vec::new();
        for i in 0..3 {
            let rx = rx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("ilink-prefetch-{}", i))
                .spawn(move || {
                    // 发送端全部 drop 后 recv 返回 Err，自动退出
                    while let Ok(task) = rx.recv() {
                        task.run();
                    }
                })
                .ok();
            if let Some(h) = handle {
                handles.push(h);
            }
        }
        *bot.prefetch_tx.lock() = Some(tx);
        *bot.prefetch_handles.lock() = handles;
    }

    fn start_media_cleanup_loop(bot: &Arc<Self>) {
        let bot = bot.clone();
        std::thread::Builder::new()
            .name("ilink-media-cleanup".into())
            .spawn(move || {
                let days = std::env::var("ILINK_MEDIA_CACHE_DAYS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(7)
                    .min(36500); // 上限 100 年，杜绝溢出
                let max_age = Duration::from_secs(days.saturating_mul(24).saturating_mul(3600));
                loop {
                    for _ in 0..3600 {
                        if !bot.running.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    if !bot.running.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    bot.cleanup_expired_media_cache(max_age);
                }
            })
            .ok();
    }

    // ── 用户管理 ─────────────────────────────────────────────

    pub fn list_users(&self) -> Vec<String> {
        self.context_tokens.read().keys().cloned().collect()
    }

    pub fn get_current_user(&self) -> Option<String> {
        self.current_user.read().clone()
    }

    /// 获取指定用户的最后一条发送消息
    /// 用于upload-media API返回消息对象给前端
    pub fn get_last_out_message(&self, user_id: &str) -> Option<serde_json::Value> {
        let messages = self.messages.read();
        messages
            .iter()
            .rev()
            .find(|m| {
                m.get("from") == Some(&serde_json::Value::String("me".to_string()))
                    && m.get("to") == Some(&serde_json::Value::String(user_id.to_string()))
            })
            .cloned()
    }

    pub fn set_current_user(&self, user_id: &str) {
        let ctx = self.context_tokens.read();
        if ctx.contains_key(user_id) {
            *self.current_user.write() = Some(user_id.to_string());
        }
        drop(ctx);
        self.save_config();
        tracing::info!("已切换到: {}", user_id);
    }

    pub fn remove_user(&self, user_id: &str) -> bool {
        {
            let mut ctx = self.context_tokens.write();
            if !ctx.contains_key(user_id) {
                return false;
            }
            ctx.remove(user_id);
        }
        self.user_token_map.write().remove(user_id);
        self.send_locks.write().remove(user_id);

        // 清理消息
        {
            let mut msgs = self.messages.write();
            msgs.retain(|m| {
                m.get("from").and_then(|v| v.as_str()) != Some(user_id)
                    && m.get("to").and_then(|v| v.as_str()) != Some(user_id)
            });
        }

        self.db.delete_user_token(user_id);
        self.db.delete_user_messages(user_id);
        self.db.delete_user_messages_v2(user_id);

        // 删除用户目录
        let user_dir = self.get_user_dir_path(user_id);
        if user_dir.exists() {
            let _ = std::fs::remove_dir_all(&user_dir);
        }

        // 切换 current_user
        {
            let ctx = self.context_tokens.read();
            if self.current_user.read().as_deref() == Some(user_id) {
                let first = ctx.keys().next().cloned();
                *self.current_user.write() = first;
            }
        }

        self.save_config();
        tracing::info!("[USER] 已删除用户: {}", user_id);
        true
    }

    /// 按 user_id 限定删除范围，杜绝跨会话删除（调用方必须传入当前 peer）。
    pub fn delete_messages_by_ids(&self, ids: &[i64], user_id: &str) -> usize {
        if ids.is_empty() || user_id.is_empty() {
            return 0;
        }
        let deleted = self.db.delete_messages_by_ids(ids, user_id);
        if deleted > 0 {
            let id_set: HashSet<i64> = ids.iter().cloned().collect();
            let mut msgs = self.messages.write();
            // P1-8: 内存侧也按 user_id 过滤，避免误删其他 peer 的同 ID 消息
            msgs.retain(|m| {
                // 不属于当前 peer 的消息一律保留
                let m_user = m
                    .get("from")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.get("to").and_then(|v| v.as_str()))
                    .unwrap_or("");
                if m_user != user_id {
                    return true;
                }
                // 属于当前 peer 且 id 在删除集 → 移除
                m.get("id")
                    .and_then(|v| v.as_i64())
                    .map(|id| !id_set.contains(&id))
                    .unwrap_or(true)
            });
        }
        deleted
    }

    fn register_user_to_account(&self, user_id: &str, ctx_token: &str, bot_token: &str) {
        self.context_tokens
            .write()
            .insert(user_id.to_string(), ctx_token.to_string());
        self.user_token_map
            .write()
            .insert(user_id.to_string(), bot_token.to_string());
        self.db.save_user_token(user_id, ctx_token, bot_token);

        // 同步到 bot_accounts
        let mut accounts = self.bot_accounts.write();
        if let Some(acct) = accounts.get_mut(bot_token) {
            if let Some(ctx_tokens) = acct.get_mut("context_tokens") {
                if let Some(map) = ctx_tokens.as_object_mut() {
                    map.insert(
                        user_id.to_string(),
                        serde_json::Value::String(ctx_token.to_string()),
                    );
                }
            }
        }
    }

    fn get_send_lock(&self, user_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.send_locks.write();
        locks.entry(user_id.to_string()).or_default().clone()
    }

    fn get_token_for_user(&self, user_id: &str) -> Option<String> {
        self.user_token_map.read().get(user_id).cloned()
    }

    // ── 目录路径 ─────────────────────────────────────────────

    fn get_user_dir_path(&self, user_id: &str) -> PathBuf {
        let hash = crypto::md5_hex(user_id.as_bytes());
        let dir_name = &hash[..16];
        self.user_data_dir.join(dir_name)
    }

    // ── 配置加载/保存 ────────────────────────────────────────

    pub fn load_config(&self) -> bool {
        let config = self.db.load_config().or_else(|| {
            // 尝试从旧 JSON 文件迁移（原始文件或迁移后的 .bak 备份）
            let cfg_path = config_file();
            let bak_path = cfg_path.with_file_name("wechat_bot_config.json.bak");
            let candidate = if cfg_path.exists() {
                Some(&cfg_path)
            } else if bak_path.exists() {
                Some(&bak_path)
            } else {
                None
            };
            if let Some(path) = candidate {
                if let Some(cfg) = crate::config::load_json_safe(path, MAX_CONFIG_SIZE) {
                    self.db.save_config(&cfg);
                    // 迁移用户 token
                    if let Some(ctx_tokens) = cfg.get("context_tokens").and_then(|v| v.as_object())
                    {
                        let user_token_map = cfg.get("user_token_map").and_then(|v| v.as_object());
                        for (uid, ctx) in ctx_tokens {
                            let bt = user_token_map
                                .and_then(|m| m.get(uid))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            self.db.save_user_token(uid, ctx.as_str().unwrap_or(""), bt);
                        }
                    }
                    // 如果是从原始文件迁移的，备份原文件
                    if path == &cfg_path {
                        let _ = std::fs::rename(&cfg_path, format!("{}.bak", cfg_path.display()));
                    }
                    return Some(cfg);
                }
            }
            None
        });

        let config = match config {
            Some(c) => c,
            None => return false,
        };

        // token 字段不再持久化到 config 表（空字符串视为 None），主 token 从 user_tokens 表反查恢复。
        let token_str = config.get("token").and_then(|v| v.as_str()).unwrap_or("");
        *self.token.write() = if token_str.is_empty() {
            None
        } else {
            Some(token_str.to_string())
        };
        *self.bot_id.write() = config
            .get("bot_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        *self.user_id.write() = config
            .get("user_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        *self.cursor.write() = config
            .get("cursor")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        *self.current_user.write() = config
            .get("current_user")
            .and_then(|v| v.as_str())
            .map(String::from);

        // 加载会话状态
        let session_status_str = config
            .get("session_status")
            .and_then(|v| v.as_str())
            .unwrap_or("disconnected");

        let session_status: SessionState = match session_status_str {
            "active" => SessionState::Active,
            "session_expired" | "expired" => SessionState::SessionExpired,
            "reauthing" => SessionState::Reauthing,
            _ => SessionState::Disconnected,
        };

        // 双重防御：持久化时 terminal 态已归一化为 disconnected，
        // 此处再拦截旧版本残留或外部直改 DB 的异常状态，重置后由 poll 重新探测。
        let session_status =
            if session_status.is_terminal() || session_status == SessionState::Reauthing {
                tracing::info!(
                    "[CONFIG] 读到运行时态 {:?}，重置为 Disconnected（由 poll 重新探测）",
                    session_status
                );
                SessionState::Disconnected
            } else {
                session_status
            };

        *self.session_status.write() = session_status;
        if session_status.is_terminal() {
            tracing::info!("[CONFIG] 检测到上次会话已过期，等待 poll 确认或重新扫码...");
        }

        // 恢复 context_tokens
        if let Some(ctx) = config.get("context_tokens").and_then(|v| v.as_object()) {
            let mut tokens = self.context_tokens.write();
            for (k, v) in ctx {
                tokens.insert(k.clone(), v.as_str().unwrap_or("").to_string());
            }
        }
        if let Some(accts) = config.get("bot_accounts").and_then(|v| v.as_object()) {
            let mut accounts = self.bot_accounts.write();
            for (k, v) in accts {
                accounts.insert(k.clone(), v.clone());
            }
        }
        if let Some(utm) = config.get("user_token_map").and_then(|v| v.as_object()) {
            let mut map = self.user_token_map.write();
            for (k, v) in utm {
                map.insert(k.clone(), v.as_str().unwrap_or("").to_string());
            }
        }

        // 从 user_tokens 表补充
        let db_tokens = self.db.list_user_tokens();
        let mut ctx = self.context_tokens.write();
        let mut utm = self.user_token_map.write();
        for (uid, (ctx_token, bot_token)) in &db_tokens {
            if !ctx.contains_key(uid) {
                ctx.insert(uid.clone(), ctx_token.clone());
            }
            if !utm.contains_key(uid) && !bot_token.is_empty() {
                utm.insert(uid.clone(), bot_token.clone());
            }
        }

        // 主 token 反查兜底：优先取 current_user 关联的 token，其次取任意非空 token。
        if self.token.read().is_none() {
            let cur_user = self.current_user.read().clone();
            let picked = cur_user
                .as_deref()
                .and_then(|u| utm.get(u))
                .cloned()
                .or_else(|| utm.values().next().cloned())
                .filter(|t| !t.is_empty());
            if let Some(t) = picked {
                *self.token.write() = Some(t);
            }
        }

        // 主 token 注册到 bot_accounts
        if let Some(ref token) = *self.token.read() {
            if !self.bot_accounts.read().contains_key(token) {
                self.bot_accounts.write().insert(
                    token.clone(),
                    serde_json::json!({
                        "bot_id": self.bot_id.read().clone().unwrap_or_default(),
                        "user_id": self.user_id.read().clone().unwrap_or_default(),
                        "cursor": *self.cursor.read(),
                        "context_tokens": *ctx,
                    }),
                );
            }
        }

        // 补充 user_token_map
        for uid in ctx.keys() {
            if !utm.contains_key(uid) {
                if let Some(ref token) = *self.token.read() {
                    utm.insert(uid.clone(), token.clone());
                }
            }
        }

        // 自动选择 current_user
        if self.current_user.read().is_none() && !ctx.is_empty() {
            *self.current_user.write() = ctx.keys().next().cloned();
        } else if let Some(ref cu) = *self.current_user.read() {
            if !ctx.contains_key(cu) {
                *self.current_user.write() = ctx.keys().next().cloned();
            }
        }

        // 加载消息
        self.load_all_messages();

        if self.token.read().is_some() {
            self.login_done
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.set_qr_login_state(QrLoginState::Confirmed, "已从缓存恢复连接");
            tracing::info!(
                "加载配置成功，{} 个会话，{} 条消息",
                ctx.len(),
                self.messages.read().len()
            );
            return true;
        }
        false
    }

    pub fn save_config(&self) {
        // 不将明文 bot_token 持久化到 config 表（凭证仅存于加密的 user_tokens 表）。
        // SessionExpired / Reauthing 为运行时态，持久化时归一化为 disconnected，
        // 避免重启后残留过期态导致前端持续展示"会话已过期"。
        let persist_status = match *self.session_status.read() {
            SessionState::SessionExpired | SessionState::Reauthing => "disconnected",
            other => other.as_str(),
        };
        let config = serde_json::json!({
            "token": "",  // 凭证，从 user_tokens 表反查
            "bot_id": *self.bot_id.read(),
            "user_id": *self.user_id.read(),
            "cursor": *self.cursor.read(),
            "context_tokens": *self.context_tokens.read(),
            "current_user": *self.current_user.read(),
            "bot_accounts": {},  // 凭证作 key，load_config 时从 user_tokens 表重建
            "user_token_map": {},  // 凭证，从 user_tokens 表反查
            "session_status": persist_status, // PR4: SessionState 序列化（运行时态归一化，见 P0-6）
        });
        self.db.save_config(&config);
    }

    // ── 消息管理 ─────────────────────────────────────────────

    fn load_all_messages(&self) {
        let all = self.db.load_all_messages();
        *self.messages.write() = all;
    }

    pub fn add_message_to_history(&self, msg: serde_json::Value) -> serde_json::Value {
        let mut msgs = self.messages.write();
        let mut msg = msg;
        // 不在内存中分配 id，由数据库自增分配，加载时由 parse_msg_with_id 填入。
        //   关键：不要用 row_id 作为 id 兜底——那会让 messages 表 JSON 的 id = row_id，
        //   导致 parse_msg_with_id 不覆盖（id>0），进而 _bumpLastMsgId 用 row_id 作为
        //   since 游标，跳过 messages 表中 id 较小但 row_id 较大的入站消息。
        //   row_id 仅保留在 JSON 中作为前端去重键，不参与轮询游标。
        if let Some(obj) = msg.as_object_mut() {
            if !obj.contains_key("id") {
                obj.insert(
                    "id".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(0)),
                );
            }
        }
        msgs.push(msg.clone());

        // 先确定 user_id 并过滤该用户消息（在截断前，避免丢失）
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let user_id = if msg_type == "out" {
            msg.get("to").and_then(|v| v.as_str())
        } else {
            msg.get("from").and_then(|v| v.as_str())
        };
        let user_id = user_id.unwrap_or("").to_string();
        if user_id.is_empty() {
            return msg;
        }
        let user_msgs: Vec<serde_json::Value> = msgs
            .iter()
            .filter(|m| {
                m.get("from").and_then(|v| v.as_str()) == Some(&user_id)
                    || m.get("to").and_then(|v| v.as_str()) == Some(&user_id)
            })
            .cloned()
            .collect();

        // 截断（在过滤 user_msgs 之后，保证该用户的完整消息集已提取）
        let max_per_user = 500;
        let total_max = 2000;
        if msgs.len() > total_max {
            // 按 user_id 分区截断，每用户保留最新 max_per_user 条。
            //   原实现 msgs.drain(0..drain_count) 全局 drain 会删除任意用户的旧消息，
            //   当被 drain 的用户下次触发 save_user_messages（DELETE WHERE user_id + INSERT）时，
            //   被 drain 的消息从 DB 永久丢失。改为按用户分区截断，确保每用户内存中始终
            //   保留最新 max_per_user 条，与 save_user_messages 的截断逻辑一致。
            let mut counts: HashMap<String, usize> = HashMap::new();
            let mut kept: Vec<serde_json::Value> = Vec::with_capacity(msgs.len());
            for m in msgs.iter().rev() {
                let m_type = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let m_uid = if m_type == "out" {
                    m.get("to").and_then(|v| v.as_str()).unwrap_or("")
                } else {
                    m.get("from").and_then(|v| v.as_str()).unwrap_or("")
                }
                .to_string();
                let cnt = counts.entry(m_uid).or_insert(0);
                if *cnt < max_per_user {
                    kept.push(m.clone());
                    *cnt += 1;
                }
            }
            kept.reverse();
            *msgs = kept;
        }

        // 所有消息统一同步保存（确保 WS 推送前 DB 已落库，避免详情视图查不到）。
        // 性能影响可接受：500 条消息 DELETE+INSERT 约 10-50ms
        // 原出站异步保存会导致图片/文本消息在详情视图中缺失（DB 还没落库时用户就刷新了）
        let new_id = self
            .db
            .save_user_messages(&user_id, &user_msgs, max_per_user);
        // 回填 messages 表自增 id 到 msg，使 WS 推送的消息带真实 id（>0），
        // 前端 dedup 可统一用 id，无需 row_id/client_id/req_id 三路去重
        if let Some(id) = new_id {
            if id > 0 {
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert(
                        "id".to_string(),
                        serde_json::Value::Number(serde_json::Number::from(id)),
                    );
                }
            }
        }

        msg // 返回带 id 的消息
    }

    // ── QR 登录 ──────────────────────────────────────────────

    fn set_qr_login_state(&self, state: QrLoginState, message: &str) {
        *self.qr_login_state.write() = state;
        *self.qr_login_message.write() = message.to_string();
        // 推送 qr_state 事件，前端 WS 收到后实时更新三态 UI。
        //   （等待扫码/已扫码待确认/登录成功/失败/过期），无需轮询 /api/wasm/qrcode。
        //   publish 在无订阅者时静默吞掉 SendError（broker 内部计数），早期/无 WS 时安全。
        self.broker.publish(
            crate::event_broker::EVENT_QR_STATE,
            serde_json::json!({
                "state": state.as_str(),
                "message": message,
                "login_done": self.login_done.load(std::sync::atomic::Ordering::Relaxed),
            }),
        );
    }

    pub fn get_qr_login_state(&self) -> QrLoginStatus {
        QrLoginStatus {
            state: *self.qr_login_state.read(),
            message: self.qr_login_message.read().clone(),
            login_done: self.login_done.load(std::sync::atomic::Ordering::Relaxed),
            has_qrcode: self.qrcode_matrix.read().is_some(),
            matrix: self.qrcode_matrix.read().clone(),
            qrcode_key: self.qrcode_key.read().clone(),
        }
    }

    fn fetch_one_qrcode(&self) -> Option<serde_json::Value> {
        let url = format!("{}/ilink/bot/get_bot_qrcode?bot_type=3", ILINK_BASE_URL);
        let resp = self
            .http_client
            .get(&url)
            .timeout(Duration::from_secs(35))
            .header(
                USER_AGENT,
                format!("iLink-Bot/1.0 (Zyn-ChatBox/{})", SCRIPT_VERSION),
            )
            .send()
            .ok()?;
        let data: serde_json::Value = resp.json().ok()?;
        if data.get("qrcode").and_then(|v| v.as_str()).is_some()
            && data
                .get("qrcode_img_content")
                .and_then(|v| v.as_str())
                .is_some()
        {
            Some(data)
        } else {
            tracing::warn!("[QR] iLink 返回数据不完整");
            None
        }
    }

    fn poll_qrcode_status(&self, qrcode_key: &str) -> Option<serde_json::Value> {
        let url = format!(
            "{}/ilink/bot/get_qrcode_status?qrcode={}",
            ILINK_BASE_URL, qrcode_key
        );
        let resp = self
            .http_client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .header("iLink-App-ClientVersion", "1.0.3")
            .send()
            .ok()?;
        resp.json().ok()
    }

    pub fn login_with_qrcode(&self) -> bool {
        let max_fetch_retries = 3;
        let max_refresh_count = 3;
        let total_timeout = 140; // 2 分 20 秒
        let status_poll_interval = 2;
        let start_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 阶段 1：获取二维码（带重试）
        self.set_qr_login_state(QrLoginState::Fetching, "正在获取二维码...");
        let mut qr_data = None;
        for attempt in 0..max_fetch_retries {
            if let Some(data) = self.fetch_one_qrcode() {
                qr_data = Some(data);
                break;
            }
            if attempt < max_fetch_retries - 1 {
                std::thread::sleep(Duration::from_secs(2u64.pow(attempt as u32)));
            }
        }
        let qr_data = match qr_data {
            Some(d) => d,
            None => {
                self.set_qr_login_state(QrLoginState::Error, "多次获取二维码失败");
                return false;
            }
        };

        // 阶段 2：轮询扫码状态（带自动刷新）
        let mut refresh_count = 0;
        'outer: while refresh_count <= max_refresh_count {
            let qrcode_key = qr_data.get("qrcode").and_then(|v| v.as_str()).unwrap_or("");
            let qrcode_url = qr_data
                .get("qrcode_img_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // 生成二维码矩阵
            let matrix = self.generate_qr_matrix(qrcode_url);
            *self.qrcode_key.write() = Some(qrcode_key.to_string());
            *self.qrcode_matrix.write() = Some(matrix);
            self.set_qr_login_state(QrLoginState::Ready, "请扫描二维码");

            // 二维码仅在网页显示，控制台只输出文字提示
            tracing::info!("[QR] 二维码已在网页显示，请用微信扫码");

            let mut scanned_notified = false;
            while !self.login_done.load(std::sync::atomic::Ordering::Relaxed) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now - start_ts > total_timeout {
                    self.set_qr_login_state(QrLoginState::Error, "登录等待超时");
                    return false;
                }

                if !self.running.load(std::sync::atomic::Ordering::Relaxed) {
                    return false;
                }

                let status = match self.poll_qrcode_status(qrcode_key) {
                    Some(s) => s,
                    None => {
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };

                let st = status.get("status").and_then(|v| v.as_str()).unwrap_or("");
                match st {
                    "scaned" if !scanned_notified => {
                        scanned_notified = true;
                        self.set_qr_login_state(QrLoginState::Scanned, "已扫码，请确认...");
                        tracing::info!("[QR] 已扫码，请在手机上确认...");
                    }
                    "confirmed" => {
                        let bot_token = status.get("bot_token").and_then(|v| v.as_str());
                        let ilink_bot_id = status.get("ilink_bot_id").and_then(|v| v.as_str());
                        let ilink_user_id = status.get("ilink_user_id").and_then(|v| v.as_str());

                        if let Some(token) = bot_token {
                            *self.token.write() = Some(token.to_string());
                            *self.bot_id.write() = Some(ilink_bot_id.unwrap_or("").to_string());
                            *self.user_id.write() = Some(ilink_user_id.unwrap_or("").to_string());

                            // 注册到 bot_accounts
                            self.bot_accounts.write().insert(
                                token.to_string(),
                                serde_json::json!({
                                    "bot_id": ilink_bot_id.unwrap_or(""),
                                    "user_id": ilink_user_id.unwrap_or(""),
                                    "cursor": "",
                                    "context_tokens": {},
                                }),
                            );

                            // 重置会话状态为活跃
                            *self.session_status.write() = SessionState::Active;

                            self.save_config();
                            self.login_done
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            self.set_qr_login_state(QrLoginState::Confirmed, "登录成功！");
                            tracing::info!("[zyn]登录成功！");
                            // 先 publish 通知前端，再恢复历史会话。
                            //   之前 fetch_and_restore_conversations 在 publish 之前，
                            //   最多 5 次 getupdates（每次 5s 超时）会阻塞 25s，
                            //   导致前端延迟收到 login_done 事件，用户以为卡住。
                            //   现在先推送事件，前端能立即跳转聊天页面；
                            //   users 列表可能暂为空，但前端轮询会自动补全。
                            self.broker.publish(
                                "status",
                                serde_json::json!({
                                    "login_done": true,
                                    "logged_in": true,
                                    "users": self.list_users(),
                                    "current_user": self.get_current_user(),
                                    "message": "扫码登录成功",
                                }),
                            );
                            // 恢复历史会话（注册用户到 account，供 start_polling 使用）
                            // S63: 移到 start_login_async 中异步执行，避免阻塞 login 返回
                            return true;
                        } else {
                            self.set_qr_login_state(
                                QrLoginState::Error,
                                "登录确认但未获取到 token",
                            );
                            return false;
                        }
                    }
                    "expired" => {
                        self.set_qr_login_state(QrLoginState::Expired, "二维码已过期");
                        refresh_count += 1;
                        if refresh_count > max_refresh_count {
                            self.set_qr_login_state(QrLoginState::Error, "二维码多次过期");
                            return false;
                        }
                        continue 'outer;
                    }
                    _ => {} // "wait" 或其他状态
                }
                std::thread::sleep(Duration::from_secs(status_poll_interval));
            }
            return self.login_done.load(std::sync::atomic::Ordering::Relaxed);
        }
        self.set_qr_login_state(QrLoginState::Error, "登录超时");
        false
    }

    pub fn start_login_async(self: &Arc<Self>) {
        if self.login_done.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // 防止并发启动多个 QR 轮询线程
        if self
            .qr_login_started
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
        {
            return;
        }
        let bot = self.clone();
        std::thread::Builder::new()
            .name("ilink-qr-login".into())
            .spawn(move || {
                let logged_in = bot.login_with_qrcode();
                if logged_in {
                    // Ponytail fix: non-owner bot must start polling to receive inbound messages
                    bot.start_polling();
                    // S63: 异步恢复历史会话（最多 5 次 getupdates，每次 5s 超时，阻塞 25s）
                    //   status 事件已在 login_with_qrcode 内 publish，异步执行不影响登录通知
                    let bot2 = bot.clone();
                    std::thread::Builder::new()
                        .name("ilink-restore-conv".into())
                        .spawn(move || {
                            bot2.fetch_and_restore_conversations();
                        })
                        .ok();
                } else if !bot.login_done.load(std::sync::atomic::Ordering::Relaxed) {
                    bot.set_qr_login_state(
                        QrLoginState::Error,
                        "登录失败，请检查网络连接后刷新页面重试",
                    );
                }
                bot.qr_login_started
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            })
            .ok();
    }

    fn fetch_and_restore_conversations(&self) {
        for _ in 0..5 {
            let cursor = self.cursor.read().clone();
            let body = serde_json::json!({"get_updates_buf": cursor, "base_info": {"channel_version": "1.0.3"}});
            let result = self.post("getupdates", &body, 5, None);
            let msgs = match result.get("msgs").and_then(|v| v.as_array()) {
                Some(m) => m,
                None => break,
            };
            for msg in msgs {
                let from_user = msg
                    .get("from_user_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let ctx_token = msg
                    .get("context_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !from_user.is_empty() && !ctx_token.is_empty() {
                    let token = self.token.read().clone().unwrap_or_default();
                    let is_new = !self.context_tokens.read().contains_key(from_user);
                    self.register_user_to_account(from_user, ctx_token, &token);
                    if is_new {
                        tracing::info!("恢复会话: {}", from_user);
                    }
                    // 提取消息（文本 + 媒体），与 on_inbound_message 内部逻辑对齐
                    let message_id = msg.get("message_id").and_then(|v| v.as_i64());
                    let time_str = msg
                        .get("create_time")
                        .and_then(|v| v.as_i64())
                        .and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|dt| {
                            dt.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string()
                        })
                        .or_else(|| {
                            msg.get("create_time")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| {
                            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
                        });
                    if let Some(items) = msg.get("item_list").and_then(|v| v.as_array()) {
                        let (text, media_list) = self.process_message_items(items);
                        if !text.is_empty() && media_list.is_empty() {
                            self.add_message_to_history(serde_json::json!({
                                "from": from_user, "to": "me", "text": text,
                                "time": time_str, "type": "in", "message_id": message_id,
                            }));
                        }
                        for mi in &media_list {
                            let media_type = mi.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            let media_type_int = match media_type {
                                "image" => 2,
                                "voice" => 3,
                                "file" => 4,
                                "video" => 5,
                                _ => 0,
                            };
                            let media_prefix = match media_type {
                                "image" => "[图片]",
                                "voice" => "[语音]",
                                "file" => "[文件]",
                                "video" => "[视频]",
                                _ => "[媒体]",
                            };
                            let media_filename =
                                mi.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                            let msg_text = if !text.is_empty() {
                                format!("{} {}", media_prefix, text)
                            } else {
                                format!("{} {}", media_prefix, media_filename)
                            };
                            let mut event = serde_json::json!({
                                "from": from_user, "to": "me", "text": msg_text,
                                "time": time_str, "type": "in", "message_id": message_id,
                                "media_type": media_type_int, "media_filename": media_filename,
                                "has_media": true,
                            });
                            if let Some(item) = mi.get("item") {
                                if let Some(cdn_media) = self.extract_cdn_media(item) {
                                    let cdn_str =
                                        serde_json::to_string(&cdn_media).unwrap_or_default();
                                    let cache_key = self.media_cache_key(&cdn_media);
                                    if let Some(obj) = event.as_object_mut() {
                                        obj.insert(
                                            "media_cdn".into(),
                                            serde_json::Value::String(cdn_str),
                                        );
                                        obj.insert(
                                            "media_cache_id".into(),
                                            serde_json::Value::String(cache_key.clone()),
                                        );
                                        if let Some(url) = self.webdav_url_for_cache_key(
                                            &cache_key,
                                            media::derive_ext("", media_filename).as_deref(),
                                        ) {
                                            obj.insert(
                                                "media_webdav_url".into(),
                                                serde_json::Value::String(url),
                                            );
                                        }
                                    }
                                }
                            }
                            // ponytail: 不在此预取媒体（submit_prefetch_task 需 &Arc<Self>），前端按 media_cache_id 按需拉取
                            self.add_message_to_history(event);
                        }
                    }
                }
            }
            // 消息全部处理成功后再更新 cursor（避免部分失败时丢失消息）
            if let Some(new_cursor) = result.get("get_updates_buf").and_then(|v| v.as_str()) {
                *self.cursor.write() = new_cursor.to_string();
            }
            if msgs.is_empty() {
                break;
            }
        }
        let ctx = self.context_tokens.read();
        if !ctx.is_empty() {
            tracing::info!("已恢复 {} 个会话", ctx.len());
        } else {
            tracing::info!("没有找到历史会话");
        }
    }

    // 持久化优先入站处理

    /// 生成一个短 trace_id (`tr_xxxxxxxx`)，用于排障 + 日志串联。
    /// S53/S25b: 使用 UUID 替代 DefaultHasher，避免碰撞风险（DefaultHasher 不是加密安全哈希）
    pub(crate) fn gen_trace_id(&self) -> String {
        format!("tr_{}", uuid::Uuid::new_v4().to_string().replace("-", ""))
    }

    /// 入站消息统一处理入口（PR2 持久化优先）。
    ///
    /// 流程：
    /// 1. 立即 `upsert_inbound_message` —— 持久化为权威源
    /// 2. 重复消息 (inserted=false) 直接 mark_processed + 丢弃
    /// 3. 新消息：构建 SSE 事件 + publish + 触发媒体下载 + mark_processed
    /// 4. 同步写老 `messages` 表（兼容老 web.rs API，不破坏前端）
    pub fn on_inbound_message(
        self: &Arc<Self>,
        raw_msg: &serde_json::Value,
        bot_token: &str,
        context_token: &str,
    ) -> Option<i64> {
        let message_id = raw_msg.get("message_id").and_then(|v| v.as_i64());
        let from_user = raw_msg
            .get("from_user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let to_user = raw_msg
            .get("to_user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let items = raw_msg
            .get("item_list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if from_user.is_empty() {
            tracing::debug!("[INBOUND] 跳过无 from_user_id 的消息");
            return None;
        }

        // 解析文本 + 媒体
        let (text, media_list) = self.process_message_items(&items);
        let item_list_json = serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());

        // 1) 持久化（先去重）
        let trace_id = self.gen_trace_id();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let save_result = self.db.upsert_inbound_message(
            &trace_id,
            bot_token,
            message_id,
            &from_user,
            &from_user,
            &to_user,
            context_token,
            &item_list_json,
            &text,
            &serde_json::to_string(raw_msg).unwrap_or_default(),
            now_ms,
        );

        if !save_result.inserted {
            if save_result.id > 0 {
                // 重复消息：仅 mark_processed，不推送 SSE
                self.db.mark_processed(save_result.id);
                tracing::debug!("[INBOUND] 重复消息 message_id={:?} 丢弃", message_id);
                return None;
            }
            // S20: upsert 失败（DB 错误，id=0），日志记录但继续写 messages 表（主表）
            //   messages_v2 是辅助表，失败不应阻断主表写入与 SSE 推送
            tracing::warn!(
                "[INBOUND] upsert_inbound_message 失败（id=0），继续写 messages 表 message_id={:?}",
                message_id
            );
        }

        // 入站消息日志（非重复消息才记录）
        // 消息文本内容降级为 debug，避免 INFO 日志泄露对话内容；
        //   保留 "收到消息" 事件为 info，满足 M15 排障需求且不暴露敏感文本。
        tracing::info!("[RECV] 收到来自 {} 的消息", from_user);
        tracing::debug!("[RECV] 消息文本: {}", safe_truncate(&text, 80));
        for mi in &media_list {
            let mt = mi.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            let fname = mi.get("filename").and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!("[RECV] 媒体类型={} 文件名={}", mt, fname);
        }

        // 2) 推送 WS + 兼容老 messages 表：每条媒体各发一条，纯文本单独一条
        // 事件必须携带 row_id，否则前端 _handleIncomingMessage 无法做
        //   会因 dedupKey=0 早退而不渲染（手机→网页需刷新才能看到）。
        // 媒体消息也需要写入老 messages 表（含 media_cache_id / media_cdn 等），
        //   否则刷新后 /api/wasm/history 拿不到缩略图字段。
        let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        // 纯文本（无媒体）
        if !text.is_empty() && media_list.is_empty() {
            let event = serde_json::json!({
                "from": from_user,
                "to": "me",
                "text": text,
                "time": now_str,
                "type": "in",
                "trace_id": trace_id,
                "message_id": message_id,
                "row_id": save_result.id,
            });
            // 保留 row_id 写入 messages 表，作为前端去重键。
            //   前端 _handleIncomingMessage 和 _fetchMessages 统一用 row_id 去重，
            //   避免 WS 事件(row_id) 和轮询(DB 自增 id) 使用不同 key 导致重复或丢失。
            //   _bumpLastMsgId 仍只用 messages 表自增 id，不使用 row_id（两表序列独立）。
            self.add_message_to_history(event.clone());
            self.broker.publish("message", event);
        }

        // 3) 每条媒体各发一条（并写入 messages 表以便刷新后仍能显示缩略图）
        for mi in &media_list {
            let media_type = mi.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let media_type_int = match media_type {
                "image" => 2,
                "voice" => 3,
                "file" => 4,
                "video" => 5,
                _ => 0,
            };
            let media_prefix = match media_type {
                "image" => "[图片]",
                "voice" => "[语音]",
                "file" => "[文件]",
                "video" => "[视频]",
                _ => "[媒体]",
            };
            let media_filename = mi.get("filename").and_then(|v| v.as_str()).unwrap_or("");

            let msg_text = if !text.is_empty() {
                format!("{} {}", media_prefix, text)
            } else {
                format!("{} {}", media_prefix, media_filename)
            };

            let mut metadata = serde_json::Map::new();
            metadata.insert(
                "media_type".into(),
                serde_json::Value::Number(media_type_int.into()),
            );
            metadata.insert(
                "media_filename".into(),
                serde_json::Value::String(media_filename.to_string()),
            );
            metadata.insert("has_media".into(), serde_json::Value::Bool(true));

            if let Some(item) = mi.get("item") {
                if let Some(cdn_media) = self.extract_cdn_media(item) {
                    let cdn_str = serde_json::to_string(&cdn_media).unwrap_or_default();
                    metadata.insert("media_cdn".into(), serde_json::Value::String(cdn_str));
                    let cache_key = self.media_cache_key(&cdn_media);
                    metadata.insert(
                        "media_cache_id".into(),
                        serde_json::Value::String(cache_key.clone()),
                    );
                    if let Some(url) = self.webdav_url_for_cache_key(
                        &cache_key,
                        media::derive_ext("", media_filename).as_deref(),
                    ) {
                        metadata.insert("media_webdav_url".into(), serde_json::Value::String(url));
                    }
                    if !self.disable_media_cache {
                        // 重新启用 submit_prefetch_task 预取媒体。
                        //   之前改为仅发布 "queued" 事件但实际不下载，
                        //   导致前端 <img src="/api/wasm/media/<cache_key>"> 404，
                        //   图片只能显示占位图。
                        //   现在 on_inbound_message 签名改为 self: &Arc<Self>，
                        //   可以直接调用 submit_prefetch_task 在后台下载。
                        self.submit_prefetch_task(
                            &cache_key,
                            &cdn_media,
                            media_filename,
                            &from_user,
                        );
                    }
                }
            }

            let mut event = serde_json::json!({
                "from": from_user,
                "to": "me",
                "text": msg_text,
                "time": now_str,
                "type": "in",
                "trace_id": trace_id,
                "message_id": message_id,
                "row_id": save_result.id,
            });
            if let Some(obj) = event.as_object_mut() {
                for (k, v) in metadata {
                    obj.insert(k, v);
                }
            }
            // 先落 messages 表（含媒体字段+row_id），再推 WS，保证刷新后仍能显示。
            // 保留 row_id 作为前端统一去重键（与纯文本一致）。
            self.add_message_to_history(event.clone());
            self.broker.publish("message", event);
        }

        // 4) 入站 ack：如果是自己发出去的消息回推（带 client_id），更新出库行 send_state=delivered
        if let Some(cid) = raw_msg.get("client_id").and_then(|v| v.as_str()) {
            if !cid.is_empty() {
                if let Some(out_row) = self.db.get_outbound_by_client_id(bot_token, cid) {
                    self.db.update_outbound_state(out_row.id, "delivered", None);
                    self.broker.publish(
                        "send_ack",
                        serde_json::json!({
                            "req_id": out_row.trace_id,
                            "client_id": cid,
                            "state": "delivered",
                            "row_id": out_row.id,
                            "to_user_id": &from_user,
                            "text": &text,
                        }),
                    );
                    // 不再推 "message" 事件。
                    //   pending 元素已由 send_ack "sent" 更新为 ✓✓，
                    //   delivered 进一步确认即可。推 "message" 会被 dedup 丢弃（同 row_id），
                    //   且若 pending 已被移除会触发错误的 fallback 移除逻辑。
                }
            }
        }

        // 5) 标 processed
        self.db.mark_processed(save_result.id);

        Some(save_result.id)
    }

    /// 启动时恢复 processed=0 的入站消息（PR2）
    /// 仅对 SSE 没下发成功的消息重投，已下发的不重复（仅在 processed=0 时才在）
    pub fn recover_unprocessed_messages(&self) {
        let rows = self.db.get_unprocessed_messages(500);
        if rows.is_empty() {
            return;
        }
        tracing::info!("[RECOVER] 启动恢复 {} 条未处理入站消息", rows.len());
        let bot_token = self.token.read().clone().unwrap_or_default();
        for row in rows {
            // 始终从 item_list_json 重新解析，避免 text+media 混合消息丢媒体。
            let items: Vec<serde_json::Value> =
                serde_json::from_str(&row.item_list_json).unwrap_or_default();
            let (mut text, media_list) = self.process_message_items(&items);
            // 兜底：item_list_json 为空或无 text_item 时回退到 row.text（legacy 行）
            if text.is_empty() {
                text = row.text.clone().unwrap_or_default();
            }
            if text.is_empty() && media_list.is_empty() {
                self.db.mark_processed(row.id);
                continue;
            }
            let time_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            if !text.is_empty() && media_list.is_empty() {
                let event = serde_json::json!({
                    "from": row.user_id,
                    "to": "me",
                    "text": text,
                    "time": time_str,
                    "type": "in",
                    "trace_id": row.trace_id,
                    "recovered": true,
                });
                self.broker.publish("message", event);
            }
            for mi in &media_list {
                let media_type = mi.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let media_type_int = match media_type {
                    "image" => 2,
                    "voice" => 3,
                    "file" => 4,
                    "video" => 5,
                    _ => 0,
                };
                let media_prefix = match media_type {
                    "image" => "[图片]",
                    "voice" => "[语音]",
                    "file" => "[文件]",
                    "video" => "[视频]",
                    _ => "[媒体]",
                };
                let media_filename = mi.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                let msg_text = if !text.is_empty() {
                    format!("{} {}", media_prefix, text)
                } else {
                    format!("{} {}", media_prefix, media_filename)
                };
                let mut event = serde_json::json!({
                    "from": row.user_id,
                    "to": "me",
                    "text": msg_text,
                    "time": time_str,
                    "type": "in",
                    "trace_id": row.trace_id,
                    "recovered": true,
                    "media_type": media_type_int,
                    "media_filename": media_filename,
                    "has_media": true,
                });
                if let Some(item) = mi.get("item") {
                    if let Some(cdn_media) = self.extract_cdn_media(item) {
                        let cdn_str = serde_json::to_string(&cdn_media).unwrap_or_default();
                        let cache_key = self.media_cache_key(&cdn_media);
                        if let Some(obj) = event.as_object_mut() {
                            obj.insert("media_cdn".into(), serde_json::Value::String(cdn_str));
                            obj.insert(
                                "media_cache_id".into(),
                                serde_json::Value::String(cache_key),
                            );
                        }
                    }
                }
                self.broker.publish("message", event);
            }
            self.db.mark_processed(row.id);
        }
        let _ = bot_token; // 静默未使用
    }

    /// 启动时恢复未完成的出站消息
    /// 检查 send_state IN ('pending','sending','failed') 的出站行，重新 spawn retry
    pub fn recover_pending_outbound(self: &Arc<Self>) {
        let rows = self.db.list_pending_outbound("");
        if rows.is_empty() {
            return;
        }
        tracing::info!("[RECOVER] 启动恢复 {} 条未完成出站消息", rows.len());
        for row in rows {
            // 跳过已标记为 processed 的（可能在 stop 前就被处理了但因竞态未及时更新状态）
            if row.send_state == "failed" && row.send_attempts >= 5 {
                tracing::info!("[RECOVER] 跳过多重重试失败 row_id={}", row.id);
                continue;
            }
            let to_user = row.to_user_id.clone().unwrap_or_default();
            let text = row.text.clone().unwrap_or_default();
            let req_id = row.trace_id.clone();
            // messages_v2.bot_id 现在存的是 SHA-256 hash（不可逆），
            //   不能再作为 bot_token 用于调用 iLink API。通过 user_token_map 反查原 token；
            //   若反查失败（用户已被删除或 token 已刷新），标记为 failed 并跳过。
            let bot_token = match self.get_token_for_user(&to_user) {
                Some(t) if !t.is_empty() => t,
                _ => {
                    tracing::warn!(
                        "[RECOVER] row_id={} 找不到 {} 的 bot_token，跳过",
                        row.id,
                        to_user
                    );
                    self.db
                        .update_outbound_state(row.id, "failed", Some("bot_token_missing"));
                    continue;
                }
            };
            let context_token = row.context_token.clone().unwrap_or_default();

            if to_user.is_empty() || text.is_empty() {
                self.db
                    .update_outbound_state(row.id, "failed", Some("incomplete_row"));
                continue;
            }

            // 重新生成 client_id 避免冲突
            let new_client_id = format!(
                "msg-{}",
                &uuid::Uuid::new_v4().to_string().replace("-", "")[..16]
            );
            let new_req_id = if req_id.is_empty() {
                self.gen_trace_id()
            } else {
                req_id.clone()
            };
            self.db
                .update_outbound_resend(row.id, &new_client_id, &new_req_id);

            let bot = self.clone();
            let cid = new_client_id.clone();
            let bid = bot_token.clone();
            let tu = to_user.clone();
            let txt = text.clone();
            let rid = new_req_id.clone();
            let row_id = row.id;

            std::thread::Builder::new()
                .name("ilink-send-recover".into())
                .spawn(move || {
                    bot.spawn_retry_send(row_id, &cid, &bid, &tu, &context_token, &txt, &rid);
                })
                .ok();
        }
    }

    // ── HTTP 通信 ────────────────────────────────────────────

    fn build_headers(&self, token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let random_uin: u32 = rand::random();
        let wechat_uin = base64::engine::general_purpose::STANDARD.encode(random_uin.to_string());
        let token_guard = self.token.read();
        let use_token = token.or(token_guard.as_deref()).unwrap_or("");

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "AuthorizationType",
            HeaderValue::from_static("ilink_bot_token"),
        );
        if let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", use_token)) {
            headers.insert(AUTHORIZATION, val);
        }
        if let Ok(val) = HeaderValue::from_str(&wechat_uin) {
            headers.insert("X-WECHAT-UIN", val);
        }
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("iLink-Bot/1.0 (Zyn-ChatBox/{})", SCRIPT_VERSION))
                .unwrap_or_else(|_| HeaderValue::from_static("iLink-Bot/1.0")),
        );
        headers.insert("iLink-App-ClientVersion", HeaderValue::from_static("1.0.3"));
        headers
    }

    /// 向 iLink API 发送 POST 请求
    /// 返回格式：ret=0 成功，ret=-1 超时，ret=-2 HTTP错误，ret=-3 网络错误，ret=-4 JSON错误
    pub fn post(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        timeout_secs: u64,
        token: Option<&str>,
    ) -> serde_json::Value {
        let url = format!("{}/ilink/bot/{}", ILINK_BASE_URL, endpoint);
        let headers = self.build_headers(token);

        let client = if endpoint == "sendmessage" {
            &self.send_client
        } else {
            &self.http_client
        };

        match client
            .post(&url)
            .timeout(Duration::from_secs(timeout_secs))
            .headers(headers)
            .json(body)
            .send()
        {
            Ok(resp) => {
                let http_status = resp.status().as_u16();
                if http_status >= 400 {
                    return serde_json::json!({"ret": -2, "errmsg": format!("http_{}", http_status), "http_status": http_status});
                }
                match resp.text() {
                    Ok(text) => {
                        if text.trim().is_empty() || text.trim() == "{}" {
                            return serde_json::json!({"ret": 0});
                        }
                        match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(mut result) => {
                                // 检查过期
                                let errcode = result.get("errcode").and_then(|v| v.as_i64());
                                let ret = result.get("ret").and_then(|v| v.as_i64());
                                if let Some(ec) = errcode {
                                    if ec != 0 && is_expired_code(ec) {
                                        if let Some(obj) = result.as_object_mut() {
                                            obj.insert(
                                                "_expired".into(),
                                                serde_json::Value::Bool(true),
                                            );
                                        }
                                    }
                                }
                                if let Some(r) = ret {
                                    if r != 0 && is_expired_code(r) {
                                        if let Some(obj) = result.as_object_mut() {
                                            obj.insert(
                                                "_expired".into(),
                                                serde_json::Value::Bool(true),
                                            );
                                        }
                                    }
                                }
                                result
                            }
                            Err(_) => serde_json::json!({"ret": -4, "errmsg": "json_parse_error"}),
                        }
                    }
                    Err(_) => serde_json::json!({"ret": -4, "errmsg": "json_parse_error"}),
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    serde_json::json!({"ret": -1, "errmsg": "timeout"})
                } else if let Some(status) = e.status() {
                    serde_json::json!({"ret": -2, "errmsg": format!("http_{}", status.as_u16()), "http_status": status.as_u16()})
                } else {
                    serde_json::json!({"ret": -3, "errmsg": e.to_string()})
                }
            }
        }
    }

    // ── 消息处理 ─────────────────────────────────────────────

    fn extract_cdn_media(&self, item: &serde_json::Value) -> Option<serde_json::Value> {
        let item_keys = ["image_item", "video_item", "file_item", "voice_item"];
        for ik in &item_keys {
            if let Some(mi) = item.get(*ik) {
                if let Some(media) = mi.get("media") {
                    let mut cdn_media = media.clone();
                    if cdn_media.get("aes_key").is_none() {
                        if let Some(aeskey) = mi.get("aeskey").and_then(|v| v.as_str()) {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(aeskey);
                            if let Some(obj) = cdn_media.as_object_mut() {
                                obj.insert("aes_key".into(), serde_json::Value::String(b64));
                            }
                        }
                    }
                    return Some(cdn_media);
                }
            }
        }
        None
    }

    fn process_message_items(
        &self,
        item_list: &[serde_json::Value],
    ) -> (String, Vec<serde_json::Value>) {
        let mut text = String::new();
        let mut media_list: Vec<serde_json::Value> = Vec::new();

        for item in item_list {
            if let Some(text_item) = item.get("text_item") {
                text = text_item
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            if let Some(img) = item.get("image_item") {
                let filename = img
                    .get("file_name")
                    .or_else(|| img.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("image.jpg");
                media_list.push(serde_json::json!({
                    "type": "image",
                    "filename": filename,
                    "item": item,
                }));
            } else if let Some(vid) = item.get("video_item") {
                let mut raw_dur = vid
                    .get("play_length")
                    .or_else(|| vid.get("duration"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if raw_dur > 0 && raw_dur <= 60 {
                    raw_dur *= 1000;
                }
                let filename = vid
                    .get("file_name")
                    .or_else(|| vid.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("video.mp4");
                media_list.push(serde_json::json!({
                    "type": "video",
                    "filename": filename,
                    "duration": raw_dur,
                    "item": item,
                }));
            } else if let Some(fil) = item.get("file_item") {
                let filename = fil
                    .get("file_name")
                    .or_else(|| fil.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("文件");
                let description = fil
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                media_list.push(serde_json::json!({
                    "type": "file",
                    "filename": filename,
                    "description": description,
                    "item": item,
                }));
            } else if let Some(voi) = item.get("voice_item") {
                let mut raw_dur = voi
                    .get("playtime")
                    .or_else(|| voi.get("duration"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if raw_dur > 0 && raw_dur <= 60 {
                    raw_dur *= 1000;
                }
                let filename = voi
                    .get("file_name")
                    .or_else(|| voi.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("voice.silk");
                media_list.push(serde_json::json!({
                    "type": "voice",
                    "filename": filename,
                    "duration": raw_dur,
                    "item": item,
                }));
            }
        }
        (text, media_list)
    }

    // ── 发送消息 ─────────────────────────────────────────────

    /// 固定 5 次重试 [0s, 5s, 15s, 60s, 300s] (参考 openilink-hub)
    #[allow(clippy::too_many_arguments)]
    fn spawn_retry_send(
        self: Arc<Self>,
        row_id: i64,
        client_id: &str,
        bot_token: &str,
        to_user_id: &str,
        context_token: &str,
        text: &str,
        req_id: &str,
    ) {
        // S32: 获取并发许可，限制同时进行的重试发送数（8），
        //   clone Arc 后 acquire，_permit 借用局部 sem 而非 self，
        //   避免与后续 self.method() 调用的借用冲突；函数结束时 drop 自动释放
        let sem = self.retry_send_sem.clone();
        let _permit = sem.acquire();
        tracing::info!(
            "[SEND] 开始重试发送 row_id={} client_id={} to={} req_id={}",
            row_id,
            client_id,
            to_user_id,
            req_id
        );
        let delays = [0u64, 5, 15, 60, 300];
        let max_attempts = 5;
        let mut attempt = 0;
        let mut empty_ctx_retries = 0u32;
        const MAX_EMPTY_CTX_RETRIES: u32 = 60; // 60 × 5s = 5 分钟
        while attempt < max_attempts {
            let delay_for_empty_ctx = 5u64; // 固定 5s，等对方发消息

            // 移除 poll_health.state == "expired" 提前终止逻辑。
            // 原实现在 poll 检测到会话过期后立即 abort 所有发送，导致：
            //   1. 用户无法发送任何消息（即使 reauth 后旧 poll 仍标记 expired）
            //   2. resend 也被阻断（resend_outbound_async → spawn_retry_send 同样被 abort）
            //   3. 与 Python 原版行为不一致（原版直接调 API，由 API 返回码决定过期）
            // 现在改为：让 API 调用自行判断过期，is_expired_code(ec) 会处理真正的过期。

            // 每次重试动态读取 bot_token（reauth 后 user_token_map 已更新为新 token）
            let effective_bot_token = match self.get_token_for_user(to_user_id) {
                Some(t) if !t.is_empty() => t,
                _ => bot_token.to_string(), // 回退到传入的 token
            };

            // 每次重试动态读取 context_token（对方发消息后会填充）
            let current_ctx = self
                .context_tokens
                .read()
                .get(to_user_id)
                .cloned()
                .unwrap_or_default();
            let effective_ctx = if current_ctx.is_empty() {
                context_token
            } else {
                &current_ctx
            };

            // 如果 context_token 为空，跳过本次发送但不消耗重试次数（等对方先发消息）
            if effective_ctx.is_empty() {
                empty_ctx_retries += 1;
                if empty_ctx_retries >= MAX_EMPTY_CTX_RETRIES {
                    tracing::warn!(
                        "[SEND] 等待 context_token 超时（{}次），标记失败 row_id={}",
                        empty_ctx_retries,
                        row_id
                    );
                    self.db
                        .update_outbound_state(row_id, "failed", Some("context_token_timeout"));
                    self.broker.publish(
                        "send_ack",
                        serde_json::json!({
                            "req_id": req_id, "client_id": client_id, "row_id": row_id,
                            "state": "failed",
                            "error": "等待对方发消息超时",
                        }),
                    );
                    return;
                }
                tracing::info!(
                    "[SEND] context_token 为空，{}s 后重试 row_id={} ({}/{})",
                    delay_for_empty_ctx,
                    row_id,
                    empty_ctx_retries,
                    MAX_EMPTY_CTX_RETRIES
                );
                // 立即推进 state 为 sending，防 outbound-scan 重复 spawn。
                self.db.update_outbound_state(row_id, "sending", None);
                std::thread::sleep(Duration::from_secs(delay_for_empty_ctx));
                continue; // attempt 不递增
            }

            let body = serde_json::json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": to_user_id,
                    "client_id": client_id,
                    "message_type": 2,
                    "message_state": 2,
                    "context_token": effective_ctx,
                    "item_list": [{"type": 1, "text_item": {"text": text}}]
                },
                "base_info": {"channel_version": "1.0.3"}
            });
            if attempt > 0 {
                let delay = delays[attempt];
                tracing::info!(
                    "[SEND] 第 {} 次重试，{}s 后 (row_id={})",
                    attempt,
                    delay,
                    row_id
                );
                std::thread::sleep(Duration::from_secs(delay));
            }

            // 每次尝试都保持 "sending" 状态
            self.db.update_outbound_state(row_id, "sending", None);
            self.broker.publish(
                "send_ack",
                serde_json::json!({
                    "req_id": req_id, "client_id": client_id, "row_id": row_id,
                    "state": "sending", "attempt": attempt,
                }),
            );

            let result = self.post("sendmessage", &body, 10, Some(&effective_bot_token));
            let ret = result.get("ret").and_then(|v| v.as_i64());
            let errcode = result.get("errcode").and_then(|v| v.as_i64());
            let errmsg = result
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            tracing::info!(
                "[SEND] iLink API 响应 row_id={} attempt={} ret={:?} errcode={:?} errmsg={}",
                row_id,
                attempt,
                ret,
                errcode,
                errmsg
            );

            // 成功（增强检测：支持备选字段）
            if is_api_response_success(&result) {
                tracing::info!(
                    "[SEND] 发送成功 row_id={} client_id={} attempt={}",
                    row_id,
                    client_id,
                    attempt
                );
                // ponytail: 仅推进 messages_v2.send_state；老 messages 表 send_state 仍为 pending，
                //   待 storage.rs 增加按 client_id 更新 messages 表 send_state 的函数后在此补同步（失败/过期同理）
                self.db.update_outbound_state(row_id, "sent", None);
                self.broker.publish(
                    "send_ack",
                    serde_json::json!({
                        "req_id": req_id, "client_id": client_id, "row_id": row_id,
                        "state": "sent",
                        "to_user_id": to_user_id, "text": text,
                    }),
                );
                // 不再推 "message" 事件。
                //   pending 元素已包含文本/时间戳，send_ack "sent" 将其状态更新为 ✓✓。
                //   推 "message" 会触发 _handleIncomingMessage 移除 pending 并渲染新元素，
                //   导致 ✓✓ 状态丢失（_renderMsg 对 send_state=sent 不显示状态图标）。
                return;
            }

            // 过期
            if let Some(ec) = errcode {
                if is_expired_code(ec) {
                    tracing::warn!("[SEND] 会话已过期 row_id={} errcode={}", row_id, ec);
                    self.db
                        .update_outbound_state(row_id, "expired", Some("session_expired"));
                    self.broker.publish(
                        "send_ack",
                        serde_json::json!({
                            "req_id": req_id, "client_id": client_id, "row_id": row_id,
                            "state": "expired",
                        }),
                    );
                    // 不调用 remove_user —— 那会删除用户所有消息和配置。
                    // 仅标记本条消息为过期，保留用户数据以供 reauth 后继续使用
                    return;
                }
            }

            // 中间失败：保持 "sending" 状态
            let resp_json = serde_json::to_string(&result).unwrap_or_default();
            let resp_preview = safe_truncate(&resp_json, 200);
            let err_msg = format!("ret={:?} errcode={:?} errmsg={}", ret, errcode, errmsg);
            if attempt < max_attempts - 1 {
                tracing::warn!(
                    "[SEND] 第 {} 次尝试失败 (row_id={}): {} resp={}，将继续重试",
                    attempt,
                    row_id,
                    err_msg,
                    resp_preview
                );
            } else {
                // 最后一次尝试也失败了
                tracing::error!(
                    "[SEND] 全部 {} 次重试耗尽 row_id={} 最后错误: {}",
                    max_attempts,
                    row_id,
                    err_msg
                );
                self.db
                    .update_outbound_state(row_id, "failed", Some("retries_exhausted"));
                self.broker.publish(
                    "send_ack",
                    serde_json::json!({
                        "req_id": req_id, "client_id": client_id, "row_id": row_id,
                        "state": "failed",
                        "error": err_msg,
                    }),
                );
            }
            attempt += 1; // 只有实际发送后才递增（空 ctx 时走 continue 不递增）
        }
    }

    /// 发送 typing 指示（"对方正在输入..."）
    /// action: "start" | "stop"
    pub fn send_typing_indicator(self: &Arc<Self>, to_user_id: &str, action: &str) -> bool {
        let ctx = self.context_tokens.read();
        let context_token = match ctx.get(to_user_id) {
            Some(t) => t.clone(),
            None => return false,
        };
        drop(ctx);

        let token = match self.get_token_for_user(to_user_id) {
            Some(t) => t,
            None => return false,
        };

        let body = serde_json::json!({
            "to_user_id": to_user_id,
            "action": action,
            "context_token": context_token,
        });

        let result = self.post("typing_indicator", &body, 5, Some(&token));
        result.get("ret").and_then(|v| v.as_i64()) == Some(0)
    }

    /// 收到入站消息后自动开始 typing + 25s 超时自动停止
    pub fn auto_typing_indicator(self: &Arc<Self>, to_user_id: &str) {
        // 去重——同一用户 25s 内已有 typing 进行中则跳过，避免高频入站消息 spawn 大量睡眠线程
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        {
            let mut map = self.last_typing_time.lock();
            if let Some(&t) = map.get(to_user_id) {
                if now - t < 25.0 {
                    return;
                }
            }
            map.insert(to_user_id.to_string(), now);
        }
        if !self.send_typing_indicator(to_user_id, "start") {
            return;
        }
        let bot = self.clone();
        let user = to_user_id.to_string();
        std::thread::Builder::new()
            .name("ilink-typing-timeout".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_secs(25));
                bot.send_typing_indicator(&user, "stop");
            })
            .ok();
    }

    /// 手动重试某条失败的出站消息
    /// 从 messages_v2 读出原始行 → 重新生成 client_id → 后台重试
    /// 返回 (client_id, req_id) 供前端 ACK 匹配
    pub fn resend_outbound_async(self: &Arc<Self>, row_id: i64) -> Option<(String, String)> {
        // 1) 读原始行
        let row = self.db.get_message_v2(row_id)?;
        if row.direction != "out" {
            tracing::warn!("[RESEND] row_id={} 不是出站消息", row_id);
            return None;
        }
        if !(row.send_state == "failed" || row.send_state == "expired" || row.send_state.is_empty())
        {
            tracing::warn!(
                "[RESEND] row_id={} 当前状态 {} 不允许重试",
                row_id,
                row.send_state
            );
            return None;
        }
        let to_user_id = row.user_id.clone();
        let text = row.text.clone().unwrap_or_default();
        let context_token = match row.context_token.clone() {
            Some(t) if !t.is_empty() => t,
            _ => {
                tracing::warn!("[RESEND] row_id={} context_token 为空，会话已过期", row_id);
                return None;
            }
        };

        // 2) S13: 保留原有 client_id（避免 messages 表 JSON 中的 client_id 与 messages_v2 不同步
        //    导致 api_history 合并失败），仅生成新的 trace_id
        let preserved_client_id = row.client_id.clone().unwrap_or_else(|| {
            // 极端情况：原 client_id 缺失，生成新兜底
            format!(
                "msg-{}",
                &uuid::Uuid::new_v4().to_string().replace("-", "")[..16]
            )
        });
        let new_req_id = format!(
            "req-{}",
            &uuid::Uuid::new_v4().to_string().replace("-", "")[..16]
        );

        // 3) 更新 row 为 pending（保持 row_id 不变，前端按 row_id 关联）
        //    传回原 client_id，保证两表 client_id 一致
        self.db
            .update_outbound_resend(row_id, &preserved_client_id, &new_req_id);

        // 4) 推 pending ACK
        self.broker.publish(
            "send_ack",
            serde_json::json!({
                "req_id": new_req_id,
                "client_id": preserved_client_id,
                "row_id": row_id,
                "state": "pending",
                "to_user_id": to_user_id,
                "text": text,
                "resend": true,
            }),
        );

        // 5) 后台重试发送
        let bot_token = match self.get_token_for_user(&to_user_id) {
            Some(t) => t,
            None => {
                tracing::warn!("[RESEND] 没有可用的 bot token");
                return None;
            }
        };
        let bot = self.clone();
        let req_id_owned = new_req_id.clone();
        let client_id_owned = preserved_client_id.clone();
        let to_user_owned = to_user_id.clone();
        let text_owned = text.clone();
        std::thread::Builder::new()
            .name("ilink-send-resend".into())
            .spawn(move || {
                bot.spawn_retry_send(
                    row_id,
                    &client_id_owned,
                    &bot_token,
                    &to_user_owned,
                    &context_token,
                    &text_owned,
                    &req_id_owned,
                );
            })
            .ok();

        Some((preserved_client_id, new_req_id))
    }

    /// Web 端同步发送（对齐 Python 原版 send_text_for_account）
    ///
    /// 与 send_text_async 的区别：
    /// - 同步阻塞调用 iLink API，HTTP 响应即最终结果（成功/失败）
    /// - 不启动后台重试线程，不推送 send_ack 状态机
    /// - 成功后推 "message" 事件让前端立即渲染（与 Python 一致）
    /// - 失败直接返回 Err，前端显示"发送失败"
    ///
    /// 返回 Ok(message_json) 或 Err(error_msg)
    pub fn send_text_sync_web(
        self: &Arc<Self>,
        to_user_id: &str,
        text: &str,
        req_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        tracing::info!(
            "[SEND-WEB] 同步发送开始 to={} text_len={}",
            to_user_id,
            text.len()
        );

        let ctx_token = self.context_tokens.read().get(to_user_id).cloned();
        let context_token = match ctx_token {
            Some(t) if !t.is_empty() => t,
            _ => {
                tracing::warn!(
                    "[SEND-WEB] {} 暂无 context_token（对方未发过消息）",
                    to_user_id
                );
                return Err("没有对方的会话（对方还未发过消息）".to_string());
            }
        };
        let use_token = match self.get_token_for_user(to_user_id) {
            Some(t) => t,
            None => {
                tracing::warn!("[SEND-WEB] 没有 {} 的 bot token", to_user_id);
                return Err("发送失败：无可用 token".to_string());
            }
        };

        let lock = self.get_send_lock(to_user_id);
        let guard = lock.try_lock_for(Duration::from_secs(4));
        if guard.is_none() {
            tracing::warn!(
                "[SEND-WEB] 等待发送锁超时 user={}",
                safe_truncate(to_user_id, 12)
            );
            return Err("发送繁忙，请稍后重试".to_string());
        }

        let client_id = format!(
            "msg-{}",
            &uuid::Uuid::new_v4().to_string().replace("-", "")[..16]
        );
        let body = serde_json::json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": client_id,
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [{"type": 1, "text_item": {"text": text}}]
            },
            "base_info": {"channel_version": "1.0.3"}
        });
        let result = self.post("sendmessage", &body, 10, Some(&use_token));
        drop(guard);

        let ret = result.get("ret").and_then(|v| v.as_i64());
        let errcode = result.get("errcode").and_then(|v| v.as_i64());
        let errmsg = result
            .get("errmsg")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        tracing::info!(
            "[SEND-WEB] iLink API 响应 ret={:?} errcode={:?} errmsg={}",
            ret,
            errcode,
            errmsg
        );

        // 增强成功检测：当 ret/errcode 不存在时也检查备选字段
        if is_api_response_success(&result) {
            tracing::info!(
                "[SEND-WEB] 发送成功 to={} client_id={}",
                to_user_id,
                client_id
            );
            let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let mut out_msg = serde_json::json!({
                "from": "me",
                "to": to_user_id,
                "text": text,
                "time": now_str,
                "type": "out",
                "client_id": client_id,
                "send_state": "sent",
            });
            // 把前端生成的 req_id 写回消息事件/DB，便于前端按精确 key 替换对应 pending 气泡。
            if let (Some(obj), Some(rid)) = (out_msg.as_object_mut(), req_id) {
                if !rid.is_empty() {
                    obj.insert("req_id".to_string(), rid.into());
                }
            }
            // 写入 messages 表（持久化）+ 推 WS "message" 事件让前端渲染
            let out_msg_with_id = self.add_message_to_history(out_msg);
            self.broker.publish("message", out_msg_with_id.clone());
            return Ok(out_msg_with_id);
        }

        if let Some(ec) = errcode {
            if is_expired_code(ec) {
                tracing::warn!("[SEND-WEB] 会话已过期 to={} errcode={}", to_user_id, ec);
                return Err("session_expired".to_string());
            }
        }
        let resp_preview = serde_json::to_string(&result).unwrap_or_default();
        tracing::warn!(
            "[SEND-WEB] 发送失败 to={} ret={:?} errcode={:?} resp={}",
            to_user_id,
            ret,
            errcode,
            safe_truncate(&resp_preview, 200)
        );
        Err(format!("发送失败: {}", errmsg))
    }

    // ── 媒体上传/下载 ────────────────────────────────────────

    /// CDN Presign：获取预签名上传 URL，客户端直传 CDN（不用服务器中转）
    /// 返回 upload_url + encrypt_query_param + aes_key
    ///
    /// 原实现 raw_md5 硬编码为空字符串 MD5——iLink 现在需要真实 MD5 校验完整性。
    ///   "d41d8cd98f00b204e9800998ecf8427e"，CDN 收到该值无法做完整性校验，
    ///   攻击者可上传与 presign 返回的 filekey 不匹配的内容。
    ///   改为调用方传入真实 file_md5（客户端用 SubtleCrypto 算 MD5 后随请求提交）。
    pub fn presign_media_upload(
        &self,
        media_type: &str,
        filename: &str,
        file_size: usize,
        file_md5: &str,
    ) -> Option<serde_json::Value> {
        let user_id = self.get_current_user()?;
        let use_token = self.get_token_for_user(&user_id)?;

        let media_type_int = match media_type {
            "image" => 1i64,
            "video" => 2i64,
            "file" => 3i64,
            _ => {
                tracing::warn!("[PRESIGN] 不支持的 media_type: {}", media_type);
                return None;
            }
        };

        let aes_key_hex = crypto::random_hex(16);
        let filekey = crypto::random_hex(16);
        // 使用调用方传入的实际 MD5，不再用空字符串占位 MD5。
        let raw_md5 = file_md5;

        let body = serde_json::json!({
            "filekey": filekey,
            "media_type": media_type_int,
            "to_user_id": user_id,
            "rawsize": file_size,
            "rawfilemd5": raw_md5,
            "filesize": file_size,
            "no_need_thumb": true,
            "aeskey": aes_key_hex,
            "base_info": {"channel_version": "1.0.3"}
        });

        let result = self.post("getuploadurl", &body, 25, Some(&use_token));
        let upload_url = result.get("upload_url").and_then(|v| v.as_str())?;
        let encrypt_query_param = result
            .get("encrypt_query_param")
            .or_else(|| result.get("encrypted_query_param"))
            .and_then(|v| v.as_str())?;

        Some(serde_json::json!({
            "ok": true,
            "upload_url": upload_url,
            "encrypt_query_param": encrypt_query_param,
            "aes_key": aes_key_hex,
            "filekey": filekey,
            "media_type": media_type,
            "filename": filename,
        }))
    }

    pub fn upload_media(
        &self,
        file_bytes: &[u8],
        filename: &str,
        media_type: i64,
        to_user_id: &str,
    ) -> Option<UploadMediaResult> {
        let use_token = self.get_token_for_user(to_user_id)?;

        let aes_key_hex = crypto::random_hex(16);
        let aes_key_bytes = hex::decode(&aes_key_hex).ok()?;
        // ponytail: ECB模式为iLink CDN协议要求,密钥每次随机生成,无法改用GCM
        let encrypted = crypto::aes_ecb_encrypt(file_bytes, &aes_key_bytes).ok()?;
        let filekey = crypto::random_hex(16);
        let raw_md5 = crypto::md5_hex(file_bytes);

        let body = serde_json::json!({
            "filekey": filekey,
            "media_type": media_type,
            "to_user_id": to_user_id,
            "rawsize": file_bytes.len(),
            "rawfilemd5": raw_md5,
            "filesize": encrypted.len(),
            "no_need_thumb": true,
            "aeskey": aes_key_hex,
            "base_info": {"channel_version": "1.0.3"}
        });

        let result = self.post("getuploadurl", &body, 25, Some(&use_token));
        let ret = result.get("ret").and_then(|v| v.as_i64());
        let errcode = result.get("errcode").and_then(|v| v.as_i64());
        let errmsg = result.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");

        // 打印 ret/errcode/errmsg，便于排查 getuploadurl 失败原因。
        if ret.map(|r| r != 0).unwrap_or(false) || errcode.map(|e| e != 0).unwrap_or(false) {
            tracing::warn!(
                "[媒体上传失败] getuploadurl 失败 file={} media_type={} rawsize={} ret={:?} errcode={:?} errmsg={}",
                filename, media_type, file_bytes.len(), ret, errcode, errmsg
            );
            return None;
        }

        let upload_param = result.get("upload_param").and_then(|v| v.as_str())?;
        let cdn_url = format!(
            "{}/upload?encrypted_query_param={}&filekey={}",
            CDN_BASE,
            percent_encoding::percent_encode(
                upload_param.as_bytes(),
                percent_encoding::NON_ALPHANUMERIC
            ),
            percent_encoding::percent_encode(
                filekey.as_bytes(),
                percent_encoding::NON_ALPHANUMERIC
            ),
        );

        let resp = self
            .cdn_client
            .post(&cdn_url)
            .timeout(Duration::from_secs(120))
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(encrypted.clone())
            .send()
            .ok()?;

        let encrypted_param = resp
            .headers()
            .get("x-encrypted-param")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let encrypted_param = match encrypted_param {
            Some(p) if !p.is_empty() => p,
            _ => {
                tracing::warn!("[媒体上传失败] CDN 响应缺少 x-encrypted-param 头");
                return None;
            }
        };

        let aes_key_b64 = base64::engine::general_purpose::STANDARD.encode(&aes_key_hex);

        Some(UploadMediaResult {
            filekey,
            encrypt_query_param: encrypted_param,
            aes_key: aes_key_b64,
            aes_key_hex,
            raw_size: file_bytes.len(),
            encrypted_size: encrypted.len(),
            md5: raw_md5,
            filename: filename.to_string(),
        })
    }

    pub fn send_image(
        &self,
        to_user_id: &str,
        image_bytes: &[u8],
        filename: &str,
        description: &str,
    ) -> bool {
        tracing::info!(
            "[SEND] 发送图片给 {} 文件={} 大小={}字节",
            to_user_id,
            filename,
            image_bytes.len()
        );
        let uploaded = match self.upload_media(image_bytes, filename, 1, to_user_id) {
            Some(u) => u,
            None => {
                tracing::warn!("[SEND] 图片上传失败 给 {}", to_user_id);
                return false;
            }
        };

        // 写入本地媒体缓存，避免刷新后通过 CDN 重新下载
        let cache_key = crypto::md5_hex(uploaded.encrypt_query_param.as_bytes());
        if !self.disable_media_cache {
            if let Err(e) = self.save_media_cache(
                &cache_key,
                image_bytes,
                media::detect_mime(image_bytes),
                filename,
            ) {
                tracing::warn!("[SEND] 图片本地缓存写入失败: {}", e);
            }
        }

        let image_item = serde_json::json!({
            "type": 2,
            "image_item": {
                "media": {
                    "encrypt_query_param": uploaded.encrypt_query_param,
                    "aes_key": uploaded.aes_key,
                    "encrypt_type": 1,
                },
                "aeskey": uploaded.aes_key_hex,
                "mid_size": uploaded.encrypted_size,
            }
        });

        let ok = self.send_media_message(
            to_user_id,
            &image_item,
            description,
            "",
            filename,
            0,
            &uploaded,
        );
        if ok {
            tracing::info!("[SEND] 图片发送成功 给 {} 文件={}", to_user_id, filename);
        } else {
            tracing::warn!("[SEND] 图片发送失败 给 {} 文件={}", to_user_id, filename);
        }
        ok
    }

    pub fn send_file(
        &self,
        to_user_id: &str,
        file_bytes: &[u8],
        filename: &str,
        description: &str,
    ) -> bool {
        tracing::info!(
            "[SEND] 发送文件给 {} 文件={} 大小={}字节",
            to_user_id,
            filename,
            file_bytes.len()
        );
        // iLink SDK 上传协议中 file 的 media_type=3（不是消息协议中的 4）。
        //   与 send_video 同一类问题，之前传 4 会让 getuploadurl 返回失败。
        let uploaded = match self.upload_media(file_bytes, filename, 3, to_user_id) {
            Some(u) => u,
            None => {
                tracing::warn!("[SEND] 文件上传失败 给 {}", to_user_id);
                return false;
            }
        };

        // 写入本地媒体缓存
        let cache_key = crypto::md5_hex(uploaded.encrypt_query_param.as_bytes());
        if !self.disable_media_cache {
            if let Err(e) =
                self.save_media_cache(&cache_key, file_bytes, "application/octet-stream", filename)
            {
                tracing::warn!("[SEND] 文件本地缓存写入失败: {}", e);
            }
        }

        let file_item = serde_json::json!({
            "type": 4,
            "file_item": {
                "media": {
                    "encrypt_query_param": uploaded.encrypt_query_param,
                    "aes_key": uploaded.aes_key,
                    "encrypt_type": 1,
                },
                "aeskey": uploaded.aes_key_hex,
                "file_name": filename,
                "file_size": uploaded.raw_size,
            }
        });

        let ok = self.send_media_message(
            to_user_id,
            &file_item,
            description,
            "",
            filename,
            0,
            &uploaded,
        );
        if ok {
            tracing::info!("[SEND] 文件发送成功 给 {} 文件={}", to_user_id, filename);
        } else {
            tracing::warn!("[SEND] 文件发送失败 给 {} 文件={}", to_user_id, filename);
        }
        ok
    }

    pub fn send_video(
        &self,
        to_user_id: &str,
        video_bytes: &[u8],
        filename: &str,
        duration: i64,
    ) -> bool {
        tracing::info!(
            "[SEND] 发送视频给 {} 文件={} 大小={}字节",
            to_user_id,
            filename,
            video_bytes.len()
        );
        // iLink SDK 上传协议中 video 的 media_type=2（不是消息协议中的 5）。
        //   之前传 5 导致 getuploadurl 返回失败、视频上传不了。
        let uploaded = match self.upload_media(video_bytes, filename, 2, to_user_id) {
            Some(u) => u,
            None => {
                tracing::warn!("[SEND] 视频上传失败 给 {}", to_user_id);
                return false;
            }
        };

        // 写入本地媒体缓存
        let cache_key = crypto::md5_hex(uploaded.encrypt_query_param.as_bytes());
        if !self.disable_media_cache {
            if let Err(e) = self.save_media_cache(
                &cache_key,
                video_bytes,
                media::detect_mime(video_bytes),
                filename,
            ) {
                tracing::warn!("[SEND] 视频本地缓存写入失败: {}", e);
            }
        }

        let video_item = serde_json::json!({
            "type": 5,
            "video_item": {
                "media": {
                    "encrypt_query_param": uploaded.encrypt_query_param,
                    "aes_key": uploaded.aes_key,
                    "encrypt_type": 1,
                },
                "aeskey": uploaded.aes_key_hex,
                "play_length": duration,
            }
        });

        let ok = self.send_media_message(
            to_user_id,
            &video_item,
            "",
            "",
            filename,
            duration,
            &uploaded,
        );
        if ok {
            tracing::info!("[SEND] 视频发送成功 给 {} 文件={}", to_user_id, filename);
        } else {
            tracing::warn!("[SEND] 视频发送失败 给 {} 文件={}", to_user_id, filename);
        }
        ok
    }

    /// S42: 发送语音消息。
    ///   iLink SDK 上传协议无 voice 专用 media_type，复用 file 的 media_type=3；
    ///   消息 item type=3（voice），voice_item 结构参考接收侧 playtime/file_name + media/aeskey。
    pub fn send_voice(
        &self,
        to_user_id: &str,
        voice_bytes: &[u8],
        filename: &str,
        duration: i64,
    ) -> bool {
        tracing::info!(
            "[SEND] 发送语音给 {} 文件={} 大小={}字节 时长={}ms",
            to_user_id,
            filename,
            voice_bytes.len(),
            duration
        );
        let uploaded = match self.upload_media(voice_bytes, filename, 3, to_user_id) {
            Some(u) => u,
            None => {
                tracing::warn!("[SEND] 语音上传失败 给 {}", to_user_id);
                return false;
            }
        };

        let cache_key = crypto::md5_hex(uploaded.encrypt_query_param.as_bytes());
        if !self.disable_media_cache {
            if let Err(e) = self.save_media_cache(&cache_key, voice_bytes, "audio/silk", filename) {
                tracing::warn!("[SEND] 语音本地缓存写入失败: {}", e);
            }
        }

        let voice_item = serde_json::json!({
            "type": 3,
            "voice_item": {
                "media": {
                    "encrypt_query_param": uploaded.encrypt_query_param,
                    "aes_key": uploaded.aes_key,
                    "encrypt_type": 1,
                },
                "aeskey": uploaded.aes_key_hex,
                "playtime": duration,
                "file_name": filename,
            }
        });

        let ok = self.send_media_message(
            to_user_id,
            &voice_item,
            "",
            "",
            filename,
            duration,
            &uploaded,
        );
        if ok {
            tracing::info!("[SEND] 语音发送成功 给 {} 文件={}", to_user_id, filename);
        } else {
            tracing::warn!("[SEND] 语音发送失败 给 {} 文件={}", to_user_id, filename);
        }
        ok
    }

    #[allow(clippy::too_many_arguments)]
    fn send_media_message(
        &self,
        to_user_id: &str,
        media_item: &serde_json::Value,
        description: &str,
        _media_data: &str,
        media_filename: &str,
        media_duration: i64,
        uploaded: &UploadMediaResult,
    ) -> bool {
        let context_token = match self.context_tokens.read().get(to_user_id).cloned() {
            Some(t) => t,
            None => return false,
        };
        let use_token = match self.get_token_for_user(to_user_id) {
            Some(t) => t,
            None => return false,
        };

        let lock = self.get_send_lock(to_user_id);
        let guard = lock.try_lock_for(Duration::from_secs(10));
        if guard.is_none() {
            return false;
        }

        // 先发描述文本
        if !description.is_empty() {
            let client_id = format!(
                "msg-{}",
                &uuid::Uuid::new_v4().to_string().replace("-", "")[..16]
            );
            let body = serde_json::json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": to_user_id,
                    "client_id": client_id,
                    "message_type": 2,
                    "message_state": 2,
                    "context_token": &context_token,
                    "item_list": [{"type": 1, "text_item": {"text": description}}]
                },
                "base_info": {"channel_version": "1.0.3"}
            });
            self.post("sendmessage", &body, 10, Some(&use_token));
        }

        let client_id = format!(
            "ilink-sdk:{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            &uuid::Uuid::new_v4().to_string().replace("-", "")[..8]
        );
        let body = serde_json::json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": client_id,
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [media_item]
            },
            "base_info": {"channel_version": "1.0.3"}
        });

        let result = {
            let mut result = serde_json::Value::Null;
            for attempt in 0..3u32 {
                result = self.post("sendmessage", &body, 10, Some(&use_token));
                let ret = result.get("ret").and_then(|v| v.as_i64());
                // Retry on timeout (-1) or network error (-3)
                if (ret == Some(-1) || ret == Some(-3)) && attempt < 2 {
                    let delay_ms = 200u64 * (1 << attempt);
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    continue;
                }
                break;
            }
            result
        };
        drop(guard);

        let ret = result.get("ret").and_then(|v| v.as_i64());
        let errcode = result.get("errcode").and_then(|v| v.as_i64());
        let success = (ret.is_none() || ret == Some(0))
            && (errcode.is_none() || errcode == Some(0))
            && !errcode.map(is_expired_code).unwrap_or(false)
            && !ret.map(is_expired_code).unwrap_or(false);

        if success {
            let type_name = match media_item.get("type").and_then(|v| v.as_i64()) {
                Some(2) => "图片",
                Some(3) => "语音",
                Some(4) => "文件",
                Some(5) => "视频",
                _ => "媒体",
            };

            // 构造 CDN 媒体信息（与入站消息格式对齐，便于前端通过 media_cdn 重新下载）
            let cdn_media = serde_json::json!({
                "encrypt_query_param": uploaded.encrypt_query_param,
                "aes_key": uploaded.aes_key,
                "aeskey": uploaded.aes_key_hex,
                "encrypt_type": 1,
            });
            let cdn_str = serde_json::to_string(&cdn_media).unwrap_or_default();
            let cache_key = crypto::md5_hex(uploaded.encrypt_query_param.as_bytes());

            let mut out_msg = serde_json::json!({
                "from": "me",
                "to": to_user_id,
                "text": format!("[{}]{}", type_name, if description.is_empty() { String::new() } else { format!(" {}", description) }),
                "time": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                "type": "out",
                "media_type": media_item.get("type"),
                "media_filename": media_filename,
                "media_duration": media_duration,
                "media_cdn": cdn_str,
                "media_cache_id": cache_key,
            });

            // 如果有 WebDAV，填入 webdav_url
            if let Some(url) = self.webdav_url_for_cache_key(
                &cache_key,
                media::derive_ext("", media_filename).as_deref(),
            ) {
                if let Some(obj) = out_msg.as_object_mut() {
                    obj.insert("media_webdav_url".into(), serde_json::Value::String(url));
                }
            }

            tracing::info!("[SEND] {}发送成功 给 {}", type_name, to_user_id);
            let out_msg_with_id = self.add_message_to_history(out_msg);
            self.broker.publish("message", out_msg_with_id);
            return true;
        }
        tracing::warn!(
            "[SEND] 媒体发送失败 给 {} ret={:?} errcode={:?}",
            to_user_id,
            ret,
            errcode
        );
        false
    }

    // ── 媒体下载 ─────────────────────────────────────────────

    pub fn media_cache_key_public(&self, cdn_media_info: &serde_json::Value) -> String {
        self.media_cache_key(cdn_media_info)
    }

    fn media_cache_key(&self, cdn_media_info: &serde_json::Value) -> String {
        let param = cdn_media_info
            .get("encrypt_query_param")
            .or_else(|| cdn_media_info.get("encrypted_query_param"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if param.is_empty() {
            // ponytail: 缺失 encrypt_query_param 时用随机串，避免不同媒体命中 md5("") 这一公开常量造成缓存错配
            return crypto::md5_hex(crypto::random_hex(16).as_bytes());
        }
        crypto::md5_hex(param.as_bytes())
    }

    fn resolve_aes_key(&self, cdn_media_info: &serde_json::Value) -> Option<Vec<u8>> {
        let aes_key_b64 = cdn_media_info
            .get("aes_key")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                let hex = cdn_media_info
                    .get("aeskey")
                    .or_else(|| cdn_media_info.get("aes_key_hex"))
                    .and_then(|v| v.as_str())?;
                Some(base64::engine::general_purpose::STANDARD.encode(hex))
            })?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(aes_key_b64)
            .ok()?;
        if decoded.len() == 16 {
            Some(decoded)
        } else {
            let hex_str = String::from_utf8_lossy(&decoded);
            hex::decode(hex_str.as_ref()).ok()
        }
    }

    fn resolve_download_url(&self, cdn_media_info: &serde_json::Value) -> Option<String> {
        let param = cdn_media_info
            .get("encrypt_query_param")
            .or_else(|| cdn_media_info.get("encrypted_query_param"))
            .and_then(|v| v.as_str())?;
        Some(format!(
            "{}/download?encrypted_query_param={}",
            CDN_BASE,
            percent_encoding::percent_encode(param.as_bytes(), percent_encoding::NON_ALPHANUMERIC)
        ))
    }

    pub fn download_media(
        &self,
        cdn_media_info: &serde_json::Value,
        filename: &str,
        _user_id: &str,
    ) -> Option<Vec<u8>> {
        let cache_key = self.media_cache_key(cdn_media_info);

        // 查缓存
        if let Some(cached) = self.get_cached_media(&cache_key) {
            return Some(cached);
        }

        // 下载
        let download_url = self.resolve_download_url(cdn_media_info)?;
        let aes_key_bytes = self.resolve_aes_key(cdn_media_info)?;

        let resp = self
            .cdn_client
            .get(&download_url)
            .timeout(Duration::from_secs(120))
            .send()
            .ok()?;
        // 预检 Content-Length，提前拒绝（仍保留以快速短路）
        if let Some(len) = resp.content_length() {
            if len > MAX_DECRYPT_SIZE as u64 {
                tracing::warn!(
                    "[媒体下载] Content-Length {} 超限 {}",
                    len,
                    MAX_DECRYPT_SIZE
                );
                return None;
            }
        }
        // S30: 流式读取 + 边读边校验大小，避免无 Content-Length 时 OOM
        let data = match read_response_with_size_limit(resp, MAX_DECRYPT_SIZE) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[媒体下载] 流式读取失败或超限: {}", e);
                return None;
            }
        };

        // ponytail: ECB模式为iLink CDN协议要求,密钥每次随机生成,无法改用GCM
        let decrypted = crypto::aes_ecb_decrypt(&data, &aes_key_bytes).ok()?;

        // MIME 检测 + 转码
        let mime = media::detect_mime(&decrypted);
        // 转码失败保留原始数据 + 真实 MIME，前端元素可降级处理（不显示误导性错误）。
        //   加载 audio/silk 会触发 error 事件，toast 显示"语音转码失败"更准确。
        let (final_data, final_mime, final_filename) = match mime {
            "audio/silk" => {
                let r = media::silk_to_wav(&decrypted);
                if r.success {
                    (r.data, "audio/wav", filename.replace(".silk", ".wav"))
                } else {
                    tracing::warn!(
                        "[媒体] SILK 转码失败，保留原始数据 mime=audio/silk filename={}",
                        filename
                    );
                    (r.data, "audio/silk", filename.to_string())
                }
            }
            "audio/amr" => {
                let r = media::ffmpeg_to_wav(&decrypted);
                if r.success {
                    (r.data, "audio/wav", filename.replace(".amr", ".wav"))
                } else {
                    tracing::warn!(
                        "[媒体] AMR 转码失败，保留原始数据 mime=audio/amr filename={}",
                        filename
                    );
                    (r.data, "audio/amr", filename.to_string())
                }
            }
            _ => (decrypted, mime, filename.to_string()),
        };

        // 缓存
        if !self.disable_media_cache {
            let _ = self.save_media_cache(&cache_key, &final_data, final_mime, &final_filename);
        }

        Some(final_data)
    }

    pub fn stream_media_from_cdn(
        &self,
        cdn_media_info: &serde_json::Value,
        filename: &str,
    ) -> Option<(Vec<u8>, String, String)> {
        let download_url = self.resolve_download_url(cdn_media_info)?;
        let aes_key_bytes = self.resolve_aes_key(cdn_media_info)?;

        let resp = self
            .cdn_client
            .get(&download_url)
            .timeout(Duration::from_secs(120))
            .send()
            .ok()?;
        // 预检 Content-Length，提前拒绝（仍保留以快速短路）
        if let Some(len) = resp.content_length() {
            if len > MAX_DECRYPT_SIZE as u64 {
                tracing::warn!(
                    "[媒体流式下载] Content-Length {} 超限 {}",
                    len,
                    MAX_DECRYPT_SIZE
                );
                return None;
            }
        }
        // S30: 流式读取 + 边读边校验大小
        let data = match read_response_with_size_limit(resp, MAX_DECRYPT_SIZE) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[媒体流式下载] 流式读取失败或超限: {}", e);
                return None;
            }
        };

        // ponytail: ECB模式为iLink CDN协议要求,密钥每次随机生成,无法改用GCM
        let decrypted = crypto::aes_ecb_decrypt(&data, &aes_key_bytes).ok()?;
        let mime = media::detect_mime(&decrypted);

        let (final_data, final_mime, final_filename) = match mime {
            "audio/silk" => {
                // 同 download_media_from_cdn，转码失败保留原始数据 + 真实 MIME。
                let r = media::silk_to_wav(&decrypted);
                if r.success {
                    (
                        r.data,
                        "audio/wav".to_string(),
                        filename.replace(".silk", ".wav"),
                    )
                } else {
                    tracing::warn!(
                        "[媒体流式] SILK 转码失败，保留原始数据 mime=audio/silk filename={}",
                        filename
                    );
                    (r.data, "audio/silk".to_string(), filename.to_string())
                }
            }
            "audio/amr" => {
                let r = media::ffmpeg_to_wav(&decrypted);
                if r.success {
                    (
                        r.data,
                        "audio/wav".to_string(),
                        filename.replace(".amr", ".wav"),
                    )
                } else {
                    tracing::warn!(
                        "[媒体流式] AMR 转码失败，保留原始数据 mime=audio/amr filename={}",
                        filename
                    );
                    (r.data, "audio/amr".to_string(), filename.to_string())
                }
            }
            _ => (decrypted, mime.to_string(), filename.to_string()),
        };

        Some((final_data, final_mime, final_filename))
    }

    // ── 媒体缓存 ─────────────────────────────────────────────

    pub fn get_cached_media(&self, cache_key: &str) -> Option<Vec<u8>> {
        if !is_valid_cache_key(cache_key) {
            tracing::warn!("[BOT] 非法 cache_key 被拒绝: {}", cache_key);
            return None;
        }
        // 先查 WebDAV
        {
            let wd_client = self.webdav_client.read();
            if let Some(ref client) = *wd_client {
                if let Some(remote) = self.db.get_media_remote(cache_key) {
                    if let Some(data) = client.download(&remote.remote_path) {
                        return Some(data);
                    }
                }
            }
        }
        // 查本地文件
        let cache_path = self
            .media_cache_dir
            .join(cache_key[..2].to_lowercase())
            .join(cache_key);
        if cache_path.exists() {
            return std::fs::read(&cache_path).ok();
        }
        None
    }

    fn save_media_cache(
        &self,
        cache_key: &str,
        data: &[u8],
        mime: &str,
        filename: &str,
    ) -> std::io::Result<()> {
        if !is_valid_cache_key(cache_key) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "非法 cache_key",
            ));
        }
        let dir = self.media_cache_dir.join(cache_key[..2].to_lowercase());
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(cache_key);
        std::fs::write(&path, data)?;
        self.db
            .save_media_meta(cache_key, mime, filename, data.len() as i64, "global");

        // 同步到 WebDAV（带 MD5 校验 + 扩展名）
        let wd_client = self.webdav_client.read();
        if let Some(ref client) = *wd_client {
            let ext = media::derive_ext(mime, filename);
            let local_md5 = crypto::md5_hex(data);
            match client.upload_with_check(
                cache_key,
                ext.as_deref(),
                data.to_vec(),
                mime,
                &local_md5,
            ) {
                Ok(result) => {
                    self.db
                        .save_media_remote(cache_key, &result.remote_path, "", &local_md5);
                    if result.skipped {
                        tracing::info!("[WebDAV] 缓存上传跳过（MD5 命中）: {}", cache_key);
                    } else if result.overwritten {
                        tracing::warn!("[WebDAV] 缓存上传覆盖远端: {}", cache_key);
                    }
                }
                Err(e) => {
                    tracing::warn!("[WebDAV] 上传缓存失败: {}", e);
                }
            }
        }
        Ok(())
    }

    /// 若 WebDAV 启用，返回该 cache_key 对应的代理 URL
    /// ext: 可选扩展名（如 ".jpg"），与新上传命名约定一致
    fn webdav_url_for_cache_key(&self, cache_key: &str, ext: Option<&str>) -> Option<String> {
        let cfg = self.webdav_config.read();
        if !cfg.enabled {
            return None;
        }
        let remote_path = {
            let wd_client = self.webdav_client.read();
            wd_client
                .as_ref()
                .map(|c| c.remote_path_for_ext(cache_key, ext))
        };
        remote_path.map(|p| format!("/api/wasm/webdav-proxy{}", p))
    }

    fn cleanup_expired_media_cache(&self, max_age: Duration) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        fn visit_dir(
            dir: &std::path::Path,
            now: Duration,
            max_age: Duration,
            expired: &mut Vec<(PathBuf, String)>,
        ) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit_dir(&path, now, max_age, expired);
                } else if let Some(name) =
                    path.file_name().and_then(|s| s.to_str()).map(str::to_owned)
                {
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            let age = modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default();
                            if now > age && now - age >= max_age {
                                expired.push((path, name));
                            }
                        }
                    }
                }
            }
        }
        let mut expired = Vec::new();
        visit_dir(&self.media_cache_dir, now, max_age, &mut expired);
        for (path, key) in expired {
            // 只有确认远端副本存在时才删除本地缓存。否则该文件仍是唯一副本，
            // 删除会让聊天附件不可恢复，也会造成媒体配额与真实数据不一致。
            if self.db.get_media_remote(&key).is_none() {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    if let Err(e) = self.db.mark_media_local_absent(&key) {
                        tracing::warn!("[媒体清理] 标记本地副本失败 key={}: {}", key, e);
                    } else {
                        tracing::info!("[媒体清理] 已删除有远端备份的过期本地缓存: {}", key);
                    }
                }
                Err(e) => tracing::warn!("[媒体清理] 删除本地缓存失败 key={}: {}", key, e),
            }
        }
    }

    fn submit_prefetch_task(
        self: &Arc<Self>,
        cache_key: &str,
        cdn_info: &serde_json::Value,
        filename: &str,
        user_id: &str,
    ) {
        let tx = self.prefetch_tx.lock().clone();
        if let Some(tx) = tx {
            let task = PrefetchTask {
                bot: self.clone(),
                cache_key: cache_key.to_string(),
                cdn_info: cdn_info.clone(),
                filename: filename.to_string(),
                user_id: user_id.to_string(),
            };
            if tx.send(task).is_err() {
                tracing::debug!("[PREFETCH] 预取队列已关闭");
            }
        }
    }

    // ── QR 码生成 ────────────────────────────────────────────

    fn generate_qr_matrix(&self, data: &str) -> Vec<Vec<String>> {
        use qrcode::QrCode;
        match QrCode::new(data) {
            Ok(code) => {
                let width = code.width();
                let mut matrix = Vec::with_capacity(width);
                for y in 0..width {
                    let mut row = Vec::with_capacity(width);
                    for x in 0..width {
                        // 与 Python 版一致：黑色用 "█"，白色用 " "
                        // 前端通过 e === " " 判断白色单元格
                        row.push(if code[(x, y)] == qrcode::Color::Dark {
                            "█".to_string()
                        } else {
                            " ".to_string()
                        });
                    }
                    matrix.push(row);
                }
                matrix
            }
            Err(_) => Vec::new(),
        }
    }

    // ── 轮询 ─────────────────────────────────────────────────

    pub fn start_polling(self: &Arc<Self>) {
        let token = self.token.read().clone();
        if let Some(ref t) = token {
            if !self.bot_accounts.read().contains_key(t) {
                self.bot_accounts.write().insert(
                    t.clone(),
                    serde_json::json!({
                        "bot_id": self.bot_id.read().clone().unwrap_or_default(),
                        "user_id": self.user_id.read().clone().unwrap_or_default(),
                        "cursor": *self.cursor.read(),
                        "context_tokens": *self.context_tokens.read(),
                    }),
                );
            }
        }

        let mut seen_tokens = HashSet::new();
        let utm = self.user_token_map.read().clone();
        for bot_token in utm.values() {
            if !bot_token.is_empty() && !seen_tokens.contains(bot_token) {
                seen_tokens.insert(bot_token.clone());
                let accounts = self.bot_accounts.read().clone();
                if let Some(account) = accounts.get(bot_token) {
                    self.start_account_poll(bot_token, account);
                }
            }
        }

        // 主 token 必须始终有轮询
        if let Some(ref t) = token {
            if !seen_tokens.contains(t) {
                let accounts = self.bot_accounts.read().clone();
                if let Some(account) = accounts.get(t) {
                    self.start_account_poll(t, account);
                }
            }
        }

        // 周期性扫描 pending outbound（每 30s）
        let bot_clone = self.clone();
        std::thread::Builder::new()
            .name("ilink-outbound-scan".into())
            .spawn(move || {
                loop {
                    for _ in 0..30 {
                        if !bot_clone.running.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    if !bot_clone.running.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let rows = bot_clone
                        .db
                        .list_pending_outbound("")
                        .into_iter()
                        // S21: 仅重试 pending 状态，不自动重试 failed（避免 update_outbound_resend 重置 send_attempts 致无限循环）
                        .filter(|r| r.send_state == "pending")
                        .collect::<Vec<_>>();
                    for row in rows {
                        if row.send_attempts >= 5 || row.send_state == "expired" {
                            continue;
                        }
                        let to_user = row.to_user_id.clone().unwrap_or_default();
                        let text = row.text.clone().unwrap_or_default();
                        if to_user.is_empty() || text.is_empty() {
                            continue;
                        }
                        let new_client_id = format!(
                            "msg-{}",
                            &uuid::Uuid::new_v4().to_string().replace("-", "")[..16]
                        );
                        let new_req_id = bot_clone.gen_trace_id();
                        let bot_id = row.bot_id.clone();
                        let context_token = row.context_token.clone().unwrap_or_default();
                        bot_clone
                            .db
                            .update_outbound_resend(row.id, &new_client_id, &new_req_id);
                        let bc = bot_clone.clone();
                        let cid = new_client_id.clone();
                        let bid = bot_id.clone();
                        let tu = to_user.clone();
                        let txt = text.clone();
                        let rid = new_req_id.clone();
                        let rid_id = row.id;
                        std::thread::Builder::new()
                            .name("ilink-send-periodic".into())
                            .spawn(move || {
                                bc.spawn_retry_send(
                                    rid_id,
                                    &cid,
                                    &bid,
                                    &tu,
                                    &context_token,
                                    &txt,
                                    &rid,
                                );
                            })
                            .ok();
                    }
                }
            })
            .ok();
    }

    fn start_account_poll(self: &Arc<Self>, bot_token: &str, account: &serde_json::Value) {
        {
            let started = self.poll_started_tokens.read();
            if started.contains(bot_token) {
                return;
            }
        }
        self.poll_started_tokens
            .write()
            .insert(bot_token.to_string());

        let bot = self.clone();
        let bot_token = bot_token.to_string();
        let account = account.clone();
        let token_short = safe_truncate(&bot_token, 16).to_string();

        // S45: 创建/重置该 token 的 cancel flag
        //   reauth 路径会设置旧 token 的 flag=true，旧 poll 线程在循环顶部检测后退出；
        //   此处插入新的 false flag 供本次 poll 使用（若旧 flag 存在则覆盖，旧线程会在下次循环检测时退出）
        let cancel_flag = {
            let mut cancels = self.poll_cancels.write();
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            cancels.insert(bot_token.clone(), flag.clone());
            flag
        };

        std::thread::Builder::new()
            .name(format!("ilink-poll-{}", token_short))
            .spawn(move || {
                let mut cursor = account
                    .get("cursor")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut backoff = 0.5f64;
                let max_backoff = 30.0f64;
                let mut session_expired_warned = false; // 避免过期 WARNING 刷屏
                let mut last_success_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let mut last_save_at = 0.0f64;
                let mut consecutive_timeouts = 0u32;

                bot.poll_health.write().insert(
                    token_short.clone(),
                    PollHealth {
                        last_success_at,
                        state: "ok".to_string(),
                        last_error: String::new(),
                        since: last_success_at,
                    },
                );

                while bot.running.load(std::sync::atomic::Ordering::Relaxed) {
                    // S45: 检查 cancel flag，reauth 时旧 poll 线程尽快退出
                    if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                        tracing::info!(
                            "[POLL {}] 收到 cancel 信号（reauth），停止旧轮询",
                            token_short
                        );
                        bot.poll_health.write().remove(&token_short);
                        break;
                    }
                    // 检查 token 是否已被 reauth 替换。
                    // 如果 bot_token 不再是主 token 且不在 bot_accounts 中，说明 reauth 已替换该 token
                    // 此时旧 poll 应退出，避免持续覆盖 session_status = SessionExpired
                    let is_still_valid = {
                        let current_token = bot.token.read().clone();
                        current_token.as_deref() == Some(bot_token.as_str())
                            || bot.bot_accounts.read().contains_key(&bot_token)
                    };
                    if !is_still_valid {
                        tracing::info!(
                            "[POLL {}] token 已被替换（reauth），停止旧轮询",
                            token_short
                        );
                        // 清理旧 poll_health 条目
                        bot.poll_health.write().remove(&token_short);
                        break;
                    }

                    let body = serde_json::json!({
                        "get_updates_buf": cursor,
                        "base_info": {"channel_version": "1.0.3"}
                    });
                    let result = bot.post("getupdates", &body, 35, Some(&bot_token));
                    let ret = result.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);

                    // 会话过期 - 不清理 context_tokens（发送可能仍可用），只标记状态并推 SSE
                    if result.get("_expired").is_some() || is_expired_code(ret) {
                        // 如果 token 已被 reauth 替换，不再覆盖 session_status。
                        // 避免 reauth 成功 → session_status=Active 后被旧 poll 覆盖回 SessionExpired
                        let token_still_valid = {
                            let current_token = bot.token.read().clone();
                            current_token.as_deref() == Some(bot_token.as_str())
                                || bot.bot_accounts.read().contains_key(&bot_token)
                        };
                        if !token_still_valid {
                            tracing::info!(
                                "[POLL {}] 会话过期但 token 已被替换（reauth），退出旧 poll",
                                token_short
                            );
                            bot.poll_health.write().remove(&token_short);
                            break;
                        }

                        if !session_expired_warned {
                            tracing::warn!("[POLL {}] 会话过期 (terminal state)", token_short);
                            session_expired_warned = true;
                        }
                        let mut health = bot.poll_health.write();
                        if let Some(h) = health.get_mut(&token_short) {
                            h.state = "expired".to_string();
                            h.last_error = "session_expired".to_string();
                        }

                        // 1. 切到终态
                        *bot.session_status.write() = SessionState::SessionExpired;

                        // 2. 保留 token + bot_id（让用户可重新扫码）
                        bot.save_config();

                        // 3. 推 SSE：明确"会话过期，请重新扫码"（带 trace_id 串联）
                        let trace_id = bot.gen_trace_id();
                        let expired_users: Vec<String> =
                            bot.context_tokens.read().keys().cloned().collect();
                        bot.broker.publish_with_id(
                            "session_status",
                            serde_json::json!({
                                "state": "session_expired",
                                "bot_id": *bot.bot_id.read(),
                                "user_id": *bot.user_id.read(),
                                "expired_users": expired_users,
                                "reauth_available": true,
                                "trace_id": trace_id,
                                "message": "iLink 会话已过期，请点击重新扫码",
                            }),
                            &trace_id,
                        );

                        // 4. 不阻塞线程：用 30s 长 backoff 继续轮询，等待 reauth 恢复
                        // 从 300s 降到 30s，reauth 后更快恢复。
                        backoff = 30.0;
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }

                    // 超时（长轮询正常超时）
                    if ret == -1 {
                        backoff = 0.5;
                        consecutive_timeouts += 1;
                        if consecutive_timeouts.is_multiple_of(20) {
                            tracing::warn!(
                                "[POLL {}] 连续 {} 次超时",
                                token_short,
                                consecutive_timeouts
                            );
                        }
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }

                    // HTTP/网络/JSON 错误
                    if ret < 0 {
                        let http_status = result.get("http_status").and_then(|v| v.as_u64());
                        match ret {
                            -2 => {
                                let code = http_status.unwrap_or(0) as u16;
                                if code == 429 {
                                    backoff = (backoff * 3.0).min(max_backoff);
                                } else if code >= 500 {
                                    backoff = (backoff * 2.0).min(max_backoff);
                                } else if code == 401 || code == 403 {
                                    backoff = 30.0;
                                } else {
                                    backoff = (backoff * 2.0).min(max_backoff);
                                }
                            }
                            -3 | -4 => {
                                backoff = (backoff * 2.0).min(8.0);
                            }
                            _ => {}
                        }
                        // Add jitter to prevent thundering herd (±20%)
                        let jitter = backoff * 0.2 * (rand::random::<f64>() - 0.5) * 2.0;
                        std::thread::sleep(Duration::from_secs_f64(backoff + jitter));
                        continue;
                    }

                    // 成功
                    backoff = 0.5;
                    consecutive_timeouts = 0;
                    last_success_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    {
                        let mut health = bot.poll_health.write();
                        if let Some(h) = health.get_mut(&token_short) {
                            h.state = "ok".to_string();
                            h.last_success_at = last_success_at;
                            h.last_error = String::new();
                        }
                    }
                    // 如果之前是过期状态，poll 成功后自动恢复。
                    if *bot.session_status.read() == SessionState::SessionExpired {
                        tracing::info!("[POLL {}] 会话恢复，自动切回 Active", token_short);
                        *bot.session_status.write() = SessionState::Active;
                        let trace_id = bot.gen_trace_id();
                        bot.broker.publish_with_id(
                            "session_status",
                            serde_json::json!({
                                "state": "active",
                                "bot_id": *bot.bot_id.read(),
                                "user_id": *bot.user_id.read(),
                                "trace_id": trace_id,
                                "message": "iLink 会话已自动恢复",
                            }),
                            &trace_id,
                        );
                    }

                    if let Some(new_cursor) = result.get("get_updates_buf").and_then(|v| v.as_str())
                    {
                        cursor = new_cursor.to_string();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        // Save cursor more frequently (5s instead of 30s) to reduce data loss on crash
                        if now - last_save_at > 5.0 {
                            bot.save_config();
                            last_save_at = now;
                        }
                    }

                    // 持久化优先入站处理
                    if let Some(msgs) = result.get("msgs").and_then(|v| v.as_array()) {
                        for msg in msgs {
                            let from_user = msg
                                .get("from_user_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let ctx_token = msg
                                .get("context_token")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            // 入站消息：先入库（去重）→ 再 publish SSE → mark_processed
                            bot.on_inbound_message(msg, &bot_token, ctx_token);

                            // 自动 typing indicator
                            // S55: spawn 前先检查 25s 去重窗口，避免每条入站消息都 spawn 一个线程
                            //   （auto_typing_indicator 内部也有去重，但 spawn 本身已浪费资源）
                            let should_spawn_typing = {
                                let now_secs = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs_f64();
                                let map = bot.last_typing_time.lock();
                                if let Some(&t) = map.get(from_user) {
                                    now_secs - t >= 25.0
                                } else {
                                    true
                                }
                                // 注：auto_typing_indicator 会自行更新 map，这里只读不写
                            };
                            if should_spawn_typing {
                                let bot_typing = bot.clone();
                                let from_user_typing = from_user.to_string();
                                std::thread::Builder::new()
                                    .name("ilink-typing".into())
                                    .spawn(move || {
                                        bot_typing.auto_typing_indicator(&from_user_typing);
                                    })
                                    .ok();
                            }

                            // Webhook 出站推送
                            if let Some(ref dispatcher) = *bot.webhook_dispatcher.read() {
                                let text = msg
                                    .get("item_list")
                                    .and_then(|v| v.as_array())
                                    .and_then(|items| items.first())
                                    .and_then(|item| item.get("text_item"))
                                    .and_then(|t| t.get("text"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let payload = crate::webhook::WebhookPayload {
                                    event: "message.new".to_string(),
                                    // 审计 M-5: 发送真实 ilink bot_id；
                                    // 此前误发完整 bot_token（上游 Bearer 凭证），等于向
                                    // webhook 接收方交出微信协议凭证
                                    bot_id: bot.bot_id.read().clone().unwrap_or_default(),
                                    from_user: from_user.to_string(),
                                    to_user: "me".to_string(),
                                    text,
                                    message_id: msg.get("message_id").and_then(|v| v.as_i64()),
                                    timestamp: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                        as i64,
                                };
                                dispatcher.deliver(&payload);
                            }

                            // 注册用户（保留旧行为：触发 user 列表更新）
                            if !from_user.is_empty() && !ctx_token.is_empty() {
                                let is_new = !bot.context_tokens.read().contains_key(from_user);
                                bot.register_user_to_account(from_user, ctx_token, &bot_token);
                                bot.save_config();
                                last_save_at = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs_f64();
                                if is_new {
                                    tracing::info!(
                                        "[USER] 新用户 {} (账号 {}...)",
                                        from_user,
                                        &token_short
                                    );
                                    bot.broker.publish(
                                        "user",
                                        serde_json::json!({
                                            "users": bot.list_users(),
                                            "current_user": bot.get_current_user(),
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }

                bot.poll_started_tokens.write().remove(&bot_token);
            })
            .ok();
    }

    // ── 添加用户（多账号）─────────────────────────────────────

    pub fn start_add_user_qrcode(self: &Arc<Self>) -> String {
        let key = uuid::Uuid::new_v4().to_string().replace("-", "")[..12].to_string();
        let result_key = key.clone();
        let bot = self.clone();

        // 初始状态为 generating
        *bot.pending_qrcode.write() = Some(serde_json::json!({
            "key": &result_key,
            "matrix": serde_json::Value::Null,
            "status": "generating",
        }));

        std::thread::Builder::new()
            .name("ilink-add-user".into())
            .spawn(move || {
                let url = format!("{}/ilink/bot/get_bot_qrcode?bot_type=3", ILINK_BASE_URL);
                let resp = match bot.http_client.get(&url).timeout(Duration::from_secs(35)).send() {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("[添加用户] 获取二维码失败: {}", e);
                        *bot.pending_qrcode.write() = Some(serde_json::json!({
                            "key": "",
                            "matrix": serde_json::Value::Null,
                            "status": "error",
                            "error": format!("{}", e),
                        }));
                        return;
                    }
                };
                let data: serde_json::Value = match resp.json() {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("[添加用户] 解析二维码响应失败: {}", e);
                        *bot.pending_qrcode.write() = Some(serde_json::json!({
                            "key": "",
                            "matrix": serde_json::Value::Null,
                            "status": "error",
                            "error": format!("{}", e),
                        }));
                        return;
                    }
                };

                let mut qrcode_key = data.get("qrcode").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let qrcode_url = data.get("qrcode_img_content").and_then(|v| v.as_str()).unwrap_or("").to_string();

                // 生成二维码矩阵
                let matrix = if !qrcode_url.is_empty() {
                    let m = bot.generate_qr_matrix(&qrcode_url);
                    serde_json::to_value(&m).unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                };

                *bot.pending_qrcode.write() = Some(serde_json::json!({
                    "key": qrcode_key,
                    "matrix": matrix,
                    "status": "waiting",
                    "started_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                }));

                if !qrcode_url.is_empty() {
                    tracing::info!("[添加用户] 二维码已生成，请在网页扫描");
                }

                let start_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // QR 状态机调整
                // - last_qr_hash: 用于去重；拉回的新 QR 与上次相同则跳过更新，避免前端闪烁
                // - 取消底部固定 sleep(2s)；改在每个分支按需 sleep
                // - 过期稳定期从 2s 延长到 30s
                let mut last_qr_hash: Option<String> = None;
                loop {
                    if !bot.running.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if now - start_ts > 180 {
                        // 超时（与 iLink QR 实际有效期 180s 对齐）
                        let mut pq = bot.pending_qrcode.write();
                        if let Some(ref mut pq) = *pq {
                            if pq.get("status").and_then(|v| v.as_str()) == Some("waiting") {
                                pq["status"] = serde_json::Value::String("timeout".into());
                            }
                        }
                        tracing::info!("[添加用户] 二维码等待超时");
                        break;
                    }

                    let status_url = format!(
                        "{}/ilink/bot/get_qrcode_status?qrcode={}",
                        ILINK_BASE_URL, qrcode_key
                    );
                    let status: serde_json::Value = match bot.http_client
                        .get(&status_url)
                        .timeout(Duration::from_secs(5))
                        .header("iLink-App-ClientVersion", "1.0.3")
                        .send()
                    {
                        Ok(r) => match r.json() {
                            Ok(d) => d,
                            Err(_) => {
                                std::thread::sleep(Duration::from_secs(1));
                                continue;
                            }
                        },
                        Err(_) => {
                            std::thread::sleep(Duration::from_secs(1));
                            continue;
                        }
                    };

                    let st = status.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    match st {
                        "scaned" => {
                            if let Some(ref mut pq) = *bot.pending_qrcode.write() {
                                pq["status"] = serde_json::Value::String("scaned".into());
                            }
                            tracing::info!("[添加用户] 已扫码，请在手机上确认...");
                            // 短轮询确认中
                            std::thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                        "confirmed" => {
                            let new_token = status.get("bot_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let new_bot_id = status.get("ilink_bot_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let new_user_id = status.get("ilink_user_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

                            if new_token.is_empty() {
                                tracing::warn!("[添加用户] 错误：未获取到 bot_token");
                                if let Some(ref mut pq) = *bot.pending_qrcode.write() {
                                    pq["status"] = serde_json::Value::String("error".into());
                                }
                                // PR4: reauth 失败时回退到 expired，让用户重试
                                if *bot.session_status.read() == SessionState::Reauthing {
                                    *bot.session_status.write() = SessionState::SessionExpired;
                                }
                                break;
                            }

                            // 检测是否是 reauth（保留主账号）
                            // reauth 时不创建新账号，直接覆盖主 token
                            let is_reauth = *bot.session_status.read() == SessionState::Reauthing;

                            if is_reauth {
                                // reauth 路径：保留 bot_id / user_id，只刷新 token
                                tracing::info!("[REAUTH] 重新扫码成功，覆盖主 token");
                                let old_token = bot.token.read().clone().unwrap_or_default();
                                let old_token_short = safe_truncate(&old_token, 16).to_string();
                                *bot.token.write() = Some(new_token.clone());
                                *bot.session_status.write() = SessionState::Active;

                                // S45: 设置旧 token 的 cancel flag，让旧 poll 线程尽快退出，
                                //   避免旧 poll 在 reauth 成功后短暂覆盖 session_status
                                if let Some(flag) = bot.poll_cancels.read().get(&old_token) {
                                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                                    tracing::info!("[REAUTH] 已设置旧 poll 线程的 cancel flag（token_short={}）", old_token_short);
                                }

                                // 清理旧 token 的 poll_health，避免 spawn_retry_send 继续使用旧 token 发送。
                                //   动态查找时命中旧 expired 状态（虽然已移除提前终止，但日志清洁仍需要）
                                {
                                    let mut health = bot.poll_health.write();
                                    health.remove(&old_token_short);
                                }
                                // 移除旧 token 的 poll_started_tokens 标记，允许重新启动 poll。
                                //   允许旧 poll 线程在下次循环时通过 token 失效检查自行退出
                                bot.poll_started_tokens.write().remove(&old_token);

                                // 更新 bot_accounts。
                                //   1. 移除旧 token 条目（避免旧 poll 继续写入 cursor）
                                //   2. 添加新 token 条目
                                bot.bot_accounts.write().remove(&old_token);
                                bot.bot_accounts.write().insert(
                                    new_token.clone(),
                                    serde_json::json!({
                                        "bot_id": &new_bot_id,
                                        "user_id": &new_user_id,
                                        "cursor": "",
                                        "context_tokens": {},
                                    }),
                                );

                                // 将 user_token_map 中所有引用旧 token 的用户更新为新 token。
                                //   否则 fetch_and_restore_conversations 之前发送的消息会用旧 token
                                {
                                    let mut utm = bot.user_token_map.write();
                                    let users_to_update: Vec<String> = utm
                                        .iter()
                                        .filter(|(_, t)| *t == &old_token)
                                        .map(|(k, _)| k.clone())
                                        .collect();
                                    for uid in &users_to_update {
                                        utm.insert(uid.clone(), new_token.clone());
                                    }
                                    if !users_to_update.is_empty() {
                                        tracing::info!("[REAUTH] 已将 {} 个用户的 bot_token 从旧 token 更新为新 token", users_to_update.len());
                                    }
                                }

                                // 推 SSE：会话恢复
                                let trace_id = bot.gen_trace_id();
                                bot.broker.publish_with_id(
                                    "session_status",
                                    serde_json::json!({
                                        "state": "active",
                                        "bot_id": *bot.bot_id.read(),
                                        "user_id": *bot.user_id.read(),
                                        "trace_id": trace_id,
                                        "message": "重新扫码成功，会话已恢复",
                                    }),
                                    &trace_id,
                                );
                                // 推 sync_required 触发前端 _fullSync，
                                //   刷新用户列表、聊天列表预览、会话状态，避免"添加后回到主界面状态未更新"
                                bot.broker.publish("sync_required", serde_json::json!({"reason": "reauth_success"}));
                            } else {
                                // 注册新账号
                                bot.bot_accounts.write().insert(
                                    new_token.clone(),
                                    serde_json::json!({
                                        "bot_id": &new_bot_id,
                                        "user_id": &new_user_id,
                                        "cursor": "",
                                        "context_tokens": {},
                                    }),
                                );

                                // 如果主 token 为空则提升为主
                                if bot.token.read().is_none() {
                                    *bot.token.write() = Some(new_token.clone());
                                    *bot.bot_id.write() = Some(new_bot_id.clone());
                                    *bot.user_id.write() = Some(new_user_id.clone());
                                    *bot.session_status.write() = SessionState::Active; // PR4: 重置会话状态
                                    bot.login_done.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                            }

                            bot.fetch_and_restore_conversations();
                            bot.save_config();
                            bot.start_account_poll(&new_token, &serde_json::json!({
                                "bot_id": new_bot_id,
                                "user_id": new_user_id,
                                "cursor": "",
                                "context_tokens": {},
                            }));

                            // 自动切换到新用户
                            {
                                let ctx = bot.context_tokens.read();
                                if bot.current_user.read().is_none() || !ctx.contains_key(bot.current_user.read().as_deref().unwrap_or("")) {
                                    if let Some(last) = ctx.keys().last() {
                                        *bot.current_user.write() = Some(last.clone());
                                    }
                                }
                            }

                            // 通知前端
                            bot.broker.publish("user", serde_json::json!({
                                "users": bot.list_users(),
                                "current_user": bot.get_current_user(),
                            }));
                            bot.broker.publish("status", serde_json::json!({
                                "logged_in": true,
                                "login_done": true,
                                "users": bot.list_users(),
                                "current_user": bot.get_current_user(),
                                "message": "新用户已添加",
                            }));

                            if let Some(ref mut pq) = *bot.pending_qrcode.write() {
                                pq["status"] = serde_json::Value::String("done".into());
                                pq["users"] = serde_json::to_value(bot.list_users()).unwrap_or(serde_json::Value::Null);
                            }
                            break;
                        }
                        "expired" => {
                            // 二维码过期，按 hub 模式 fetch 一次新 QR + 哈希去重 + 30s 稳定期
                            tracing::debug!("[添加用户] 二维码已过期，准备刷新");
                            let refresh_url = format!("{}/ilink/bot/get_bot_qrcode?bot_type=3", ILINK_BASE_URL);
                            let mut refreshed = false;
                            match bot.http_client.get(&refresh_url).timeout(Duration::from_secs(35)).send() {
                                Ok(r) => {
                                    if let Ok(new_data) = r.json::<serde_json::Value>() {
                                        let new_key = new_data.get("qrcode").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        let new_url = new_data.get("qrcode_img_content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        if !new_key.is_empty() && !new_url.is_empty() {
                                            // 哈希去重：与上次相同则跳过
                                            let new_hash = {
                                                use std::collections::hash_map::DefaultHasher;
                                                use std::hash::{Hash, Hasher};
                                                let mut h = DefaultHasher::new();
                                                new_key.hash(&mut h);
                                                new_url.hash(&mut h);
                                                format!("{:x}", h.finish())
                                            };
                                            if last_qr_hash.as_deref() != Some(&new_hash) {
                                                qrcode_key = new_key;
                                                let matrix = bot.generate_qr_matrix(&new_url);
                                                if let Some(ref mut pq) = *bot.pending_qrcode.write() {
                                                    pq["key"] = serde_json::Value::String(qrcode_key.clone());
                                                    pq["matrix"] = serde_json::to_value(&matrix).unwrap_or(serde_json::Value::Null);
                                                    pq["status"] = serde_json::Value::String("waiting".into());
                                                }
                                                last_qr_hash = Some(new_hash);
                                                tracing::info!("[添加用户] 二维码已刷新（新 hash），请在网页查看");

                                            } else {
                                                tracing::debug!("[添加用户] 二维码 hash 未变，跳过更新");
                                            }
                                            refreshed = true;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("[添加用户] 刷新二维码失败: {}", e);
                                }
                            }
                            if !refreshed {
                                // 刷新失败，标记过期
                                if let Some(ref mut pq) = *bot.pending_qrcode.write() {
                                    pq["status"] = serde_json::Value::String("expired".into());
                                }
                                tracing::info!("[添加用户] 二维码已过期且刷新失败");
                                break;
                            }
                            // 30s 稳定期（hub 模式），避免频繁刷新
                            std::thread::sleep(Duration::from_secs(30));
                            continue;
                        }
                        _ => {
                            // waiting 等中间态：2s 短轮询
                            std::thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                    }
                }
            })
            .ok();

        result_key
    }

    /// 触发重新扫码（保留 bot_id / 凭证，重新绑定会话）
    /// 参考 openilink-hub bind.go StartBind
    /// 复用 start_add_user_qrcode 的 QR 生成+轮询流程，但 confirmed 分支会识别 Reauthing
    /// 状态并保留主账号，覆盖 token。
    pub fn start_reauth_qrcode(self: &Arc<Self>) -> String {
        // 切到 Reauthing 状态（confirmed 分支会检查这个标志）
        *self.session_status.write() = SessionState::Reauthing;
        tracing::info!("[REAUTH] 启动重新扫码，保留主账号");
        // 复用 add_user 流程（不再创建新账号，confirmed 分支走 is_reauth 路径）
        self.start_add_user_qrcode()
    }

    pub fn get_add_user_status(&self) -> serde_json::Value {
        let pending = self.pending_qrcode.read().clone();
        match pending {
            Some(pq) => {
                // 前端期望 qrcode_status 字段（而非 status），与 Python 版一致
                let qrcode_status = pq.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let matrix = pq.get("matrix").cloned().unwrap_or(serde_json::Value::Null);
                let mut result = serde_json::json!({
                    "key": pq.get("key").and_then(|v| v.as_str()).unwrap_or(""),
                    "qrcode_status": qrcode_status,
                    "matrix": matrix,
                });
                // 保留其他字段
                if let Some(obj) = pq.as_object() {
                    if let Some(result_obj) = result.as_object_mut() {
                        for (k, v) in obj {
                            if k != "status" && !result_obj.contains_key(k) {
                                result_obj.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                result
            }
            None => serde_json::Value::Null,
        }
    }

    // ── WebDAV ────────────────────────────────────────────────

    pub fn reload_webdav_client(&self) -> anyhow::Result<()> {
        let cfg = self.webdav_config.read();
        if cfg.enabled && !cfg.url.is_empty() {
            // 在创建 WebDavClient 前先做 SSRF 校验（含 DNS 重绑定防护），
            //   不通过则拒绝创建 client（WebDavClient::new 内部也会校验并 log warn，
            //   但这里直接拒绝更安全，避免不安全 URL 仍能发起请求）。
            if !is_ssrf_safe_url(&cfg.url) {
                anyhow::bail!(
                    "WebDAV URL 未通过 SSRF 校验（禁止 IP 字面量与内网域名）: {}",
                    cfg.url
                );
            }
            let client =
                WebDavClient::new(&cfg.url, &cfg.username, &cfg.password, &cfg.base_path, 60)?;
            let arc_client = Arc::new(client);
            // 注册到三级存储
            let mut ts = self.tiered_storage.write();
            ts.remove_backend_by_name("WebDAV");
            ts.add_backend(Box::new(WebDavStorageBackend::new(arc_client.clone())));
            drop(ts);
            *self.webdav_client.write() = Some(arc_client);
        } else {
            *self.webdav_client.write() = None;
            self.tiered_storage.write().remove_backend_by_name("WebDAV");
        }
        Ok(())
    }

    pub fn get_webdav_settings(&self) -> serde_json::Value {
        let cfg = self.webdav_config.read();
        let mut result = serde_json::to_value(&*cfg).unwrap_or_default();
        // 密码打码
        if let Some(obj) = result.as_object_mut() {
            if !cfg.password.is_empty() {
                obj.insert(
                    "password".into(),
                    serde_json::Value::String("********".into()),
                );
            }
        }
        result
    }

    pub fn save_webdav_settings(&self, config: &WebDavConfig) -> anyhow::Result<()> {
        let password = if config.password.is_empty() || config.password == "********" {
            self.webdav_config.read().password.clone()
        } else {
            config.password.clone()
        };
        let new_config = WebDavConfig {
            password,
            ..config.clone()
        };
        if new_config.enabled {
            WebDavClient::new(
                &new_config.url,
                &new_config.username,
                &new_config.password,
                &new_config.base_path,
                60,
            )?;
        }
        let previous = self.webdav_config.read().clone();
        self.db.save_webdav_config(&new_config)?;
        *self.webdav_config.write() = new_config;
        if let Err(e) = self.reload_webdav_client() {
            *self.webdav_config.write() = previous.clone();
            if let Err(restore_error) = self.db.save_webdav_config(&previous) {
                tracing::error!(
                    "[WebDAV] 配置回滚写入失败，需人工检查 user.db: {}",
                    restore_error
                );
            }
            let _ = self.reload_webdav_client();
            return Err(e);
        }
        Ok(())
    }

    pub fn test_webdav_connection(
        &self,
        url: &str,
        username: &str,
        password: &str,
        base_path: &str,
    ) -> serde_json::Value {
        if !is_ssrf_safe_url(url) {
            return serde_json::json!({"ok": false, "error": "URL 不合规：禁止 IP 字面量与内网域名"});
        }
        match WebDavClient::new(url, username, password, base_path, 15) {
            Ok(client) => client.test_connection(),
            Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
        }
    }

    pub fn is_webdav_enabled(&self) -> bool {
        self.webdav_config.read().enabled
    }

    pub fn is_traffic_saver_enabled(&self) -> bool {
        self.webdav_config.read().traffic_saver
    }

    pub fn update_traffic_saver(&self, enabled: bool) {
        self.webdav_config.write().traffic_saver = enabled;
        self.db.update_webdav_traffic_saver(enabled);
    }

    pub fn get_webdav_migration_state(&self) -> WebDavMigrateState {
        self.webdav_migrate_state.read().clone()
    }

    pub fn start_webdav_migration_async(self: &Arc<Self>) {
        // S17: 在 spawn 前设置 running=true，避免调用方在 spawn 间隙重复触发
        // 调用方（api_webdav_migrate）应已持有写锁并检查 running，本函数仅在 spawn 前确认设置
        {
            let mut state = self.webdav_migrate_state.write();
            state.running = true;
        }
        let bot = self.clone();
        std::thread::Builder::new()
            .name("ilink-webdav-migrate".into())
            .spawn(move || {
                // 迁移：遍历所有 media_meta，上传后删除本地
                let metas = bot.db.list_media_meta_all();
                let total = metas.len();
                // 预估总字节数（用于进度条）
                let bytes_total: u64 = metas.iter().map(|m| m.size.max(0) as u64).sum();
                let mut bytes_done: u64 = 0;
                let mut last_tick = std::time::Instant::now();
                let mut last_bytes_done: u64 = 0;

                {
                    let mut state = bot.webdav_migrate_state.write();
                    // S17: running 已在 spawn 前设置，此处仅初始化统计字段
                    state.total = total;
                    state.uploaded = 0;
                    state.skipped = 0;
                    state.failed = 0;
                    state.deleted_local = 0;
                    state.overwritten = 0;
                    state.bytes_total = bytes_total;
                    state.bytes_done = 0;
                    state.current_file_bytes = 0;
                    state.current_file_size = 0;
                    state.bytes_per_sec = 0.0;
                    state.eta_seconds = 0.0;
                    state.started_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    state.error = String::new();
                }

                // 迁移逻辑：遍历 media_meta，上传 WebDAV，记录 remote_path，删除本地文件
                for meta in &metas {
                    if !bot.running.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let cache_key = &meta.cache_key;
                    let file_size = meta.size.max(0) as u64;
                    if cache_key.is_empty() {
                        {
                            let mut state = bot.webdav_migrate_state.write();
                            state.skipped += 1;
                            state.current = cache_key.clone();
                            state.current_file_size = 0;
                            state.current_file_bytes = 0;
                        }
                        continue;
                    }
                    // 跳过已有远程记录的
                    if bot.db.get_media_remote(cache_key).is_some() {
                        {
                            let mut state = bot.webdav_migrate_state.write();
                            state.skipped += 1;
                            state.current = cache_key.clone();
                            state.current_file_size = 0;
                            state.current_file_bytes = 0;
                            bytes_done = bytes_done.saturating_add(file_size);
                            state.bytes_done = bytes_done;
                        }
                        continue;
                    }
                    // 从本地文件读取
                    let cache_path = bot
                        .media_cache_dir
                        .join(safe_truncate(cache_key, 2).to_lowercase())
                        .join(cache_key);
                    let data = match std::fs::read(&cache_path) {
                        Ok(d) => d,
                        Err(_) => {
                            tracing::warn!("[WebDAV 迁移] 本地文件不存在: {}", cache_key);
                            let mut state = bot.webdav_migrate_state.write();
                            state.failed += 1;
                            state.current = cache_key.clone();
                            state.current_file_size = 0;
                            state.current_file_bytes = 0;
                            bytes_done = bytes_done.saturating_add(file_size);
                            state.bytes_done = bytes_done;
                            continue;
                        }
                    };
                    let actual_size = data.len() as u64;
                    {
                        let mut state = bot.webdav_migrate_state.write();
                        state.current_file_size = actual_size;
                        state.current_file_bytes = 0;
                        state.current = cache_key.clone();
                    }
                    // 上传到 WebDAV（带 MD5 校验 + 扩展名）
                    let ext = media::derive_ext(&meta.mime, &meta.filename);
                    let local_md5 = crypto::md5_hex(&data);
                    let wd_client = bot.webdav_client.read();
                    if let Some(ref client) = *wd_client {
                        match client.upload_with_check(
                            cache_key,
                            ext.as_deref(),
                            data,
                            &meta.mime,
                            &local_md5,
                        ) {
                            Ok(result) => {
                                bot.db.save_media_remote(
                                    cache_key,
                                    &result.remote_path,
                                    "",
                                    &local_md5,
                                );
                                // 上传成功后删除本地文件
                                let _ = std::fs::remove_file(&cache_path);
                                if let Err(e) = bot.db.mark_media_local_absent(cache_key) {
                                    tracing::warn!(
                                        "[WebDAV 迁移] 标记本地副本已删除失败 {}: {}",
                                        cache_key,
                                        e
                                    );
                                }
                                let mut state = bot.webdav_migrate_state.write();
                                if result.skipped {
                                    state.skipped += 1;
                                } else {
                                    state.uploaded += 1;
                                    state.deleted_local += 1;
                                }
                                if result.overwritten {
                                    state.overwritten += 1;
                                }
                                state.current_file_bytes = actual_size;
                                bytes_done = bytes_done.saturating_add(actual_size);
                                state.bytes_done = bytes_done;
                            }
                            Err(e) => {
                                tracing::warn!("[WebDAV 迁移] 上传失败 {}: {}", cache_key, e);
                                let mut state = bot.webdav_migrate_state.write();
                                state.failed += 1;
                                state.current = cache_key.clone();
                                bytes_done = bytes_done.saturating_add(actual_size);
                                state.bytes_done = bytes_done;
                            }
                        }
                    } else {
                        let mut state = bot.webdav_migrate_state.write();
                        state.failed += 1;
                        state.error = "WebDAV 客户端不可用".to_string();
                        break;
                    }

                    // 速率 / ETA 计算（指数滑动平均 α=0.3）
                    let now = std::time::Instant::now();
                    let dt = now.duration_since(last_tick).as_secs_f64();
                    if dt >= 0.5 {
                        let delta_bytes = bytes_done.saturating_sub(last_bytes_done) as f64;
                        let inst_bps = if dt > 0.0 { delta_bytes / dt } else { 0.0 };
                        last_tick = now;
                        last_bytes_done = bytes_done;
                        let remaining = bytes_total.saturating_sub(bytes_done);
                        let mut state = bot.webdav_migrate_state.write();
                        // S76: 用 EMA 平滑速率（α=0.2），消除进度抖动
                        state.update_ema_rate(inst_bps);
                        let bytes_per_sec = state.ema_rate;
                        let eta = if bytes_per_sec > 1024.0 {
                            remaining as f64 / bytes_per_sec
                        } else {
                            0.0
                        };
                        state.eta_seconds = eta;
                    }
                }

                {
                    let mut state = bot.webdav_migrate_state.write();
                    state.running = false;
                    state.bytes_done = bytes_total; // 最终对齐
                    state.bytes_per_sec = 0.0;
                    state.eta_seconds = 0.0;
                    state.current_file_size = 0;
                    state.current_file_bytes = 0;
                    state.finished_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                }
            })
            .ok();
    }

    // ── 生命周期 ─────────────────────────────────────────────

    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // 排空保存队列
        {
            let msgs = self.messages.read().clone();
            let mut seen = HashSet::new();
            if let Some(tx) = self.save_tx.lock().take() {
                // 收集所有实际用户 ID（from 和 to，排除 "me"）
                let mut all_user_ids = Vec::new();
                for msg in &msgs {
                    for field in &["from", "to"] {
                        if let Some(uid) = msg.get(*field).and_then(|v| v.as_str()) {
                            if uid != "me" && seen.insert(uid.to_string()) {
                                all_user_ids.push(uid.to_string());
                            }
                        }
                    }
                }
                // 为每个实际用户保存其相关消息
                for uid in &all_user_ids {
                    let user_msgs: Vec<serde_json::Value> = msgs
                        .iter()
                        .filter(|m| {
                            m.get("from").and_then(|v| v.as_str()) == Some(uid)
                                || m.get("to").and_then(|v| v.as_str()) == Some(uid)
                        })
                        .cloned()
                        .collect();
                    let _ = tx.send(SaveTask {
                        user_id: uid.to_string(),
                        messages: user_msgs,
                        max_per_user: 500,
                        db: self.db.clone(),
                    });
                }
                // 关闭发送端，让工作线程处理完退出
                drop(tx);
            }
            // 先 take 出 handle 释放锁，再 join，避免持锁阻塞
            let save_handle = self.save_handle.lock().take();
            if let Some(handle) = save_handle {
                let _ = handle.join();
            }
        }
        // 关闭媒体预取线程池
        {
            if let Some(tx) = self.prefetch_tx.lock().take() {
                drop(tx);
            }
            let handles: Vec<_> = self.prefetch_handles.lock().drain(..).collect();
            for h in handles {
                let _ = h.join();
            }
        }
        self.poll_started_tokens.write().clear();
    }
    pub fn open_browser(&self) {
        let url = format!("http://localhost:{}", self.web_port);
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", "", &url])
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        }
    }
}
