// Axum Web 服务器：路由、SSE、鉴权、文件上传
// 衍生/开发 请 标注 原仓库 "https://github.com/zynsync/Zyn-iLink-ChatBox" 与原作者。

use crate::auth::Auth;
use crate::bot::safe_truncate;
use crate::bot_manager::{BotManager, Feature, QuotaDim, QuotaExceeded};
use crate::config::SCRIPT_VERSION;
use crate::media;
use crate::models::{AuthUser, PublicAppUser, SharedBot};
use crate::storage::{
    is_supported_system_setting, setting_truthy, validate_system_setting, SystemDatabase,
};

use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{
    connect_info::ConnectInfo, DefaultBodyLimit, Extension, Multipart, Path, Query, Request, State,
};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self as axum_mw, Next};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use parking_lot::RwLock;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

// ── 请求/响应结构 ───────────────────────────────────────────

#[derive(Deserialize)]
#[allow(dead_code)]
struct SendTextRequest {
    text: String,
    /// 前端生成 req_id 用于 ACK 匹配（缺省时后端自动生成）。
    #[serde(default)]
    req_id: Option<String>,
}

#[derive(Deserialize)]
struct SwitchUserRequest {
    user_id: String,
}

#[derive(Deserialize)]
struct DeleteUserRequest {
    user_id: String,
}

#[derive(Deserialize)]
struct LoginRequest {
    /// Phase 1 多用户：username 必填。空字符串或缺失时回退为 "owner"
    /// （兼容旧前端只发 password 的过渡期）。
    #[serde(default)]
    username: Option<String>,
    password: String,
    /// Phase 1.5: 记住我 — 登录时生成设备令牌
    #[serde(default)]
    remember_me: Option<bool>,
    /// Phase 1.5: 设备名称（用于标识已登录设备）
    #[serde(default)]
    device_name: Option<String>,
}

/// Phase 4: 注册请求体
#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    password: String,
    #[serde(default)]
    confirm_password: Option<String>,
    /// 用户同意的守则版本（必须等于当前 terms_version）
    #[serde(default)]
    agreed_terms_ver: Option<String>,
    /// 邀请码（allow_open=off 时必填）
    #[serde(default)]
    invite_code: Option<String>,
    /// Phase 1.5: 记住我 — 注册时生成设备令牌
    #[serde(default)]
    remember_me: Option<bool>,
    /// Phase 1.5: 设备名称（用于标识已登录设备）
    #[serde(default)]
    device_name: Option<String>,
}

/// S-NEW 管理员：创建邀请码请求体
#[derive(Deserialize)]
struct AdminCreateInviteRequest {
    /// 有效期（天），0 = 永久
    #[serde(default)]
    days: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

/// S-NEW 管理员：撤销邀请码请求体
#[derive(Deserialize)]
struct AdminRevokeInviteRequest {
    code: String,
}

#[derive(Deserialize)]
struct SetPasswordRequest {
    /// 旧字段，保留以兼容旧前端：当 new_password 缺失时作为新密码使用。
    #[serde(default)]
    password: Option<String>,
    /// Phase 1 多用户：修改密码需要旧密码二次认证。
    #[serde(default)]
    old_password: Option<String>,
    /// Phase 1 多用户：新密码（旧前端发 password 字段时回退取此）。
    #[serde(default)]
    new_password: Option<String>,
}

/// Phase 1.4: 设置邮箱请求体
#[derive(Deserialize)]
struct SetEmailRequest {
    email: String,
}

/// Phase 1.5: 记住我 — 自动登录请求体
#[derive(Deserialize)]
struct AutoLoginRequest {
    #[serde(default)]
    device_token: Option<String>,
}

/// Phase 1.5: 撤销设备令牌请求体
#[derive(Deserialize)]
struct RevokeDeviceTokenRequest {
    token: String,
}

/// 登出请求体（可选 device_token）。
///   revoke_device = true 时同时撤销该用户全部 device_token，
///   防止"记住我"令牌在登出后仍可自动登录。
#[derive(Deserialize)]
struct LogoutRequest {
    #[serde(default)]
    revoke_device: Option<bool>,
}

/// Phase 2: 管理员创建用户请求体
#[derive(Deserialize)]
struct AdminCreateUserRequest {
    username: String,
    password: String,
    #[serde(default = "default_user_role")]
    role: String,
}

fn default_user_role() -> String {
    "user".to_string()
}

/// Phase 2: 管理员操作用户请求体
#[derive(Deserialize)]
struct AdminUserActionRequest {
    user: String,
}

/// Phase 2: 管理员设置系统配置请求体
#[derive(Deserialize)]
struct AdminSetSettingRequest {
    key: String,
    value: String,
}

/// Phase 1.1: 封禁 IP 请求体
#[derive(Deserialize)]
struct AdminBanIpRequest {
    ip: String,
    #[serde(default)]
    reason: String,
    #[serde(default = "default_ban_days")]
    days: i64,
    /// 封禁回环、私网或超大网段时必须由管理员显式确认。
    #[serde(default)]
    confirm_dangerous: bool,
}

fn default_ban_days() -> i64 {
    7
}

/// Phase 3: 启动隧道请求体
#[derive(Deserialize)]
struct TunnelStartRequest {
    #[serde(default = "default_tunnel_port")]
    port: u16,
    #[serde(default = "default_tunnel_remote")]
    remote: u16,
    #[serde(default)]
    subdomain: String,
}

fn default_tunnel_port() -> u16 {
    8888
}
fn default_tunnel_remote() -> u16 {
    80
}

/// 远程端口校验（serveo 侧公网端口，仅校验 1..=65535）。
fn validate_tunnel_remote_port(port: u16) -> Result<(), &'static str> {
    if port == 0 {
        return Err("远程端口不能为 0");
    }
    Ok(())
}

/// 本地端口白名单（审计 M-4）：默认仅允许转发本服务自身的监听端口，
/// 防止被劫持的管理员会话或本地用户把 RDP/数据库等内网服务经 serveo 暴露公网。
/// 确需转发其他本地端口时，设置 ILINK_TUNNEL_ALLOW_PORTS=端口1,端口2 显式放行。
fn validate_tunnel_local_port(port: u16) -> Result<(), &'static str> {
    if port == 0 {
        return Err("端口不能为 0");
    }
    let web_port = std::env::var("ILINK_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8888);
    let extra: Vec<u16> = std::env::var("ILINK_TUNNEL_ALLOW_PORTS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<u16>().ok())
        .collect();
    if port == web_port || extra.contains(&port) {
        Ok(())
    } else {
        Err("本地端口不在白名单内：默认仅允许本服务端口，其他端口需设置 ILINK_TUNNEL_ALLOW_PORTS 显式放行")
    }
}

#[derive(Deserialize)]
struct TrafficSaverRequest {
    traffic_saver: bool,
}

#[derive(Deserialize)]
struct AdminBroadcastRequest {
    message: String,
    #[serde(default)]
    level: String, // "info", "warn", "error"
}

#[derive(Deserialize)]
struct MediaQuery {
    force: Option<String>,
}

#[derive(Deserialize)]
struct MessagesQuery {
    since: Option<i64>,
    user: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    user: Option<String>,
    limit: Option<usize>,
    // 游标分页：取 id < before 的最新 N 条消息。
    //   首次加载不传，翻页时传上一页最早消息的 id。
    before: Option<i64>,
}

/// export-history 改用 POST + JSON body（对齐前端，避免 URL 长度限制）。
#[derive(Deserialize)]
struct ExportHistoryRequest {
    user_id: String,
    #[serde(default)]
    nickname: Option<String>,
}

// ── 输入验证辅助函数 ─────────────────

/// 验证消息文本：非空且长度 <= 20000
fn validate_message_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("消息内容不能为空".to_string());
    }
    if text.len() > 20000 {
        return Err(format!(
            "消息过长（{} 字符），最大允许 20000 字符",
            text.len()
        ));
    }
    Ok(())
}

/// 验证 user_id：非空
fn validate_user_id(user_id: &str) -> Result<(), String> {
    if user_id.is_empty() {
        return Err("user_id 不能为空".to_string());
    }
    Ok(())
}

/// 验证消息 ID 列表：非空
fn validate_message_ids(ids: &[i64]) -> Result<(), String> {
    if ids.is_empty() {
        return Err("ids 列表不能为空".to_string());
    }
    Ok(())
}

/// 验证上传文件：大小限制和类型限制
const MAX_UPLOAD_SIZE: usize = 50 * 1024 * 1024; // 50MB
const ALLOWED_IMAGE_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
];
const ALLOWED_VIDEO_TYPES: &[&str] = &[
    "video/mp4",
    "video/webm",
    "video/quicktime",
    "video/x-msvideo",
];

/// 通过文件魔数检测实际 MIME 类型
fn detect_mime_type(data: &[u8]) -> Option<&'static str> {
    if data.len() < 4 {
        return None;
    }

    // 图片类型魔数检测
    match &data[0..4] {
        [0xFF, 0xD8, 0xFF, _] => return Some("image/jpeg"), // JPEG
        [0x89, 0x50, 0x4E, 0x47] => return Some("image/png"), // PNG
        [0x47, 0x49, 0x46, 0x38] => return Some("image/gif"), // GIF
        [0x42, 0x4D, _, _] => return Some("image/bmp"),     // BMP
        _ => {}
    }

    // WebP: RIFF....WEBP
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }

    // 视频类型魔数检测
    if data.len() >= 12 {
        // MP4/MOV: ....ftyp (如 00 00 00 18 66 74 79 70)
        if &data[4..8] == b"ftyp" {
            return Some("video/mp4");
        }
        // QuickTime: ....moov or ....mdat
        if &data[4..8] == b"moov" || &data[4..8] == b"mdat" || &data[4..8] == b"wide" {
            return Some("video/quicktime");
        }
    }

    // WebM/MKV: 0x1A 0x45 0xDF 0xA3
    if data[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Some("video/webm");
    }

    // AVI: RIFF....AVI
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..11] == b"AVI" {
        return Some("video/x-msvideo");
    }

    None
}

fn validate_upload_file(data: &[u8], media_type: &str) -> Result<(), String> {
    if data.is_empty() {
        return Err("文件内容不能为空".to_string());
    }
    if data.len() > MAX_UPLOAD_SIZE {
        return Err(format!(
            "文件过大（{} 字节），最大允许 {} 字节",
            data.len(),
            MAX_UPLOAD_SIZE
        ));
    }

    // 验证实际文件类型与声称类型匹配（防止类型伪造攻击）
    match media_type {
        "image" => {
            let detected = detect_mime_type(data)
                .ok_or_else(|| "无法识别图片格式，可能不是有效的图片文件".to_string())?;
            if !ALLOWED_IMAGE_TYPES.contains(&detected) {
                return Err(format!(
                    "不允许的图片类型: {}，允许的类型: {:?}",
                    detected, ALLOWED_IMAGE_TYPES
                ));
            }
        }
        "video" => {
            let detected = detect_mime_type(data)
                .ok_or_else(|| "无法识别视频格式，可能不是有效的视频文件".to_string())?;
            if !ALLOWED_VIDEO_TYPES.contains(&detected) {
                return Err(format!(
                    "不允许的视频类型: {}，允许的类型: {:?}",
                    detected, ALLOWED_VIDEO_TYPES
                ));
            }
        }
        "file" => {
            // 通用文件，无额外限制
        }
        _ => {
            return Err(format!("不支持的媒体类型: {}", media_type));
        }
    }
    Ok(())
}

/// 文件名清洗：移除路径分隔符、空字节等危险字符。
/// 去除 `..` 段、绝对路径前缀（`/`、`C:\` 等 Windows 盘符）、控制字符 `\0-\x1f`，
/// 超长截断到 255 字节（按 char boundary）。空结果回退为 "file"。
fn sanitize_filename(name: &str) -> String {
    // 去掉 Windows 盘符前缀（如 C:\），后续 split 会自然剥离前导分隔符
    let stripped = if name.len() >= 2 {
        let bytes = name.as_bytes();
        if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            &name[2..]
        } else {
            name
        }
    } else {
        name
    };
    // 按路径分隔符切分，丢弃空段与 `.`、`..` 段，防止跨目录
    let mut parts: Vec<&str> = stripped
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    if parts.is_empty() {
        return "file".to_string();
    }
    // 取最后一段作为文件名（剥离任何目录前缀）
    let last = parts.pop().unwrap_or("file");
    // 过滤控制字符 \0-\x1f 与 \x7f
    let cleaned: String = last
        .chars()
        .filter(|c| (*c as u32) >= 0x20 && (*c as u32) != 0x7f)
        .collect();
    if cleaned.is_empty() {
        return "file".to_string();
    }
    // 截断到 255 字节（按 UTF-8 char boundary）
    if cleaned.len() <= 255 {
        cleaned
    } else {
        safe_truncate(&cleaned, 255).to_string()
    }
}

/// 校验 WebDAV base_path，拒绝 `..` 段与控制字符。
/// base_path 是 WebDAV 服务端路径（如 "/ilink-media"），允许子路径但不允许穿越。
/// 返回清洗后的安全路径；若含 `..` 段则返回 None（调用方应拒绝）。
fn sanitize_webdav_base_path(base_path: &str) -> Option<String> {
    // 过滤控制字符
    let cleaned: String = base_path
        .chars()
        .filter(|c| (*c as u32) >= 0x20 && (*c as u32) != 0x7f)
        .collect();
    // 按 / 切分，检查是否含 .. 或绝对盘符前缀
    let stripped = if cleaned.len() >= 2 {
        let bytes = cleaned.as_bytes();
        if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            &cleaned[2..]
        } else {
            &cleaned[..]
        }
    } else {
        &cleaned[..]
    };
    let segs: Vec<&str> = stripped.split(['/', '\\']).collect();
    if segs.contains(&"..") {
        return None; // 含穿越段，拒绝
    }
    // 重新拼接为 / 分隔的路径，保证前导 /
    let joined = segs
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("/{}", joined))
}

/// 校验 webdav-proxy 的 remote_path 必须落在 base_path 之下，防止路径遍历。
/// 且剩余段匹配 cache_key 布局（`<2hex>/<32hex>` 或裸 `<32hex>`，可带扩展名）。
/// 防止任意路径读取绕过。
fn validate_webdav_proxy_path(remote_path: &str, base_path: &str) -> bool {
    // 归一化 base_path：补前导 /，去尾随 /
    let mut bp = if base_path.is_empty() {
        String::from("/")
    } else {
        base_path.to_string()
    };
    if !bp.starts_with('/') {
        bp = format!("/{}", bp);
    }
    while bp.len() > 1 && bp.ends_with('/') {
        bp.pop();
    }
    // 归一化 remote_path：补前导 /
    let rp = if remote_path.starts_with('/') {
        remote_path.to_string()
    } else {
        format!("/{}", remote_path)
    };
    // rp 必须以 base_path 为路径边界前缀
    let prefix_ok = if bp == "/" {
        true
    } else {
        rp == bp || rp.starts_with(&format!("{}/", bp))
    };
    if !prefix_ok {
        return false;
    }
    let rest = if bp == "/" { &rp[..] } else { &rp[bp.len()..] };
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let is_hex = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
    // S75: base_path="/" 时前缀校验恒成立，额外强制最终段为 32 位 hex（MD5 cache_key），
    //   防止越权读取任意路径
    if bp == "/" {
        let last = segs.last().copied().unwrap_or("");
        let h = last.rsplit_once('.').map(|(h, _)| h).unwrap_or(last);
        if !(is_hex(h) && h.len() == 32) {
            return false;
        }
    }
    // 取除扩展名后的 hex 主体
    match segs.len() {
        2 => {
            let bucket = segs[0];
            let key = segs[1];
            let h = key.rsplit_once('.').map(|(h, _)| h).unwrap_or(key);
            is_hex(bucket) && bucket.len() == 2 && is_hex(h) && h.len() == 32
        }
        1 => {
            let key = segs[0];
            let h = key.rsplit_once('.').map(|(h, _)| h).unwrap_or(key);
            is_hex(h) && h.len() == 32
        }
        _ => false,
    }
}

// ── 应用状态 ────────────────────────────────────────────────

/// Phase 3: 配额超限统一响应构造（U9：携带人类可读 message + dim/quota/used）。
/// status 由调用方按维度决定：字节/计数类 → 413，msg_per_day → 429。
fn quota_exceeded_response(e: QuotaExceeded, status: StatusCode) -> Response {
    (
        status,
        Json(serde_json::json!({
            "success": false,
            "error": "quota_exceeded",
            "dim": e.dim.as_str(),
            "quota": e.quota,
            "used": e.used,
            "message": e.message,
        })),
    )
        .into_response()
}

/// 统一内部错误响应：详细错误进 tracing::warn!（含 action 上下文），客户端只看通用消息。
///   原多处 `format!("设置失败: {}", e)` / `e.to_string()` 直接把 anyhow::Error /
///   rusqlite::Error 拼接到客户端可见的 error 字段，可能泄露 SQL 错误、表名、
///   文件路径等内部信息，便于攻击者侦察系统结构。
///   修复：详细错误进 tracing::warn!（含 action 上下文），客户端只看通用消息。
///   仅用于系统错误（DB/IO/内部异常）；业务逻辑错误（如"用户名已存在"）应直接返回具体消息。
fn internal_error_response(action: &str, e: impl std::fmt::Display) -> Response {
    tracing::warn!("[M14] internal_error: action={} error={}", action, e);
    Json(serde_json::json!({
        "success": false,
        "error": "internal_error",
        "message": "操作失败，请稍后重试或联系管理员"
    }))
    .into_response()
}

/// Phase 3 (S2): 限速超限统一响应（429 + retry_after 提示）。
fn rate_limited_response(action: &str) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "success": false,
            "error": "rate_limited",
            "message": format!("操作过于频繁，请稍后重试（{}）", action),
        })),
    )
        .into_response()
}

/// Phase 3 (§7.3): 功能开关关闭统一响应（403 Forbidden + 人类可读提示）。
/// 用于 upload/webdav/custom_webdav 等被管理员关闭时拒绝请求。
fn feature_disabled_response(feature: &str) -> Response {
    let hint = match feature {
        "upload" => "管理员已禁用文件/媒体上传功能",
        "webdav" => "管理员已禁用 WebDAV 存储功能",
        "custom_webdav" => "管理员已禁用自定义 WebDAV 服务器",
        _ => "该功能已被管理员禁用",
    };
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "success": false,
            "error": "feature_disabled",
            "feature": feature,
            "message": hint,
        })),
    )
        .into_response()
}

/// 审计日志写入辅助函数。
/// 静默丢弃写入错误（`let _ = ...`），关键操作（删 owner、改 owner 密码、删用户）改用 audit_log_or_fail。
///   若 SQLite 磁盘满/锁竞争/权限问题导致审计日志写不进去，运维无任何告警，
///   违反"审计可追溯"原则（删 owner、改 owner 密码等高危操作无记录 = 无法事后追责）。
///
///   本函数：
///   - 成功返回 true
///   - 失败 tracing::warn! 详细上下文（actor/action/target/错误），便于运维定位
///   - 返回 bool 让调用方自行决定是否阻断（关键操作应阻断，普通操作仅记录）
///
///   关键操作（按审计报告 P1-13）：删 owner、改 owner 密码、删用户、解封 IP
///   这些操作的调用方需检查返回值，失败时拒绝继续执行并返回 500。
///
///   实现委托给 SystemDatabase::audit_log_warn，避免与 storage.rs 逻辑重复。
fn audit_log(
    system_db: &crate::storage::SystemDatabase,
    actor: &str,
    action: &str,
    target: Option<&str>,
    detail_json: Option<&str>,
) -> bool {
    system_db.audit_log_warn(actor, action, target, detail_json)
}

struct AppState {
    bot: Arc<BotManager>,
    auth: Auth,
    /// Phase 1 多用户：系统库（system.db）句柄，与 Auth 共享。
    system_db: Arc<SystemDatabase>,
    web_dir: std::path::PathBuf,
    request_count: std::sync::atomic::AtomicU64,
    boot_time: f64,
    webdav_proxy_sem: Arc<tokio::sync::Semaphore>,
    /// 全局上传并发限制信号量（防止上传打满带宽/IO）。
    ///   防止 N 个用户同时上传 50MB 文件导致 OOM（50MB × N 内存膨胀）。
    ///   默认允许 4 个并发上传，其余请求排队等待。
    upload_sem: Arc<tokio::sync::Semaphore>,
    /// IP 封禁进程内缓存（启动时从 DB 加载）。
    ///   原 ip_ban_check 中间件每请求查 DB（is_ip_banned），高并发下 DB 锁争用严重。
    ///   改为 RwLock<HashSet> 缓存，TTL 30s 刷新；封禁/解封 API 主动刷新缓存。
    ip_ban_cache: Arc<RwLock<IpBanCache>>,
}

/// IP 封禁缓存结构。
///   - `banned`：当前有效封禁的 IP 字符串集合（已过滤过期记录）。
///   - `last_refresh`：上次从 DB 全量刷新的时间。
///   - TTL 由 ILINK_IP_BAN_CACHE_TTL_SECS 环境变量控制（默认 30s，最小 5s，最大 300s）。
struct IpBanCache {
    banned: Vec<IpNetwork>,
    last_refresh: Instant,
    initialized: bool,
}

impl IpBanCache {
    fn new() -> Self {
        Self {
            banned: Vec::new(),
            last_refresh: Instant::now(),
            initialized: false,
        }
    }

