    // ─── WebSocket 实时事件流（替代 SSE，支持双向通信） ──────
    let _ws = null;
    let _esReconnectTimer = null;
    let _esFailedCount = 0;
    let _esConnectingSince = 0;         // WS 开始 CONNECTING 的时间戳
    const _ES_MAX_FAIL = 10;             // 连续失败 N 次后降级为轮询
    const _ES_CONNECTING_TIMEOUT = 15000; // CONNECTING 状态超时 15s 降级
    const _POLL_FALLBACK_INTERVAL = 1000; // 降级后快速轮询（1s），确保消息秒级到达
    const _POLL_WS_CONNECTED_INTERVAL = 3000; // WS 连接成功后的兜底轮询频率（3s）
    const _WS_RECONNECT_BACKOFF_BASE = 2000;  // WS 重连基础退避 ms
    let _esReconnectDelay = _WS_RECONNECT_BACKOFF_BASE;
    // FIX M10 (2026-07-20): 缓存 WebSocket 状态常量，避免老浏览器/adblock 拦截下
    //   访问未定义的 WebSocket 抛 ReferenceError。OPEN/CONNECTING 数值在标准中是 1/0，
    //   用硬编码兜底即可。
    const _WS_OPEN = typeof WebSocket !== 'undefined' ? WebSocket.OPEN : 1;
    const _WS_CONNECTING = typeof WebSocket !== 'undefined' ? WebSocket.CONNECTING : 0;

    // FIX P1-15 (2026-07-20): 显式 WS 状态机，避免 _startSSE/_stopSSE 反复触发。
    //   原实现 _startSSE 内部先调 _stopSSE()，而 _showChat / _startPoll / _checkSseAuth
    //   等多处都会调 _startSSE，导致 WS 反复建连/断开（conn_id=1 7ms 断开）。
    //   状态机：
    //     'idle'       — 无连接，_startSSE 可建新连接
    //     'connecting' — 正在握手，_startSSE 直接 return，_stopSSE 可强制关闭
    //     'open'       — 已连接，_startSSE 直接 return，_stopSSE 可关闭
    //     'closing'    — 正在关闭，_startSSE 直接 return（等 onclose 后由调用方重连）
    //   _startSSE 禁止内部调 _stopSSE——IDLE 状态下直接建连即可，无需先关闭。
    let _wsState = 'idle';

    // FIX 2026-07-15: WS message 事件防抖触发 REST 拉取
    //   参考 openilink-hub 的 "WS 通知 + REST 数据" 模式：
    //   WS 收到 message 事件后不直接渲染，而是触发 _fetchMessages 从 REST 拉取，
    //   彻底消除 WS 推送与 _loadHistory 的竞态。
    //   多条 WS 消息快速到达时，防抖合并为一次 REST 请求。
    // FIX 2026-07-16: 防抖时间 200ms → 50ms，降低消息接收延迟。
    //   50ms 仍能合并同一瞬间到达的批量消息，但单条消息延迟从 200ms 降到 50ms。
    let _fetchMsgTimer = null;
    // FIX S16: WS 应用层 ping 定时器，防止 60s 空闲被代理/网关断开
    let _wsPingTimer = null;
    const _scheduleFetchMessages = function() {
        if (_fetchMsgTimer) clearTimeout(_fetchMsgTimer);
        _fetchMsgTimer = setTimeout(function() {
            _fetchMsgTimer = null;
            _fetchMessages().catch(function(err) { console.warn("[fetch-messages] scheduled error:", err && err.message || err); });
        }, 50);
    };

    const _normalizeId = function(id) {
        if (id === null || id === undefined) return "";
        return String(id).trim();
    };

    const _handleIncomingMessage = function(e) {
        if (!e) return;
        var targetUser = _normalizeId(e.type === 'in' ? e.from : e.to);
        if (!targetUser) {
            console.warn("[MSG-RECV] 早退：targetUser 为空");
            return;
        }
        if (!_state.displayedIds[targetUser]) _state.displayedIds[targetUser] = new Set();
        // FIX 2026-07-20: 去重键使用命名空间（id:/row:/client:/req:/media:）。
        //   messages 表自增 id 与 messages_v2 表 row_id 是两个独立序列，
        //   之前混在同一 Set 导致 id=N 的消息与 row_id=N 的消息互相误判为重复，
        //   偶数 id 入站消息被整行跳过。现在由 _anyKeyDisplayed/_addAllDedupKeys
        //   统一处理命名空间键，任意命中即视为已渲染，渲染后补全所有键避免后续重复。
        if (typeof _anyKeyDisplayed === 'function' && _anyKeyDisplayed(_state.displayedIds[targetUser], e)) {
            if (typeof _addAllDedupKeys === 'function') _addAllDedupKeys(_state.displayedIds[targetUser], e);
            return;
        }
        // FIX 2026-07-15: 历史加载期间到达的消息暂存队列，**不立即加入 displayedIds**。
        //   之前的实现是先 add(dedupKey) 再判断 _loadingHistory 推入队列，
        //   导致 _loadHistory 在 finally 处理队列时再次调用本函数，
        //   因 dedupKey 已存在被早退，队列消息永远丢失。
        //   现在改为：先暂存队列，等 _loadHistory 重建 displayedIds 后再处理，
        //   届时 dedup 检查通过，消息能正常渲染。
        if (_state._loadingHistory) {
            _state._messageQueue.push(e);
            return;
        }
        // FIX 2026-07-16/2026-07-20: 把所有可用键都加入 displayedIds（已命名空间隔离），
        //   防止后续 WS/轮询用不同键重复渲染。
        if (typeof _addAllDedupKeys === 'function') {
            _addAllDedupKeys(_state.displayedIds[targetUser], e);
        }
        // 只对 messages 表的自增 id（>0）更新轮询游标；
        // row_id（messages_v2 表）不能用于 messages 表游标，两表序列独立。
        if (typeof e.id === 'number' && e.id > 0) {
            _bumpLastMsgId(e.id);
        }
        // FIX: 不再用 row_id (messages_v2表) 兜底 bump lastMsgId (messages表游标)。
        //   两表 ID 序列独立，row_id 可能远大于 messages 表最大 ID，
        //   导致游标跳过所有未读消息。仅用 messages 表的 id 更新游标。
        // Update lastMessages for chat list preview
        var fromUser = _normalizeId(e.from) || _state.currentUser;
        if (fromUser) {
            _state.lastMessages[fromUser] = {
                text: e.text || '',
                time: e.time || '',
                media_type: e.media_type
            };
            if (_state.view === 'list') {
                _renderChatList();
            }
        }
        var curUser = _normalizeId(_state.currentUser);
        var alreadyRendered = false;
        // ── 替换对应的 pending 发送中元素（保持发送顺序） ──
        // 只按 req_id / row_id / client_id 精确匹配；不再退避到“删第一个 pending”，
        // 否则快速连续发送时 WS 事件可能误删/误排其他消息的 pending 气泡。
        if (e.type === 'out') {
            var pendingId = null;
            if (e.req_id && _state.pendingByReqId[e.req_id]) pendingId = _state.pendingByReqId[e.req_id];
            else if (e.row_id && _state.pendingByRowId[e.row_id]) pendingId = _state.pendingByRowId[e.row_id];
            else if (e.client_id && _state.pendingByClientId[e.client_id]) pendingId = _state.pendingByClientId[e.client_id];

            var pendingEl = null;
            if (pendingId) {
                // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
                //   避免 pendingId 含 "]" 等特殊字符逃逸 CSS 选择器。
                pendingEl = _findByDataAttr('data-sending-id', pendingId);
            }
            if (pendingEl) {
                // 用真实消息替换原 pending 气泡，保证顺序与发送顺序一致
                if (_state.view === 'chat' && curUser && targetUser === curUser) {
                    _replacePendingWithRendered(pendingEl, e);
                    alreadyRendered = true;
                } else {
                    pendingEl.remove();
                }
            }
            if (e.req_id) delete _state.pendingByReqId[e.req_id];
            if (e.row_id) delete _state.pendingByRowId[e.row_id];
            if (e.client_id) delete _state.pendingByClientId[e.client_id];
        }
        // 只有在当前聊天视图中且匹配当前用户时才渲染气泡
        // FIX 2026-07-15: 出站消息即使 row_id 为 0 也要渲染，避免消息丢失
        if (!alreadyRendered && _state.view === 'chat' && curUser && targetUser === curUser) {
            var renderedEl = _renderMsg(e);
            // FIX: 出站消息用 row_id 作为 data-msg-id（id 可能为 0），方便 send_ack 回溯
            // 即使 row_id 为 0 也要设置，避免后续无法匹配
            if (renderedEl && e.type === 'out') {
                if (e.row_id) {
                    renderedEl.dataset.msgId = e.row_id;
                    renderedEl.dataset.rowId = e.row_id;
                } else if (e.client_id) {
                    renderedEl.dataset.clientId = e.client_id;
                } else if (e.req_id) {
                    renderedEl.dataset.reqId = e.req_id;
                }
            }
            // FIX L17 (2026-07-20): 实时推送的消息加 aria-live="polite"，
            //   屏幕阅读器会播报新消息；历史加载的消息不加，避免一次性播报 50 条。
            //   仅对入站消息播报（出站消息用户自己发送，无需播报）。
            if (renderedEl && e.type === 'in') {
                renderedEl.setAttribute('aria-live', 'polite');
                renderedEl.setAttribute('role', 'article');
                var ariaLabel = (e.from || '未知') + ' 发来消息' + (e.text ? '：' + e.text.slice(0, 100) : '');
                renderedEl.setAttribute('aria-label', ariaLabel);
            }
        }
    };

    // ─── 获取后端状态（含 poll_health） ─────────────────────
    const _fetchStatus = function() {
        _get("status").then(function(s) {
            if (!s) return;
            // 从 REST 响应检测 login_done，避免等 WS 事件才触发 _showChat
            if (s.login_done && s.logged_in && (_state.view === 'init' || _state.view === 'login')) {
                if (typeof _handleStatusEvent === 'function') {
                    _handleStatusEvent(s);
                    return;
                }
            }
            if (s.poll_health) {
                _state.pollHealth = s.poll_health;
                _updateConnStatus();
            }
        }).catch(function() {});
    };

    // ─── 全量同步：SSE 连接建立/重连时调用，重新拉取所有数据 ───
    // 与 stop polling 不同，本函数始终保留轮询作为兜底
    const _fullSync = function(reason) {
        var why = reason || "manual";
        _loadUsers();
        _loadChatListPreviews();
        _fetchStatus();
        // 在聊天视图时，强制重拉历史（覆盖可能错过的消息）
        if (_state.view === 'chat' && _state.currentUser) {
            _loadHistory(_state.currentUser);
        } else {
            _fetchMessages();
        }
    };

    const _startSSE = async function() {
        // FIX P1-15 (2026-07-20): 显式状态机——禁止 _startSSE 内部调 _stopSSE。
        //   原实现起手就调 _stopSSE()，而 _showChat / _startPoll / _checkSseAuth 等多处都会
        //   调 _startSSE，导致 WS 反复建连/断开（conn_id=1 7ms 断开）。
        //   现按状态机：
        //     IDLE → 直接建连（无需先 _stopSSE，因为根本没有活动连接）
        //     CONNECTING/OPEN → 已有连接，直接 return（保留兜底轮询）
        //     CLOSING → 正在关闭，等 onclose 后由调用方再调 _startSSE
        if (_wsState === 'connecting' || _wsState === 'open') {
            _startPollFallback();
            return;
        }
        if (_wsState === 'closing') {
            // 等 onclose 触发后由调用方再调 _startSSE（避免与正在关闭的 _ws 冲突）
            return;
        }
        // _wsState === 'idle'，可以建连
        // FIX 2026-07-19: 不再以 _state.token 为门槛判断是否建 WS。
        //   H-7 (2026-07-18) 重构后 token 移到了 HttpOnly Cookie，JS 拿不到 token，
        //   _state.token 永远为空字符串。auth.html 登录成功也只跳转到 /chat，
        //   没有任何地方把 token 写回 _state.token（refresh-session 才有这逻辑，
        //   而 refresh-session 又被 zn-main.js 的 if (!_state.token) return 守卫拦住，
        //   形成"token 为空→不刷新→token 继续为空"的死锁）。
        //   Cookie 鉴权由浏览器自动处理：同源 WS upgrade 会自动带 Cookie，
        //   server 端 api_ws_upgrade 已支持 cookie 提取（extract_session_token）。
        //   鉴权失败时 server 返回 401，onclose 会触发指数退避重连，
        //   不会刷屏——且未登录用户会被 chat.html 顶部 XHR 探测 /api/wasm/me
        //   跳转到 /auth（chat.html:374-378），多道保护。

        // WS 建立期间先用轮询兜底，防止消息丢失
        _startPollFallback();
        _esConnectingSince = Date.now();
        _wsState = 'connecting';
        // FIX M3 (2026-07-20): 防御性重置 _esReconnectDelay，避免极罕见场景下
        //   onopen 未触发但 delay 已被前次 onclose 翻倍累积，
        //   导致第二次断网后首次重连等待时间过长。
        _esReconnectDelay = _WS_RECONNECT_BACKOFF_BASE;

        // 构建 WebSocket URL（同源 ws:// 或 wss://）
        // FIX F-7/H-7 (2026-07-18): 不再把 token 放入 URL query（会进入反代访问日志/Referer）。
        //   浏览器同源 WS 自动携带 HttpOnly Cookie (session_token)，后端 api_ws_upgrade 优先从
        //   cookie 提取 token，已无需 URL 传参。URL token 仅供旧客户端/curl 调试。
        //   保留 location.search 中已有 token 的清理逻辑（防御性，处理历史链接残留）。
        var wsProto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        var wsUrl = wsProto + '//' + location.host + '/api/ws';
        try {
            _ws = new WebSocket(wsUrl);
        } catch (err) {
            console.warn("[WS] create failed:", err);
            _wsState = 'idle';
            _startPollFallback();
            return;
        }

        _ws.onopen = function() {
            _esFailedCount = 0;
            _esReconnectDelay = _WS_RECONNECT_BACKOFF_BASE;
            _esConnectingSince = 0;
            _wsState = 'open';
            // Phase 5 (S8): WS token 收口——连接成功后清掉 URL 中可能残留的 token 参数，
            //   防止通过历史记录/Referer 泄露（防御性：当前 token 来自 __ZN_SESSION_TOKEN 而非 URL）
            try {
                if (location.search && /[?&](token|session_token)=/.test(location.search)) {
                    var cleanUrl = location.pathname + location.hash;
                    history.replaceState(null, "", cleanUrl);
                }
            } catch (e) { /* ignore */ }
            // 连接成功 → 全量同步
            _fullSync("ws_open");
            // FIX 2026-07-15: 不停止轮询，改为降低频率作为兜底。
            //   参考 openilink-hub：WS 推送可能因各种原因失败（broker 滞后、连接断开等），
            //   保持低频轮询确保消息始终能到达。
            _lowerPollFrequency();
            // FIX S16: 启动应用层 ping（25s 一次），防止 60s 空闲被代理/网关断开
            // FIX L13 (2026-07-20): ping 发送时记录时间戳，收到 pong 计算 RTT
            if (_wsPingTimer) clearInterval(_wsPingTimer);
            _wsPingTimer = setInterval(function() {
                if (_ws && _ws.readyState === _WS_OPEN) {
                    try {
                        _state._pingSentAt = Date.now();
                        _ws.send(JSON.stringify({type: "ping"}));
                    } catch (e) {}
                }
            }, 25000);
            _updateConnStatus();
        };

        _ws.onmessage = function(ev) {
            try {
                var envelope = JSON.parse(ev.data);
                if (!envelope || !envelope.event) return;
                var eventType = envelope.event;
                var data = envelope.data;
                switch (eventType) {
                    case "message":
                        // FIX 2026-07-15: 参考 openilink-hub "WS 通知 + REST 数据" 模式。
                        //   WS 收到 message 事件后，不直接用 WS 数据渲染，
                        //   而是触发 _fetchMessages 从 REST 拉取最新消息。
                        //   这样渲染统一走 REST 路径，彻底消除 WS 与 _loadHistory 的竞态。
                        //   出站消息先处理 pending 元素移除（即时 UI 反馈）。
                        if (data && data.type === 'out') {
                            // FIX M7 (2026-07-20): 兜底 typeof，避免冷启动时 _handleIncomingMessage 未定义抛错。
                            if (typeof _handleIncomingMessage === 'function') _handleIncomingMessage(data);
                        }
                        _scheduleFetchMessages();
                        break;
                    case "status":
                        _handleStatusEvent(data);
                        break;
                    case "user":
                        if (data.users) _state.users = data.users;
                        if (data.current_user) _state.currentUser = data.current_user;
                        if (_state.view === 'list') _renderChatList();
                        break;
                    case "ping":
                        /* keep-alive */
                        break;
                    // FIX L13 (2026-07-20): 收到 pong 计算 RTT，维护最近 5 次滑动平均
                    case "pong":
                        if (_state._pingSentAt > 0) {
                            var rtt = Date.now() - _state._pingSentAt;
                            _state._pingSentAt = 0;
                            _state._rttHistory.push(rtt);
                            if (_state._rttHistory.length > 5) _state._rttHistory.shift();
                            var sum = 0;
                            for (var i = 0; i < _state._rttHistory.length; i++) sum += _state._rttHistory[i];
                            _state._rttAvg = Math.round(sum / _state._rttHistory.length);
                            // RTT 突变（>500ms）立即刷新状态条
                            _updateConnStatus();
                        }
                        break;
                    case "send_ack":
                        _handleSendAck(data);
                        break;
                    case "session_status":
                        _handleSessionStatus(data);
                        break;
                    case "media_cache_update":
                        _handleMediaCacheUpdate(data);
                        break;
                    case "sync_required":
                        // Client lagged, force re-sync
                        _fullSync("sync_required");
                        break;
                    case "global_notification":
                        _handleGlobalNotification(data);
                        break;
                    // REPL /notify 和 /broadcast 通过 broker publish "notification" 事件。
                    //   /notify data 含 target_uid（仅目标用户可见）
                    //   /broadcast data 无 target_uid（全局广播，所有用户可见）
                    //   按 target_uid 过滤：非目标用户忽略，目标用户/全局 → 显示
                    case "notification":
                        if (data && data.target_uid && data.target_uid !== _state.webUid) {
                            // 私信通知但不是给当前用户 → 忽略
                            break;
                        }
                        _handleGlobalNotification(data);
                        break;
                    // FIX U4 (2026-07-20): QR 登录状态变更事件——事件驱动替代 1.5-3s 轮询。
                    //   后端 set_qr_login_state 每次状态变化均推送，前端立即响应：
                    //   - confirmed + login_done：立即 _checkStatus 触发 _showChat
                    //   - 其他状态：立即触发 _loadQR 拉取最新 matrix + 更新三态 UI
                    //   _qrPollActive=false（已进入聊天视图）时忽略，避免 reauth 干扰。
                    case "qr_state":
                        if (typeof _qrPollActive !== 'undefined' && _qrPollActive) {
                            var qrSt = (data && data.state) || "";
                            var qrDone = !!(data && data.login_done);
                            if (qrSt === "confirmed" && qrDone && typeof _checkStatus === 'function') {
                                _stopQRPoll();
                                _checkStatus().catch(function(err) {
                                    console.warn("[qr_state] checkStatus error:", err && err.message || err);
                                });
                            } else if (typeof _loadQR === 'function') {
                                _loadQR();
                            }
                        }
                        break;
                }
            } catch (e) {
                console.warn("[WS] parse error", e);
            }
        };

        _ws.onclose = function(ev) {
            console.warn("[WS] closed code=" + ev.code + " reason=" + ev.reason);
            // FIX P1-15 (2026-07-20): 状态机——onclose 后回到 idle，允许重连。
            _wsState = 'idle';
            _ws = null;
            _updateConnStatus();
            _esFailedCount += 1;

            // FIX 2026-07-15: WS 断开时恢复快速轮询，确保消息不丢
            _startPollFallback();

            // 检查 token 是否过期
            _checkSseAuth();

            if (_esFailedCount >= _ES_MAX_FAIL) {
                console.warn("[NETWORK] WS 连续失败 " + _esFailedCount + " 次，降级为轮询 + 延迟重连");
            }
            // FIX S43: 若正在刷新 token，不要立即重连，等刷新完成由 _checkSseAuth 路径触发 _startSSE
            if (_refreshingToken) {
                _reconnectPending = true;
                return;
            }
            if (!_esReconnectTimer) {
                _esReconnectTimer = setTimeout(function() {
                    _esReconnectTimer = null;
                    _startSSE();
                }, _esReconnectDelay);
                _esReconnectDelay = Math.min(_esReconnectDelay * 2, 30000);
            }
        };

        _ws.onerror = function(err) {
            console.warn("[WS] error:", err);
            _updateConnStatus();
        };
    };

    // ── 分离出的 status 事件处理（WS/SSE 共用）──────────
    const _handleStatusEvent = function(s) {
        if (s.login_done && s.logged_in) {
            // FIX 2026-07-16: 收到 login_done 后主动停止 QR 轮询，
            //   避免与 _showChat 竞态双触发（QR 轮询也会检测到 login_done 并调 _showChat）。
            if (typeof _stopQRPoll === 'function') _stopQRPoll();
            // FIX 2026-07-16: 如果已在聊天视图，只更新 users 列表，不重复 _showChat。
            //   避免后端多次推送 status 事件导致重复 toast 和 UI 重置。
            if (_state.view === 'list') {
                var newUsers = s.users || [];
                var usersChanged = newUsers.length !== (_state.users || []).length ||
                    newUsers.slice().sort().join(',') !== (_state.users || []).slice().sort().join(',');
                if (usersChanged) {
                    _state.users = newUsers;
                    if (s.current_user) _state.currentUser = s.current_user;
                    _renderChatList();
                    _loadChatListPreviews();
                }
                return;
            }
            // 首次进入聊天视图
            if (typeof _showChat === 'function') {
                _showChat(s);
                return;
            }
            return;
        }
        if (s.session_expired) {
            var expiredUsers = s.expired_users || [];
            var hasValidAccounts = s.has_valid_accounts;
            if (hasValidAccounts && expiredUsers.length > 0) {
                _toast("一个账号会话已过期，已移除相关用户");
                expiredUsers.forEach(function(uid) {
                    if (_state.displayedIds[uid]) {
                        delete _state.displayedIds[uid];
                    }
                });
                if (s.users && Array.isArray(s.users)) {
                    _state.users = s.users;
                } else {
                    _state.users = _state.users.filter(function(u) {
                        return expiredUsers.indexOf(u) === -1;
                    });
                }
                if (expiredUsers.indexOf(_state.currentUser) !== -1) {
                    _state.currentUser = s.current_user || (_state.users.length > 0 ? _state.users[0] : null);
                }
                if (_state.view === 'chat' && !_state.currentUser) {
                    _backToChatList();
                } else if (_state.view === 'list') {
                    _renderChatList();
                } else if (_state.view === 'chat' && _state.currentUser) {
                    _openChat(_state.currentUser);
                }
            } else {
                // 所有账号均已过期：显示 banner 提示，而非立即跳转二维码
                // 用户可选择点击"重新扫码"或进入设置页操作
                _toast("iLink 会话已过期，请重新扫码连接");
                _state.users = [];
                _state.currentUser = null;
                _state.displayedIds = {};
                _state.pendingByReqId = {};
                _state.pendingByRowId = {};
                _state.pendingByClientId = {};
                // 显示状态栏 banner（含"重新扫码"按钮）
                if (typeof _renderSessionBanner === 'function') {
                    _renderSessionBanner(true, "iLink 会话已过期，请重新扫码连接", []);
                }
                // FIX P0: 退出聊天视图，但保留 chat-list-page 可见（否则所有页面 display:none → 白屏）
                var chatPage = document.getElementById("chat-page");
                if (chatPage) chatPage.classList.remove("active");
                var chatListPage = document.getElementById("chat-list-page");
                if (chatListPage) chatListPage.classList.add("active");
                _state.view = 'list';
                _renderChatList();
            }
            return;
        }
        if (s.users && Array.isArray(s.users)) {
            _state.users = s.users;
            if (s.current_user && (!_state.currentUser || _state.view === 'list' || _state.view === 'users')) {
                _state.currentUser = s.current_user;
            }
            if (_state.view === 'list') _renderChatList();
        }
        if (s.poll_health) {
            _state.pollHealth = s.poll_health;
            _updateConnStatus();
            if (_state.view === 'list') _renderChatList();
        }
    };

    // ── 分离出的 media_cache_update 处理 ──────────────────
    const _handleMediaCacheUpdate = function(data) {
        if (!data || !data.cache_key) return;
        var cacheKey = data.cache_key;
        var cdnInfo = data.cdn_info;
        var msgId = data.msg_id;
        var status = data.status || "";

        if (msgId) {
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 msgId 含 "]" 等特殊字符逃逸 CSS 选择器；再手动排除 data-sending-id 元素。
            var msgRow = _findByDataAttr('data-msg-id', msgId);
            if (msgRow && msgRow.hasAttribute('data-sending-id')) msgRow = null;
            if (msgRow && msgRow._msgData && !msgRow._msgData.media_cache_id) {
                msgRow._msgData.media_cache_id = cacheKey;
                var spinner = msgRow.querySelector('.msg-send-loading');
                if (spinner) spinner.remove();
                var fileEl = msgRow.querySelector('[data-cdn-file="1"]');
                if (fileEl) {
                    var hint = fileEl.querySelector('.bubble-media-file-hint');
                    if (hint) hint.textContent = '点击下载';
                }
            }
        }

        // FIX 2026-07-15: 当后端预取完成 (status=ready/cached) 时，
        //   重载所有引用此 cache_key 的 <img>/<video>（之前因缓存未命中 404 显示占位图）。
        // FIX S14: WebDAV 模式下 img.src 是 proxy URL 不含 cacheKey，
        //   改为同时匹配 data-cache-key 属性（在 zn-render.js / zn-media.js 中渲染时附加）。
        // FIX S44: cacheKey 直接拼进 CSS 选择器未转义，改用 querySelectorAll + 遍历检查 dataset.cacheKey。
        if (status === "ready" || status === "cached") {
            var mediaUrl = '/api/wasm/media/' + cacheKey;
            var allImgs = document.querySelectorAll('img.bubble-media-img');
            allImgs.forEach(function(img) {
                // FIX S14: data-cache-key 可能附加在 img 自身或祖先 .bubble-media-img-wrap 上
                var imgCacheKey = img.dataset.cacheKey || null;
                if (!imgCacheKey) {
                    var wrap = img.closest('.bubble-media-img-wrap');
                    imgCacheKey = wrap ? wrap.dataset.cacheKey : null;
                }
                if (imgCacheKey === cacheKey ||
                    img.src.indexOf(mediaUrl) !== -1 ||
                    img.src.indexOf(cacheKey) !== -1) {
                    // 强制重载：附加时间戳绕过浏览器缓存
                    img.src = mediaUrl + '?t=' + Date.now();
                }
            });
            var allVids = document.querySelectorAll('video.bubble-media-video-thumb-vid');
            allVids.forEach(function(vid) {
                // FIX S14: data-cache-key 可能附加在 .bubble-media-video-thumb 或 .bubble-media-img-wrap 上
                var vidCacheKey = vid.dataset.cacheKey || null;
                if (!vidCacheKey) {
                    var thumb = vid.closest('.bubble-media-video-thumb');
                    vidCacheKey = thumb ? thumb.dataset.cacheKey : null;
                }
                if (!vidCacheKey) {
                    var wrap = vid.closest('.bubble-media-img-wrap');
                    vidCacheKey = wrap ? wrap.dataset.cacheKey : null;
                }
                if (vidCacheKey === cacheKey ||
                    vid.src.indexOf(mediaUrl) !== -1 ||
                    vid.src.indexOf(cacheKey) !== -1) {
                    vid.src = mediaUrl + '?t=' + Date.now();
                    vid.load();
                }
            });
            // 也处理 loading 占位元素中的 data-cdn 匹配
        }

        var loadingEls = document.querySelectorAll('.bubble-media-loading[data-cdn]');
        for (var i = 0; i < loadingEls.length; i++) {
            var el = loadingEls[i];
            var elCdn = el.dataset.cdn || "";
            var elCdnObj = null;
            try { elCdnObj = JSON.parse(elCdn); } catch(e) {}
            var match = false;
            if (cdnInfo && elCdnObj) {
                // FIX S66: 改为比较具体字段（encrypt_query_param / aes_key / file_size），
                //   JSON.stringify 对象键顺序不确定，可能误判不匹配
                var c1 = cdnInfo.encrypt_query_param || cdnInfo.encrypted_query_param || "";
                var c2 = elCdnObj.encrypt_query_param || elCdnObj.encrypted_query_param || "";
                var k1 = cdnInfo.aes_key || "";
                var k2 = elCdnObj.aes_key || "";
                var s1 = cdnInfo.file_size || 0;
                var s2 = elCdnObj.file_size || 0;
                match = (c1 === c2) && (k1 === k2) && (s1 === s2) && (!!c1 || !!k1);
            } else if (cdnInfo && elCdn) {
                // FIX S66: 字符串比对，只在两边都是字符串时才直接对比
                if (typeof elCdn === 'string' && typeof cdnInfo === 'string') {
                    match = (elCdn === cdnInfo);
                } else if (typeof elCdn === 'string') {
                    // elCdn 是 JSON 字符串，重新 parse 后按字段比对
                    try {
                        var parsedCdn = JSON.parse(elCdn);
                        var c1b = cdnInfo.encrypt_query_param || cdnInfo.encrypted_query_param || "";
                        var c2b = parsedCdn.encrypt_query_param || parsedCdn.encrypted_query_param || "";
                        var k1b = cdnInfo.aes_key || "";
                        var k2b = parsedCdn.aes_key || "";
                        var s1b = cdnInfo.file_size || 0;
                        var s2b = parsedCdn.file_size || 0;
                        match = (c1b === c2b) && (k1b === k2b) && (s1b === s2b) && (!!c1b || !!k1b);
                    } catch(e2) {}
                }
            }
            if (!match) continue;

            var mediaType = el.dataset.mediaType || "image";
            var cacheUrl = '/api/wasm/media/' + cacheKey;
            el.removeAttribute("data-loading");
            el.classList.remove("bubble-media-loading");
            el.removeAttribute("data-cdn");
            el.removeAttribute("data-media-type");

            if (mediaType === "voice") {
                el.dataset.action = 'play-voice';
                el.dataset.cacheId = cacheKey;
                var voiceRow = el.closest('.msg-row');
                var voiceMd = voiceRow && voiceRow._msgData;
                var voiceDur = (voiceMd && voiceMd.media_duration) ? Math.ceil(voiceMd.media_duration / 1000) : 1;
                // FIX M2/M5 (2026-07-20): 收敛到 _voiceBarHtml（zn-core.js），与 _renderMsg 共享。
                var voiceBars = _voiceBarHtml(voiceMd, voiceDur);
                el.innerHTML = _svgVoice + '<div class="bubble-media-voice-bars">' + voiceBars + '</div><div class="bubble-media-voice-dur">' + voiceDur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div>';
            } else if (mediaType === "video") {
                el.innerHTML = '<div class="bubble-media-video-thumb" data-cache-key="' + _escapeAttr(String(cacheKey)) + '" data-action="play-video" data-video-src="' + _escapeAttr(cacheUrl) + '"><video class="bubble-media-video-thumb-vid" src="' + _escapeAttr(cacheUrl) + '" preload="metadata" muted playsinline></video><div class="bubble-media-play-btn">' + _svgPlay + '</div></div>';
            } else if (mediaType === "image") {
                // FIX S14: 同时附加 data-cache-key，方便后续 media_cache_update 重载
                el.innerHTML = '<img class="bubble-media-img" data-cache-key="' + _escapeAttr(String(cacheKey)) + '" src="' + _escapeAttr(cacheUrl) + '" alt="图片" />';
            } else {
                el.dataset.cacheId = cacheKey;
            }
            var row = el.closest('.msg-row');
            if (row) {
                if (row._msgData) row._msgData.media_cache_id = cacheKey;
                var spinner2 = row.querySelector('.msg-send-loading');
                if (spinner2) spinner2.remove();
            }
        }
    };

    // ── 全局通知处理 ──────────────────────────────────────────
    function _handleGlobalNotification(data) {
        var bars = [document.getElementById("global-notification-bar"), document.getElementById("global-notification-bar-chat")];
        if (!data || !data.message || data.level === "clear") {
            bars.forEach(function(bar) {
                if (bar) { bar.className = "global-notification-bar"; bar.style.display = "none"; bar.innerHTML = ""; }
            });
            return;
        }
        var level = data.level || "info";
        bars.forEach(function(bar) {
            if (!bar) return;
            bar.className = "global-notification-bar show " + level;
            bar.style.display = "block";
            // FIX L7 (2026-07-20): 改用 addEventListener 绑定关闭按钮，
            //   避免 inline onclick 在 CSP 严格模式下被拦截。
            bar.innerHTML = '<span>' + _escape(data.message) + '</span><button class="notif-close" type="button">&times;</button>';
            var closeBtn = bar.querySelector('.notif-close');
            if (closeBtn) {
                closeBtn.addEventListener('click', function() {
                    bar.style.display = 'none';
                });
            }
        });
    }

    // Fetch current global notification on startup
    fetch('/api/wasm/notification').then(function(r) { return r.json(); }).then(function(res) {
        if (res.success && res.notification && res.notification.message) {
            _handleGlobalNotification(res.notification);
        }
    }).catch(function() {});

    const _stopSSE = function() {
        // FIX P1-15 (2026-07-20): 显式状态机——_stopSSE 必须显式调用。
        //   设置 'closing' 状态，避免 _startSSE 在关闭过程中重复建连。
        //   onclose 触发后状态自动回 'idle'（_ws.onclose 内重置）。
        _wsState = 'closing';
        if (_esReconnectTimer) {
            clearTimeout(_esReconnectTimer);
            _esReconnectTimer = null;
        }
        // FIX S7: 清理 50ms 防抖定时器，避免泄漏（_stopSSE 之后仍可能触发已调度拉取）
        if (_fetchMsgTimer) {
            clearTimeout(_fetchMsgTimer);
            _fetchMsgTimer = null;
        }
        if (_wsPingTimer) {
            clearInterval(_wsPingTimer);
            _wsPingTimer = null;
        }
        if (_ws) {
            try { _ws.close(); } catch (e) {}
            _ws = null;
        }
        // 关闭完成后回到 idle（onclose 不会触发，因为 _ws 已置 null）
        _wsState = 'idle';
        _stopPollFallback();
    };

    // ── 通用：检查 token 是否过期（WS/SSE 共用）────
    let _sseAuthCheckPending = false;
    // FIX S43: 标记 token 正在刷新。WS onclose 触发重连时若正在刷新 token，
    //   应等刷新完成（由 _checkSseAuth 成功路径重新调用 _startSSE）再重连，
    //   否则旧 token 重连会立即 401，造成循环。
    let _refreshingToken = false;
    let _reconnectPending = false;
    const _checkSseAuth = function() {
        if (_sseAuthCheckPending) return;
        _sseAuthCheckPending = true;
        var xhr = new XMLHttpRequest();
        xhr.open("GET", "/api/wasm/stats", true);
        // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie。
        xhr.timeout = 5000;
        xhr.onload = function() {
            _sseAuthCheckPending = false;
            if (xhr.status === 401) {
                console.warn("[AUTH] token 可能已失效，尝试刷新");
                _refreshingToken = true;
                _get("refresh-session").then(function(res) {
                    _refreshingToken = false;
                    // FIX P0-8 (2026-07-20): refresh-session 不再在响应体返回 session_token，
                    //   仅通过 Set-Cookie 刷新浏览器 Cookie。判断 success 即可。
                    if (res && res.success) {
                        // 标记登录态（具体值不重要，仅用于触发 12h refresh / online 重连等逻辑）
                        _state.token = "httponly-cookie";
                        // FIX P0-5 (2026-07-20): 同步显式登录态布尔值，
                        //   替代原 _state.token 真值判断（Cookie 迁移后 _state.token 恒为 ""）。
                        _state.loggedIn = true;
                        _startSSE();
                        // FIX S43: 如果在刷新期间 WS onclose 标记了 reconnectPending，已在 _startSSE 中重连，清除标志
                        _reconnectPending = false;
                    } else {
                        console.warn("[AUTH] token 刷新失败，触发页面刷新");
                        _handle401();
                    }
                }).catch(function() {
                    _refreshingToken = false;
                    console.warn("[AUTH] token 刷新异常，触发页面刷新");
                    _handle401();
                });
            }
        };
        xhr.onerror = function() { _sseAuthCheckPending = false; };
        xhr.ontimeout = function() { _sseAuthCheckPending = false; };
        xhr.send();
    };

    // FIX U7 (2026-07-20): 单一 busy 标志改为按请求类型区分的三个独立标志。
    //   原实现 Promise.all 串联三个请求，其中任一（如 _fetchMessages）挂起会
    //   同时阻塞 _loadUsers 与 _loadChatListPreviews，导致用户列表/会话预览
    //   在消息接口异常时无法刷新。改为每个请求独立判断 + 独立 busy 标志后，
    //   单接口慢/失败不影响其他接口的正常轮询节奏。
    const _setPollInterval = function(ms) {
        if (_state.pollInterval) clearInterval(_state.pollInterval);
        var busyFetch = false, busyUsers = false, busyPreviews = false;
        _state.pollInterval = setInterval(function() {
            if (!busyFetch) {
                busyFetch = true;
                _fetchMessages().catch(function() {}).then(function() { busyFetch = false; });
            }
            if (!busyUsers) {
                busyUsers = true;
                _loadUsers().catch(function() {}).then(function() { busyUsers = false; });
            }
            if (!busyPreviews) {
                busyPreviews = true;
                _loadChatListPreviews().catch(function() {}).then(function() { busyPreviews = false; });
            }
        }, ms);
        _updateConnStatus();
    };

    const _startPollFallback = function() {
        if (_state.pollInterval) return;
        _setPollInterval(_POLL_FALLBACK_INTERVAL);
    };

    // FIX 2026-07-15: WS 连接成功时降低轮询频率（而非停止），作为兜底
    const _lowerPollFrequency = function() {
        _setPollInterval(_POLL_WS_CONNECTED_INTERVAL);
    };

    const _stopPollFallback = function() {
        if (_state.pollInterval) {
            clearInterval(_state.pollInterval);
            _state.pollInterval = null;
        }
        _updateConnStatus();
    };

    // 兼容旧接口名：仍叫 _startPoll
    // FIX 2026-07-15: 参考 Python 版，启动快速轮询作为主要消息获取方式。
    //   WS/SSE 仅作为通知触发立即轮询，确保消息始终能到达。
    //   之前只依赖 WS，WS 连接失败时消息完全丢失。
    const _startPoll = function() {
        _startPollFallback();  // 启动 1s 快速轮询
        _startSSE();           // 同时启动 WS（WS 成功后降频到 3s）
        // PR4: 启动时拉一次 session 状态，立即显示终态 banner
        if (_state._checkSessionStatus) _state._checkSessionStatus();
        // 立即拉一次 status，触发 login_done 检测（不等首次轮询）
        _fetchStatus();
    };

    // ─── 可见性变化：切回前台时强制全量同步 ───
    // 浏览器后台会节流 setInterval/SSE，可能错过消息；切回前台必须做一次全量同步
    const _onVisibilityChange = function() {
        // FIX P0-5 (2026-07-20): 用 _state.loggedIn 替换 _state.token 真值判断，
        //   Cookie 迁移后 _state.token 恒为 ""，原守卫导致切回标签页同步失效。
        if (document.visibilityState === 'visible' && _state.loggedIn) {
            // FIX S36: 历史加载进行中时不要重入 _loadHistory，否则会与正在进行的加载产生竞态
            if (_state._loadingHistory) return;
            // 先快速 fetchMessages 一次拿新消息
            _fetchMessages();
            // 如果在聊天视图，重拉 history（覆盖所有可能的丢失）
            if (_state.view === 'chat' && _state.currentUser) {
                _loadHistory(_state.currentUser);
            }
            _fetchStatus();
            // 同时确保 SSE 是连接状态
            if (!_ws || _ws.readyState !== _WS_OPEN) {
                _startSSE();
            }
        }
    };

    // ── PR3: 处理 send_ack 事件 ────────────────────────────
