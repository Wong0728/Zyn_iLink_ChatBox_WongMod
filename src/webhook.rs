// Webhook 出站推送模块
// 收到入站消息后 fire-and-forget POST 到配置的 webhook URL(s)
// 参考 openilink-hub 的 sink/webhook.go

use crossbeam_channel::{bounded, Sender};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use reqwest::blocking::Client;
use serde::Serialize;
use sha2::Sha256;
use std::sync::Arc;
use std::time::{Duration, Instant};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub event: String, // "message.new"
    pub bot_id: String,
    pub from_user: String,
    pub to_user: String,
    pub text: String,
    pub message_id: Option<i64>,
    pub timestamp: i64,
}

/// S32b: 投递作业（放入队列由 worker 线程消费）
struct WebhookJob {
    url: String,
    body: String,
}

pub struct WebhookDispatcher {
    // URL 列表支持运行时动态移除不安全 URL（周期性 SSRF 重新校验）。
    urls: Arc<Mutex<Vec<String>>>,
    #[allow(dead_code)]
    client: Client,
    tx: Sender<WebhookJob>,
    #[allow(dead_code)]
    token: Option<String>,
    // 上次 SSRF 校验时间，用于周期性重新校验。
    last_validated: Arc<Mutex<Instant>>,
}

impl WebhookDispatcher {
    /// 从环境变量创建。
    /// ILINK_WEBHOOK_URLS: 逗号分隔的 URL 列表
    /// ILINK_WEBHOOK_TOKEN: 可选 HMAC 签名 token
    pub fn from_env() -> Option<Self> {
        let urls: Vec<String> = std::env::var("ILINK_WEBHOOK_URLS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        // SSRF 防护：拒绝 IP 字面量 / localhost / 内网域名（复用 bot::is_ssrf_safe_url）
        let urls: Vec<String> = urls
            .into_iter()
            .filter(|u| {
                if crate::bot::is_ssrf_safe_url(u) {
                    true
                } else {
                    tracing::warn!("[WEBHOOK] 拒绝不安全 URL（SSRF 防护）: {}", u);
                    false
                }
            })
            .collect();
        if urls.is_empty() {
            return None;
        }

        let client = Client::builder()
            // 禁止跟随重定向：URL 仅经入队时校验，302 跳向内网地址会绕过 SSRF 校验（审计 M-2）
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;

        // S57: HMAC 签名 token
        let token = std::env::var("ILINK_WEBHOOK_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());

        // 创建有界投递队列 + 单 worker 线程，防止消息堆积导致内存耗尽
        // 容量 1000：高峰突发可缓冲，超限则丢弃（webhook 为 best-effort 通知）
        let (tx, rx) = bounded::<WebhookJob>(1000);
        let worker_client = client.clone();
        let worker_token = token.clone();
        std::thread::Builder::new()
            .name("ilink-webhook-worker".into())
            .spawn(move || {
                // S17b: worker 线程为独立 std::thread，reqwest::blocking 在此使用安全
                for job in rx {
                    deliver_once(&worker_client, &job.url, &job.body, worker_token.as_deref());
                }
            })
            .ok()?;

        Some(Self {
            urls: Arc::new(Mutex::new(urls)),
            client,
            tx,
            token,
            last_validated: Arc::new(Mutex::new(Instant::now())),
        })
    }

    /// Fire-and-forget 投递。S32b: 将 job 推入队列由 worker 线程处理（非阻塞）。
    pub fn deliver(&self, payload: &WebhookPayload) {
        let body = match serde_json::to_string(payload) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("[WEBHOOK] payload 序列化失败: {}", e);
                return;
            }
        };

        // Webhook URL 周期性重新校验（默认每 1 小时），防止 DNS 重绑定绕过 SSRF 防护。
        // 校验间隔可通过 ILINK_WEBHOOK_REVALIDATE_HOURS 环境变量调整（1-24 小时）。
        let revalidate_secs: u64 = std::env::var("ILINK_WEBHOOK_REVALIDATE_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1)
            .clamp(1, 24)
            * 3600;
        let urls_snapshot = {
            let mut urls_guard = self.urls.lock();
            let mut last = self.last_validated.lock();
            if last.elapsed().as_secs() >= revalidate_secs {
                let before_count = urls_guard.len();
                urls_guard.retain(|u| {
                    if crate::bot::is_ssrf_safe_url(u) {
                        true
                    } else {
                        tracing::warn!("[WEBHOOK] 周期校验移除不安全 URL: {}", u);
                        false
                    }
                });
                *last = Instant::now();
                if urls_guard.len() < before_count {
                    tracing::info!(
                        "[WEBHOOK] 周期校验完成，URL 数 {} → {}",
                        before_count,
                        urls_guard.len()
                    );
                }
            }
            urls_guard.clone()
        };

        for url in &urls_snapshot {
            match self.tx.try_send(WebhookJob {
                url: url.clone(),
                body: body.clone(),
            }) {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    tracing::warn!("[WEBHOOK] 投递队列已满 (>=1000)，丢弃 webhook: {}", url);
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    tracing::error!("[WEBHOOK] worker 线程已退出，webhook 不可用");
                }
            }
        }
    }

    /// 返回 webhook 状态供管理面板展示：(urls, has_token, secs_since_last_validate)。
    /// token 本身不返回（敏感），仅返回是否已设置。
    pub fn get_status(&self) -> (Vec<String>, bool, u64) {
        let urls = self.urls.lock().clone();
        let has_token = self.token.is_some();
        let secs = self.last_validated.lock().elapsed().as_secs();
        (urls, has_token, secs)
    }
}

/// S17b: 在 worker 线程中执行 blocking 投递，带 3 次重试 [1s, 5s, 15s]
fn deliver_once(client: &Client, url: &str, body: &str, token: Option<&str>) {
    let retry_delays = [1u64, 5, 15];
    for (attempt, &delay) in retry_delays.iter().enumerate() {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(delay));
        }
        let mut req = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "iLink-WM1-Webhook/1.0")
            .body(body.to_string());
        // S57: HMAC-SHA256 签名
        if let Some(t) = token {
            let sig = hmac_sha256_hex(t.as_bytes(), body.as_bytes());
            req = req.header("X-Webhook-Signature", sig);
        }
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if (200..300).contains(&status) {
                    tracing::debug!("[WEBHOOK] 投递成功 {} HTTP {}", url, status);
                    return;
                }
                if status < 500 && status != 429 {
                    tracing::warn!("[WEBHOOK] 投递失败 {} HTTP {} (非可重试)", url, status);
                    return;
                }
                tracing::warn!(
                    "[WEBHOOK] 投递可重试失败 {} HTTP {} attempt={}",
                    url,
                    status,
                    attempt + 1
                );
            }
            Err(e) => {
                tracing::warn!(
                    "[WEBHOOK] 投递网络异常 {} attempt={}: {}",
                    url,
                    attempt + 1,
                    e
                );
            }
        }
    }
    tracing::error!("[WEBHOOK] 投递重试耗尽 {}", url);
}

/// 使用 `hmac` + `sha2` crate 计算 HMAC-SHA256 hex 签名。
fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any size");
    mac.update(message);
    hex::encode(mac.finalize().into_bytes())
}