    /// TTL（秒），从环境变量读取，默认 30s。
    fn ttl_secs() -> u64 {
        std::env::var("ILINK_IP_BAN_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(5, 300)
    }

    /// 从 DB 全量刷新封禁列表（过滤已过期记录）。
    fn refresh_from_db(&mut self, system_db: &SystemDatabase) {
        let now = chrono::Utc::now().to_rfc3339();
        let bans = system_db.list_ip_bans();
        self.banned.clear();
        for ban in bans {
            // 过滤已过期记录（expires_at 非空且 < now）
            if let Some(ref exp) = ban.expires_at {
                if exp.as_str() < now.as_str() {
                    continue;
                }
            }
            match IpNetwork::parse(&ban.ip) {
                Ok(network) => self.banned.push(network),
                Err(error) => tracing::warn!(
                    "[SECURITY] 忽略数据库中的无效 IP/CIDR 封禁项 {:?}: {}",
                    ban.ip,
                    error
                ),
            }
        }
        self.last_refresh = Instant::now();
        self.initialized = true;
    }

    /// 检查 IP 是否被封禁（带 TTL 刷新）。
    /// 返回 true 表示被封禁。
    fn is_banned(&mut self, system_db: &SystemDatabase, ip: &str) -> bool {
        // 用 saturating_sub 避免时间回退/underflow panic。
        let elapsed = Instant::now()
            .saturating_duration_since(self.last_refresh)
            .as_secs();
        if !self.initialized || elapsed >= Self::ttl_secs() {
            self.refresh_from_db(system_db);
        }
        ip.parse::<IpAddr>()
            .ok()
            .is_some_and(|addr| self.banned.iter().any(|network| network.contains(addr)))
    }
}

#[derive(Clone, Copy)]
struct IpNetwork {
    network: IpAddr,
    prefix: u8,
}

impl IpNetwork {
    fn parse(input: &str) -> anyhow::Result<Self> {
        let input = input.trim();
        let (address, prefix) = match input.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (input, None),
        };
        let address: IpAddr = address
            .parse()
            .map_err(|_| anyhow::anyhow!("无效的 IP 地址"))?;
        let max_prefix = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(value) => value
                .parse::<u8>()
                .map_err(|_| anyhow::anyhow!("无效的 CIDR 前缀"))?,
            None => max_prefix,
        };
        if prefix > max_prefix {
            anyhow::bail!("CIDR 前缀超出范围");
        }
        let network = match address {
            IpAddr::V4(address) => {
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - prefix)
                };
                IpAddr::V4(Ipv4Addr::from(u32::from(address) & mask))
            }
            IpAddr::V6(address) => {
                let mask = if prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - prefix)
                };
                IpAddr::V6(Ipv6Addr::from(u128::from(address) & mask))
            }
        };
        Ok(Self { network, prefix })
    }

    fn contains(self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                u32::from(network) == (u32::from(address) & mask)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                u128::from(network) == (u128::from(address) & mask)
            }
            _ => false,
        }
    }

    fn canonical(self) -> String {
        let host_prefix = if self.network.is_ipv4() { 32 } else { 128 };
        if self.prefix == host_prefix {
            self.network.to_string()
        } else {
            format!("{}/{}", self.network, self.prefix)
        }
    }

    fn dangerous(self) -> bool {
        let host_prefix = if self.network.is_ipv4() { 32 } else { 128 };
        if self.prefix < host_prefix {
            return true;
        }
        match self.network {
            IpAddr::V4(ip) => {
                ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.is_private()
                    || ip.is_link_local()
                    || ip.is_multicast()
            }
            IpAddr::V6(ip) => {
                ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
                    || ip.is_multicast()
            }
        }
    }
}

pub(crate) fn normalize_ip_network(input: &str) -> anyhow::Result<(String, bool)> {
    let network = IpNetwork::parse(input)?;
    Ok((network.canonical(), network.dangerous()))
}

// ── 鉴权中间件 ──────────────────────────────────────────────

/// BotManager::get_or_create_bot 返回 `anyhow::Result`，初始化失败时返回 503 并降级处理（不再 panic）。
///   bot 创建失败（DB 损坏 / HTTP 客户端构建失败）不再 panic 拖垮全站。
///   本辅助函数把 Err 统一转成 HTTP 503，让单个用户的失败只影响该用户。
async fn get_bot_or_503(
    state: &Arc<AppState>,
    uid: i64,
) -> Result<crate::models::SharedBot, Response> {
    state.bot.get_or_create_bot(uid).await.map_err(|e| {
        tracing::error!("[WEB] uid={} bot 不可用: {:#}", uid, e);
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "success": false,
                "error": "bot_unavailable",
                "message": "用户会话初始化失败，请稍后重试或联系管理员"
            })),
        )
            .into_response()
    })
}

fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    for val in headers.get_all(header::COOKIE) {
        if let Ok(s) = val.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(stripped) = part.strip_prefix("session_token=") {
                    return Some(stripped.to_string());
                }
            }
        }
    }
    None
}

/// 从 Cookie 头提取 device_token。
fn extract_device_token(headers: &HeaderMap) -> Option<String> {
    for val in headers.get_all(header::COOKIE) {
        if let Ok(s) = val.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(stripped) = part.strip_prefix("device_token=") {
                    return Some(stripped.to_string());
                }
            }
        }
    }
    None
}

/// IP 封禁中间件：对所有路由（含公开路由）检查客户端 IP 是否被封禁。
/// 在 require_session 之前运行，确保被封禁 IP 无法访问任何端点。
async fn ip_ban_check(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    // ClientIp 缺失时 fail-closed 拒绝请求，避免 Origin/Referer CSRF 校验被绕过。
    //   原 impl 在 None 时直接放行，等于 set_client_ip 中间件未运行时
    //   被封禁 IP 仍可访问所有端点。缺失即说明中间件链异常，应拒绝而非放行。
    let client_ip = request.extensions().get::<ClientIp>().copied();
    match client_ip {
        Some(client_ip) => {
            let ip_str = client_ip.0.to_string();
            // 使用进程内缓存替代每请求 DB 查询，减少锁争用。
            //   原 state.system_db.is_ip_banned(&ip_str) 每请求查 DB，高并发下锁争用严重。
            //   现在通过 RwLock<HashSet> 缓存，TTL 30s 刷新；封禁/解封 API 主动刷新。
            let is_banned = {
                let mut cache = state.ip_ban_cache.write();
                cache.is_banned(&state.system_db, &ip_str)
            };
            if is_banned {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "success": false, "error": "ip_banned",
                        "message": "您的 IP 已被封禁，无法访问此服务"
                    })),
                )
                    .into_response());
            }
            Ok(next.run(request).await)
        }
        None => {
            tracing::warn!(
                "[M4] ip_ban_check: ClientIp extension 缺失，fail-closed 拒绝请求 (path={})",
                request.uri().path()
            );
            Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "success": false,
                    "error": "ip_resolution_failed",
                    "message": "无法确定客户端 IP，已拒绝访问。请检查反向代理与中间件配置。"
                })),
            )
                .into_response())
        }
    }
}

/// Phase 1 多用户：统一 session 校验 + 注入 AuthUser。
///   1. 所有路由统一 session 校验（媒体/webdav-proxy 不再豁免），
///      改由 cache_key 反查 + Cookie session 双重保护）
///   2. 非 GET 校验 Origin（CSRF 防护）
///   3. extract_session_token → state.auth.verify_session(token) → 取 SessionInfo
///      - 未通过 → 401
///   4. 续期 session
///   5. 注入 AuthUser{uid, role} 到 request extension，供 handler 取用
async fn require_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    // CSRF: 非 GET 方法校验 Origin，防止跨站表单伪造
    //   loopback 直连放行；其余请求要求 Origin 在 allowed_origins 中
    let method = request.method().clone();
    let client_ip_loopback = request
        .extensions()
        .get::<ClientIp>()
        .map(|c| c.0.is_loopback())
        .unwrap_or(false);
    if method != axum::http::Method::GET {
        // Origin 存在时校验，**同时**强制要求必须有 Origin（原实现仅在 Origin 存在时校验，缺失时整个块跳过）。
        //   非 loopback 请求缺失 Origin 时，回退检查 Referer；两者均缺失视为可疑请求，拒绝。
        if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
            if !client_ip_loopback && !is_origin_allowed(origin, state.bot.web_port()) {
                // 返回 JSON body，前端能 JSON.parse 出 message。
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "success": false, "error": "forbidden",
                        "message": "请求来源不被信任（Origin 校验失败）"
                    })),
                )
                    .into_response());
            }
        } else if !client_ip_loopback {
            // 无 Origin 头：回退到 Referer 校验（部分浏览器隐私模式 / curl 可能不发 Origin）
            let referer_ok = headers
                .get(header::REFERER)
                .and_then(|v| v.to_str().ok())
                .and_then(|r| url::Url::parse(r).ok())
                .map(|u| {
                    let host = u.host_str().unwrap_or("");
                    let port = u
                        .port()
                        .unwrap_or(if u.scheme() == "https" { 443 } else { 80 });
                    let origin = format!("{}://{}:{}", u.scheme(), host, port);
                    is_origin_allowed(&origin, state.bot.web_port())
                })
                .unwrap_or(false);
            if !referer_ok {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "success": false, "error": "forbidden",
                        "message": "缺少 Origin/Referer 头或来源不被信任"
                    })),
                )
                    .into_response());
            }
        }
    }
    // 1. 提取并验证 session token（system.db.sessions + S1 惰性清理）
    // 401 分支也统一改 JSON body，加 message 字段。
    let token = extract_session_token(&headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "success": false, "error": "unauthorized",
                "message": "未登录或会话已过期"
            })),
        )
            .into_response()
    })?;
    let info = state.auth.verify_session(&token).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "success": false, "error": "unauthorized",
                "message": "会话无效或已过期，请重新登录"
            })),
        )
            .into_response()
    })?;

    // 2. 续期 session（30 天滑动窗口）
    state.auth.renew_session(&token);

    // 3. 注入 AuthUser 到 request extension（handler 可通过 Extension<AuthUser> 取用）
    request.extensions_mut().insert(AuthUser {
        uid: info.uid,
        role: info.role,
    });

    Ok(next.run(request).await)
}

// ── 安全与网络工具 ──────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
struct ClientIp(IpAddr);

// S52: 用 once_cell 缓存 trusted_proxies，避免每请求重新解析环境变量
static TRUSTED_PROXIES: once_cell::sync::Lazy<Vec<IpAddr>> = once_cell::sync::Lazy::new(|| {
    std::env::var("ILINK_TRUSTED_PROXIES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
});

fn trusted_proxies() -> &'static Vec<IpAddr> {
    &TRUSTED_PROXIES
}

fn parse_ip(s: &str) -> Option<IpAddr> {
    s.trim().parse().ok()
}

// 审计 M-9: 上次"同机反代未配置可信代理"告警的 UNIX 秒（0 = 从未告警），10 分钟节流
static LAST_LOOPBACK_PROXY_WARN: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// 从 X-Forwarded-For / X-Real-IP / 直连地址解析真实客户端 IP
fn real_client_ip(headers: &HeaderMap, direct: Option<IpAddr>) -> IpAddr {
    let direct_ip = direct.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    // 隧道场景：公网访客经本地 ssh 回连，直连对端恒为 loopback。隧道活跃期间
    // 将 loopback 视为可信代理，从 serveo 追加的 XFF 还原真实 IP（从右往左取
    // 第一个非可信地址；serveo 把访客真实 IP 追加在最右，伪造左侧条目无法冒充）。
    // 否则所有隧道访客都记为 127.0.0.1：限流/封禁折叠成单桶，一人触发全站被限。
    let tunnel_loopback = direct_ip.is_loopback() && crate::tunnel::tunnel_active();
    let is_trusted =
        |ip: &IpAddr| trusted_proxies().contains(ip) || (tunnel_loopback && ip.is_loopback());
    // S2: 未配置可信代理且隧道未活跃时直接返回直连 IP，不读取任何
    // X-Forwarded-For / X-Real-IP 头，防止客户端伪造 XFF 欺骗来源判定。
    if trusted_proxies().is_empty() && !tunnel_loopback {
        // 审计 M-9: 直连为 loopback 且携带 X-Forwarded-For，说明前方存在未登记的
        // 同机反向代理。此形态下所有公网访客都被记为 127.0.0.1：非 GET 请求跳过
        // Origin/Referer CSRF 校验、登录限流按 127.0.0.1 全站共桶（可被用来锁死
        // 全站登录）、admin intranet 模式对经反代的公网流量放行。
        if direct_ip.is_loopback() && headers.contains_key("X-Forwarded-For") {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last = LAST_LOOPBACK_PROXY_WARN.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs.saturating_sub(last) >= 600
                && LAST_LOOPBACK_PROXY_WARN
                    .compare_exchange(last, now_secs, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed)
                    .is_ok()
            {
                tracing::warn!(
                    "[M9] 检测到 loopback 直连携带 X-Forwarded-For：疑似同机反向代理未配置 ILINK_TRUSTED_PROXIES。\
                     此形态下经反代进来的公网请求会被当作 127.0.0.1（CSRF Origin 校验被跳过、限流折叠为单桶、\
                     intranet 管理面板对公网可达）。请设置 ILINK_TRUSTED_PROXIES=127.0.0.1 并由反代透传 X-Forwarded-For。"
                );
            }
        }
        return direct_ip;
    }
    if let Some(xff) = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()) {
        let parts: Vec<&str> = xff
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        // 从右往左找第一个不在可信代理列表中的 IP
        for ip_str in parts.iter().rev() {
            if let Some(ip) = parse_ip(ip_str) {
                if is_trusted(&ip) {
                    continue;
                }
                return ip;
            }
        }
        // 全部是可信代理，返回最左边的
        if let Some(ip) = parts.first().and_then(|s| parse_ip(s)) {
            return ip;
        }
    }
    if let Some(xri) = headers
        .get("X-Real-IP")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_ip)
    {
        if !is_trusted(&xri) {
            return xri;
        }
    }
    direct_ip
}

async fn set_client_ip(request: Request, next: Next) -> Response {
    let direct = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let ip = real_client_ip(request.headers(), direct);
    let mut request = request;
    request.extensions_mut().insert(ClientIp(ip));
    next.run(request).await
}

/// Phase 5 (S5): 全局安全响应头中间件。
///   - `X-Content-Type-Options: nosniff`  防止浏览器 MIME 嗅探（尤其是 media 代理路由）
///   - `X-Frame-Options: DENY`             防止点击劫持（页面不可被 iframe 嵌入）
///   - `Referrer-Policy: no-referrer`      防止 Referer 泄露（含 token 的 URL 不外泄）
///   - `Content-Security-Policy` 限制脚本/连接/媒体来源，发生 XSS 时阻挡外发数据
///   - `Strict-Transport-Security` HTTPS 模式下追加 HSTS，防止降级到 HTTP
///   - `Cache-Control` 对 HTML 已在 handler 设置，此处不覆盖
async fn security_headers(request: Request, next: Next) -> Response {
    // 在 next.run 之前抓取请求头判断 HTTPS（响应阶段 headers 已被消费）。
    let is_https = is_https_request(request.headers());
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if !headers.contains_key("x-content-type-options") {
        headers.insert(
            header::HeaderName::from_static("x-content-type-options"),
            header::HeaderValue::from_static("nosniff"),
        );
    }
    if !headers.contains_key("x-frame-options") {
        headers.insert(
            header::HeaderName::from_static("x-frame-options"),
            header::HeaderValue::from_static("DENY"),
        );
    }
    if !headers.contains_key("referrer-policy") {
        headers.insert(
            header::HeaderName::from_static("referrer-policy"),
            header::HeaderValue::from_static("no-referrer"),
        );
    }
    // Content-Security-Policy：限制脚本/连接/媒体来源，阻挡 XSS 外发数据。
    // 策略说明：script-src 含 'unsafe-inline'（现有 HTML 依赖内联脚本）；
    // connect-src 仅允许同源 + ws/wss；img-src 允许 data/blob + CDN；
    // frame-ancestors + base-uri + form-action 均为 'self'。
    if !headers.contains_key("content-security-policy") {
        if let Ok(csp) = header::HeaderValue::from_str(
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             connect-src 'self' ws: wss:; \
             img-src 'self' data: blob: https://ms.188850.xyz; \
             media-src 'self' blob: data:; \
             style-src 'self' 'unsafe-inline'; \
             font-src 'self' data:; \
             frame-ancestors 'none'; \
             base-uri 'self'; \
             form-action 'self'",
        ) {
            headers.insert(
                header::HeaderName::from_static("content-security-policy"),
                csp,
            );
        }
    }
    // HSTS 仅在 HTTPS（含 ILINK_FORCE_HTTPS）下追加，max-age=1 年 + includeSubDomains。
    if is_https && !headers.contains_key("strict-transport-security") {
        if let Ok(hsts) = header::HeaderValue::from_str("max-age=31536000; includeSubDomains") {
            headers.insert(
                header::HeaderName::from_static("strict-transport-security"),
                hsts,
            );
        }
    }
    response
}

/// IP 是否属于内网（loopback / 私网 / link-local / fc00::/7）。
/// admin_ip_guard 与 /static 管理资源守卫（static_admin_assets_guard）共用。
fn is_intranet_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                // fe80::/10 link-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // fc00::/7 unique local
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Admin panel access restriction.
/// 读取 system_settings.admin.web_access 决定访问策略:
///   off      → 完全关闭前端管理（403）
///   intranet → 仅内网 IP 可访问（默认，兼容旧行为）
///   open     → 公网可访问（session 鉴权仍生效）
/// 应用到 /admin 和 /api/admin/* 路由。
async fn admin_ip_guard(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    let mode = state
        .system_db
        .get_setting("admin.web_access")
        .unwrap_or_else(|| "intranet".to_string());
    match mode.as_str() {
        "off" => Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false,
                "error": "admin_disabled",
                "message": "管理员前端已关闭，请通过 CLI (ilink-wm1 admin ...) 操作"
            })),
        )
            .into_response()),
        "open" => Ok(next.run(request).await),
        _ /* "intranet" */ => {
            // ClientIp extension 缺失时 fail-closed 拒绝请求（中间件链异常时不应放行）。
            let client_ip = request.extensions().get::<ClientIp>().copied();
            match client_ip {
                Some(client_ip) => {
                    if !is_intranet_ip(client_ip.0) {
                        return Err((
                            StatusCode::FORBIDDEN,
                            Json(serde_json::json!({
                                "success": false,
                                "error": "admin_restricted",
                                "message": "管理面板仅限内网访问。请通过 SSH 隧道、内网地址访问，或用 CLI (ilink-wm1 admin webset set open) 开启公网访问。"
                            })),
                        )
                            .into_response());
                    }
                    Ok(next.run(request).await)
                }
                None => {
                    tracing::warn!(
                        "[M4] admin_ip_guard: ClientIp extension 缺失，fail-closed 拒绝请求 (path={})",
                        request.uri().path()
                    );
                    Err((
                        StatusCode::FORBIDDEN,
                        Json(serde_json::json!({
                            "success": false,
                            "error": "admin_ip_resolution_failed",
                            "message": "无法确定客户端 IP，已拒绝访问以保护管理面板。请检查反向代理与中间件配置。"
                        })),
                    )
                        .into_response())
                }
            }
        }
    }
}

/// 审计 M-8: /static 匿名暴露管理面板源码。
/// 将 admin.html / zn-admin.js 收敛到与 /admin 相同的访问策略
/// （system_settings.admin.web_access：off / intranet / open）之下，
/// 避免公网直接下载管理 API 结构。其余静态资源不受影响。
/// 注：/static 路由在全局 set_client_ip 层之后合并，无 ClientIp 扩展，
/// 此处直接用 ConnectInfo + real_client_ip 自行计算客户端 IP。
async fn static_admin_assets_guard(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    const ADMIN_ASSETS: [&str; 2] = ["/static/admin.html", "/static/zn-admin.js"];
    let path = request.uri().path();
    if !ADMIN_ASSETS.iter().any(|p| path.eq_ignore_ascii_case(p)) {
        return next.run(request).await;
    }
    let deny = |message: &str| {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false,
                "error": "admin_restricted",
                "message": message
            })),
        )
            .into_response()
    };
    let mode = state
        .system_db
        .get_setting("admin.web_access")
        .unwrap_or_else(|| "intranet".to_string());
    match mode.as_str() {
        "off" => deny("管理员前端已关闭，管理面板资源不可访问"),
        "open" => next.run(request).await,
        _ => {
            let direct = request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ci| ci.0.ip());
            let ip = real_client_ip(request.headers(), direct);
            if is_intranet_ip(ip) {
                next.run(request).await
            } else {
                deny("管理面板资源仅限内网访问。请通过 SSH 隧道、内网地址访问，或用 CLI (ilink-wm1 admin webset set open) 开启公网访问。")
            }
        }
    }
}

/// Phase 5 (§8.2): system_settings 敏感键白名单。
///   返回 true 表示该键的值不得通过任何 Web API 返回给前端（仅 CLI 可读）。
///   当前敏感键：管理员口令相关、JWT/加密密钥、WebDAV 服务端凭据。
///   注：用户级 WebDAV 配置走 bot.get_webdav_settings()，已对 password 打码 "********"。
/// 已在 api_admin_settings 中应用，过滤敏感键。
fn is_sensitive_setting(key: &str) -> bool {
    matches!(
        key,
        "admin_password"
            | "admin_password_hash"
            | "admin_salt"
            | "jwt_secret"
            | "session_secret"
            | "encryption_key"
            | "server_storage.webdav_url"
            | "server_storage.webdav_username"
            | "server_storage.webdav_password"
            | "server_storage.webdav_base_path"
    )
}

/// 去掉路径最后一段的扩展名。
/// 如 `/ilink-media/ab/abcdef.jpg` → `/ilink-media/ab/abcdef`
/// 仅处理最后一段（文件名部分），不碰目录段。
/// 扩展名长度 >10 或不含点号则返回 None（视为无扩展名可去）。
fn strip_last_extension(path: &str) -> Option<String> {
    // 找最后一个 '/'
    let last_slash = path.rfind('/')?;
    let (dir, filename) = path.split_at(last_slash + 1);
    // 文件名中找最后一个 '.'
    let dot = filename.rfind('.')?;
    if dot == 0 {
        return None; // 隐藏文件 .xxx 不处理
    }
    let ext = &filename[dot..];
    if ext.len() > 10 {
        return None; // 异常长扩展名，不处理
    }
    let base = &filename[..dot];
    if base.is_empty() {
        return None;
    }
    Some(format!("{}{}", dir, base))
}

