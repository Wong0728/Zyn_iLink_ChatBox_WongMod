// WebSocket Push Hub — 替代 SSE，支持双向通信
// 参考 openilink-hub 的 push.Hub 设计：订阅模型 + 心跳 + 消息回放

use crate::event_broker::EventBroker;
use axum::extract::ws::{Message, WebSocket};
use futures::StreamExt;
use parking_lot::RwLock;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);
// 统计 hub 转发的消息数，便于排查"消息需刷新才显示"问题
static MSG_FORWARD_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ClientMessage {
    Subscribe {
        data: SubscribeData,
    },
    Unsubscribe {
        data: UnsubscribeData,
    },
    Ping,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeData {}

#[derive(Debug, Clone, Deserialize)]
pub struct UnsubscribeData {}

/// 单个 WebSocket 连接
struct Connection {
    tx: tokio::sync::mpsc::Sender<String>,
}

/// WebSocket Push Hub
pub struct PushHub {
    connections: RwLock<HashMap<u64, Connection>>,
    event_counter: AtomicU64,
    broker: Arc<EventBroker>,
}

impl PushHub {
    pub fn new(broker: Arc<EventBroker>) -> Arc<Self> {
        let hub = Arc::new(Self {
            connections: RwLock::new(HashMap::new()),
            event_counter: AtomicU64::new(1),
            broker,
        });
        // 启动广播转发线程
        Self::start_broadcast_forward(hub.clone());
        // 启动心跳线程
        Self::start_heartbeat(hub.clone());
        hub
    }

    /// 分配全局递增事件 ID
    pub fn next_event_id(&self) -> u64 {
        self.event_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// 从 broker 订阅 → 转发到所有 WS 连接
    fn start_broadcast_forward(hub: Arc<Self>) {
        let mut rx = hub.broker.subscribe();
        let hub_clone = hub.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let evt_id = hub_clone.next_event_id();
                        let conn_count = {
                            let conns = hub_clone.connections.read();
                            conns.len()
                        };
                        // message 事件转发日志，排查"消息需刷新才显示"
                        if event.event_type == "message" {
                            let total = MSG_FORWARD_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                            tracing::info!(
                                "[PUSH] 转发 message 事件 #{} 当前连接数={} 累计转发={} from={:?}",
                                evt_id,
                                conn_count,
                                total,
                                event.data.get("from").and_then(|v| v.as_str())
                            );
                        }
                        let payload = serde_json::json!({
                            "event": event.event_type,
                            "data": event.data,
                            "id": evt_id,
                        });
                        let text = serde_json::to_string(&payload).unwrap_or_default();
                        hub_clone.broadcast(&text);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // 广播落后，通知所有客户端重新同步
                        tracing::warn!(
                            "[PUSH] broadcast 滞后，丢失 {} 条事件，通知客户端重同步",
                            n
                        );
                        // S62: 上报 lagged 计数到 broker 统计
                        hub_clone.broker.record_lagged(n);
                        let payload = serde_json::json!({
                            "event": "sync_required",
                            "data": {"missed": n},
                            "id": hub_clone.next_event_id(),
                        });
                        let text = serde_json::to_string(&payload).unwrap_or_default();
                        hub_clone.broadcast(&text);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// 30 秒心跳
    fn start_heartbeat(hub: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let payload = serde_json::json!({
                    "event": "ping",
                    "data": {},
                    "id": 0,
                });
                let text = serde_json::to_string(&payload).unwrap_or_default();
                hub.broadcast(&text);
            }
        });
    }

    /// 新客户端连接
    pub fn connect(&self) -> (u64, tokio::sync::mpsc::Receiver<String>) {
        let id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
        // S15: 容量从 128 提升到 512，降低慢客户端被踢概率
        let (tx, rx) = tokio::sync::mpsc::channel(512);
        let conn = Connection { tx };
        self.connections.write().insert(id, conn);
        (id, rx)
    }

    /// 断开连接
    pub fn disconnect(&self, conn_id: u64) {
        self.connections.write().remove(&conn_id);
    }

    /// 当前连接数
    pub fn connection_count(&self) -> usize {
        self.connections.read().len()
    }

    /// 向所有连接广播文本；发送失败（满/已断）的连接移除
    fn broadcast(&self, text: &str) {
        let mut dead: Vec<u64> = Vec::new();
        {
            let conns = self.connections.read();
            for (id, c) in conns.iter() {
                if c.tx.try_send(text.to_string()).is_err() {
                    // S15: channel 满，慢客户端背压处理
                    // 先尝试发 sync_required 通知（前端可据此触发 full sync），
                    // 若仍失败才标记移除
                    let sync_notice = serde_json::json!({
                        "event": "sync_required",
                        "data": {"reason": "backpressure"},
                        "id": 0,
                    });
                    if c.tx.try_send(sync_notice.to_string()).is_err() {
                        tracing::warn!(
                            "[PUSH] 连接 {} channel 满，sync_required 仍失败，移除连接",
                            id
                        );
                        dead.push(*id);
                    }
                }
            }
        }
        if !dead.is_empty() {
            let mut conns = self.connections.write();
            for id in &dead {
                conns.remove(id);
            }
        }
    }
}

