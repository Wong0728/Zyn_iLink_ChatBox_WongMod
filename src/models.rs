// 共享数据结构

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// 消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct Message {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default, rename = "type")]
    pub msg_type: String, // "in" | "out"
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub media_cdn: Option<serde_json::Value>,
    #[serde(default)]
    pub media_cache_id: Option<String>,
    #[serde(default)]
    pub webdav_url: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// 媒体类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MediaType {
    Image = 2,
    Voice = 3,
    File = 4,
    Video = 5,
}

/// QR 登录状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum QrLoginState {
    #[default]
    Idle,
    Fetching,
    Ready,
    Scanned,
    Confirmed,
    Expired,
    Error,
}

impl QrLoginState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Fetching => "fetching",
            Self::Ready => "ready",
            Self::Scanned => "scanned",
            Self::Confirmed => "confirmed",
            Self::Expired => "expired",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for QrLoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// QR 登录状态信息
#[derive(Debug, Clone, Serialize)]
pub struct QrLoginStatus {
    pub state: QrLoginState,
    pub message: String,
    pub login_done: bool,
    pub has_qrcode: bool,
    /// 二维码矩阵：与 Python 版一致，用字符串 "█"/" " 而非布尔值
    /// 前端通过 e === " " 判断白色单元格
    pub matrix: Option<Vec<Vec<String>>>,
    pub qrcode_key: Option<String>,
}

impl Default for QrLoginStatus {
    fn default() -> Self {
        Self {
            state: QrLoginState::Idle,
            message: String::new(),
            login_done: false,
            has_qrcode: false,
            matrix: None,
            qrcode_key: None,
        }
    }
}

/// 轮询健康状态
#[derive(Debug, Clone, Serialize)]
pub struct PollHealth {
    pub last_success_at: f64,
    pub state: String, // "ok" | "error" | "expired"
    pub last_error: String,
    pub since: f64,
}

/// WebDAV 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebDavConfig {
    pub enabled: bool,
    pub url: String,
    pub username: String,
    pub password: String,
    pub base_path: String,
    pub traffic_saver: bool,
    pub auto_migrate_on_save: bool,
    pub updated_at: String,
}

/// WebDAV 迁移状态
#[derive(Debug, Clone, Serialize, Default)]
pub struct WebDavMigrateState {
    pub running: bool,
    pub total: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub deleted_local: usize,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub current_file_bytes: u64,
    pub current_file_size: u64,
    pub bytes_per_sec: f64,
    /// S76: EMA 平滑后的速率（α=0.2），用于消除进度抖动
    #[serde(skip)]
    pub ema_rate: f64,
    pub eta_seconds: f64,
    pub overwritten: usize,
    pub current: String,
    pub started_at: f64,
    pub finished_at: f64,
    pub error: String,
}

impl WebDavMigrateState {
    /// S76: 用 EMA 平滑瞬时速率，同时更新 bytes_per_sec。
    /// ema_rate = ema_rate * 0.8 + instant_rate * 0.2
    pub fn update_ema_rate(&mut self, instant_rate: f64) {
        if self.ema_rate == 0.0 {
            self.ema_rate = instant_rate;
        } else {
            self.ema_rate = self.ema_rate * 0.8 + instant_rate * 0.2;
        }
        self.bytes_per_sec = self.ema_rate;
    }
}

/// 会话过期错误码
pub const EXPIRED_CODES: &[i64] = &[-14, 40014, 1002];

pub fn is_expired_code(ret: i64) -> bool {
    EXPIRED_CODES.contains(&ret)
}