fn is_https_request(headers: &HeaderMap) -> bool {
    if std::env::var("ILINK_FORCE_HTTPS")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
    {
        return true;
    }
    // S50: 未配置可信代理时不信任 X-Forwarded-Proto，基于实际连接判断。
    //   例外：隧道活跃期间 loopback 直连来自本地 ssh（serveo 侧 TLS 终止），
    //   其 X-Forwarded-Proto 可信——否则隧道访客的会话 cookie 缺 Secure 属性。
    if trusted_proxies().is_empty() && !crate::tunnel::tunnel_active() {
        return false;
    }
    headers
        .get("X-Forwarded-Proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

fn allowed_origins(port: u16) -> AllowOrigin {
    let raw = std::env::var("ILINK_ALLOWED_ORIGINS").unwrap_or_default();
    // 拒绝 `*` 通配符（安全风险）。
    //   原 `AllowOrigin::any()` 会让任意站点发起跨域请求，配合 `SameSite=Strict` cookie
    //   虽不致直接泄露凭据，但会摧毁 CSRF 防护的另一道防线（Origin 白名单）。
    //   显式误配 `*` 视为不安全配置，回退到仅 loopback。
    if raw.trim() == "*" {
        tracing::warn!("[CORS] ILINK_ALLOWED_ORIGINS=* 被拒绝（不安全配置），回退到仅 loopback");
        return AllowOrigin::list(vec![
            HeaderValue::from_str(&format!("http://127.0.0.1:{}", port))
                .unwrap_or_else(|_| HeaderValue::from_static("http://127.0.0.1")),
            HeaderValue::from_str(&format!("http://localhost:{}", port))
                .unwrap_or_else(|_| HeaderValue::from_static("http://localhost")),
        ]);
    }
    let origins: Vec<HeaderValue> = if raw.trim().is_empty() {
        vec![
            format!("http://127.0.0.1:{}", port),
            format!("http://localhost:{}", port),
        ]
    } else {
        raw.split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }
    .into_iter()
    .filter_map(|s| HeaderValue::from_str(&s).ok())
    .collect();
    if origins.is_empty() {
        // S49: 解析失败不回退 any，仅允许 loopback，避免误配导致跨域完全开放
        let loopback = vec![
            HeaderValue::from_str(&format!("http://127.0.0.1:{}", port))
                .unwrap_or_else(|_| HeaderValue::from_static("http://127.0.0.1")),
            HeaderValue::from_str(&format!("http://localhost:{}", port))
                .unwrap_or_else(|_| HeaderValue::from_static("http://localhost")),
        ];
        AllowOrigin::list(loopback)
    } else {
        AllowOrigin::list(origins)
    }
}

/// S15/S11: 判断 Origin 是否在允许列表中（或 CORS 配置为 any）
fn is_origin_allowed(origin: &str, port: u16) -> bool {
    let raw = std::env::var("ILINK_ALLOWED_ORIGINS").unwrap_or_default();
    // 同 allowed_origins，拒绝 `*`。
    if raw.trim() == "*" {
        return false;
    }
    let allowed: Vec<String> = if raw.trim().is_empty() {
        vec![
            format!("http://127.0.0.1:{}", port),
            format!("http://localhost:{}", port),
        ]
    } else {
        raw.split(',')
            .map(|s| s.trim())
            .map(|s| s.to_string())
            .collect()
    };
    if allowed.iter().any(|a| a == origin) {
        return true;
    }
    // 隧道公网 origin 放行：经 serveo.net 隧道访问的浏览器，其 Origin 是隧道
    // 域名（如 https://x.serveo.net），不在默认 loopback 白名单内。真实 IP
    // 还原后（real_client_ip）这些访客不再表现为 loopback，不放行会误杀
    // 全部隧道访客的登录 / WebSocket / 会话校验。
    if let Some(base) = crate::tunnel::public_origin() {
        if origin == base {
            return true;
        }
        // serveo 对明文 http 入口可能先跳 https，两种 scheme 都放行。
        if let Some(stripped) = base.strip_prefix("https://") {
            if origin == format!("http://{}", stripped) {
                return true;
            }
        }
    }
    false
}

/// S11: 请求来源是否可信——loopback 直连，或 Origin 在允许列表中
// ceiling=is_request_trusted 当前无调用方，保留以备 CSRF 信任链扩展。
#[allow(dead_code)]
fn is_request_trusted(headers: &HeaderMap, client_ip: IpAddr, port: u16) -> bool {
    if client_ip.is_loopback() {
        return true;
    }
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        return is_origin_allowed(origin, port);
    }
    false
}

fn parse_body_limit(raw: &str) -> usize {
    let raw = raw.trim().to_lowercase();
    let (num_part, mult) = if raw.ends_with("mb") {
        (&raw[..raw.len() - 2], 1024 * 1024)
    } else if raw.ends_with("kb") {
        (&raw[..raw.len() - 2], 1024)
    } else if raw.ends_with("gb") {
        (&raw[..raw.len() - 2], 1024 * 1024 * 1024)
    } else {
        (raw.as_str(), 1)
    };
    num_part
        .trim()
        .parse::<f64>()
        .map(|n| (n * mult as f64) as usize)
        .unwrap_or(50 * 1024 * 1024)
}

fn max_request_body_size() -> usize {
    // S3: 默认 100MB（复用 webdav.rs MAX_DOWNLOAD_SIZE），可被 ILINK_MAX_REQUEST_BODY 覆盖。
    //   媒体端点继承此全局上限；JSON 端点在路由层单独收紧到 64KB。
    std::env::var("ILINK_MAX_REQUEST_BODY")
        .map(|v| parse_body_limit(&v))
        .unwrap_or(100 * 1024 * 1024)
}

async fn count_request(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    state
        .request_count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    next.run(request).await
}

// ── 路由 ────────────────────────────────────────────────────

fn find_web_dir() -> std::path::PathBuf {
    // 优先使用源码 web/（通过 Cargo.toml 定位），找不到时回退到二进制旁 web/。
    //   避免 target/debug/web/ 过期导致前端修改不生效。
    //   仅在源码 web/ 不存在时（如 release 部署）回退到 exe 旁边的 web/。
    // 1. 向上查找 Cargo.toml 定位项目根，优先用源码 web/
    if let Ok(exe) = std::env::current_exe() {
        let mut p = exe.clone();
        for _ in 0..6 {
            if let Some(parent) = p.parent() {
                if parent.join("Cargo.toml").exists() {
                    let source_web = parent.join("web");
                    if source_web.join("landing.html").exists() {
                        return source_web;
                    }
                    break;
                }
                p = parent.to_path_buf();
            } else {
                break;
            }
        }
    }
    // 2. 当前工作目录
    let cwd_web = std::path::PathBuf::from("web");
    if cwd_web.join("landing.html").exists() {
        return cwd_web;
    }
    // 3. 可执行文件所在目录
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let exe_web = parent.join("web");
            if exe_web.join("landing.html").exists() {
                return exe_web;
            }
        }
    }
    // 4. dist/web 目录（开发构建产物）
    let dist_web = std::path::PathBuf::from("dist/web");
    if dist_web.join("landing.html").exists() {
        return dist_web;
    }
    // 5. target/debug/web 目录（调试构建）
    let target_debug_web = std::path::PathBuf::from("target/debug/web");
    if target_debug_web.join("landing.html").exists() {
        return target_debug_web;
    }
    // 6. 回退到当前工作目录
    cwd_web
}

// ── 管理员身份校验辅助 ─────────────────────────────────

/// 检查当前用户是否为 owner 或 admin，否则返回 403
#[allow(clippy::result_large_err)]
fn require_admin(user: &AuthUser) -> Result<(), Response> {
    if user.role != "owner" && user.role != "admin" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false, "error": "forbidden",
                "message": "需要管理员权限"
            })),
        )
            .into_response());
    }
    Ok(())
}

/// 检查当前用户是否为 owner，否则返回 403。
///   admin 不能执行创建 owner 等高危操作。
#[allow(clippy::result_large_err)]
fn require_owner(user: &AuthUser) -> Result<(), Response> {
    if user.role != "owner" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false, "error": "forbidden",
                "message": "需要 owner 权限"
            })),
        )
            .into_response());
    }
    Ok(())
}

// ── 邮箱 API ─────────────────────────────────────────

async fn api_set_email(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetEmailRequest>,
) -> Response {
    let email = body.email.trim();
    // 基本格式校验
    if !email.is_empty() {
        let has_at = email.contains('@');
        let has_dot = email.contains('.');
        if !has_at || !has_dot || email.len() < 5 || email.len() > 254 {
            return Json(serde_json::json!({
                "success": false, "error": "邮箱格式无效"
            }))
            .into_response();
        }
    }
    match state.system_db.set_user_email(user.uid, email) {
        Ok(_) => {
            // 审计日志写入失败时 warn 告警，不阻断业务。
            audit_log(
                &state.system_db,
                &format!("uid={}", user.uid),
                "user.set-email",
                Some(&format!("uid={}", user.uid)),
                Some(&format!("{{\"email\":\"{}\"}}", email)),
            );
            Json(serde_json::json!({"success": true})).into_response()
        }
        Err(e) => internal_error_response("set_email", e),
    }
}

async fn api_get_email(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let email = state.system_db.get_user_email(user.uid).unwrap_or_default();
    Json(serde_json::json!({
        "success": true,
        "email": email,
    }))
}

// ── 设备令牌（浏览器自动登录）API ────────────────────

/// 登录扩展：登录时如果带 device_name，则生成并返回 device_token
/// 此函数在 api_login 中被调用
async fn api_auto_login(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    headers: HeaderMap,
    Json(body): Json<AutoLoginRequest>,
) -> Response {
    // 不再直接读取 X-Real-IP（可由客户端伪造），改用 set_client_ip 中间件解析的 ClientIp extension。
    let client_ip_loopback = client_ip.0.is_loopback();
    if !client_ip_loopback {
        let origin_ok = headers
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .map(|o| is_origin_allowed(o, state.bot.web_port()))
            .unwrap_or(false);
        if !origin_ok {
            // 回退检查 Referer
            let referer_ok = headers
                .get(header::REFERER)
                .and_then(|v| v.to_str().ok())
                .and_then(|r| url::Url::parse(r).ok())
                .map(|u| {
                    let host = u.host_str().unwrap_or("");
                    let port = u
                        .port()
                        .unwrap_or(if u.scheme() == "https" { 443 } else { 80 });
                    let origin = format!("{}://{}:{}", u.scheme(), host, port);
                    is_origin_allowed(&origin, state.bot.web_port())
                })
                .unwrap_or(false);
            if !referer_ok {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "success": false, "error": "forbidden",
                        "message": "请求来源不被信任（Origin/Referer 校验失败）"
                    })),
                )
                    .into_response();
            }
        }
    }

    // 优先从 Cookie 读取 device_token，回退到请求体（兼容旧前端）
    let device_token = extract_device_token(&headers)
        .or_else(|| body.device_token.clone())
        .unwrap_or_default();
    if device_token.is_empty() {
        return Json(serde_json::json!({
            "success": false, "error": "缺少设备令牌"
        }))
        .into_response();
    }
    let uid = match state.system_db.verify_device_token(&device_token) {
        Some(uid) => uid,
        None => {
            return Json(serde_json::json!({
                "success": false, "error": "设备令牌无效或已过期"
            }))
            .into_response();
        }
    };
    // 签发 session token
    let token = match state.auth.create_session(uid) {
        Some(t) => t,
        None => {
            return Json(serde_json::json!({
                "success": false, "error": "会话创建失败"
            }))
            .into_response();
        }
    };
    // HTTPS 下追加 Secure 标志，避免 cookie 经 HTTP 明文链路泄露。
    let secure = is_https_request(&headers);
    let cookie_str = format!(
        "session_token={}; Max-Age=2592000; Path=/; SameSite=Strict; HttpOnly{}",
        token,
        if secure { "; Secure" } else { "" }
    );
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json");
    if let Ok(v) = HeaderValue::from_str(&cookie_str) {
        builder = builder.header(header::SET_COOKIE, v);
    }
    match builder.body(
        serde_json::json!({
            "success": true,
            "uid": uid,
        })
        .to_string()
        .into(),
    ) {
        Ok(resp) => resp,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn api_list_device_tokens(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let tokens = state.system_db.list_device_tokens(user.uid);
    Json(serde_json::json!({
        "success": true,
        "tokens": tokens,
    }))
}

async fn api_revoke_device_token(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<RevokeDeviceTokenRequest>,
) -> Response {
    // 校验 token 所有权：先查所属 uid，与当前 uid 比对；owner/admin 可跨用户撤销。
    match state.system_db.get_device_token_owner(&body.token) {
        Some(token_uid) => {
            if token_uid != user.uid && user.role != "owner" && user.role != "admin" {
                // 跨用户撤销尝试记审计日志（即使被拒绝）。
                audit_log(
                    &state.system_db,
                    &format!("uid={}", user.uid),
                    "device_token.revoke_denied",
                    Some(&format!("token_uid={}", token_uid)),
                    Some("{\"reason\":\"not_owner\"}"),
                );
                return Json(serde_json::json!({
                    "success": false,
                    "error": "无权撤销他人的设备令牌"
                }))
                .into_response();
            }
        }
        None => {
            // token 不存在——可能是已撤销或本身无效，幂等返回 success（避免泄露 token 是否存在）
            return Json(serde_json::json!({"success": true})).into_response();
        }
    }
    match state.system_db.revoke_device_token(&body.token) {
        Ok(_) => {
            // 审计日志写入失败时 warn 告警，不阻断业务。
            audit_log(
                &state.system_db,
                &format!("uid={}", user.uid),
                "device_token.revoke",
                Some(&format!(
                    "token_hash_prefix={}...",
                    &crate::crypto::sha256_hex(body.token.as_bytes())[..8]
                )),
                None,
            );
            Json(serde_json::json!({"success": true})).into_response()
        }
        Err(e) => internal_error_response("revoke_device_token", e),
    }
}

// ── 管理员 API（需要 owner/admin 角色）────────────────

async fn api_admin_users(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let users = state.system_db.list_users();
    // 转 PublicAppUser 剥离 password_hash / salt / iterations，
    //   避免 admin 接口向前端泄露密码哈希（admin 会话被劫持或管理面板 XSS
    //   即可拿到哈希后离线爆破）。
    let public_users: Vec<PublicAppUser> = users.iter().map(Into::into).collect();
    Ok(Json(serde_json::json!({
        "success": true,
        "users": public_users,
    })))
}

// ── 改造方案 §二：单个用户的 Bot 状态 ─────────────────────
async fn api_admin_bot_status(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let uid: i64 = params.get("uid").and_then(|v| v.parse().ok()).unwrap_or(0);
    if uid == 0 {
        return Ok(Json(
            serde_json::json!({"success": false, "error": "invalid uid"}),
        ));
    }
    let status = state.bot.bot_status(uid);
    Ok(Json(serde_json::json!({"success": true, "data": status})))
}

// ── 改造方案 §三：修改用户配额 ──────────────────────────
#[derive(Deserialize)]
struct AdminSetQuotaRequest {
    user: String,
    #[serde(default)]
    upload_bytes: Option<i64>,
    #[serde(default)]
    download_bytes: Option<i64>,
    #[serde(default)]
    media_bytes: Option<i64>,
    #[serde(default)]
    msg_per_day: Option<i64>,
    #[serde(default)]
    media_count: Option<i64>,
}

async fn api_admin_user_quota(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminSetQuotaRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let quota = serde_json::json!({
        "upload_bytes": body.upload_bytes,
        "download_bytes": body.download_bytes,
        "media_bytes": body.media_bytes,
        "msg_per_day": body.msg_per_day,
        "media_count": body.media_count,
    });
    match state.bot.update_user_quota(&body.user, &quota) {
        Ok(_) => Ok(Json(serde_json::json!({"success": true}))),
        Err(e) => Ok(Json(
            serde_json::json!({"success": false, "error": e.to_string()}),
        )),
    }
}

// ── 改造方案 §三：修改用户功能开关 ──────────────────────────
#[derive(Deserialize)]
struct AdminSetFeaturesRequest {
    user: String,
    #[serde(default)]
    upload: Option<i64>,
    #[serde(default)]
    webdav: Option<i64>,
    #[serde(default)]
    custom_webdav: Option<i64>,
}

async fn api_admin_user_features(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminSetFeaturesRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let features = serde_json::json!({
        "upload": body.upload,
        "webdav": body.webdav,
        "custom_webdav": body.custom_webdav,
    });
    match state.bot.update_user_features(&body.user, &features) {
        Ok(_) => Ok(Json(serde_json::json!({"success": true}))),
        Err(e) => Ok(Json(
            serde_json::json!({"success": false, "error": e.to_string()}),
        )),
    }
}

async fn api_admin_user_create(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminCreateUserRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流（防暴力提权），
    //   防止会话被劫持后批量创建账号。
    let admin_rl_key = format!("{}|admin.user.create", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    // admin 不能创建 owner 账号，避免 admin 被入侵后通过
    //   创建 owner 实现特权提升与持久化入口。仅 owner 可创建 owner/admin/user。
    //   admin 仅可创建 admin/user。非法 role 一律拒绝。
    let allowed_role = match user.role.as_str() {
        "owner" => match body.role.as_str() {
            "owner" | "admin" | "user" => body.role.clone(),
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "success": false, "error": "invalid_role",
                        "message": "非法角色，仅支持 owner/admin/user"
                    })),
                )
                    .into_response());
            }
        },
        "admin" => match body.role.as_str() {
            "admin" | "user" => body.role.clone(),
            "owner" => {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "success": false, "error": "forbidden",
                        "message": "admin 无权创建 owner 账号"
                    })),
                )
                    .into_response());
            }
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "success": false, "error": "invalid_role",
                        "message": "非法角色，admin 仅可创建 admin/user"
                    })),
                )
                    .into_response());
            }
        },
        _ => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "success": false, "error": "forbidden",
                    "message": "无权创建账号"
                })),
            )
                .into_response());
        }
    };
    match state
        .auth
        .create_user(&body.username, &body.password, &allowed_role)
    {
        Ok(uid) => {
            // 审计日志写入失败时 warn 告警，不阻断业务。
            audit_log(
                &state.system_db,
                &format!("uid={}", user.uid),
                "admin.user.create",
                Some(&format!("uid={}", uid)),
                Some(&format!(
                    "{{\"username\":\"{}\",\"role\":\"{}\"}}",
                    body.username, allowed_role
                )),
            );
            Ok(Json(serde_json::json!({
                "success": true, "uid": uid, "username": body.username, "role": allowed_role
            }))
            .into_response())
        }
        Err(e) => Ok(Json(serde_json::json!({
            "success": false, "error": e
        }))
        .into_response()),
    }
}

async fn api_admin_user_disable(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminUserActionRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.user.disable", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    let u = state
        .system_db
        .get_user_by_username(&body.user)
        .ok_or_else(|| {
            Json(serde_json::json!({
                "success": false, "error": "用户不存在"
            }))
            .into_response()
        })?;
    if u.role == "owner" {
        return Ok(Json(serde_json::json!({
            "success": false, "error": "不可禁用 owner 账号"
        }))
        .into_response());
    }
    // 禁用用户时立即撤销该用户全部 session + device_token。
    //   原实现仅改 status，被禁用户最长 30 天（session TTL）内仍可正常访问，
    //   "禁用"操作形同虚设。
    // 三步原子化：用 transaction() 包裹 disable + revoke sessions + revoke tokens。
    //   update_user_status(disabled) + delete_all_sessions + revoke_all_device_tokens。
    //   原三步独立调用，中途崩溃会留下中间态（status=disabled 但 session 仍可用，
    //   或 session 清了但 status 还是 active）。atomic 函数任一步失败全部回滚。
    //   降级策略：atomic 失败时再单独调 update_user_status 保证 status 至少改为 disabled，
    //   session/device_token 可能残留（下次轮询或重新登录会被 status=disabled 拒绝）。
    let (sessions_ok, device_tokens_ok, status_changed) = match state
        .system_db
        .disable_user_atomic(u.id)
    {
        Ok(()) => (true, true, true),
        Err(e) => {
            tracing::warn!(
                "[admin.user.disable] disable_user_atomic 失败 uid={}: {}，降级为单独 update_user_status",
                u.id, e
            );
            match state.system_db.update_user_status(u.id, "disabled") {
                Ok(()) => (false, false, true),
                Err(e2) => {
                    tracing::error!(
                        "[admin.user.disable] update_user_status 也失败 uid={}: {}（用户可能仍 active）",
                        u.id, e2
                    );
                    (false, false, false)
                }
            }
        }
    };
    if !status_changed {
        return Ok(Json(serde_json::json!({
            "success": false,
            "error": "禁用失败：无法更新用户状态，请检查数据库",
            "sessions_revoked": false,
            "device_tokens_revoked": false
        }))
        .into_response());
    }
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", user.uid),
        "admin.user.disable",
        Some(&format!("uid={}", u.id)),
        Some(&format!(
            "{{\"username\":\"{}\",\"sessions_revoked\":{},\"device_tokens_revoked\":{}}}",
            u.username, sessions_ok, device_tokens_ok
        )),
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "sessions_revoked": sessions_ok,
        "device_tokens_revoked": device_tokens_ok
    }))
    .into_response())
}

async fn api_admin_user_enable(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminUserActionRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.user.enable", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    let u = state
        .system_db
        .get_user_by_username(&body.user)
        .ok_or_else(|| {
            Json(serde_json::json!({
                "success": false, "error": "用户不存在"
            }))
            .into_response()
        })?;
    let _ = state.system_db.update_user_status(u.id, "active");
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", user.uid),
        "admin.user.enable",
        Some(&format!("uid={}", u.id)),
        Some(&format!("{{\"username\":\"{}\"}}", u.username)),
    );
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

