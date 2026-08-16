// SSE 事件总线：bot 线程生产 → tokio broadcast → SSE 消费

#![allow(dead_code)]

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

#[allow(dead_code)]
pub const EVENT_MESSAGE: &str = "message";
pub const EVENT_STATUS: &str = "status";
pub const EVENT_USER: &str = "user";
pub const EVENT_PING: &str = "ping";
pub const EVENT_MEDIA_CACHE_UPDATE: &str = "media_cache_update";
pub const EVENT_SYNC_REQUIRED: &str = "sync_required";
// 会话状态变更事件
pub const EVENT_SESSION_STATUS: &str = "session_status";
// 出站消息 ACK 事件（pending/sending/sent/delivered/failed/expired）
pub const EVENT_SEND_ACK: &str = "send_ack";
// QR 登录状态变更事件。
//   后端 set_qr_login_state 每次状态变化均推送，前端 WS 收到后实时更新三态 UI。
pub const EVENT_QR_STATE: &str = "qr_state";

#[derive(Debug, Clone, Serialize)]
pub struct BrokerEvent {
    pub event_type: String,
    pub data: serde_json::Value,
    pub id: Option<String>,
}

pub struct EventBroker {
    sender: broadcast::Sender<BrokerEvent>,
    dropped: AtomicU64, // 无订阅者时的 SendError 计数
    lagged: AtomicU64,  // S62: 订阅者 lagged 丢失事件总数（由消费者上报）
}

impl EventBroker {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(2000);
        Self {
            sender,
            dropped: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
        }
    }

    fn record_send_result(&self, result: Result<usize, broadcast::error::SendError<BrokerEvent>>) {
        if result.is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// S62: 消费者遇到 Lagged 时上报丢失事件数，由 push.rs 调用
    pub fn record_lagged(&self, n: u64) {
        self.lagged.fetch_add(n, Ordering::Relaxed);
    }

    /// 从任意线程发布事件（线程安全）
    pub fn publish(&self, event_type: &str, data: serde_json::Value) {
        let result = self.sender.send(BrokerEvent {
            event_type: event_type.to_string(),
            data,
            id: None,
        });
        self.record_send_result(result);
    }

    /// 发布带 id 的事件
    pub fn publish_with_id(&self, event_type: &str, data: serde_json::Value, id: &str) {
        let result = self.sender.send(BrokerEvent {
            event_type: event_type.to_string(),
            data,
            id: Some(id.to_string()),
        });
        self.record_send_result(result);
    }

    /// 订阅事件流
    pub fn subscribe(&self) -> broadcast::Receiver<BrokerEvent> {
        self.sender.subscribe()
    }

    /// 订阅者数量
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// 实际语义为 publish 时无活跃订阅者导致的 SendError 次数，
    /// 并非"队列满/滞后丢弃"（broadcast 满时返回 Ok + Lagged，不在此计数）。
    pub fn no_subscriber_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 兼容别名，供 web.rs 等旧调用方使用
    #[deprecated(since = "1.0.0", note = "use no_subscriber_count instead")]
    pub fn dropped_count(&self) -> u64 {
        self.no_subscriber_count()
    }

    /// S62: 订阅者 lagged 丢失事件总数（由消费者 push.rs 上报）。
    /// 与 dropped_count 互补：dropped = 无订阅者发送失败，lagged = 有订阅者但跟不上。
    pub fn lagged_count(&self) -> u64 {
        self.lagged.load(Ordering::Relaxed)
    }
}

impl Default for EventBroker {
    fn default() -> Self {
        Self::new()
    }
}