/// 判断 iLink API 响应是否表示成功。
/// 标准成功：ret=0 或 errcode=0；备选成功：code=0 / status=0 / success=true。
/// 当 ret/errcode 均不存在时视为成功（HTTP 200 无错误信号 = 已送达），
/// 避免 iLink API 返回非标准格式导致误判失败。
pub fn is_api_response_success(result: &serde_json::Value) -> bool {
    // 标准成功字段
    if result.get("ret").and_then(|v| v.as_i64()) == Some(0) {
        return true;
    }
    if result.get("errcode").and_then(|v| v.as_i64()) == Some(0) {
        return true;
    }
    // 备选成功字段（部分 API 版本/demo 模式使用不同字段名）
    if result.get("code").and_then(|v| v.as_i64()) == Some(0) {
        return true;
    }
    if result.get("status").and_then(|v| v.as_i64()) == Some(0) {
        return true;
    }
    if result.get("success").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    // 显式失败检查——有 ret/errcode 但非 0
    if let Some(ec) = result.get("errcode").and_then(|v| v.as_i64()) {
        if ec != 0 {
            return false;
        }
    }
    if let Some(r) = result.get("ret").and_then(|v| v.as_i64()) {
        if r != 0 {
            return false;
        }
    }
    // ret/errcode/status/code 均不存在 → 无失败信号，视为成功
    true
}

/// 会话状态机
/// 参考 openilink-hub-main `provider.Status() = "session_expired"`（终态）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SessionState {
    /// 初始 / 退出登录
    #[default]
    Disconnected,
    /// 正常轮询中
    Active,
    /// 终态：iLink 会话过期，必须重新扫码（保留 token）
    SessionExpired,
    /// 用户点击"重新扫码"后短暂状态
    Reauthing,
}

impl SessionState {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Active => "active",
            Self::SessionExpired => "session_expired",
            Self::Reauthing => "reauthing",
        }
    }

    /// 是否为终态（不可自动恢复）
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::SessionExpired)
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 上传媒体结果
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UploadMediaResult {
    pub filekey: String,
    pub encrypt_query_param: String,
    pub aes_key: String,
    pub aes_key_hex: String,
    pub raw_size: usize,
    pub encrypted_size: usize,
    pub md5: String,
    pub filename: String,
}

/// 媒体元数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaMeta {
    pub cache_key: String,
    pub mime: String,
    pub filename: String,
    pub size: i64,
}

/// 远程媒体记录
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaRemote {
    pub remote_path: String,
    pub uploaded_at: String,
    pub user_id: String,
    pub content_md5: String,
}

/// 应用用户（system.db.users 表对应结构）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AppUser {
    pub id: i64,
    pub username: String,
    pub role: String,          // 'owner' | 'admin' | 'user'
    pub status: String,        // 'active' | 'disabled'
    pub password_hash: String, // 十六进制
    pub salt: String,
    pub iterations: i64,
    // 配额（0 = 用系统默认）
    pub quota_upload_bytes: i64,
    pub used_upload_bytes: i64,
    pub used_upload_date: Option<String>, // YYYY-MM-DD
    pub quota_download_bytes: i64,
    pub used_download_bytes: i64,
    pub used_download_date: Option<String>, // YYYY-MM-DD
    pub quota_media_bytes: i64,
    pub used_media_bytes: i64,
    pub quota_msg_per_day: i64,
    pub used_msg_today: i64,
    pub used_msg_date: Option<String>, // YYYY-MM-DD
    pub quota_media_count: i64,
    pub used_media_count: i64,
    // 功能开关（v2.1 C2：3 个 INTEGER 列而非 features_json）
    pub allow_upload: i64,
    pub allow_webdav: i64,
    pub allow_custom_webdav: i64,
    // 邮箱（仅作联系字段，不用于认证；L16 邮箱验证已按用户要求移除）
    pub email: Option<String>,
    // 守则
    pub agreed_terms_ver: Option<String>,
    pub agreed_terms_at: Option<String>,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