async fn api_admin_user_delete(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminUserActionRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.user.delete", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    let u = state
        .system_db
        .get_user_by_username(&body.user)
        .ok_or_else(|| {
            Json(serde_json::json!({
                "success": false, "error": "用户不存在"
            }))
            .into_response()
        })?;
    // 删除 owner 账号是高危操作，仅 owner 可执行。
    //   admin 仍可删除普通 user/admin 账号。
    if u.role == "owner" {
        require_owner(&user)?;
        let owners: Vec<_> = state
            .system_db
            .list_users()
            .into_iter()
            .filter(|x| x.role == "owner" && x.status == "active")
            .collect();
        if owners.len() <= 1 {
            return Ok(Json(serde_json::json!({
                "success": false, "error": "不可删除最后一个 owner"
            }))
            .into_response());
        }
    }
    // 删用户（含删 owner）属关键操作，审计日志写入失败必须阻断。
    //   原 impl 直接 `let _ = delete_user(...)`，没有任何审计记录——一旦被滥用（恶意 admin
    //   批量删用户、入侵后清理账号）将完全无据可查。现先写审计日志，失败则拒绝执行删除。
    //   审计字段：actor（执行者 uid）、target（被删 uid）、detail（用户名+角色）。
    let actor = format!("uid={}", user.uid);
    let target = format!("uid={}", u.id);
    let detail = format!(
        "{{\"username\":\"{}\",\"role\":\"{}\"}}",
        u.username, u.role
    );
    if !audit_log(
        &state.system_db,
        &actor,
        "admin.user.delete",
        Some(&target),
        Some(&detail),
    ) {
        return Ok(Json(serde_json::json!({
            "success": false,
            "error": "audit_log_failed",
            "message": "审计日志写入失败，拒绝执行删除操作以保护可追溯性"
        }))
        .into_response());
    }
    let _ = state.system_db.delete_user(u.id);
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

async fn api_admin_invites(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let invites = state.system_db.list_invites();
    Ok(Json(serde_json::json!({
        "success": true,
        "invites": invites,
    })))
}

async fn api_admin_invite_create(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminCreateInviteRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.invite.create", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 20, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    let code = state
        .system_db
        .allocate_invite_code()
        .map_err(|e| internal_error_response("allocate_invite_code", e))?;
    let expires_at = body.days.unwrap_or(30);
    let expires_at_str = if expires_at > 0 {
        Some(
            chrono::Utc::now()
                .checked_add_signed(chrono::Duration::days(expires_at))
                .unwrap_or_else(chrono::Utc::now)
                .to_rfc3339(),
        )
    } else {
        None
    };
    let _ = state
        .system_db
        .create_invite(&code, expires_at_str.as_deref(), body.note.as_deref());
    Ok(Json(serde_json::json!({
        "success": true, "code": code
    }))
    .into_response())
}

async fn api_admin_invite_revoke(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminRevokeInviteRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.invite.revoke", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 20, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    let _ = state.system_db.revoke_invite(&body.code);
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

async fn api_admin_settings(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    // 原实现直接返回 list_settings() 全量键值，
    //   导致 jwt_secret / session_secret / encryption_key / admin_password_hash /
    //   admin_salt / server_storage.webdav_password 等明文泄漏到前端。
    //   管理员会话被劫持或管理面板 XSS 即可伪造任意会话、接管系统。
    // 只返回运行时真正支持的规范键，避免展示“保存成功但不生效”的历史别名。
    let settings = state.system_db.list_settings();
    let filtered: Vec<&crate::models::SystemSetting> = settings
        .iter()
        .filter(|s| is_supported_system_setting(&s.key) && !is_sensitive_setting(&s.key))
        .collect();
    Ok(Json(serde_json::json!({
        "success": true,
        "settings": filtered,
    })))
}

async fn api_admin_set_setting(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminSetSettingRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.setting.set", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 20, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    // 仅接受预定义 key/value（白名单），被入侵 admin 不可覆盖任意系统配置。
    //   （如 allow_open_registration、master_key 路径等）。改为白名单校验，仅允许已知配置项。
    if let Err(error) = validate_system_setting(&body.key, &body.value) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "invalid_setting",
                "message": error.to_string()
            })),
        )
            .into_response());
    }
    // value 长度上限（防超大 value 撑爆 system.db）
    if body.value.len() > 65536 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "value_too_large",
                "message": "配置值过大（上限 64KB）"
            })),
        )
            .into_response());
    }
    state
        .system_db
        .set_setting(&body.key, &body.value)
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "success": false,
                    "error": "setting_write_failed",
                    "message": error.to_string()
                })),
            )
                .into_response()
        })?;
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", user.uid),
        "admin.setting",
        Some(&body.key),
        Some(&format!("{{\"value_len\":{}}}", body.value.len())),
    );
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

async fn api_admin_stats(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let users = state.system_db.list_users();
    let active = users.iter().filter(|u| u.status == "active").count();
    let disabled = users.iter().filter(|u| u.status == "disabled").count();
    let invites = state.system_db.list_invites();
    let active_invites = invites.iter().filter(|i| i.status == "active").count();
    let settings_count = state.system_db.list_settings().len();
    // 用 audit_log_count 取真实总数，避免"近 1000 条"误导管理员。
    let audit_count = state.system_db.audit_log_count() as usize;

    // System resource info
    let sys_info = {
        use sysinfo::System;
        let mut sys = System::new();
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        // Wait a bit for CPU usage to be accurate
        // std::thread::sleep 阻塞 Tokio 工作线程，
        //   高并发下拖慢整个服务。改用 tokio::time::sleep 让出调度器。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        sys.refresh_cpu_usage();

        let mem_total = sys.total_memory() / 1024 / 1024; // MB
        let mem_used = sys.used_memory() / 1024 / 1024;
        let cpu_usage = sys.global_cpu_usage();

        // Disk info for current directory
        let disk_total;
        let disk_used;
        {
            use sysinfo::Disks;
            let disks = Disks::new_with_refreshed_list();
            let current_dir = std::env::current_dir().unwrap_or_default();
            let disk = disks
                .iter()
                .find(|d| current_dir.starts_with(d.mount_point()))
                .or_else(|| disks.iter().next());
            match disk {
                Some(d) => {
                    disk_total = d.total_space() / 1024 / 1024;
                    disk_used = (d.total_space() - d.available_space()) / 1024 / 1024;
                }
                None => {
                    disk_total = 0;
                    disk_used = 0;
                }
            }
        }

        let uptime = System::uptime();

        serde_json::json!({
            "mem_total_mb": mem_total,
            "mem_used_mb": mem_used,
            "cpu_usage_percent": format!("{:.1}", cpu_usage),
            "disk_total_mb": disk_total,
            "disk_used_mb": disk_used,
            "uptime_secs": uptime,
        })
    };

    // 暴露 Webhook 状态供管理面板展示。
    //   仅 owner/admin 可见，URL 列表已通过 SSRF 校验（不含 token 本身）。
    let webhook_info = match state.bot.webhook_status() {
        Some((urls, has_token, secs)) => serde_json::json!({
            "enabled": true,
            "urls": urls,
            "url_count": urls.len(),
            "has_token": has_token,
            "secs_since_validate": secs,
        }),
        None => serde_json::json!({
            "enabled": false,
            "urls": [],
            "url_count": 0,
            "has_token": false,
            "secs_since_validate": 0,
        }),
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "stats": {
            "users_total": users.len(),
            "users_active": active,
            "users_disabled": disabled,
            "invites_total": invites.len(),
            "invites_active": active_invites,
            "settings_count": settings_count,
            "audit_recent": audit_count,
        },
        "system": sys_info,
        "webhook": webhook_info,
    })))
}

async fn api_admin_audit(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    // 上限可配置（ILINK_AUDIT_LIMIT），并返回总数 + 保留天数，
    //   便于前端展示"共 N 条 / 显示最近 M 条 / 保留 D 天"。
    let configured_limit = std::env::var("ILINK_AUDIT_LIMIT")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1000)
        .clamp(1, 10000);
    let retention_days = std::env::var("ILINK_AUDIT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(90);
    let logs = state.system_db.list_audit(configured_limit);
    let total = state.system_db.audit_log_count();
    Ok(Json(serde_json::json!({
        "success": true,
        "logs": logs,
        "total": total,
        "limit": configured_limit,
        "retention_days": retention_days,
    })))
}

async fn api_admin_ip_bans(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let bans = state.system_db.list_ip_bans();
    Ok(Json(serde_json::json!({
        "success": true,
        "bans": bans,
    })))
}

async fn api_admin_ip_ban(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminBanIpRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.ip.ban", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    let (network, dangerous) = normalize_ip_network(&body.ip).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "invalid_ip_network",
                "message": error.to_string()
            })),
        )
            .into_response()
    })?;
    if dangerous && !body.confirm_dangerous {
        return Ok(Json(serde_json::json!({
            "success": false,
            "error": "confirmation_required",
            "message": "该地址属于 CIDR 网段、回环/私网或基础设施地址，必须显式确认后才能封禁"
        }))
        .into_response());
    }
    let days = if body.days > 0 { Some(body.days) } else { None };
    state
        .system_db
        .ban_ip(&network, &body.reason, &format!("uid={}", user.uid), days)
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "success": false,
                    "error": "ip_ban_write_failed",
                    "message": error.to_string()
                })),
            )
                .into_response()
        })?;
    // 审计日志写入失败时 warn 告警，不阻断业务。
    //   ban_ip 是普通操作（封 IP）——封禁动作本身已生效，审计失败仅告警。
    //   解封才需阻断（见 api_admin_ip_unban）。
    audit_log(
        &state.system_db,
        &format!("uid={}", user.uid),
        "admin.ip.ban",
        Some(&network),
        Some(&format!(
            "{{\"days\":{},\"reason\":\"{}\"}}",
            body.days, body.reason
        )),
    );
    // 封禁后主动刷新缓存，确保新封禁 IP 立即生效（不等 TTL）。
    {
        let mut cache = state.ip_ban_cache.write();
        cache.refresh_from_db(&state.system_db);
    }
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

async fn api_admin_ip_unban(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminUserActionRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    // 管理员 destructive 操作按管理员 uid 限流。
    let admin_rl_key = format!("{}|admin.ip.unban", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    // 解封 IP 属关键操作，审计日志写入失败必须阻断。
    //   原 impl 先 unban_ip 后写审计，若审计失败则该次解封无记录——攻击者可借机解封
    //   自己的攻击 IP 而运维无察觉。现先写审计日志，失败则拒绝执行解封。
    //   审计字段：actor（执行者 uid）、target（被解封 IP）、detail（None）。
    let (network, _) = normalize_ip_network(&body.user).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "invalid_ip_network",
                "message": error.to_string()
            })),
        )
            .into_response()
    })?;
    let actor = format!("uid={}", user.uid);
    if !audit_log(
        &state.system_db,
        &actor,
        "admin.ip.unban",
        Some(&network),
        None,
    ) {
        return Ok(Json(serde_json::json!({
            "success": false,
            "error": "audit_log_failed",
            "message": "审计日志写入失败，拒绝执行解封操作以保护可追溯性"
        }))
        .into_response());
    }
    state.system_db.unban_ip(&network).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "success": false,
                "error": "ip_unban_write_failed",
                "message": error.to_string()
            })),
        )
            .into_response()
    })?;
    // 解封后主动刷新缓存，确保解封立即生效（不等 TTL）。
    {
        let mut cache = state.ip_ban_cache.write();
        cache.refresh_from_db(&state.system_db);
    }
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

// ── 内网穿透 API（Phase 3）─────────────────────────

async fn api_admin_tunnel_start(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TunnelStartRequest>,
) -> Result<Response, Response> {
    if user.role != "owner" && user.role != "admin" {
        return Ok(
            Json(serde_json::json!({"success": false, "error": "需要管理员权限"})).into_response(),
        );
    }
    // 管理员 destructive/高风险操作单独限流，防止会话被劫持后批量执行。
    let admin_rl_key = format!("{}|admin.tunnel.start", user.uid);
    if state.bot.check_rate_limit(&admin_rl_key, 10, 60.0) {
        return Ok(rate_limited_response("admin"));
    }
    // 本地端口与远程端口均需通过白名单校验。
    if let Err(reason) = validate_tunnel_local_port(body.port) {
        return Ok(Json(serde_json::json!({"success": false, "error": reason})).into_response());
    }
    if let Err(reason) = validate_tunnel_remote_port(body.remote) {
        return Ok(Json(serde_json::json!({"success": false, "error": reason})).into_response());
    }
    match crate::admin::get_tunnel_manager().start(body.port, body.remote, &body.subdomain) {
        Ok(()) => Ok(Json(serde_json::json!({"success": true})).into_response()),
        Err(e) => Ok(Json(serde_json::json!({"success": false, "error": e})).into_response()),
    }
}

async fn api_admin_tunnel_stop(Extension(user): Extension<AuthUser>) -> Result<Response, Response> {
    if user.role != "owner" && user.role != "admin" {
        return Ok(
            Json(serde_json::json!({"success": false, "error": "需要管理员权限"})).into_response(),
        );
    }
    match crate::admin::get_tunnel_manager().stop() {
        Ok(()) => Ok(Json(serde_json::json!({"success": true})).into_response()),
        Err(e) => Ok(Json(serde_json::json!({"success": false, "error": e})).into_response()),
    }
}

async fn api_admin_tunnel_status(
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, Response> {
    if user.role != "owner" && user.role != "admin" {
        return Err(
            Json(serde_json::json!({"success": false, "error": "需要管理员权限"})).into_response(),
        );
    }
    let mgr = crate::admin::get_tunnel_manager();
    let info = mgr.status();
    let logs = mgr.logs(50);
    // 单端点返回全字段（与 Python /admin-tunnel-status 一致），减少前端轮询请求。
    Ok(Json(serde_json::json!({
        "success": true,
        "tunnel": {
            "running": info.state == crate::tunnel::TunnelState::Running,
            "state": format!("{:?}", info.state),
            "local_port": info.local_port,
            "remote_port": info.remote_port,
            "subdomain": info.subdomain,
            "public_url": info.public_url,
            "pid": info.pid,
        },
        "logs": logs,
    })))
}

/// 批量查询多个用户的 Bot 状态，避免 N 次单用户轮询。
/// 用法：GET /api/admin/bot-status-batch?uids=1,2,3,4
async fn api_admin_bot_status_batch(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, Response> {
    require_admin(&user)?;
    let uids_str = params.get("uids").map(String::as_str).unwrap_or("");
    let mut statuses = serde_json::Map::new();
    for part in uids_str.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Ok(uid) = part.parse::<i64>() {
            statuses.insert(uid.to_string(), state.bot.bot_status(uid));
        }
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "statuses": statuses,
    })))
}

// ── 全局通知广播 ────────────────────────────────────────────

async fn api_admin_broadcast(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminBroadcastRequest>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    if body.message.trim().is_empty() {
        return Ok(
            Json(serde_json::json!({"success": false, "error": "消息内容不能为空"}))
                .into_response(),
        );
    }
    let level = match body.level.as_str() {
        "warn" | "error" | "info" => body.level.as_str(),
        _ => "info",
    };
    // Store in system_settings for persistence
    let notification = serde_json::json!({
        "message": body.message.trim(),
        "level": level,
        "time": chrono::Local::now().to_rfc3339(),
        "author": format!("uid={}", user.uid),
    });
    let _ = state
        .system_db
        .set_setting("global_notification", &notification.to_string());
    // Broadcast via per-user bot brokers to all connected WS clients
    let event_data = serde_json::json!({
        "message": body.message.trim(),
        "level": level,
        "time": chrono::Local::now().to_rfc3339(),
    });
    state
        .bot
        .broadcast_to_all_bots("global_notification", event_data);
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", user.uid),
        "admin.broadcast",
        Some(level),
        Some(&body.message),
    );
    tracing::info!(
        "[Admin] uid={} 发送全局通知 level={} msg={}",
        user.uid,
        level,
        body.message
    );
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

/// Clear the global notification
async fn api_admin_broadcast_clear(
    Extension(user): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<Response, Response> {
    require_admin(&user)?;
    let _ = state.system_db.set_setting("global_notification", "");
    state.bot.broadcast_to_all_bots(
        "global_notification",
        serde_json::json!({
            "message": "", "level": "clear"
        }),
    );
    Ok(Json(serde_json::json!({"success": true})).into_response())
}

/// Get current global notification (public, for clients on connect)
async fn api_notification(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let raw = state
        .system_db
        .get_setting("global_notification")
        .unwrap_or_default();
    if raw.is_empty() {
        return Json(serde_json::json!({"success": true, "notification": null}));
    }
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => Json(serde_json::json!({"success": true, "notification": v})),
        Err(_) => Json(serde_json::json!({"success": true, "notification": null})),
    }
}

// ── 登录扩展：支持 device_token 生成（"记住我"）────────

/// 在 api_login 原有的 login 函数基础上，如果请求携带 device_name，生成 device_token。
/// 通过修改 api_login 末尾注入 device_token 逻辑来实现。
pub fn create_app(bot: Arc<BotManager>, system_db: Arc<SystemDatabase>) -> Router {
    // Phase 1 多用户：Auth 背靠 system.db，不再用 bot.db
    let auth = Auth::new(system_db.clone());
    let boot_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let web_dir = find_web_dir();

    let state = Arc::new(AppState {
        bot: bot.clone(),
        auth,
        system_db,
        web_dir: web_dir.clone(),
        request_count: std::sync::atomic::AtomicU64::new(0),
        boot_time,
        webdav_proxy_sem: Arc::new(tokio::sync::Semaphore::new(2)),
        upload_sem: Arc::new(tokio::sync::Semaphore::new(4)),
        // IP 封禁缓存初始化（启动时从 DB 加载一次）。
        ip_ban_cache: Arc::new(RwLock::new(IpBanCache::new())),
    });

    let cors = CorsLayer::new()
        .allow_origin(allowed_origins(bot.web_port()))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-session-token"),
        ]);

    // 公开路由
    let public = Router::new()
        .route("/", get(index_landing))
        .route("/chat", get(index_chat))
        .route("/register", get(index_register))
        .route("/auth", get(index_auth))
        .route("/terms", get(index_terms))
        .route("/favicon.ico", get(favicon))
        .route("/healthz", get(healthz))
        .route("/api/ws", get(api_ws_upgrade))
        .route("/api/wasm/auth-status", get(auth_status))
        .route("/api/wasm/login", post(api_login))
        .route("/api/wasm/register", post(api_register))
        .route("/api/wasm/terms", get(api_terms))
        .route("/api/wasm/guide", get(api_guide))
        .route("/api/wasm/register-status", get(api_register_status))
        .route("/api/wasm/refresh-session", get(api_refresh_session))
        // Phase 1.5: 自动登录（使用设备令牌 / cookie）
        .route("/api/wasm/auto-login", post(api_auto_login))
        // 外部链接配置（docs URL, terms URL）
        .route("/api/wasm/links", get(api_links))
        // 公开站点信息（前端用于动态渲染站点名/品牌，无需登录）
        .route("/api/wasm/site-info", get(api_site_info))
        // 全局通知（客户端连接时获取当前通知）
        .route("/api/wasm/notification", get(api_notification))
        // S3: 公开路由仅收小 JSON（login），收紧到 64KB
        .layer(DefaultBodyLimit::max(64 * 1024));

    // 鉴权路由（不含 catch-all，需放在 merge 之后）
    // S3: 按请求体大小分两组——JSON 端点收紧到 64KB（防 huge-body DoS），
    //   媒体上传端点（收文件 payload）放宽到全局上限 100MB。
    //   两组合并后再统一挂 require_session 中间件。
    let protected_json = Router::new()
        .route("/api/wasm/stats", get(api_stats))
        .route("/api/wasm/status", get(api_status))
        // Phase 3 (P3): 聚合配额/用量/功能/存储目标，前端 30s 轮询
        .route("/api/wasm/me", get(api_me))
        // 会话状态 + 重新扫码端点
        .route("/api/wasm/session-status", get(api_session_status))
        .route("/api/wasm/reauth-start", post(api_reauth_start))
        .route("/api/wasm/reauth-poll", get(api_add_user_status))
        // 出站消息恢复端点
        .route("/api/wasm/outbound-pending", get(api_outbound_pending))
        .route("/api/wasm/outbound-resend", post(api_outbound_resend))
        .route("/api/wasm/qrcode", get(api_qrcode))
        .route("/api/wasm/messages", get(api_messages))
        .route("/api/wasm/users", get(api_users))
        .route("/api/wasm/chat-previews", get(api_chat_previews))
        .route("/api/wasm/history", get(api_history))
        .route("/api/wasm/about", get(api_about))
        .route("/api/wasm/add-user-status", get(api_add_user_status))
        .route("/api/wasm/add-user-start", post(api_add_user_start))
        .route("/api/wasm/send", post(api_send))
        .route("/api/wasm/typing", post(api_typing))
        .route("/api/wasm/media-presign", post(api_media_presign))
        .route("/api/wasm/download-media", post(api_download_media))
        .route("/api/wasm/switch-user", post(api_switch_user))
        .route("/api/wasm/delete-user", post(api_delete_user))
        .route("/api/wasm/batch-delete", post(api_batch_delete))
        .route("/api/wasm/clear-messages", post(api_clear_messages))
        .route("/api/wasm/delete-messages", post(api_delete_messages))
        .route(
            "/api/wasm/webdav-settings",
            get(api_webdav_get).post(api_webdav_save),
        )
        .route("/api/wasm/webdav-test", post(api_webdav_test))
        .route(
            "/api/wasm/webdav-traffic-saver",
            post(api_webdav_traffic_saver),
        )
        .route("/api/wasm/webdav-migrate", post(api_webdav_migrate))
        .route(
            "/api/wasm/webdav-migrate-status",
            get(api_webdav_migrate_status),
        )
        .route("/api/wasm/webdav-auth", get(api_webdav_auth))
        .route("/api/wasm/export-history", post(api_export_history))
        .route("/api/wasm/media/:cache_key", get(api_media))
        .route("/api/wasm/webdav-proxy/*remote_path", get(api_webdav_proxy))
        // 登出端点
        .route("/api/wasm/logout", post(api_logout))
        // set-password 移至 protected_json（需 require_session 注入 AuthUser），
        //   原在 public 组会导致 Extension<AuthUser> 取不到 → 500。
        .route("/api/wasm/set-password", post(api_set_password))
        // Phase 1.4: 邮箱设置
        .route("/api/wasm/set-email", post(api_set_email))
        .route("/api/wasm/email", get(api_get_email))
        // Phase 1.5: 设备令牌管理
        .route("/api/wasm/device-tokens", get(api_list_device_tokens))
        .route(
            "/api/wasm/device-token-revoke",
            post(api_revoke_device_token),
        )
        .layer(DefaultBodyLimit::max(64 * 1024));

    // 媒体端点：接收文件 payload（multipart / base64 / raw bytes），放宽到全局上限
    let protected_media = Router::new()
        .route("/api/wasm/media-stream", post(api_media_stream))
        .route("/api/wasm/send-media", post(api_send_media))
        .route("/api/wasm/upload-media", post(api_upload_media));

    // Phase 2: 管理员 API 路由（受 require_session + 角色检查 + 内网 IP 限制保护）
    let admin_api = Router::new()
        .route("/api/admin/users", get(api_admin_users))
        .route("/api/admin/user/create", post(api_admin_user_create))
        .route("/api/admin/user/disable", post(api_admin_user_disable))
        .route("/api/admin/user/enable", post(api_admin_user_enable))
        .route("/api/admin/user/delete", post(api_admin_user_delete))
        // 改造方案 §二：Bot 状态（单个 + 批量）
        .route("/api/admin/bot-status", get(api_admin_bot_status))
        .route(
            "/api/admin/bot-status-batch",
            get(api_admin_bot_status_batch),
        )
        // 改造方案 §三：用户配额/功能
        .route("/api/admin/user/quota", post(api_admin_user_quota))
        .route("/api/admin/user/features", post(api_admin_user_features))
        .route("/api/admin/invites", get(api_admin_invites))
        .route("/api/admin/invite/create", post(api_admin_invite_create))
        .route("/api/admin/invite/revoke", post(api_admin_invite_revoke))
        .route("/api/admin/settings", get(api_admin_settings))
        .route("/api/admin/setting", post(api_admin_set_setting))
        .route("/api/admin/stats", get(api_admin_stats))
        .route("/api/admin/audit", get(api_admin_audit))
        .route("/api/admin/ip-bans", get(api_admin_ip_bans))
        .route("/api/admin/ip-ban", post(api_admin_ip_ban))
        .route("/api/admin/ip-unban", post(api_admin_ip_unban))
        // Phase 3: 内网穿透管理（status 端点合并原 status + logs，与 Python 一致）
        .route("/api/admin/tunnel/start", post(api_admin_tunnel_start))
        .route("/api/admin/tunnel/stop", post(api_admin_tunnel_stop))
        .route("/api/admin/tunnel/status", get(api_admin_tunnel_status))
        // 全局通知广播
        .route("/api/admin/broadcast", post(api_admin_broadcast))
        .route(
            "/api/admin/broadcast-clear",
            post(api_admin_broadcast_clear),
        )
        .layer(axum_mw::from_fn_with_state(state.clone(), admin_ip_guard));

    // 管理面板页面路由（仅限内网访问）
    let admin_page = Router::new()
        .route("/admin", get(index_admin))
        .layer(axum_mw::from_fn_with_state(state.clone(), admin_ip_guard));

    let protected = protected_json
        .merge(protected_media)
        .merge(admin_api)
        .layer(axum_mw::from_fn_with_state(state.clone(), require_session));

    let app = Router::new()
        .merge(public)
        .merge(admin_page)
        .merge(protected)
        .layer(axum_mw::from_fn_with_state(state.clone(), count_request))
        // IP 封禁中间件：在 set_client_ip 之后，对所有路由检查 IP 封禁
        .layer(axum_mw::from_fn_with_state(state.clone(), ip_ban_check))
        .layer(axum_mw::from_fn(set_client_ip))
        // Phase 5 (S5): 全局安全响应头（nosniff / DENY / no-referrer）
        .layer(axum_mw::from_fn(security_headers))
        .layer(DefaultBodyLimit::max(max_request_body_size()))
        .layer(cors)
        // state 需在下方静态路由块中继续使用（审计 M-8 的守卫层），传 clone 保留所有权
        .with_state(state.clone());

    // 静态文件挂载
    //   nest_service 接管 /static/* 未匹配的路径。
    if web_dir.exists() {
        let static_service =
            tower_http::services::ServeDir::new(&web_dir).append_index_html_on_directories(false);
        // 前端 JS 为源码直供、无构建步骤，易被浏览器长缓存，
        //   导致修改 zn-*.js 后刷新仍加载旧版（如二维码弹窗逻辑不生效）。
        //   对 /static/* 强制 no-cache，确保每次都拉取最新文件。
        let static_route = Router::new()
            .nest_service("/static", static_service)
            // 审计 M-8: 管理面板源码（admin.html / zn-admin.js）收敛到 admin.web_access 策略
            .layer(axum_mw::from_fn_with_state(
                state.clone(),
                static_admin_assets_guard,
            ))
            .layer(SetResponseHeaderLayer::overriding(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static(
                    "no-store, no-cache, must-revalidate, max-age=0",
                ),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                axum::http::header::PRAGMA,
                axum::http::HeaderValue::from_static("no-cache"),
            ));
        app.merge(static_route)
    } else {
        app
    }
}

