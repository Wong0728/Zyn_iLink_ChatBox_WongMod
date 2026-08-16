    const _loadHistory = async function(userId) {
        _state._loadingHistory = true;
        // FIX U1 (2026-07-20): 切换聊天时关闭旧 observer，重置游标分页状态
        if (_state._historyObserver) {
            try { _state._historyObserver.disconnect(); } catch (e) {}
            _state._historyObserver = null;
            _state._historyObserverUser = null;
        }
        _state._historyOldestId = null;
        _state._historyHasMore = false;
        _state._loadingOlder = false;
        try {
            // FIX U1 (2026-07-20): 默认 50 条（原 500 条），上拉加载更多
            const t = userId ? `history?user=${encodeURIComponent(userId)}&limit=50` : "history?limit=50";
            var n;
            try {
                n = await _get(t);
            } catch(err) {
                console.error("[history] fetch error:", err);
                var ma = document.getElementById("messages-area");
                if (ma) ma.innerHTML = '<div class="empty-state"><div class="empty-state-icon"><svg viewBox="0 0 24 24" width="48" height="48" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg></div><div>加载失败: ' + _escape(String(err.message || err)) + '</div><div style="font-size:11px;color:var(--text-hint);margin-top:4px;">请刷新页面或检查后端日志</div></div>';
                return;
            }
            if (!n || n.error) {
                var ma = document.getElementById("messages-area");
                if (ma) ma.innerHTML = '<div class="empty-state"><div class="empty-state-icon"><svg viewBox="0 0 24 24" width="48" height="48" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg></div><div>' + _escape((n && n.error) || '加载失败') + '</div></div>';
                return;
            }
            const o = n.messages || [];
            const i = document.getElementById("messages-area");
            if (!i) return;
            if (o.length === 0) {
                i.innerHTML = '<div class="empty-state"><div class="empty-state-icon"><svg viewBox="0 0 24 24" width="48" height="48" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"/></svg></div><div>暂无消息</div></div>';
                return;
            }
            // 重建 DOM 前，先为当前用户创建独立的 displayedIds 集合
            if (userId) {
                _state.displayedIds[userId] = new Set();
            }
            i.innerHTML = "";
            // FIX U1: 顶部 sentinel 元素，IntersectionObserver 监听它触发加载更早消息
            var sentinel = document.createElement("div");
            sentinel.className = "history-sentinel";
            sentinel.setAttribute("aria-hidden", "true");
            i.appendChild(sentinel);
            o.forEach((function(msg) {
                // FIX 2026-07-20: 去重键已做命名空间隔离（id:/row:/client:/req:/media:），
                //   不再使用无命名空间的 dedupKey，避免 messages.id 与 messages_v2.row_id 互相碰撞。
                if (userId && _state.displayedIds[userId] &&
                    typeof _anyKeyDisplayed === 'function' &&
                    _anyKeyDisplayed(_state.displayedIds[userId], msg)) {
                    return;
                }
                _renderMsg(msg);
                if (userId) {
                    if (!_state.displayedIds[userId]) _state.displayedIds[userId] = new Set();
                    if (typeof _addAllDedupKeys === 'function') {
                        _addAllDedupKeys(_state.displayedIds[userId], msg);
                    }
                }
            }));
            const e2 = Math.max.apply(null, o.map((function(e) {
                return (typeof e.id === "number") ? e.id : 0;
            })));
            _bumpLastMsgId(e2);
            // FIX U1: 记录最旧消息 id（ASC 顺序，第一条最旧）+ has_more 状态
            _state._historyOldestId = (typeof o[0].id === 'number' && o[0].id > 0) ? o[0].id : null;
            _state._historyHasMore = !!n.has_more;
            // FIX U1: 启动 IntersectionObserver 监听 sentinel
            _setupHistoryObserver(userId, sentinel);
            i.scrollTop = i.scrollHeight;
        } finally {
            _state._loadingHistory = false;
            // FIX 2026-07-15: 处理历史加载期间缓存的消息
            // FIX L15 (2026-07-20): 单条消息处理抛错会中断后续 forEach，逐条 try/catch 隔离。
            if (_state._messageQueue && _state._messageQueue.length > 0) {
                var queuedMsgs = _state._messageQueue.splice(0, _state._messageQueue.length);
                queuedMsgs.forEach(function(msg) {
                    try {
                        // FIX M7 (2026-07-20): 兜底 typeof 守卫，防止冷启动时函数未定义。
                        if (typeof _handleIncomingMessage === 'function') _handleIncomingMessage(msg);
                    } catch (e) {
                        console.warn("[history] queue handler error:", e && e.message || e);
                    }
                });
            }
            // FIX P1-16 (2026-07-20): 历史加载完成后立即拉取一次增量。
            //   原实现 _fetchMessages 在 _loadingHistory 期间直接 return，导致历史加载
            //   期间通过 REST 轮询到达的新消息被丢弃（队列只接 WS 事件）。
            //   _loadHistory 完成后 _bumpLastMsgId 已更新 _state.lastMsgId 为历史最大 id，
            //   此处调用 _fetchMessages 会基于新 lastMsgId 拉取增量（包括加载期间漏掉的消息）。
            //   不入队方案（替代：把 REST 消息也入队）——入队会导致重复处理，
            //   而 _fetchMessages 内部已有 displayedIds 去重保护，直接调用更安全。
            try {
                _fetchMessages().catch(function(err) {
                    console.warn("[history] post-load fetch error:", err && err.message || err);
                });
            } catch (e) { /* ignore */ }
        }
    };

    // FIX U1 (2026-07-20): IntersectionObserver 监听顶部 sentinel，
    //   进入视口时触发 _loadOlderMessages 拉取更早消息。
    //   observer 与 userId 绑定，切换聊天时 _loadHistory 会 disconnect 旧实例。
    const _setupHistoryObserver = function(userId, sentinel) {
        if (!sentinel) return;
        // 不支持 IntersectionObserver 的浏览器（旧 Safari）静默降级——仅显示首屏 50 条
        if (typeof IntersectionObserver === 'undefined') return;
        var observer = new IntersectionObserver(function(entries) {
            entries.forEach(function(entry) {
                if (entry.isIntersecting && _state._historyHasMore && !_state._loadingOlder && !_state._loadingHistory) {
                    _loadOlderMessages(userId);
                }
            });
            // rootMargin: 顶部 200px 内触发，避免用户必须滚到最顶端
        }, {
            root: document.getElementById("messages-area"),
            rootMargin: "200px 0px 0px 0px",
            threshold: 0
        });
        observer.observe(sentinel);
        _state._historyObserver = observer;
        _state._historyObserverUser = userId;
    };

    // FIX U1 (2026-07-20): 上拉加载更早消息。
    //   - 调用 history?before=oldestId 拉取 id < oldestId 的 50 条
    //   - 反向遍历（新→旧），每条 _renderMsg 后 insertBefore(sentinel.nextSibling)
    //     使最旧消息紧贴 sentinel 之后，最新加载消息靠近原首条
    //   - 保持滚动位置：scrollTop += (newScrollHeight - prevScrollHeight)
    const _loadOlderMessages = async function(userId) {
        if (_state._loadingOlder) return;
        if (!_state._historyHasMore) return;
        if (!userId || _state._historyObserverUser !== userId) return;
        if (!_state._historyOldestId) return;
        var area = document.getElementById("messages-area");
        if (!area) return;
        var sentinel = area.querySelector(".history-sentinel");
        if (!sentinel) return;

        _state._loadingOlder = true;
        sentinel.classList.add("loading");
        try {
            var url = "history?user=" + encodeURIComponent(userId) + "&limit=50&before=" + encodeURIComponent(_state._historyOldestId);
            var res = await _get(url);
            if (!res || res.error) return;
            var msgs = res.messages || [];
            if (msgs.length === 0) {
                _state._historyHasMore = false;
                return;
            }
            // 更新 has_more（后端基于多查 1 条判断）
            _state._historyHasMore = !!res.has_more;
            // ASC 顺序第一条最旧，作为下次 before 游标
            var newOldest = (typeof msgs[0].id === 'number' && msgs[0].id > 0) ? msgs[0].id : null;
            if (newOldest) _state._historyOldestId = newOldest;

            // 保存滚动位置（用于渲染后恢复视觉位置）
            var prevScrollHeight = area.scrollHeight;
            var prevScrollTop = area.scrollTop;

            // 反向遍历（新→旧），让最旧消息最终紧贴 sentinel 之后
            for (var idx = msgs.length - 1; idx >= 0; idx--) {
                var msg = msgs[idx];
                if (!_state.displayedIds[userId]) _state.displayedIds[userId] = new Set();
                if (typeof _anyKeyDisplayed === 'function' && _anyKeyDisplayed(_state.displayedIds[userId], msg)) {
                    if (typeof _addAllDedupKeys === 'function') _addAllDedupKeys(_state.displayedIds[userId], msg);
                    continue;
                }
                var rendered = _renderMsg(msg);
                if (rendered) {
                    // 把 appendChild 加到底部的元素移到 sentinel 之后
                    area.insertBefore(rendered, sentinel.nextSibling);
                    if (typeof _addAllDedupKeys === 'function') {
                        _addAllDedupKeys(_state.displayedIds[userId], msg);
                    }
                }
            }
            // 恢复滚动位置：新增高度 = 新 scrollHeight - 旧 scrollHeight，
            //   保持用户视觉位置不变（顶部新增的旧消息推到视口上方）
            var newScrollHeight = area.scrollHeight;
            area.scrollTop = prevScrollTop + (newScrollHeight - prevScrollHeight);
        } catch (err) {
            console.warn("[history] load older error:", err && err.message || err);
        } finally {
            _state._loadingOlder = false;
            sentinel.classList.remove("loading");
        }
    };
    
    // FIX 2026-07-20: 统一去重辅助函数（已加命名空间隔离）。
    //   问题 1：OUTBOUND 消息走 sync send 时 add_message_to_history 返回的 msg.id=0，
    //   WS 事件和 HTTP 响应都用 client_id 作为 dedupKey；
    //   但 REST 轮询时 parse_msg_with_id 把 id 覆盖为 DB 自增 id（如 22），
    //   导致 dedupKey=22 与之前的 client_id 不匹配，重复渲染气泡。
    //   问题 2（关键）：messages 表自增 id 与 messages_v2 表 row_id 是两个独立序列，
    //   若直接放进同一个 Set，id=N 的消息会与 row_id=N 的消息互相误判为重复，
    //   导致整行消息被跳过（本次表现为所有偶数 id 入站消息消失）。
    //   解决：用 "id:123"、"row:456"、"client:..."、"req:..."、"media:..." 做命名空间键；
    //         渲染前检查所有可用键，任意命中即跳过；渲染后补全所有键，防止后续重复。
    // FIX 2026-07-16: 媒体消息（图片/视频/文件/语音）补 media_cache_id 作为去重键。
    //   根因：OUTBOUND 媒体消息在后端 send_media_message 中构造的 out_msg 不含
    //   client_id/req_id/row_id（id=0），upload response、WS event、REST poll 三路
    //   只有 media_cache_id 是稳定的（同一文件同一 cache_key），用它能跨三路去重，
    //   否则图片会被渲染 3 次（setTimeout + WS echo + REST 轮询各一次）。
    const _anyKeyDisplayed = function(set, e) {
        if (!set) return false;
        if (typeof e.id === 'number' && e.id > 0 && set.has('id:' + e.id)) return true;
        if (typeof e.row_id === 'number' && e.row_id > 0 && set.has('row:' + e.row_id)) return true;
        if (e.client_id && set.has('client:' + e.client_id)) return true;
        if (e.req_id && set.has('req:' + e.req_id)) return true;
        if (e.media_cache_id && set.has('media:' + e.media_cache_id)) return true;
        return false;
    };
    const _addAllDedupKeys = function(set, e) {
        if (!set) return;
        if (typeof e.id === 'number' && e.id > 0) set.add('id:' + e.id);
        if (typeof e.row_id === 'number' && e.row_id > 0) set.add('row:' + e.row_id);
        if (e.client_id) set.add('client:' + e.client_id);
        if (e.req_id) set.add('req:' + e.req_id);
        if (e.media_cache_id) set.add('media:' + e.media_cache_id);
    };

    const _fetchMessages = async function() {
        // FIX: During history load, skip fetch entirely. Messages arriving via WS
        // are queued and processed after _loadHistory completes.
        if (_state._loadingHistory) return;
        const userParam = _state.currentUser ? "&user=" + encodeURIComponent(_state.currentUser) : "";
        try {
            const t = await _get("messages?since=" + _state.lastMsgId + userParam);
            if (t && t.messages) {
                t.messages.forEach((function(e) {
                    var targetUser = _normalizeId(e.type === 'in' ? e.from : e.to) || _state.currentUser;
                    if (!targetUser) return;
                    if (!_state.displayedIds[targetUser]) _state.displayedIds[targetUser] = new Set();
                    if (_anyKeyDisplayed(_state.displayedIds[targetUser], e)) {
                        _addAllDedupKeys(_state.displayedIds[targetUser], e);
                        if (typeof e.id === 'number' && e.id > 0) _bumpLastMsgId(e.id);
                        return;
                    }
                    if (_state.view === 'chat' && _state.currentUser && targetUser === _state.currentUser) {
                        _renderMsg(e);
                    }
                    _addAllDedupKeys(_state.displayedIds[targetUser], e);
                    if (typeof e.id === 'number' && e.id > 0) {
                        _bumpLastMsgId(e.id);
                    }
                    var fromUser = e.from || _state.currentUser;
                    if (fromUser) {
                        _state.lastMessages[fromUser] = {
                            text: e.text || '',
                            time: e.time || '',
                            media_type: e.media_type
                        };
                    }
                }));
                if (_state.view === 'list') {
                    _renderChatList();
                }
            }
        } catch (err) {
            console.warn("[fetch-messages] error:", err && err.message || err);
        }
    };