/// 处理 WebSocket 连接生命周期
pub async fn handle_ws(mut ws: WebSocket, hub: Arc<PushHub>, bot: crate::models::SharedBot) {
    let (conn_id, mut hub_rx) = hub.connect();
    tracing::info!(
        "[WS] 连接建立 conn_id={} 当前总连接数={}",
        conn_id,
        hub.connection_count()
    );

    // 发送初始状态
    let init = serde_json::json!({
        "event": "status",
        "data": {
            "current_user": bot.get_current_user(),
            "users": bot.list_users(),
            "login_done": bot.login_done.load(std::sync::atomic::Ordering::Relaxed),
            "conn_id": conn_id,
        },
        "id": 0,
    });
    let _ = ws
        .send(Message::Text(
            serde_json::to_string(&init).unwrap_or_default(),
        ))
        .await;

    // 移除"消息追赶"机制。前端 _ws.onopen 会 _fullSync → _loadHistory 主动拉取 DB 历史，
    //   消息追赶推送与 _loadHistory 产生竞态。

    // 主循环：同时处理 hub 推送、客户端消息、服务端心跳、空闲超时
    // S16: 心跳从 30s 改为 25s，确保在 60s 空闲超时前至少发送 2 次 Ping
    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    heartbeat.tick().await; // 丢弃首次立即触发
    loop {
        tokio::select! {
            hub_msg = hub_rx.recv() => {
                match hub_msg {
                    Some(text) => {
                        if ws.send(Message::Text(text)).await.is_err() {
                            tracing::warn!("[WS] 发送失败，连接可能已断开 conn_id={}", conn_id);
                            break;
                        }
                    }
                    None => break,
                }
            }
            client_msg = tokio::time::timeout(Duration::from_secs(60), ws.next()) => {
                match client_msg {
                    Err(_) => {
                        tracing::warn!("[WS] 空闲超时 60s，断开连接 conn_id={}", conn_id);
                        break;
                    }
                    Ok(Some(Ok(Message::Text(t)))) => {
                        if let Ok(cm) = serde_json::from_str::<ClientMessage>(&t) {
                            match cm {
                                // 删除 last_event_id dead code。预留字段从未用于实际逻辑。
                                ClientMessage::Subscribe { data: _ } => {
                                    // 不再处理 last_event_id
                                }
                                ClientMessage::Ping => {
                                    let _ = ws
                                        .send(Message::Text(r#"{"event":"pong","data":{},"id":0}"#.into()))
                                        .await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Some(Ok(Message::Ping(data)))) => {
                        let _ = ws.send(Message::Pong(data)).await;
                    }
                    Ok(Some(Ok(Message::Close(_)))) | Ok(None) => break,
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                if ws.send(Message::Ping(vec![])).await.is_err() {
                    tracing::warn!("[WS] 心跳发送失败，断开连接 conn_id={}", conn_id);
                    break;
                }
            }
        }
    }

    hub.disconnect(conn_id);
    tracing::info!(
        "[WS] 连接断开 conn_id={} 剩余连接数={}",
        conn_id,
        hub.connection_count()
    );
}