// ── 公开路由处理 ────────────────────────────────────────────

async fn index_landing(State(state): State<Arc<AppState>>) -> Response {
    let web_dir = state.web_dir.clone();
    let landing_path = web_dir.join("landing.html");
    let html = match std::fs::read_to_string(&landing_path) {
        Ok(h) => h,
        Err(_) => {
            "<html><body><h1>Zyn iLink ChatBox</h1><p>Landing page not found</p></body></html>"
                .to_string()
        }
    };
    // HTML 加 no-cache 头，避免浏览器缓存旧 HTML。
    (
        StatusCode::OK,
        [
            (
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ),
            (header::PRAGMA, header::HeaderValue::from_static("no-cache")),
        ],
        Html(html),
    )
        .into_response()
}

/// 认证页 HTML 服务（serve web/auth.html）—— 合并登录 + 注册
async fn index_auth(State(state): State<Arc<AppState>>) -> Response {
    let web_dir = state.web_dir.clone();
    let auth_path = web_dir.join("auth.html");
    let html = match std::fs::read_to_string(&auth_path) {
        Ok(h) => h,
        Err(_) => {
            "<html><body><h1>登录</h1><p>auth.html not found</p><p><a href=\"/\">返回首页</a></p></body></html>"
                .to_string()
        }
    };
    (
        StatusCode::OK,
        [
            (
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ),
            (header::PRAGMA, header::HeaderValue::from_static("no-cache")),
        ],
        Html(html),
    )
        .into_response()
}

/// 旧 /register 路由：重定向到 /auth?mode=register
async fn index_register() -> Response {
    (
        StatusCode::MOVED_PERMANENTLY,
        [(header::LOCATION, "/auth?mode=register")],
        Html(""),
    )
        .into_response()
}

/// Phase 4: 使用守则独立页面（serve web/terms.html）
/// 公开访问（无需登录），供注册前用户阅读守则。
async fn index_terms(State(state): State<Arc<AppState>>) -> Response {
    let web_dir = state.web_dir.clone();
    let terms_path = web_dir.join("terms.html");
    let html = match std::fs::read_to_string(&terms_path) {
        Ok(h) => h,
        Err(_) => {
            "<html><body><h1>使用守则</h1><p>terms.html not found</p><p><a href=\"/\">返回首页</a></p></body></html>"
                .to_string()
        }
    };
    (
        StatusCode::OK,
        [
            (
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ),
            (header::PRAGMA, header::HeaderValue::from_static("no-cache")),
        ],
        Html(html),
    )
        .into_response()
}

/// Phase 4: 获取使用守则文本+版本
async fn api_terms(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let version = state
        .system_db
        .get_setting("terms_version")
        .unwrap_or_else(|| "1.0".to_string());
    let text = state
        .system_db
        .get_setting("terms_text")
        .unwrap_or_default();
    let url = state.system_db.get_setting("terms.url").unwrap_or_default();
    Json(serde_json::json!({
        "success": true,
        "version": version,
        "text": text,
        "url": url,
    }))
}

/// Get external links configuration (docs URL, terms URL)
async fn api_links(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let docs_url = state.system_db.get_setting("docs.url").unwrap_or_default();
    let terms_url = state.system_db.get_setting("terms.url").unwrap_or_default();
    Json(serde_json::json!({
        "success": true,
        "docs_url": docs_url,
        "terms_url": terms_url,
    }))
}

/// 公开站点信息（无需登录）— 前端 zn-site.js 用于动态渲染站点名/品牌
/// 返回 { success, site_name, version }。site_name 缺省时回退到 "Zyn iLink ChatBox · WongMod"。
async fn api_site_info(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let site_name = state
        .system_db
        .get_setting("site_name")
        .unwrap_or_else(|| "Zyn iLink ChatBox · WongMod".to_string());
    let version = format!("v{}", SCRIPT_VERSION);
    Json(serde_json::json!({
        "success": true,
        "site_name": site_name,
        "version": version,
    }))
}

/// 获取随项目实际分发的管理指南（Markdown 原文）。
async fn api_guide(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let project_dir = state
        .web_dir
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let guide_path = [
        project_dir.join("部署指南.md"),
        project_dir.join("分发").join("README.md"),
    ]
    .into_iter()
    .find(|path| path.is_file());

    match guide_path {
        Some(path) => match tokio::fs::read_to_string(path).await {
            Ok(text) => Json(serde_json::json!({
                "success": true,
                "title": "使用与管理指南",
                "format": "markdown",
                "text": text,
                "exists": true,
            })),
            Err(_) => Json(serde_json::json!({
                "success": true,
                "title": "使用与管理指南",
                "format": "markdown",
                "text": "",
                "exists": false,
                "message": "指南文件不存在，请联系管理员。",
            })),
        },
        None => Json(serde_json::json!({
            "success": true,
            "title": "使用与管理指南",
            "format": "markdown",
            "text": "",
            "exists": false,
            "message": "指南文件不存在，请联系管理员。",
        })),
    }
}

/// Phase 4: 注册状态查询——返回开放注册/邀请码注册开关
async fn api_register_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let truthy = |k: &str, default: bool| {
        state
            .system_db
            .get_setting(k)
            .map(|value| setting_truthy(&value))
            .unwrap_or(default)
    };
    let allow_open = truthy("allow_open_registration", false);
    let allow_invite = truthy("allow_invite_registration", true);
    Json(serde_json::json!({
        "success": true,
        "allow_open": allow_open,
        "allow_invite": allow_invite,
        // closed = 两个都关，前端据此显示"注册已关闭"
        "closed": !allow_open && !allow_invite,
    }))
}

/// Phase 4: 用户注册
/// 校验顺序（§5.2）：用户名正则 → 密码强度 → 两次密码一致 → 守则同意 →
///   注册模式（开放/邀请码）→ IP 限速 → 写库 → 标邀请码 used → 建 user.db → 签发 session
async fn api_register(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Response {
    // S2: 注册限速 5 次/300s 按 IP（防批量注册）
    let rl_key = format!("{}|register", client_ip.0);
    if state.bot.check_rate_limit(&rl_key, 5, 300.0) {
        return rate_limited_response("register");
    }

    // 1. 用户名正则：^[A-Za-z0-9_-]{3,32}$
    // 与 CLI `admin init` / `first_run_setup` 用户名规则统一，
    //   允许字母、数字、下划线、连字符。原 Web 注册拒绝 `-` 但 CLI 允许，规则不一致
    //   会让用户在 CLI 创建的账号无法在 Web 完成同名注册（误判为非法字符）。
    let username = body.username.trim();
    if username.len() < 3
        || username.len() > 32
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Json(serde_json::json!({
            "success": false,
            "error": "用户名只能包含字母、数字、下划线、连字符，长度 3-32 位"
        }))
        .into_response();
    }

    // 1b. S-NEW 用户名保留字黑名单（防止冒充管理员/系统账号）
    //   放在长度正则之后：先剔除格式非法输入，避免对超长/含非法字符输入也跑一遍黑名单。
    //   大小写不敏感匹配（to_lowercase），覆盖 Admin/OWNER 等变体。
    const RESERVED_USERNAMES: &[&str] = &[
        "admin",
        "administrator",
        "owner",
        "root",
        "system",
        "sys",
        "api",
        "www",
        "web",
        "support",
        "help",
        "info",
        "service",
        "official",
        "staff",
        "mod",
        "moderator",
        "operator",
        "null",
        "undefined",
        "test",
        "guest",
        "anonymous",
        "ilink",
        "zyn",
        "chatbox",
        "config",
        "settings",
        "login",
        "register",
        "logout",
        "user",
        "users",
        "all",
        "me",
        "self",
        "superuser",
        "su",
    ];
    let username_lower = username.to_lowercase();
    if RESERVED_USERNAMES.contains(&username_lower.as_str()) {
        return Json(serde_json::json!({
            "success": false,
            "error": "该用户名为系统保留名，请更换"
        }))
        .into_response();
    }

    // 2. 密码强度（复用 auth.rs check_password_strength）
    if let Err(e) = crate::auth::Auth::check_password_strength(&body.password) {
        return Json(serde_json::json!({
            "success": false,
            "error": e
        }))
        .into_response();
    }

    // 3. 两次密码一致
    // 区分"未提供 confirm_password"和"两次密码不一致"两种错误。
    //   原实现 `unwrap_or("")` 会把"前端省略字段"和"用户漏填"都归为"两次密码不一致"，
    //   误导用户。现在显式区分：未提供时提示字段缺失，不一致时才提示"两次密码不一致"。
    let confirm = match body.confirm_password.as_deref() {
        None => {
            return Json(serde_json::json!({
                "success": false,
                "error": "请再次输入密码以确认"
            }))
            .into_response();
        }
        Some(c) => c,
    };
    if body.password != confirm {
        return Json(serde_json::json!({
            "success": false,
            "error": "两次输入的密码不一致"
        }))
        .into_response();
    }

    // 4. 守则同意版本必须等于当前 terms_version
    let terms_version = state
        .system_db
        .get_setting("terms_version")
        .unwrap_or_else(|| "1.0".to_string());
    if body.agreed_terms_ver.as_deref() != Some(terms_version.as_str()) {
        return Json(serde_json::json!({
            "success": false,
            "error": "请阅读并同意使用守则"
        }))
        .into_response();
    }

    // 5. 注册模式校验
    let truthy = |k: &str, default: bool| {
        state
            .system_db
            .get_setting(k)
            .map(|value| setting_truthy(&value))
            .unwrap_or(default)
    };
    let allow_open = truthy("allow_open_registration", false);
    let allow_invite = truthy("allow_invite_registration", true);
    if !allow_open && !allow_invite {
        return Json(serde_json::json!({
            "success": false,
            "error": "注册已关闭，请联系管理员"
        }))
        .into_response();
    }
    let need_invite = !allow_open; // 关闭开放注册时必须有邀请码
    if need_invite {
        let code = body.invite_code.as_deref().unwrap_or("").trim();
        if code.is_empty() {
            return Json(serde_json::json!({
                "success": false,
                "error": "请输入邀请码"
            }))
            .into_response();
        }
    }

    // 6. 调整注册顺序为"先 use_invite 占位 → create_user → 回填 uid"。
    //   原顺序（先 create_user → 再 use_invite）在 create_user 成功但 use_invite 失败时，
    //   需要回滚 delete_user，存在极小时间窗的并发竞争 + 删除失败导致账号残留风险。
    //   新顺序：先用 use_invite 占位 uid=0 锁定邀请码（防止并发重复使用），
    //   create_user 成功后回填真实 uid；失败则 restore_invite 回滚邀请码。
    //   优势：用户名冲突等 create_user 失败场景下不会消耗邀请码。
    let invite_code_str: String = if need_invite {
        let code = body.invite_code.as_deref().unwrap_or("").trim().to_string();
        if let Err(e) = state.system_db.use_invite(&code, 0) {
            tracing::warn!("[register] 邀请码占位失败 code={}: {}", code, e);
            return Json(serde_json::json!({
                "success": false,
                "error": "邀请码无效、已使用或已过期"
            }))
            .into_response();
        }
        code
    } else {
        String::new()
    };

    // 7. 创建用户（Auth::create_user 内部已做密码强度+盐+哈希）
    let uid = match state.auth.create_user(username, &body.password, "user") {
        Ok(uid) => uid,
        Err(e) => {
            // S6: 用户名冲突明确返回"用户名已存在"（注册场景可接受枚举）
            let msg = if e.to_lowercase().contains("unique") || e.contains("已存在") {
                "用户名已存在".to_string()
            } else {
                e
            };
            // create_user 失败时回滚邀请码到 active 状态。
            if need_invite {
                if let Err(e2) = state.system_db.restore_invite(&invite_code_str) {
                    tracing::error!(
                        "[register] restore_invite 失败 code={}: {} — 邀请码可能被消耗！",
                        invite_code_str,
                        e2
                    );
                }
            }
            return Json(serde_json::json!({
                "success": false,
                "error": msg
            }))
            .into_response();
        }
    };

    // 8. 回填邀请码的真实 uid（从占位 0 更新为真实 uid）
    if need_invite {
        if let Err(e) = state.system_db.update_invite_uid(&invite_code_str, uid) {
            tracing::warn!(
                "[register] update_invite_uid 失败 code={} uid={}: {}",
                invite_code_str,
                uid,
                e
            );
            // 不阻断注册（邀请码已 used，仅 used_by 仍为 0，不影响功能）
        }
    }

    // 9. 创建 users/<uid>/user.db（Database::new_for_user 自动建表）
    // Database::new_for_user 返回 Result，不再 panic，
    //   因此无需 catch_unwind 包装。spawn_blocking 仍保留（rusqlite Connection::open
    //   是阻塞调用，不应在 async 上下文执行）。
    let uid_for_closure = uid;
    let user_db_result = tokio::task::spawn_blocking(move || {
        crate::storage::Database::new_for_user(uid_for_closure)
    })
    .await;
    let user_db_error = match user_db_result {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(format!("{:#}", e)),
        Err(e) => Some(format!("初始化任务失败: {}", e)),
    };
    if let Some(error) = user_db_error {
        tracing::error!("[register] user.db 创建失败 uid={}, 回滚: {}", uid, error);
        if let Err(e) = state.system_db.delete_user(uid) {
            tracing::error!(
                "[register] 回滚 delete_user 失败 uid={}: {} — 账号残留！",
                uid,
                e
            );
        }
        // 用户创建失败也需回滚邀请码。
        if need_invite {
            if let Err(e2) = state.system_db.restore_invite(&invite_code_str) {
                tracing::error!(
                    "[register] restore_invite 失败 code={}: {}",
                    invite_code_str,
                    e2
                );
            }
        }
        return Json(serde_json::json!({
            "success": false, "error": "用户数据库初始化失败，请稍后重试或联系管理员"
        }))
        .into_response();
    }

    // 10. 记录守则同意版本；失败同样回滚半成品账户。
    if let Err(e) = state.system_db.set_user_agreed_terms(uid, &terms_version) {
        tracing::error!("[register] 记录守则同意失败 uid={}, 回滚: {}", uid, e);
        if let Err(delete_error) = state.system_db.delete_user(uid) {
            tracing::error!(
                "[register] 回滚 delete_user 失败 uid={}: {} — 账号残留！",
                uid,
                delete_error
            );
        }
        if need_invite {
            if let Err(restore_error) = state.system_db.restore_invite(&invite_code_str) {
                tracing::error!(
                    "[register] restore_invite 失败 code={}: {}",
                    invite_code_str,
                    restore_error
                );
            }
        }
        return Json(serde_json::json!({
            "success": false, "error": "账号初始化失败，请稍后重试"
        }))
        .into_response();
    }

    // 11. 签发 session 自动登录
    let token = match state.auth.create_session(uid) {
        Some(t) => t,
        None => {
            // 会话签发失败回滚，释放用户名让用户可用新邀请码重试。
            if let Err(e) = state.system_db.delete_user(uid) {
                tracing::error!(
                    "[register] create_session 失败后回滚 delete_user uid={}: {}",
                    uid,
                    e
                );
            }
            // 同样回滚邀请码。
            if need_invite {
                if let Err(e2) = state.system_db.restore_invite(&invite_code_str) {
                    tracing::error!(
                        "[register] restore_invite 失败 code={}: {}",
                        invite_code_str,
                        e2
                    );
                }
            }
            return Json(serde_json::json!({
                "success": false, "error": "账号创建失败（会话签发异常），请稍后重试"
            }))
            .into_response();
        }
    };

    // 11. 清除注册限速 + 审计
    state.bot.clear_rate_limit(&rl_key);
    // 审计日志写入失败时 warn 告警，不阻断业务。
    //   注册已成功（DB 行已写入），审计失败仅告警——回滚注册代价过高。
    audit_log(
        &state.system_db,
        &format!("uid={}", uid),
        "register",
        Some(&format!("username={}", username)),
        Some(&format!("{{\"ip\":\"{}\"}}", client_ip.0)),
    );

    // Phase 1.5: 如果注册请求了 "记住我"，生成 device_token
    let device_token = if body.remember_me.unwrap_or(false) {
        let device_name = body.device_name.unwrap_or_default();
        state
            .system_db
            .create_device_token(uid, &device_name, 30)
            .ok()
    } else {
        None
    };

    // 12. 设置 cookie + 返回（与 api_login 一致的 cookie 属性）
    let secure = is_https_request(&headers);
    let session_cookie = format!(
        "session_token={}; Max-Age=2592000; Path=/; SameSite=Strict; HttpOnly{}",
        token,
        if secure { "; Secure" } else { "" }
    );
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json");
    if let Ok(v) = HeaderValue::from_str(&session_cookie) {
        builder = builder.header(header::SET_COOKIE, v);
    }
    // device_token 通过 Cookie 下发（30 天固定有效期）
    if let Some(ref dt) = device_token {
        let dt_cookie = format!(
            "device_token={}; Max-Age=2592000; Path=/; SameSite=Strict; HttpOnly{}",
            dt,
            if secure { "; Secure" } else { "" }
        );
        if let Ok(v) = HeaderValue::from_str(&dt_cookie) {
            builder = builder.header(header::SET_COOKIE, v);
        }
    }
    let mut resp = serde_json::json!({
        "success": true,
        "uid": uid,
        "username": username,
    });
    if device_token.is_some() {
        resp["device_token"] = serde_json::Value::Bool(true);
    }
    match builder.body(resp.to_string().into()) {
        Ok(resp) => resp,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn index_admin(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let web_dir = state.web_dir.clone();
    let admin_path = web_dir.join("admin.html");
    let html = match std::fs::read_to_string(&admin_path) {
        Ok(h) => h,
        Err(_) => "<html><body><h1>管理面板</h1><p>admin.html not found</p><p><a href=\"/\">返回首页</a></p></body></html>".to_string(),
    };
    // 不再把 session_token 写入 HTML 源码（仅通过 HttpOnly Cookie 传递）。
    //   仅校验 cookie 中的 session 有效并续期；token 通过 HttpOnly Cookie 传递，
    //   前端 fetch/XHR/WS 同源自动携带 cookie，无需 JS 可读 token。
    //   未认证则不强制重定向（admin 页有自己的登录入口），但 token 留空触发前端跳转。
    let authed = match extract_session_token(&headers) {
        Some(tok) if state.auth.verify_session(&tok).is_some() => {
            state.auth.renew_session(&tok);
            true
        }
        _ => false,
    };
    let auth_required = if authed { "true" } else { "false" };
    let html = html.replace("{{AUTH_REQUIRED}}", auth_required);
    (
        StatusCode::OK,
        [
            (
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ),
            (header::PRAGMA, header::HeaderValue::from_static("no-cache")),
        ],
        Html(html),
    )
        .into_response()
}

async fn index_chat(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    // 未认证用户直接重定向到 /auth 认证页面，不再返回 chat.html
    // 仅校验 cookie，不再把 token 嵌入 HTML。
    let authed = match extract_session_token(&headers) {
        Some(tok) if state.auth.verify_session(&tok).is_some() => {
            state.auth.renew_session(&tok);
            true
        }
        _ => false,
    };
    if !authed {
        return (StatusCode::FOUND, [(header::LOCATION, "/auth")], Html("")).into_response();
    }
    let web_dir = state.web_dir.clone();
    let chat_path = web_dir.join("chat.html");
    let html = match std::fs::read_to_string(&chat_path) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("[WEB] 必需页面缺失 {}: {}", chat_path.display(), e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CACHE_CONTROL, "no-store")],
                Html("<html><body><h1>部署错误</h1><p>缺少必需的 web/chat.html，请重新部署完整 Web 资源。</p></body></html>"),
            )
                .into_response();
        }
    };
    let html = html.replace("{{AUTH_REQUIRED}}", "true");
    (
        StatusCode::OK,
        [
            (
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
            ),
            (header::PRAGMA, header::HeaderValue::from_static("no-cache")),
        ],
        Html(html),
    )
        .into_response()
}