/// 公开用户信息 DTO（用于 API 响应序列化，不包含密码哈希等敏感字段）。
///
/// 原 AppUser derive Serialize 后直接序列化会泄露 password_hash / salt / iterations，
/// 即使是 admin 接口也不应向前端暴露这些字段——admin 会话被劫持或管理面板 XSS
/// 即可拿到哈希后离线爆破。PublicAppUser 显式排除敏感字段，仅保留展示/管理所需的
/// 非敏感信息（id / username / role / status / 配额 / 功能开关 / 邮箱 / 时间戳）。
///
/// 所有把 AppUser 序列化到 HTTP 响应的 API 必须先转换为 PublicAppUser。
/// 内部代码（auth 校验、改密等）仍直接用 AppUser 访问 password_hash。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PublicAppUser {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub status: String,
    pub quota_upload_bytes: i64,
    pub used_upload_bytes: i64,
    pub used_upload_date: Option<String>,
    pub quota_download_bytes: i64,
    pub used_download_bytes: i64,
    pub used_download_date: Option<String>,
    pub quota_media_bytes: i64,
    pub used_media_bytes: i64,
    pub quota_msg_per_day: i64,
    pub used_msg_today: i64,
    pub used_msg_date: Option<String>,
    pub quota_media_count: i64,
    pub used_media_count: i64,
    pub allow_upload: i64,
    pub allow_webdav: i64,
    pub allow_custom_webdav: i64,
    pub email: Option<String>,
    pub agreed_terms_ver: Option<String>,
    pub agreed_terms_at: Option<String>,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

impl From<&AppUser> for PublicAppUser {
    fn from(u: &AppUser) -> Self {
        PublicAppUser {
            id: u.id,
            username: u.username.clone(),
            role: u.role.clone(),
            status: u.status.clone(),
            quota_upload_bytes: u.quota_upload_bytes,
            used_upload_bytes: u.used_upload_bytes,
            used_upload_date: u.used_upload_date.clone(),
            quota_download_bytes: u.quota_download_bytes,
            used_download_bytes: u.used_download_bytes,
            used_download_date: u.used_download_date.clone(),
            quota_media_bytes: u.quota_media_bytes,
            used_media_bytes: u.used_media_bytes,
            quota_msg_per_day: u.quota_msg_per_day,
            used_msg_today: u.used_msg_today,
            used_msg_date: u.used_msg_date.clone(),
            quota_media_count: u.quota_media_count,
            used_media_count: u.used_media_count,
            allow_upload: u.allow_upload,
            allow_webdav: u.allow_webdav,
            allow_custom_webdav: u.allow_custom_webdav,
            email: u.email.clone(),
            agreed_terms_ver: u.agreed_terms_ver.clone(),
            agreed_terms_at: u.agreed_terms_at.clone(),
            created_at: u.created_at.clone(),
            last_login_at: u.last_login_at.clone(),
        }
    }
}

impl From<AppUser> for PublicAppUser {
    fn from(u: AppUser) -> Self {
        PublicAppUser::from(&u)
    }
}

/// 已认证用户（通过 axum Extension 注入到请求）
///
/// 设计取舍：role 使用 String 而非 i32/`&'static str`。
/// 原因：axum Extension 仅要求 Clone，String 实现 Clone 但不实现 Copy，
/// 故此处只 derive 到 Clone（不 Copy）。如需 Copy 语义，调用方应改用 i32 枚举。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AuthUser {
    pub uid: i64,
    pub role: String, // 'owner' | 'admin' | 'user'
}

/// 系统设置（system.db.settings 表的 KV 记录）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct SystemSetting {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// 邀请码（system.db.invite_codes 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct InviteCode {
    pub code: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub used_by: Option<i64>,
    pub used_at: Option<String>,
    pub status: String, // 'valid' | 'used' | 'expired' | 'revoked'
    pub note: Option<String>,
}

/// 审计日志（system.db.audit_logs 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AuditLog {
    pub id: i64,
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub detail_json: Option<String>,
    pub created_at: String,
}

/// IP 封禁记录（system.db.ip_bans 表）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct IpBan {
    pub id: i64,
    pub ip: String,
    pub reason: String,
    pub banned_by: String,
    pub created_at: String,
    pub expires_at: Option<String>,
}

/// 设备令牌（system.db.device_tokens 表 — 浏览器"记住我"自动登录）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct DeviceToken {
    pub token: String,
    pub uid: i64,
    pub device_name: String,
    pub created_at: String,
    pub expires_at: String,
    pub last_used_at: Option<String>,
}

/// 类型别名：共享 Bot 实例
pub type SharedBot = Arc<crate::bot::WeChatiLinkBot>;