async fn favicon() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

async fn auth_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Phase 1 多用户：始终要求登录。has_password 兼容旧前端字段，恒为 true。
    // has_any_user 为 false 表示需要先 `admin init` 建账号。
    let has_any_user = !state.system_db.list_users().is_empty();
    Json(serde_json::json!({
        "has_password": has_any_user,
        "need_setup_key": false,
        "multiuser": true,
        "has_any_user": has_any_user,
    }))
}

async fn api_login(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Response, StatusCode> {
    let build_json_response = |status: StatusCode,
                               value: serde_json::Value,
                               cookie: Option<HeaderValue>|
     -> Result<Response, StatusCode> {
        let mut builder = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(c) = cookie {
            builder = builder.header(header::SET_COOKIE, c);
        }
        builder
            .body(value.to_string().into())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    };

    // Phase 1 多用户：username 缺失或空 → 回退 "owner"（兼容旧前端）
    let username = body
        .username
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("owner");

    // 移除 SESSION-REUSE 免密登录路径（由 device_token Cookie 替代）。
    //   原实现：携带同用户名有效 Cookie 即可跳过密码校验直接获取新 token，
    //   即使密码已修改（旧 token 删除前的窗口内）。token 泄露 → 无需密码永久续期。
    //   现在登录端点一律验证密码；会话续期走独立的 /api/wasm/refresh-session 端点（已有）。
    //   密码修改时 auth.rs:449-468 已撤销该用户全部旧会话，登录端点验证密码即可闭环。
    let uid: i64;
    let role: String;

    // 限速检查前移到密码校验之前（防暴力破解更高效）。
    //   原实现先校验密码再限速，前 3 次失败不计入限速，存在低频暴力破解窗口。
    //   现在按 IP 维度先检查限速，超限直接 429，不再消耗 PBKDF2 计算资源。
    //   Q-1 已确认：限速策略保持 300s/3次。
    let rl_key = format!("{}|login", client_ip.0);
    if state.bot.check_rate_limit(&rl_key, 3, 300.0) {
        return Ok(rate_limited_response("login"));
    }
    // 叠加账号维度限流，防止攻击者用代理池对同一账号无限尝试。
    //   原实现仅按 IP 维度限流（3 次/300s），攻击者用代理池可绕过 IP 限制对单账号暴力破解。
    //   现增加账号维度：5 次/900s（15 分钟）锁定，超过阈值后该账号 15 分钟内无法登录。
    //   使用 is_rate_limited 仅检查不增加计数——避免"检查锁定状态"本身消耗一次失败额度。
    //   锁定提示与"用户名或密码错误"一致，避免泄露账号是否锁定（侧信道）。
    let user_rl_key = format!("user|{}|login_fail", username);
    if state.bot.is_rate_limited(&user_rl_key, 5, 900.0) {
        // 账号锁定事件记审计日志，供运维审查暴力破解尝试。
        audit_log(
            &state.system_db,
            &format!("username={}", username),
            "login.account_locked",
            Some(&format!("username={}", username)),
            Some(&format!(
                "{{\"ip\":\"{}\",\"window_secs\":900,\"max_attempts\":5}}",
                client_ip.0
            )),
        );
        tracing::warn!(
            "[AUTH] 账号被锁定（多次登录失败）username={} ip={} window=900s",
            username,
            client_ip.0
        );
        // 不返回"账号已锁定"，统一返回"用户名或密码错误"避免侧信道
        // 状态码改为 429（TOO_MANY_REQUESTS），
        //   WAF/IDS 可基于状态码识别暴力破解并自动封禁源 IP。
        //   错误消息保持"用户名或密码错误"以维持侧信道防护。
        return build_json_response(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({"success": false, "error": "用户名或密码错误"}),
            None,
        );
    }
    // 校验用户名 + 密码（S6: 用户名/密码错统一返回 None，不泄露用户名是否存在）
    match state.auth.verify_user_credentials(username, &body.password) {
        Some((u, r)) => {
            uid = u;
            role = r;
        }
        None => {
            // 登录失败时增加账号维度计数（与 IP 维度并行）。
            //   check_rate_limit 已超限返回 true 不再增加计数，避免计数无限增长。
            //   阈值 5 次/900s：第 5 次失败后账号锁定 15 分钟。
            let _ = state.bot.check_rate_limit(&user_rl_key, 5, 900.0);
            // 所有登录失败路径统一返回"用户名或密码错误"，
            //   不再返回"账号已禁用"差异化错误。原实现允许攻击者通过响应差异
            //   确认用户名是否存在 + 该账号是否被禁用，构成侧信道。
            //   禁用状态改为记审计日志供管理员查看。
            if let Some(cred) = state.system_db.get_user_credentials(username) {
                let iterations = if cred.iterations > 0 {
                    cred.iterations as u32
                } else {
                    crate::config::PBKDF2_ITERATIONS
                };
                let digest = crate::crypto::pbkdf2_hash(&body.password, &cred.salt, iterations);
                if crate::crypto::constant_time_compare(&digest, &cred.password_hash)
                    && cred.status != "active"
                {
                    // 密码正确但账号被禁用：仅记审计日志，不向前端泄露差异
                    // 审计日志写入失败时 warn 告警，不阻断业务。
                    audit_log(
                        &state.system_db,
                        &format!("username={}", username),
                        "login.disabled_blocked",
                        Some(&format!("uid={}", cred.uid)),
                        Some(&format!(
                            "{{\"ip\":\"{}\",\"status\":\"{}\"}}",
                            client_ip.0, cred.status
                        )),
                    );
                    tracing::info!(
                        "[AUTH] 登录被阻断（账号已禁用）username={} uid={} status={}",
                        username,
                        cred.uid,
                        cred.status
                    );
                }
            }
            // 状态码改为 401（UNAUTHORIZED），
            //   WAF/IDS 可基于 401 状态码检测暴力破解并自动封禁源 IP。
            //   错误消息保持"用户名或密码错误"以维持侧信道防护。
            return build_json_response(
                StatusCode::UNAUTHORIZED,
                serde_json::json!({"success": false, "error": "用户名或密码错误"}),
                None,
            );
        }
    }

    // 登录成功：清除该 IP 的登录限速记录 + 账号维度失败计数
    state
        .bot
        .clear_rate_limit(&format!("{}|login", client_ip.0));
    // 清除账号维度失败计数——成功登录后重置锁定窗口。
    state.bot.clear_rate_limit(&user_rl_key);

    // 防止 session 固定攻击：登录/注册成功、自动登录后签发新 session token。
    //   登录成功时一律颁发新 token，作废旧 token（DB + 内存），
    //   前端从响应体 session_token 字段取新值覆盖 _state.token。
    let old_token = extract_session_token(&headers);
    if let Some(ref old) = old_token {
        state.auth.delete_session(old);
    }
    // Phase 1 多用户：通过 system.db.sessions 颁发新 token
    let new_token = match state.auth.create_session(uid) {
        Some(t) => t,
        None => {
            return build_json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"success": false, "error": "会话创建失败"}),
                None,
            );
        }
    };

    // 审计日志写入失败时 warn 告警，不阻断业务。
    //   登录已成功（session 已发），审计失败仅告警——回滚 session 代价过高。
    audit_log(
        &state.system_db,
        &format!("uid={}", uid),
        "login",
        Some(&format!("uid={}", uid)),
        Some(&format!(
            "{{\"ip\":\"{}\",\"role\":\"{}\"}}",
            client_ip.0, role
        )),
    );

    let mut cookie_headers: Vec<HeaderValue> = Vec::new();
    let secure = is_https_request(&headers);
    let cookie_str = format!(
        "session_token={}; Max-Age=2592000; Path=/; SameSite=Strict; HttpOnly{}",
        new_token,
        if secure { "; Secure" } else { "" }
    );
    if let Ok(v) = HeaderValue::from_str(&cookie_str) {
        cookie_headers.push(v);
    }

    // Phase 1.5: 如果请求了 "记住我"，生成 device_token 并通过 Cookie 下发
    let device_token = if body.remember_me.unwrap_or(false) {
        let device_name = body.device_name.unwrap_or_default();
        let dt = state
            .system_db
            .create_device_token(uid, &device_name, 30)
            .ok();
        if let Some(ref dt_str) = dt {
            let dt_cookie = format!(
                "device_token={}; Max-Age=2592000; Path=/; SameSite=Strict; HttpOnly{}",
                dt_str,
                if secure { "; Secure" } else { "" }
            );
            if let Ok(v) = HeaderValue::from_str(&dt_cookie) {
                cookie_headers.push(v);
            }
        }
        dt
    } else {
        None
    };

    let mut resp = serde_json::json!({"success": true, "uid": uid, "role": role});
    if device_token.is_some() {
        resp["device_token"] = serde_json::Value::Bool(true); // 仅告知前端已设置 cookie
    }

    // 构建响应，包含多个 Set-Cookie 头
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json");
    for c in &cookie_headers {
        builder = builder.header(header::SET_COOKIE, c.clone());
    }
    builder
        .body(resp.to_string().into())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// 显式登出端点。
/// 作废当前 token（system.db.sessions），同时下发 Set-Cookie 清浏览器 cookie。
/// 支持 revoke_device 选项，登出时同时撤销该用户全部 device_token。
async fn api_logout(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    headers: HeaderMap,
    Json(body): Json<LogoutRequest>,
) -> Response {
    if let Some(token) = extract_session_token(&headers) {
        state.auth.delete_session(&token);
    }
    // 可选撤销该用户全部 device_token。
    //   前端勾选"同时登出记住我设备"时传 revoke_device=true。
    //   撤销失败仅记录 warn，不阻塞登出（会话已删除，攻击者无法用旧 session）。
    if body.revoke_device.unwrap_or(false) {
        if let Err(e) = state.system_db.revoke_all_device_tokens(auth_user.uid) {
            tracing::warn!(
                "[AUTH] logout: revoke_all_device_tokens 失败 uid={}: {}",
                auth_user.uid,
                e
            );
        }
    }
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", auth_user.uid),
        "logout",
        Some(&format!("uid={}", auth_user.uid)),
        Some(&format!(
            "{{\"revoke_device\":{}}}",
            body.revoke_device.unwrap_or(false)
        )),
    );
    // 始终清当前 session；仅当用户明确要求撤销“记住我”时清设备 Cookie。
    let secure = is_https_request(&headers);
    let clear_session = format!(
        "session_token=; Max-Age=0; Path=/; SameSite=Strict; HttpOnly{}",
        if secure { "; Secure" } else { "" }
    );
    let clear_device = format!(
        "device_token=; Max-Age=0; Path=/; SameSite=Strict; HttpOnly{}",
        if secure { "; Secure" } else { "" }
    );
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json");
    if let Ok(v) = HeaderValue::from_str(&clear_session) {
        builder = builder.header(header::SET_COOKIE, v);
    }
    if body.revoke_device.unwrap_or(false) {
        if let Ok(v) = HeaderValue::from_str(&clear_device) {
            builder = builder.header(header::SET_COOKIE, v);
        }
    }
    builder
        .body(serde_json::json!({"success": true}).to_string().into())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Phase 1 多用户：修改密码（替代旧版 api_set_password）。
///   - 必须已登录（require_session 注入 AuthUser）
///   - 旧前端发 `{ password, setup_key }` 时：new_password 取自 password 字段，old_password 缺失 → 报错
///   - 新前端发 `{ old_password, new_password }` 时：正常走 change_password 流程
async fn api_set_password(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthUser>,
    Json(body): Json<SetPasswordRequest>,
) -> Response {
    // S2: 改密限速（5 次/300s，按 uid 维度）
    let rl_key = format!("{}|change-password", auth_user.uid);
    if state.bot.check_rate_limit(&rl_key, 5, 300.0) {
        return rate_limited_response("change-password");
    }
    // 兼容旧前端：new_password 缺失时回退到 password 字段
    let new_password = match body.new_password.as_deref().or(body.password.as_deref()) {
        Some(p) if !p.is_empty() => p,
        _ => {
            return Json(serde_json::json!({"success": false, "error": "新密码不能为空"}))
                .into_response();
        }
    };
    let old_password = body.old_password.as_deref().unwrap_or("");

    match state
        .auth
        .change_password(auth_user.uid, old_password, new_password)
    {
        Ok(()) => {
            // 改密成功：清除限速记录
            state.bot.clear_rate_limit(&rl_key);

            // 审计日志写入失败时 warn 告警，不阻断业务。
            //   改密已成功（DB 已更新），审计失败仅告警——回滚密码代价过高。
            audit_log(
                &state.system_db,
                &format!("uid={}", auth_user.uid),
                "password.change",
                Some(&format!("uid={}", auth_user.uid)),
                Some("{\"all_sessions_revoked\":true}"),
            );
            // 提示前端当前 session 已失效，需要用新密码重新登录
            Json(serde_json::json!({
                "success": true,
                "message": "密码已修改，所有会话已失效，请使用新密码重新登录",
                "require_relogin": true
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({"success": false, "error": e})).into_response(),
    }
}

async fn api_refresh_session(
    State(state): State<Arc<AppState>>,
    Extension(_client_ip): Extension<ClientIp>,
    headers: HeaderMap,
) -> Response {
    // Phase 1 多用户：必须携带有效 session token 才能续期，不再支持无 token 颁发。
    if let Some(token) = extract_session_token(&headers) {
        if let Some(info) = state.auth.verify_session(&token) {
            state.auth.renew_session(&token);
            // 续期 DB 的同时设置 Set-Cookie，刷新浏览器 Cookie 的 Max-Age 倒计时，
            //   否则 30 天后 Cookie 过期、浏览器不再发送，所有请求 401。
            let secure = is_https_request(&headers);
            let cookie_str = format!(
                "session_token={}; Max-Age=2592000; Path=/; SameSite=Strict; HttpOnly{}",
                token,
                if secure { "; Secure" } else { "" }
            );
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json");
            if let Ok(v) = HeaderValue::from_str(&cookie_str) {
                builder = builder.header(header::SET_COOKIE, v);
            }
            // 响应体不再返回 session_token，仅通过 Set-Cookie 下发。
            // 失败分支也返回 JSON，统一错误响应格式。
            return match builder.body(
                serde_json::to_string(&serde_json::json!({
                    "success": true,
                    "uid": info.uid,
                    "role": info.role,
                }))
                .unwrap_or_default()
                .into(),
            ) {
                Ok(resp) => resp,
                Err(_) => Json(serde_json::json!({
                    "success": true,
                    "uid": info.uid,
                    "role": info.role,
                }))
                .into_response(),
            };
        }
    }
    // 统一 JSON 错误响应，避免与其他端点格式不一致
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "success": false,
            "error": "unauthorized",
            "message": "需要登录"
        })),
    )
        .into_response()
}

// ── 鉴权路由处理 ────────────────────────────────────────────

async fn api_stats(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as f64;
    let uptime = now - state.boot_time;
    let req_count = state
        .request_count
        .load(std::sync::atomic::Ordering::Relaxed);
    Json(serde_json::json!({
        "uptime": uptime,
        "requests": req_count,
        "subscribers": bot.broker.subscriber_count(),
        "dropped": bot.broker.no_subscriber_count(),
        "users": bot.list_users().len(),
        "messages": bot.messages.read().len(),
    }))
}

async fn api_status(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let qr_state = bot.get_qr_login_state();
    let health = bot.poll_health.read().clone();
    let login_done = bot.login_done.load(std::sync::atomic::Ordering::Relaxed);
    let logged_in = login_done && bot.token.read().is_some();
    let has_token = bot.token.read().is_some();
    let session_state = bot.session_status.read().as_str().to_string();
    let bot_accounts: Vec<serde_json::Value> = bot.bot_accounts.read().values().map(|info| {
            let obj = info.clone();
            // 脱敏：不返回完整 token，只返回 bot_id + user_id
            serde_json::json!({
                "bot_id": obj.get("bot_id").and_then(|v| v.as_str()).unwrap_or(""),
                "user_id": obj.get("user_id").and_then(|v| v.as_str()).unwrap_or(""),
                "is_primary": obj.get("bot_id").and_then(|v| v.as_str()) == bot.bot_id.read().as_deref(),
            })
        })
        .collect();
    Json(serde_json::json!({
        "logged_in": logged_in,
        "login_done": login_done,
        "has_token": has_token,
        "session_state": session_state,
        "bot_accounts": bot_accounts,
        "users": bot.list_users(),
        "current_user": bot.get_current_user(),
        "qr_state": qr_state,
        "poll_health": health,
        "subscribers": bot.broker.subscriber_count(),
        "webdav_enabled": bot.is_webdav_enabled(),
        "traffic_saver": bot.is_traffic_saver_enabled(),
    }))
}

/// 详细的会话状态（含 SessionState 终态 + expired_users）
/// 前端用此端点判断是否显示"会话过期"banner 和"重新扫码"按钮。
async fn api_session_status(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    use crate::models::SessionState;
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let session_state = *bot.session_status.read();
    let bot_id = bot.bot_id.read().clone();
    let user_id = bot.user_id.read().clone();
    let login_done = bot.login_done.load(std::sync::atomic::Ordering::Relaxed);
    let logged_in = login_done && bot.token.read().is_some();
    let expired_users: Vec<String> = bot
        .list_users()
        .into_iter()
        .filter(|u| {
            bot.context_tokens
                .read()
                .get(u)
                .map(|s| s.is_empty())
                .unwrap_or(true)
        })
        .collect();

    let reauth_available = session_state.is_terminal() || session_state == SessionState::Reauthing;

    Json(serde_json::json!({
        "session_state": session_state.as_str(),
        "is_terminal": session_state.is_terminal(),
        "logged_in": logged_in,
        "bot_id": bot_id,
        "user_id": user_id,
        "expired_users": expired_users,
        "reauth_available": reauth_available,
        "message": match session_state {
            SessionState::Active => "iLink 会话正常",
            SessionState::SessionExpired => "iLink 会话已过期，请点击重新扫码",
            SessionState::Reauthing => "正在等待重新扫码...",
            SessionState::Disconnected => "未登录",
        },
    }))
}

/// 触发重新扫码（保留主账号，刷新 token）
async fn api_reauth_start(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let req_id = tokio::task::spawn_blocking(move || bot.start_reauth_qrcode())
        .await
        .unwrap_or_default();
    Json(serde_json::json!({
        "ok": true,
        "success": true,
        "req_id": req_id,
        "message": "请使用微信扫描二维码以重新绑定",
    }))
}

/// 列出未完结的出站消息（前端 F5 刷新时恢复显示）
async fn api_outbound_pending(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Query(q): Query<OutboundPendingQuery>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let user_id = q.user.unwrap_or_default();
    if user_id.is_empty() {
        return Json(serde_json::json!({"success": false, "error": "user 不能为空"}));
    }
    let _ = q.limit; // 当前 storage 层 list_pending_outbound 暂未实现 limit，保留字段供未来扩展
    let rows = bot.db.list_pending_outbound(&user_id);
    Json(serde_json::json!({
        "success": true,
        "messages": rows.iter().map(|r| {
            serde_json::json!({
                "row_id": r.id,
                "req_id": r.trace_id,  // 兼容：req_id 存于 trace_id
                "client_id": r.client_id,
                "to_user_id": r.user_id,
                "text": r.text.clone().unwrap_or_default(),
                "send_state": r.send_state,
                "send_attempts": r.send_attempts,
                "created_at_ms": r.created_at_ms,
            })
        }).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct OutboundPendingQuery {
    user: Option<String>,
    limit: Option<usize>,
}

/// 手动重试某条出站消息
async fn api_outbound_resend(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<OutboundResendRequest>,
) -> Response {
    // ponytail HIGH-3: 触发实际微信发送，必须与 api_send 共享限速（100/60s），防绕过
    let rl_key = format!("{}|send-message", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 100, 60.0) {
        return rate_limited_response("send-message");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    if body.row_id <= 0 {
        return Json(serde_json::json!({"success": false, "error": "row_id 无效"})).into_response();
    }
    // S9: 横向越权校验——该消息的 to_user_id 必须等于当前用户
    let row_id = body.row_id;
    let current = bot.get_current_user();
    match bot.db.get_message_v2(row_id) {
        Some(m) => {
            let to_user = m.to_user_id.unwrap_or_default();
            if current.as_deref() != Some(to_user.as_str()) {
                return Json(serde_json::json!({"success": false, "error": "无权操作此消息"}))
                    .into_response();
            }
        }
        None => {
            return Json(serde_json::json!({"success": false, "error": "消息不存在"}))
                .into_response()
        }
    }
    let bot = bot.clone();
    let result = tokio::task::spawn_blocking(move || bot.resend_outbound_async(row_id)).await;
    match result {
        Ok(Some((client_id, req_id))) => Json(serde_json::json!({
            "ok": true, "success": true,
            "client_id": client_id, "req_id": req_id, "row_id": row_id,
            "state": "pending",
        })).into_response(),
        Ok(None) => Json(serde_json::json!({"ok": false, "success": false, "error": "重试失败：消息不存在或会话已过期"})).into_response(),
        // 不向客户端泄露内部错误细节，仅记录日志。
        Err(e) => {
            tracing::error!("[WEB] outbound-resend spawn_blocking 异常 row_id={} error={}", row_id, e);
            Json(serde_json::json!({"ok": false, "success": false, "error": "内部错误"})).into_response()
        }
    }
}

#[derive(Deserialize)]
struct OutboundResendRequest {
    row_id: i64,
}

async fn api_qrcode(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    if !bot.login_done.load(std::sync::atomic::Ordering::Relaxed) {
        bot.start_login_async();
    }
    let qr_state = bot.get_qr_login_state();
    let login_done = qr_state.login_done;
    let redirect_to_chat = login_done && bot.token.read().is_some();
    Json(serde_json::json!({
        "state": qr_state.state,
        "message": qr_state.message,
        "has_qrcode": qr_state.has_qrcode,
        "matrix": qr_state.matrix,
        "qrcode_key": qr_state.qrcode_key,
        "login_done": login_done,
        "redirect_to_chat": redirect_to_chat,
    }))
}

async fn api_messages(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Query(query): Query<MessagesQuery>,
) -> Response {
    // 按 uid 限速 60 次/60s，防止高频拉取造成 CPU/内存压力
    let rl_key = format!("{}|messages", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 60, 60.0) {
        return rate_limited_response("messages");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await { Ok(b) => b, Err(_) => return Json(serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"})).into_response() };
    let filtered = bot.db.query_messages(
        query.since,
        query.user.as_deref(),
        query.limit.unwrap_or(100),
    );
    Json(serde_json::json!({"messages": filtered})).into_response()
}

async fn api_users(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    Json(serde_json::json!({
        "users": bot.list_users(),
        "current_user": bot.get_current_user(),
    }))
}

async fn api_chat_previews(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let users = bot.list_users();
    let previews = bot.db.query_latest_message_per_user(&users);
    Json(serde_json::json!({"previews": previews}))
}

async fn api_history(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    // 按 uid 限速 60 次/60s，防止高频拉取造成 CPU/内存压力
    let rl_key = format!("{}|history", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 60, 60.0) {
        return rate_limited_response("history");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await { Ok(b) => b, Err(_) => return Json(serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"})).into_response() };
    let user = query.user.as_deref().filter(|s| !s.is_empty());
    // 限制单页上限 200，默认 50；超 200 自动截断。
    let limit = query.limit.unwrap_or(50).min(200);
    let before = query.before.filter(|v| *v > 0);
    // 多查 1 条用于判断是否还有更早消息（has_more），返回时剔除
    let probe_limit = limit + 1;
    let mut msgs = bot.db.query_history_messages(user, probe_limit, before);
    let has_more = msgs.len() > limit;
    if has_more {
        msgs.truncate(limit);
    }

    // 合并 messages_v2 的最新 send_state：spawn_retry_send 只更新 messages_v2
    // 用 client_id 关联 messages_v2 的最新状态覆盖 messages 表
    let client_ids: Vec<String> = msgs
        .iter()
        .filter(|m| m.get("type").and_then(|v| v.as_str()) == Some("out"))
        .filter_map(|m| {
            m.get("client_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    if !client_ids.is_empty() {
        let states = bot.db.get_outbound_states_by_client_ids(&client_ids);
        for msg in msgs.iter_mut() {
            if let Some(cid) = msg.get("client_id").and_then(|v| v.as_str()) {
                if let Some(ss) = states.get(cid) {
                    if let Some(obj) = msg.as_object_mut() {
                        obj.insert(
                            "send_state".to_string(),
                            serde_json::Value::String(ss.clone()),
                        );
                    }
                }
            }
        }
    }

    Json(serde_json::json!({"messages": msgs, "has_more": has_more})).into_response()
}

async fn api_about() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": SCRIPT_VERSION,
        "author": "ZynSync",
    }))
}

/// Phase 3 (P3): 聚合返回当前登录用户的配额/用量/功能/存储目标。
/// 前端 30s 轮询此端点渲染用量条（U3）与功能开关，配额预检也用此数据（C3）。
///
/// 响应结构（§7.2 + §7.3）：
///   { uid, username, role, quota:{5 维}, used:{5 维}, features:{3 开关}, storage_target }
///
/// quota==0 表示"用系统默认/不限"，前端按 0 渲染为"不限"。
/// storage_target 由 webdav_enabled 派生：启用 webdav → "own_webdav"，否则 "server"。
async fn api_me(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let user = state.system_db.get_user_by_id(auth.uid);
    let (username, role) = match &user {
        Some(u) => (u.username.clone(), u.role.clone()),
        None => (String::new(), String::new()),
    };
    // 用量：优先取内存计数器（权威读），回退 DB 值
    let counters = state.bot.get_or_create_quota(auth.uid);
    let used_json = serde_json::json!({
        "upload_bytes": counters.get(QuotaDim::UploadBytes),
        "download_bytes": counters.get(QuotaDim::DownloadBytes),
        "media_bytes": counters.get(QuotaDim::MediaBytes),
        "msg_per_day": counters.get(QuotaDim::MsgToday),
        "media_count": counters.get(QuotaDim::MediaCount),
    });
    // 配额：用户字段 > 0 用用户值，否则回退系统默认（与 effective_quota 一致）
    let q = |dim: QuotaDim| -> u64 {
        match &user {
            Some(u) => {
                let uq = dim.appuser_quota(u);
                if uq > 0 {
                    return uq as u64;
                }
                state
                    .system_db
                    .get_setting(dim.system_default_key())
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(|v| v.max(0) as u64)
                    .unwrap_or(0)
            }
            None => 0,
        }
    };
    let quota_json = serde_json::json!({
        "upload_bytes": q(QuotaDim::UploadBytes),
        "download_bytes": q(QuotaDim::DownloadBytes),
        "media_bytes": q(QuotaDim::MediaBytes),
        "msg_per_day": q(QuotaDim::MsgToday),
        "media_count": q(QuotaDim::MediaCount),
    });
    // 功能开关：系统默认 ∧ 用户级
    let features_json = serde_json::json!({
        "upload": state.bot.check_feature(auth.uid, Feature::Upload),
        "webdav": state.bot.check_feature(auth.uid, Feature::Webdav),
        "custom_webdav": state.bot.check_feature(auth.uid, Feature::CustomWebdav),
    });
    // 存储目标：webdav 启用 → own_webdav，否则 server
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let storage_target = if bot.is_webdav_enabled() {
        "own_webdav"
    } else {
        "server"
    };
    Json(serde_json::json!({
        "uid": auth.uid,
        "username": username,
        "role": role,
        "quota": quota_json,
        "used": used_json,
        "features": features_json,
        "storage_target": storage_target,
    }))
}

async fn api_add_user_status(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let mut result = bot.get_add_user_status();
    // 确保响应包含 success 字段（与文档一致）
    if let Some(obj) = result.as_object_mut() {
        obj.insert("success".to_string(), serde_json::Value::Bool(true));
    } else {
        result = serde_json::json!({"success": true});
    }
    Json(result)
}

async fn api_add_user_start(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // 检查是否已有进行中的添加操作
    {
        let pending = bot.pending_qrcode.read();
        if let Some(ref pq) = *pending {
            let status = pq.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "waiting" || status == "scaned" || status == "generating" {
                return Json(serde_json::json!({"status": "already_running"}));
            }
        }
    }
    let key = bot.start_add_user_qrcode();
    // 立即返回，前端会通过 add-user-status 轮询获取 matrix
    Json(serde_json::json!({"success": true, "status": "started", "key": key}))
}

async fn api_media(
    Extension(auth): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Path(cache_key): Path<String>,
    Query(query): Query<MediaQuery>,
    _headers: HeaderMap,
) -> Response {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(bot) => bot,
        Err(response) => return response,
    };
    // 授权以当前认证用户的持久化 user.db 为准；不扫描其他已加载 Bot，也不依赖
    // 可丢失的内存反向索引。
    if !bot.db.owns_media(&cache_key) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if bot.is_traffic_saver_enabled() && query.force.is_none() {
        return (
            StatusCode::FORBIDDEN,
            [(
                header::HeaderName::from_static("x-traffic-saver"),
                header::HeaderValue::from_static("1"),
            )],
            "Traffic saver enabled",
        )
            .into_response();
    }

    // get_cached_media 在 WebDAV 启用时调用 WebDavClient.download()（reqwest::blocking），
    //   用 spawn_blocking 把阻塞调用移到独立线程，防止 runtime panic。
    let data = tokio::task::spawn_blocking(move || bot.get_cached_media(&cache_key))
        .await
        .ok()
        .flatten();
    if let Some(data) = data {
        let mime = media::detect_mime(&data);
        let content_type = match header::HeaderValue::from_str(mime) {
            Ok(v) => v,
            Err(_) => header::HeaderValue::from_static("application/octet-stream"),
        };
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("private, no-store"),
                ),
            ],
            data,
        )
            .into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

// ── CDN Presign 直传 ─────────────────────────────────────
#[derive(Deserialize)]
struct PresignRequest {
    #[serde(default)]
    media_type: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    file_size: usize,
    // 客户端必须随请求提交真实 MD5，替代原占位空字符串 MD5。
    //   32 位小写 hex；非法格式直接 400 拒绝。
    #[serde(default)]
    file_md5: String,
}

async fn api_media_presign(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<PresignRequest>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // presign_media_upload 内部调用 self.post()（reqwest::blocking），用 spawn_blocking 包裹。
    // 前置校验 file_size，超限直接拒绝，避免无谓的预签名请求。
    if body.file_size > MAX_UPLOAD_SIZE {
        return Json(serde_json::json!({
            "ok": false,
            "error": format!("文件过大（{} 字节），最大允许 {} 字节", body.file_size, MAX_UPLOAD_SIZE)
        }));
    }
    // 校验 file_md5 格式（32 位小写 hex），拒绝占位/空 MD5，防止 CDN 完整性校验失效。
    let md5_trimmed = body.file_md5.trim();
    if md5_trimmed.len() != 32 || !md5_trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Json(serde_json::json!({
            "ok": false,
            "error": "file_md5 必须为 32 位十六进制 MD5 哈希"
        }));
    }
    let bot = bot.clone();
    let media_type = body.media_type.clone();
    let file_name = body.file_name.clone();
    let file_size = body.file_size;
    let file_md5 = md5_trimmed.to_string();
    let result = tokio::task::spawn_blocking(move || {
        bot.presign_media_upload(&media_type, &file_name, file_size, &file_md5)
    })
    .await
    .ok()
    .flatten();
    match result {
        Some(r) => Json(r),
        None => Json(serde_json::json!({"ok": false, "error": "获取预签名 URL 失败"})),
    }
}

async fn api_media_stream(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    body: Bytes,
) -> Response {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let cdn_info: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let filename = cdn_info
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("media")
        .to_string();
    // stream_media_from_cdn 使用 cdn_client（reqwest::blocking），用 spawn_blocking 包裹。
    let bot = bot.clone();
    let result =
        tokio::task::spawn_blocking(move || bot.stream_media_from_cdn(&cdn_info, &filename))
            .await
            .ok()
            .flatten();
    match result {
        Some((data, mime, _)) => {
            // Phase 3 (§7.2): 下载配额校验——download_bytes → 413
            let dl_size = data.len() as i64;
            if let Err(e) = state
                .bot
                .reserve_quota(auth.uid, &[(QuotaDim::DownloadBytes, dl_size)])
            {
                return quota_exceeded_response(e, StatusCode::PAYLOAD_TOO_LARGE);
            }
            let content_type = match header::HeaderValue::from_str(&mime) {
                Ok(v) => v,
                Err(_) => header::HeaderValue::from_static("application/octet-stream"),
            };
            (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// WebSocket 升级端点（替代 SSE）
///
/// 显式校验 Origin，防止 Cross-Site WebSocket Hijacking (CSWSH)。
///   WS 升级是 GET，中间件跳过 GET 的 Origin 校验，此处独立验证。
/// 仅支持 Cookie + Sec-WebSocket-Protocol 鉴权，不再接受 URL query token。
async fn api_ws_upgrade(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // WS Origin 校验：loopback 直连放行（开发场景），其余必须通过 Origin 校验。
    // ClientIp 由 set_client_ip 中间件解析，不读取 x-real-ip 头（防伪造）。
    let client_ip_loopback = client_ip.0.is_loopback();
    // trusted_proxies 仅用于解析真实客户端 IP，与是否放行无关。
    let is_direct_loopback = client_ip_loopback;
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !is_direct_loopback && !is_origin_allowed(origin, state.bot.web_port()) {
            tracing::warn!("[WS] Origin 校验失败 origin={}", origin);
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "success": false,
                    "error": "forbidden",
                    "message": "WebSocket Origin 校验失败"
                })),
            )
                .into_response();
        }
    } else if !is_direct_loopback {
        // 非 loopback 且无 Origin 头：拒绝（浏览器 WS 升级必带 Origin）
        tracing::warn!("[WS] 非 loopback 请求缺少 Origin 头，拒绝升级");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false,
                "error": "forbidden",
                "message": "缺少 Origin 头"
            })),
        )
            .into_response();
    }

    // token 提取 — Cookie > Sec-WebSocket-Protocol
    let (token, token_source) = if let Some(tok) = extract_session_token(&headers) {
        (tok, "cookie")
    } else if let Some(proto) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
    {
        // Sec-WebSocket-Protocol 可能是逗号分隔列表，取第一个非空 token
        let tok = proto
            .split(',')
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .unwrap_or("");
        if tok.is_empty() {
            (String::new(), "missing")
        } else {
            (tok.to_string(), "sec-websocket-protocol")
        }
    } else {
        (String::new(), "missing")
    };

    if token.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "success": false,
                "error": "unauthorized",
                "message": "缺少认证凭据（Cookie / Sec-WebSocket-Protocol 均未提供）"
            })),
        )
            .into_response();
    }

    let info = match state.auth.verify_session(&token) {
        Some(i) => i,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": "unauthorized",
                    "message": "token 无效或已过期"
                })),
            )
                .into_response();
        }
    };
    // Phase 5 (S8): 访问日志脱敏——只记 uid，绝不记录 token 明文
    tracing::info!(
        "[WS] 连接建立 uid={} role={} auth_via={}",
        info.uid,
        info.role,
        token_source
    );
    state.auth.renew_session(&token);
    // Phase 2: 按 info.uid 取该用户的 bot + hub（每用户独立 broker，WS 隔离）
    let bot = match get_bot_or_503(&state, info.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let hub = state.bot.get_or_create_hub(info.uid, &bot);
    ws.on_upgrade(move |socket| crate::push::handle_ws(socket, hub, bot))
}

async fn api_send(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<SendTextRequest>,
) -> Response {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // 输入验证
    if let Err(e) = validate_message_text(&body.text) {
        tracing::warn!("[WEB] api_send 输入验证失败: {}", e);
        return Json(serde_json::json!({"success": false, "error": e})).into_response();
    }

    // Phase 3 (S2): 发消息限速（100 次/60s，按 uid 维度）
    let rl_key = format!("{}|send-message", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 100, 60.0) {
        return rate_limited_response("send-message");
    }

    let current = bot.get_current_user();
    let user = match current {
        Some(u) => u,
        None => {
            tracing::warn!("[WEB] api_send 无当前用户（current_user 为空）");
            // 错误消息改为可操作提示。
            return Json(serde_json::json!({
                "success": false,
                "error": "请先在左侧联系人列表中选择一个对话后再发送消息"
            }))
            .into_response();
        }
    };
    if let Err(e) = state
        .bot
        .reserve_quota(auth.uid, &[(QuotaDim::MsgToday, 1)])
    {
        return quota_exceeded_response(e, StatusCode::TOO_MANY_REQUESTS);
    }

    tracing::info!(
        "[WEB] api_send 收到发送请求 user={} text_len={}",
        user,
        body.text.len()
    );

    let bot = bot.clone();
    let user_clone = user.clone();
    let text_clone = body.text.clone();
    let req_id_clone = body.req_id.clone();

    // 改为同步发送（对齐 Python 原版）
    //   HTTP 响应即最终结果（成功/失败），不依赖 ACK 状态机
    //   - 成功后后端推 "message" 事件，前端立即渲染
    //   - 失败直接返回 error，前端显示"发送失败"
    //   - 不再启动后台重试线程，避免 1 秒内推 expired/failed ACK 导致感叹号误判
    let result = tokio::task::spawn_blocking(move || {
        bot.send_text_sync_web(&user_clone, &text_clone, req_id_clone.as_deref())
    })
    .await;

    match result {
        Ok(Ok(msg)) => {
            tracing::info!("[WEB] api_send 同步发送成功 user={}", user);
            // 返回格式与 Python 原版一致：{ success: true, message: {...} }
            // 不返回 row_id，前端走旧版同步路径（立即渲染，无 pending 状态机）
            Json(serde_json::json!({
                "ok": true,
                "success": true,
                "message": msg,
            }))
            .into_response()
        }
        Ok(Err(e)) => {
            state
                .bot
                .release_quota(auth.uid, &[(QuotaDim::MsgToday, 1)]);
            tracing::warn!("[WEB] api_send 同步发送失败 user={} error={}", user, e);
            let is_expired = e == "session_expired";
            Json(serde_json::json!({
                "ok": false,
                "success": false,
                "error": e,
                "session_expired": is_expired,
            }))
            .into_response()
        }
        Err(e) => {
            state
                .bot
                .release_quota(auth.uid, &[(QuotaDim::MsgToday, 1)]);
            tracing::error!(
                "[WEB] api_send spawn_blocking 异常 user={} error={}",
                user,
                e
            );
            // 不向客户端泄露内部错误细节
            Json(serde_json::json!({
                "ok": false,
                "success": false,
                "error": "内部错误",
            }))
            .into_response()
        }
    }
}

#[derive(Deserialize)]
struct TypingRequest {
    to: String,
    action: String, // "start" | "stop"
}

async fn api_typing(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<TypingRequest>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // send_typing_indicator 内部调用 self.post()（reqwest::blocking），用 spawn_blocking 包裹。
    let bot = bot.clone();
    let to = body.to.clone();
    let action = body.action.clone();
    let ok = tokio::task::spawn_blocking(move || bot.send_typing_indicator(&to, &action))
        .await
        .unwrap_or(false);
    Json(serde_json::json!({"ok": ok}))
}

async fn send_media_inner(
    bot: SharedBot,
    user: String,
    file_bytes: Vec<u8>,
    filename: String,
    media_type: String,
    description: String,
) -> bool {
    match tokio::task::spawn_blocking(move || match media_type.as_str() {
        "image" => bot.send_image(&user, &file_bytes, &filename, &description),
        "video" => bot.send_video(&user, &file_bytes, &filename, 0),
        "file" => bot.send_file(&user, &file_bytes, &filename, &description),
        "voice" => bot.send_voice(&user, &file_bytes, &filename, 0),
        _ => false,
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("[WEB] send_media spawn_blocking panic: {}", e);
            false
        }
    }
}

async fn api_send_media(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // Phase 3 (§7.3): 功能开关——管理员禁用 Upload 时拒绝
    if !state.bot.check_feature(auth.uid, Feature::Upload) {
        return feature_disabled_response("upload");
    }
    // ponytail HIGH-2: 与 api_upload_media 一致的限速（30 次/60s），防 send-media 绕过 upload-media 限速
    let rl_key = format!("{}|upload-media", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 30, 60.0) {
        return rate_limited_response("upload-media");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let media_type = body
        .get("media_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let filename = body
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("file")
        .to_string();
    let file_data_b64 = body
        .get("file_data")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let file_bytes = match base64::engine::general_purpose::STANDARD.decode(&file_data_b64) {
        Ok(b) => b,
        Err(_) => {
            return Json(serde_json::json!({"success": false, "error": "base64 解码失败"}))
                .into_response()
        }
    };

    // 与 api_upload_media 一致，base64 解码后校验文件大小与类型。
    if let Err(e) = validate_upload_file(&file_bytes, &media_type) {
        return Json(serde_json::json!({"success": false, "error": e})).into_response();
    }
    // 文件名清洗，防目录穿越与控制字符注入。
    let filename = sanitize_filename(&filename);

    let size = file_bytes.len() as i64;
    let current = bot.get_current_user();
    let user = match current {
        Some(u) => u,
        // 与 api_send 文本端点统一，改为可操作提示。
        None => return Json(serde_json::json!({"success": false, "error": "请先在左侧联系人列表中选择一个对话后再发送消息"})).into_response(),
    };
    let media_reservation = [
        (QuotaDim::UploadBytes, size),
        (QuotaDim::MediaCount, 1),
        (QuotaDim::MediaBytes, size),
    ];
    if let Err(e) = state.bot.reserve_quota(auth.uid, &media_reservation) {
        return quota_exceeded_response(e, StatusCode::PAYLOAD_TOO_LARGE);
    }

    let ok = send_media_inner(
        bot.clone(),
        user,
        file_bytes,
        filename,
        media_type,
        description,
    )
    .await;

    if !ok {
        state.bot.release_quota(auth.uid, &media_reservation);
    }
    Json(serde_json::json!({"success": ok})).into_response()
}

async fn api_upload_media(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    mut multipart: Multipart,
) -> Response {
    // Phase 3 (§7.3): 功能开关——管理员禁用 Upload 时拒绝
    if !state.bot.check_feature(auth.uid, Feature::Upload) {
        return feature_disabled_response("upload");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await { Ok(b) => b, Err(_) => return Json(serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"})).into_response() };
    // Phase 3 (S2): 上传限速（30 次/60s，按 uid 维度）
    let rl_key = format!("{}|upload-media", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 30, 60.0) {
        return rate_limited_response("upload-media");
    }
    // 全局上传并发限制，防止 N×50MB 并发上传 OOM。
    //   _permit 在函数返回前一直持有，限制最多 4 个上传同时处理。
    let _upload_permit = match state.upload_sem.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => {
            return Json(serde_json::json!({"success": false, "error": "系统繁忙，请稍后重试"}))
                .into_response()
        }
    };
    let mut media_type = "file".to_string();
    let mut filename = "file".to_string();
    let mut file_bytes = Vec::new();

    while let Ok(Some(mut field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "media_type" => {
                media_type = field.text().await.unwrap_or_default();
            }
            "filename" => {
                filename = field.text().await.unwrap_or_default();
            }
            "file" => {
                // 流式读取文件字段，边读边检查大小，超过 MAX_UPLOAD_SIZE 立即中止。
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            file_bytes.extend_from_slice(&chunk);
                            if file_bytes.len() > MAX_UPLOAD_SIZE {
                                return Json(serde_json::json!({
                                    "success": false,
                                    "error": format!("文件过大，最大允许 {} 字节", MAX_UPLOAD_SIZE)
                                }))
                                .into_response();
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            }
            _ => {}
        }
    }

    // 输入验证
    if let Err(e) = validate_upload_file(&file_bytes, &media_type) {
        return Json(serde_json::json!({"success": false, "error": e})).into_response();
    }
    // 文件名清洗，防目录穿越与控制字符注入。
    let filename = sanitize_filename(&filename);

    let size = file_bytes.len() as i64;
    let current = bot.get_current_user();
    let user = match current {
        Some(u) => u,
        // 与 api_send 文本端点统一，改为可操作提示。
        None => return Json(serde_json::json!({"success": false, "error": "请先在左侧联系人列表中选择一个对话后再发送消息"})).into_response(),
    };
    let media_reservation = [
        (QuotaDim::UploadBytes, size),
        (QuotaDim::MediaCount, 1),
        (QuotaDim::MediaBytes, size),
    ];
    if let Err(e) = state.bot.reserve_quota(auth.uid, &media_reservation) {
        return quota_exceeded_response(e, StatusCode::PAYLOAD_TOO_LARGE);
    }

    let bot_for_result = bot.clone();
    let ok = send_media_inner(
        bot.clone(),
        user.clone(),
        file_bytes,
        filename,
        media_type,
        String::new(),
    )
    .await;

    // 如果成功,返回消息对象给前端
    if ok {
        let last_msg = bot_for_result.get_last_out_message(&user);
        Json(serde_json::json!({
            "success": true,
            "message": last_msg
        }))
        .into_response()
    } else {
        state.bot.release_quota(auth.uid, &media_reservation);
        Json(serde_json::json!({"success": false, "error": "发送失败"})).into_response()
    }
}

async fn api_download_media(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let cdn_info = body.get("cdn_info").cloned().unwrap_or_default();
    let filename = body
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("media");
    let user_id = bot.get_current_user().unwrap_or_default();

    let bot = bot.clone();
    let filename = filename.to_string();
    let user_id = user_id.clone();
    // 提前计算 cache_key，避免 spawn_blocking move 后再借用。
    let cache_key = bot.media_cache_key_public(&cdn_info);
    match tokio::task::spawn_blocking(move || bot.download_media(&cdn_info, &filename, &user_id))
        .await
    {
        Ok(Some(data)) => {
            // Phase 3 (§7.2): 下载配额校验——download_bytes → 413
            //   下载已完成才知大小，超限则不投递数据（已耗带宽，但配额护栏优先）
            let dl_size = data.len() as i64;
            if let Err(e) = state
                .bot
                .reserve_quota(auth.uid, &[(QuotaDim::DownloadBytes, dl_size)])
            {
                return quota_exceeded_response(e, StatusCode::PAYLOAD_TOO_LARGE);
            }
            let mime = crate::media::detect_mime(&data);
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            Json(serde_json::json!({
                "success": true,
                "cache_key": cache_key,
                "data": b64,
                "mime": mime,
            }))
            .into_response()
        }
        _ => Json(serde_json::json!({
            "success": false,
            "error": "download_failed",
        }))
        .into_response(),
    }
}

async fn api_switch_user(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<SwitchUserRequest>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // 输入验证
    if let Err(e) = validate_user_id(&body.user_id) {
        return Json(serde_json::json!({"success": false, "error": e}));
    }
    // 验证 user_id 是否存在
    let users = bot.list_users();
    if !users.contains(&body.user_id) {
        return Json(
            serde_json::json!({"success": false, "error": format!("用户 {} 不存在", body.user_id)}),
        );
    }

    bot.set_current_user(&body.user_id);
    Json(serde_json::json!({"success": true}))
}

async fn api_delete_user(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<DeleteUserRequest>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // S9: 横向越权校验——仅允许操作当前用户
    let current = bot.get_current_user();
    if current.as_deref() != Some(body.user_id.as_str()) {
        return Json(serde_json::json!({"success": false, "error": "无权操作此用户"}));
    }
    let ok = bot.remove_user(&body.user_id);
    bot.broker.publish(
        "user",
        serde_json::json!({
            "users": bot.list_users(),
            "current_user": bot.get_current_user(),
        }),
    );
    Json(serde_json::json!({"success": ok}))
}

async fn api_batch_delete(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let user_ids: Vec<String> = body
        .get("user_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // S9: 横向越权校验——所有 user_id 必须等于当前用户（单用户模式下仅一个合法 user_id）
    let current = bot.get_current_user();
    for uid in &user_ids {
        if current.as_deref() != Some(uid.as_str()) {
            return Json(serde_json::json!({"success": false, "error": "无权操作此用户"}));
        }
    }
    let mut deleted = 0;
    for uid in &user_ids {
        if bot.remove_user(uid) {
            deleted += 1;
        }
    }
    bot.broker.publish(
        "user",
        serde_json::json!({
            "users": bot.list_users(),
            "current_user": bot.get_current_user(),
        }),
    );
    Json(serde_json::json!({"success": true, "deleted": deleted}))
}

async fn api_clear_messages(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let user_id = body.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
    if !user_id.is_empty() {
        // S9: 横向越权校验——仅允许清除当前用户的消息
        let current = bot.get_current_user();
        if current.as_deref() != Some(user_id) {
            return Json(serde_json::json!({"success": false, "error": "无权操作此用户消息"}));
        }
        bot.db.delete_user_messages(user_id);
        let mut msgs = bot.messages.write();
        msgs.retain(|m| {
            m.get("from").and_then(|v| v.as_str()) != Some(user_id)
                && m.get("to").and_then(|v| v.as_str()) != Some(user_id)
        });
    }
    Json(serde_json::json!({"success": true}))
}

async fn api_delete_messages(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let ids: Vec<i64> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    // 输入验证
    if let Err(e) = validate_message_ids(&ids) {
        return Json(serde_json::json!({"success": false, "error": e}));
    }

    // 所有权校验：只能删除当前选中 peer 的消息（防 IDOR）。
    //   前端可传 user_id 显式指定；未传时使用 bot.current_user。
    //   若 user_id 与 bot.current_user 不一致，直接拒绝。
    let current_peer = bot.get_current_user().unwrap_or_default();
    let requested_user = body.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
    let scope_user = if requested_user.is_empty() {
        current_peer.clone()
    } else {
        requested_user.to_string()
    };
    if scope_user.is_empty() {
        return Json(serde_json::json!({
            "success": false,
            "error": "no_peer_selected",
            "message": "请先选择一个聊天会话再删除消息"
        }));
    }
    if !current_peer.is_empty() && scope_user != current_peer {
        return Json(serde_json::json!({
            "success": false,
            "error": "无权操作此用户消息"
        }));
    }

    let deleted = bot.delete_messages_by_ids(&ids, &scope_user);
    Json(serde_json::json!({"success": true, "deleted": deleted}))
}

async fn api_webdav_proxy(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Path(remote_path): Path<String>,
) -> Response {
    // 从 remote_path 提取 cache_key，并只在当前认证用户的数据库中校验所有权。
    let cache_key = {
        let segs: Vec<&str> = remote_path.split('/').filter(|s| !s.is_empty()).collect();
        let last = segs.last().copied().unwrap_or("");
        last.rsplit_once('.')
            .map(|(h, _)| h)
            .unwrap_or(last)
            .to_string()
    };
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(bot) => bot,
        Err(response) => return response,
    };
    if !bot.db.owns_media(&cache_key) {
        tracing::warn!(
            "[WEB] webdav-proxy 拒绝非本人媒体访问 auth_uid={} cache_key={}",
            auth.uid,
            cache_key
        );
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false,
                "error": "forbidden",
                "message": "无权访问此媒体资源"
            })),
        )
            .into_response();
    }
    let client = { bot.webdav_client.read().clone() };
    let Some(client) = client else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))],
            "WebDAV 未启用",
        )
            .into_response();
    };

    // 校验 remote_path 必须落在 base_path 之下，防止路径遍历。
    //   且路径段匹配 cache_key 布局（<2hex>/<32hex>），防止任意路径读取绕过。
    //   require_session 已恢复对本路径的 session 校验，路径白名单作为深度防御。
    let base_path = bot.webdav_config.read().base_path.clone();
    if !validate_webdav_proxy_path(&remote_path, &base_path) {
        tracing::warn!(
            "[WEB] webdav-proxy 拒绝越权路径 remote_path={} base_path={}",
            remote_path,
            base_path
        );
        // Phase 5 (Fix-4): 403 返回 JSON body（前端 _loadWebDavMedia 可解析 message toast）
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "success": false,
                "error": "forbidden",
                "message": "WebDAV 路径越权访问被拒绝"
            })),
        )
            .into_response();
    }

    let path = if remote_path.starts_with('/') {
        remote_path
    } else {
        format!("/{}", remote_path)
    };

    let sem = state.webdav_proxy_sem.clone();
    let permit = match sem.acquire_owned().await {
        Ok(p) => p,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let data = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // 先试请求路径本身
        if let Some(data) = client.download(&path) {
            return Some(data);
        }
        // 兼容回退：若路径带扩展名（如 .jpg），试无扩展名路径（旧命名约定）
        // 这是决策 5（媒体命名加后缀）的向后兼容逻辑
        if let Some(no_ext) = strip_last_extension(&path) {
            tracing::debug!("[WebDAV proxy] 回退尝试无扩展名路径: {} → {}", path, no_ext);
            return client.download(&no_ext);
        }
        None
    })
    .await
    .ok()
    .flatten();

    match data {
        Some(bytes) => {
            // S31: 防御性响应大小限制（100MB），超限返回 413。
            //   webdav.rs::download 已在下载层做 Content-Length + 流式累计限制（S31b），
            //   超限时返回 None → 此处落到 404。此检查作为防御兜底，确保即使
            //   download 实现变更也不会向客户端吐出超限响应。
            //   ponytail: 无法在 web.rs 层做 pre-download Content-Length 检查并返回 413，
            //   因 WebDavClient::request 为私有、head_check 不返回 Content-Length，
            //   且 download() 对 404 与超限都返回 None 无法区分。
            const MAX_PROXY_RESPONSE_SIZE: usize = 100 * 1024 * 1024;
            if bytes.len() > MAX_PROXY_RESPONSE_SIZE {
                // Phase 5 (Fix-4): 413 返回 JSON body（前端 _loadWebDavMedia 可解析 message toast）
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(serde_json::json!({
                        "success": false,
                        "error": "payload_too_large",
                        "message": "WebDAV 响应超过 100MB 限制"
                    })),
                )
                    .into_response();
            }
            let mime = crate::media::detect_mime(&bytes);
            let content_type = match header::HeaderValue::from_str(mime) {
                Ok(v) => v,
                Err(_) => header::HeaderValue::from_static("application/octet-stream"),
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    (
                        header::CACHE_CONTROL,
                        header::HeaderValue::from_static("private, no-store"),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn api_webdav_get(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // 与 api_webdav_save 返回格式一致：{success, settings}
    // 之前返回扁平对象，前端 _loadWebDAVSettings 检查 res.success 时失败，
    // 导致已保存的配置无法回显，用户被迫重新填写所有字段
    let settings = bot.get_webdav_settings();
    Json(serde_json::json!({"success": true, "settings": settings}))
}

async fn api_webdav_save(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    // Phase 3 (§7.3): 功能开关——管理员禁用 Webdav 时拒绝所有保存
    if !state.bot.check_feature(auth.uid, Feature::Webdav) {
        return feature_disabled_response("webdav");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let config = crate::models::WebDavConfig {
        enabled: body
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        url: body
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        username: body
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        password: body
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        base_path: body
            .get("base_path")
            .and_then(|v| v.as_str())
            .unwrap_or("/ilink-media")
            .to_string(),
        traffic_saver: body
            .get("traffic_saver")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        auto_migrate_on_save: body
            .get("auto_migrate_on_save")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        updated_at: String::new(),
    };
    // Phase 3 (§7.3): 功能开关——管理员禁用 CustomWebdav 时拒绝自定义 URL 配置
    //   仅允许使用系统预置 WebDAV（url 为空表示沿用既有配置或系统默认）
    if !config.url.is_empty() && !state.bot.check_feature(auth.uid, Feature::CustomWebdav) {
        return feature_disabled_response("custom_webdav");
    }
    // SSRF 校验 url，禁止 IP 字面量与内网域名。
    //   is_ssrf_safe_url 为 crate 内 pub(crate) 自由函数，复用 bot.rs 逻辑避免重复实现
    // S61: SSRF 校验不依赖 enabled，只要提交了 url 就校验（空 url 跳过），
    //   防止 enabled=false 时绕过校验保存内网 URL，后续开启即触发 SSRF
    if !config.url.is_empty() && !crate::bot::is_ssrf_safe_url(&config.url) {
        return Json(serde_json::json!({
            "success": false,
            "error": "URL 不合规：禁止 IP 字面量与 localhost/内网域名"
        }))
        .into_response();
    }
    // S4: base_path 路径穿越防护——拒绝 `..` 段，清洗控制字符，归一化为 / 前缀
    let mut config = config;
    config.base_path = match sanitize_webdav_base_path(&config.base_path) {
        Some(p) => p,
        None => {
            return Json(serde_json::json!({
                "success": false,
                "error": "base_path 不合规：禁止包含 .. 穿越段"
            }))
            .into_response();
        }
    };
    // reqwest::blocking::Client 内部会创建自己的 Tokio runtime，
    // 在 async 上下文中直接创建会导致 "Cannot drop a runtime in a runtime context" panic。
    // 用 spawn_blocking 将 save_webdav_settings（内含 reload_webdav_client → WebDavClient::new）
    // 转移到独立阻塞线程执行。
    let should_migrate = config.enabled && config.auto_migrate_on_save;
    let bot_for_save = bot.clone();
    let config_for_save = config.clone();
    match tokio::task::spawn_blocking(move || bot_for_save.save_webdav_settings(&config_for_save))
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("[WebDAV] uid={} 保存配置失败: {:#}", auth.uid, e);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": "webdav_save_failed",
                    "message": format!("WebDAV 配置保存失败：{}", e)
                })),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("[WebDAV] uid={} 保存任务失败: {}", auth.uid, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "success": false,
                    "error": "webdav_save_task_failed",
                    "message": "WebDAV 配置保存任务异常，请稍后重试"
                })),
            )
                .into_response();
        }
    }
    // 决策 B：保存时若 enabled + auto_migrate_on_save 均为 true，自动触发迁移
    if should_migrate {
        let migrate_state = bot.get_webdav_migration_state();
        if !migrate_state.running {
            tracing::info!("[WebDAV] auto_migrate_on_save 已启用，自动触发迁移");
            bot.start_webdav_migration_async();
        }
    }
    tracing::info!(
        "[WebDAV] uid={} 保存配置 enabled={}",
        auth.uid,
        config.enabled
    );
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", auth.uid),
        "webdav.save",
        Some(&format!("enabled={}", config.enabled)),
        None,
    );
    // 返回最新配置（含打码密码），供前端刷新 dirty 状态
    let settings = bot.get_webdav_settings();
    Json(serde_json::json!({"success": true, "settings": settings})).into_response()
}

async fn api_webdav_test(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(_body): Json<serde_json::Value>,
) -> Response {
    // Phase 3 (§7.3): 功能开关——管理员禁用 Webdav 时拒绝测试
    if !state.bot.check_feature(auth.uid, Feature::Webdav) {
        return feature_disabled_response("webdav");
    }
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // Phase 3 (S2): WebDAV 测试限速（5 次/300s，按 uid 维度，防 SSRF 探测滥用）
    let rl_key = format!("{}|webdav-test", auth.uid);
    if state.bot.check_rate_limit(&rl_key, 5, 300.0) {
        return rate_limited_response("webdav-test");
    }
    // 改造 3.2：从 DB 读取已保存的生效配置测试，不再信任前端 body 里的临时字段
    let cfg = {
        let c = bot.webdav_config.read();
        c.clone()
    };
    if cfg.url.is_empty() {
        return Json(serde_json::json!({
            "ok": false,
            "message": "尚未保存配置，请先保存后再测试",
            "status": 0
        }))
        .into_response();
    }
    // 二次 SSRF 校验。配置在 save 时已校验过，但管理员可能在 save 后直接改 DB，
    //   测试前再校验一次，防止 enabled=false 时绕过校验保存的内网 URL。
    if !crate::bot::is_ssrf_safe_url(&cfg.url) {
        tracing::warn!("[WEB] WebDAV 测试被 SSRF 校验拒绝 url={}", cfg.url);
        return Json(serde_json::json!({
            "ok": false,
            "message": "URL 不安全（禁止 IP 字面量与内网域名），请检查配置",
            "status": 0
        }))
        .into_response();
    }
    let bot = bot.clone();
    // spawn_blocking 任务 panic 时返回通用错误消息，详情进日志供管理员排查。
    let result = tokio::task::spawn_blocking(move || {
        bot.test_webdav_connection(&cfg.url, &cfg.username, &cfg.password, &cfg.base_path)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("[WEB] WebDAV 测试任务异常 uid={} err={:?}", auth.uid, e);
        serde_json::json!({
            "ok": false,
            "message": "WebDAV 测试失败，请稍后重试",
            "status": 0
        })
    });
    Json(result).into_response()
}

async fn api_webdav_traffic_saver(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<TrafficSaverRequest>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // Check if WebDAV is enabled for this user
    let settings = bot.get_webdav_settings();
    if !settings
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Json(
            serde_json::json!({"success": false, "error": "webdav_not_enabled", "message": "请先启用 WebDAV 再修改此设置"}),
        );
    }
    bot.update_traffic_saver(body.traffic_saver);
    tracing::info!(
        "[WebDAV] uid={} 切换省流量模式: {}",
        auth.uid,
        body.traffic_saver
    );
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", auth.uid),
        "webdav.traffic_saver",
        Some(&format!("{}", body.traffic_saver)),
        None,
    );
    Json(serde_json::json!({"success": true}))
}

async fn api_webdav_migrate(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // Check if WebDAV is enabled for this user
    let settings = bot.get_webdav_settings();
    if !settings
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Json(
            serde_json::json!({"success": false, "error": "webdav_not_enabled", "message": "请先启用 WebDAV 再执行迁移"}),
        );
    }
    // S17: 原子地检查并设置 running，消除检查-启动竞态。
    //   start_webdav_migration_async 在 spawn 前会再次确认 running=true，此处设置不会冲突。
    {
        let mut st = bot.webdav_migrate_state.write();
        if st.running {
            return Json(serde_json::json!({"success": false, "error": "迁移正在进行中"}));
        }
        st.running = true;
    }
    tracing::info!("[WebDAV] uid={} 触发手动迁移", auth.uid);
    // 审计日志写入失败时 warn 告警，不阻断业务。
    audit_log(
        &state.system_db,
        &format!("uid={}", auth.uid),
        "webdav.migrate",
        Some("manual"),
        None,
    );
    bot.start_webdav_migration_async();
    Json(serde_json::json!({"success": true}))
}

async fn api_webdav_migrate_status(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    // 与其它 webdav-* 端点保持一致：返回 {success, state} 包装。
    // 之前直接返回裸 WebDavMigrateState，前端 _pollMigrateStatus 检查 res.success
    // 永远为 undefined → 每次轮询都被提前 return → 状态栏永远卡在"正在启动迁移任务..."
    // 用户刷新设置页时看到旧状态文本，误以为迁移在跑/后端没响应。
    let state = bot.get_webdav_migration_state();
    Json(
        serde_json::json!({"success": true, "state": serde_json::to_value(state).unwrap_or_default()}),
    )
}

async fn api_webdav_auth(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
) -> Json<serde_json::Value> {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(_) => {
            return Json(
                serde_json::json!({"success": false, "error": "bot_unavailable", "message": "用户会话初始化失败，请稍后重试"}),
            )
        }
    };
    let cfg = bot.webdav_config.read();
    if !cfg.enabled || cfg.username.is_empty() {
        return Json(serde_json::json!({"ok": false, "error": "WebDAV 未启用"}));
    }
    // 仅返回启用状态与 endpoint host，不返回凭证
    // 前端如需访问 WebDAV 应走服务端代理路由，由服务端注入凭证
    // S60: 用 url::Url::parse 解析 host，避免 split('/') 脆弱解析
    let host = url::Url::parse(&cfg.url)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default();
    Json(serde_json::json!({
        "ok": true,
        "enabled": true,
        "host": host,
        "base_path": cfg.base_path,
    }))
}

/// handler 改用 POST + Json<ExportHistoryRequest>（对齐前端）
async fn api_export_history(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthUser>,
    Json(body): Json<ExportHistoryRequest>,
) -> Response {
    let bot = match get_bot_or_503(&state, auth.uid).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let user_id = body.user_id;
    let nickname = body.nickname.unwrap_or_default();
    if user_id.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let html = bot.db.export_user_messages_html(&user_id, &nickname);
    let disposition = format!("attachment; filename=\"chat-{}.html\"", user_id);
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                match header::HeaderValue::from_str(&disposition) {
                    Ok(v) => v,
                    Err(_) => header::HeaderValue::from_static("attachment"),
                },
            ),
        ],
        html,
    )
        .into_response()
}

// ── 服务器启动/停止 ─────────────────────────────────────────

/// 接入 `with_graceful_shutdown`，收到 SIGTERM/SIGINT 时
///   axum 停止接受新连接并等待已有连接处理完毕，
///   避免 SQLite WAL 未 checkpoint、配额未 flush、消息状态永久 pending。
///
/// 信号监听放在 main.rs，此处仅接收外部 shutdown 信号。
pub async fn start_server(bot: Arc<BotManager>, system_db: Arc<SystemDatabase>) {
    let port = bot.web_port();
    // 不再读环境变量 ILINK_HOST，改为统一通过 config::bind_host()。
    let host = crate::config::bind_host();
    // IP 字面量（含 IPv6，bind_host 已去方括号）直接构造 SocketAddr，
    // 避免 ":::8888"/"::1:8888" 这类字符串拼接解析失败。
    let addr: std::net::SocketAddr = match host.parse::<IpAddr>() {
        Ok(ip) => SocketAddr::new(ip, port),
        Err(_) => format!("{}:{}", host, port)
            .parse()
            .expect("无效的监听地址"),
    };

    let app = create_app(bot, system_db);
    tracing::info!("[WEB] 启动 Web 服务: http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("无法绑定端口");
    // 在 SIGTERM/SIGINT 触发后，axum 进入 graceful drain 阶段。
    //   主 main.rs 监听信号 → bot.stop() + 配额 flush → 退出。
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        tracing::error!("[WEB] 服务器错误: {}", e);
    }
    tracing::info!("[WEB] Web 服务已优雅退出");
}

/// 监听 SIGINT (Ctrl+C) 和 SIGTERM (容器/k8s 滚动更新)，返回时触发 graceful shutdown。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("[SIGNAL] 收到 SIGINT (Ctrl+C)，开始优雅关闭...");
            println!("[SIGNAL] 收到 SIGINT (Ctrl+C)，开始优雅关闭...");
        },
        _ = terminate => {
            tracing::info!("[SIGNAL] 收到 SIGTERM，开始优雅关闭...");
            println!("[SIGNAL] 收到 SIGTERM，开始优雅关闭...");
        },
    }
}
