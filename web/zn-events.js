    // 后端异步发送时按状态机推送 ACK：
    //   pending (本地插入) → sending (开始 POST) → sent (HTTP 200)
    //   → delivered (SSE 反向 ACK) / failed (重试用尽) / expired (会话过期)
    // 前端用 req_id / row_id / client_id 三种 key 匹配到对应 pending DOM 元素，更新其视觉状态
    const _handleSendAck = function(ack) {
        if (!ack) return;
        var state = ack.state || "";
        var reqId = ack.req_id || "";
        var clientId = ack.client_id || "";
        var rowId = ack.row_id;

        // 命中 pending DOM 元素
        var pendingId = null;
        if (reqId && _state.pendingByReqId[reqId]) pendingId = _state.pendingByReqId[reqId];
        else if (rowId && _state.pendingByRowId[rowId]) pendingId = _state.pendingByRowId[rowId];
        else if (clientId && _state.pendingByClientId[clientId]) pendingId = _state.pendingByClientId[clientId];

        // 已被后端真实消息替换（成功投递后 SSE 推送了带数字 id 的同条消息，pending 元素已被 _handleIncomingMessage 移除）
        // 此种情况需在真实消息 DOM 上更新状态图标，不能直接返回让 spinner 卡住
        if (!pendingId) {
            // 取消该 req_id 的延迟失败 toast（如果有）
            if (reqId && _state._pendingFailureToast[reqId]) {
                clearTimeout(_state._pendingFailureToast[reqId]);
                delete _state._pendingFailureToast[reqId];
            }
            // FIX 2026-07-15: pending 元素已被真实消息替换后，仍需在真实元素上推进状态：
            //   - sent/delivered：清掉 spinner（避免前端看到永久转圈，又因转圈时间长而误以为发送失败）
            //   - failed/expired：打上红色感叹号 + toast
            var realEl = null;
            // 优先通过 rowId 查找
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 rowId 含 "]" 等特殊字符逃逸 CSS 选择器。
            if (rowId) {
                realEl = _findByDataAttr('data-msg-id', rowId) || _findByDataAttr('data-row-id', rowId);
            }
            // 如果找不到，尝试通过 clientId 查找（row_id 可能为 0）
            if (!realEl && clientId) {
                realEl = _findByDataAttr('data-client-id', clientId);
            }
            // 如果还找不到，尝试通过 reqId 查找
            if (!realEl && reqId) {
                realEl = _findByDataAttr('data-req-id', reqId);
            }
            if (realEl && (state === "sent" || state === "delivered")) {
                var bubbleS = realEl.querySelector('.bubble');
                var timeRowS = bubbleS ? bubbleS.querySelector('.msg-time-row') : null;
                if (timeRowS) {
                    var oldStatus = timeRowS.querySelector('.msg-send-status');
                    if (oldStatus) oldStatus.remove();
                    var okSpan = document.createElement('span');
                    okSpan.className = 'msg-send-status msg-send-delivered';
                    okSpan.textContent = "✓";
                    timeRowS.insertBefore(okSpan, timeRowS.firstChild);
                }
                // 如果之前显示过失败(!)，SSE 现在确认已送达，弹出成功 toast
                if (realEl.dataset.failedState) {
                    delete realEl.dataset.failedState;
                    _toast("消息已送达 ✓");
                }
            } else if (realEl && (state === "failed" || state === "expired")) {
                realEl.dataset.failedState = state;
                // FIX S21: 仅在 rowId 有值时设置 dataset，避免 undefined 变成字符串 "undefined"
                if (rowId !== undefined && rowId !== null) realEl.dataset.rowId = rowId;
                var bubble = realEl.querySelector('.bubble');
                var timeRow = bubble ? bubble.querySelector('.msg-time-row') : null;
                if (timeRow) {
                    var oldStatus2 = timeRow.querySelector('.msg-send-status');
                    if (oldStatus2) oldStatus2.remove();
                    var failSpan = document.createElement('span');
                    failSpan.className = 'msg-send-status msg-send-fail';
                    failSpan.textContent = '!';
                    failSpan.style.cursor = 'pointer';
                    failSpan.onclick = function(ev) {
                        ev.stopPropagation();
                        _resendFailed(realEl);
                    };
                    timeRow.insertBefore(failSpan, timeRow.firstChild);
                }
                if (state === "expired") {
                    _toast("iLink 会话已过期，消息未送达。可点击红色感叹号重试，或点击顶部重新扫码。");
                } else {
                    _toast("发送失败，可点击红色感叹号重试");
                }
            }
            if (reqId) delete _state.pendingByReqId[reqId];
            if (rowId) delete _state.pendingByRowId[rowId];
            if (clientId) delete _state.pendingByClientId[clientId];
            return;
        }

        // FIX P1-11 (2026-07-20): pendingId 含不可信的 m.req_id / m.row_id，
        //   用 _findByDataAttr 避免选择器拼接注入。
        var el = _findByDataAttr('data-sending-id', pendingId);
        var statusEl = el ? el.querySelector('.msg-send-status') : null;

        if (state === "pending") {
            // 首次确认入库：保留 spinner
            // FIX: 清除延迟失败 toast（后端已确认入库，不应再标记失败）
            if (reqId && _state._pendingFailureToast[reqId]) {
                clearTimeout(_state._pendingFailureToast[reqId]);
                delete _state._pendingFailureToast[reqId];
            }
            if (statusEl) {
                statusEl.className = "msg-send-status";
                statusEl.innerHTML = '<div class="msg-send-loading"></div>';
            }
        } else if (state === "sending") {
            // 正在 POST：保留 spinner
            // FIX: 清除延迟失败 toast（后端正在发送，不应再标记失败）
            if (reqId && _state._pendingFailureToast[reqId]) {
                clearTimeout(_state._pendingFailureToast[reqId]);
                delete _state._pendingFailureToast[reqId];
            }
            if (statusEl) {
                statusEl.className = "msg-send-status";
                statusEl.innerHTML = '<div class="msg-send-loading"></div>';
            }
        } else if (state === "sent" || state === "delivered") {
            // 取消该 req_id 的延迟失败 toast（如果有）
            if (reqId && _state._pendingFailureToast[reqId]) {
                clearTimeout(_state._pendingFailureToast[reqId]);
                delete _state._pendingFailureToast[reqId];
            }
            // 平台发送成功/已送达：一律显示 ✓ (或者 ✓ 均视为成功发送，防止 delivered ACK 因网络延迟导致一直卡在发送失败/转圈)
            if (statusEl) {
                statusEl.className = "msg-send-status msg-send-delivered";
                statusEl.textContent = "✓";
            }
            // FIX 2026-07-15: 在 pending 元素上设置 data-msg-id（供 media_cache_update 查找）
            if (el && rowId) {
                el.dataset.msgId = rowId;
                el.dataset.rowId = rowId;
            }
            // FIX 2026-07-15: 更新聊天列表预览（不再推 "message" 事件，需手动更新）
            var toUser = ack.to_user_id || (el ? el.dataset.targetUser : null);
            var msgText = ack.text || "";
            if (toUser && msgText) {
                _state.lastMessages[toUser] = {
                    text: msgText,
                    time: new Date().toTimeString().slice(0, 8),
                };
                if (_state.view === 'list') _renderChatList();
            }
            // 如果之前显示过失败状态(!)，SSE 现在确认已送达，弹出成功 toast
            if (el && el.dataset.failedState) {
                delete el.dataset.failedState;
                _toast("消息已送达 ✓");
            }
        } else if (state === "failed" || state === "expired") {
            // 取消该 req_id 的延迟失败 toast（如果有）, 立即弹出确认的失败 toast
            if (reqId && _state._pendingFailureToast[reqId]) {
                clearTimeout(_state._pendingFailureToast[reqId]);
                delete _state._pendingFailureToast[reqId];
            }
            // 失败/过期：显示红色感叹号（可点击重试）
            if (el) {
                el.dataset.failedState = state;
                el.dataset.reqId = reqId || "";
                el.dataset.rowId = rowId || "";
            }
            if (statusEl) {
                statusEl.className = "msg-send-status msg-send-fail";
                statusEl.textContent = "!";
                statusEl.style.cursor = "pointer";
                statusEl.onclick = function(ev) {
                    ev.stopPropagation();
                    _resendFailed(el);
                };
            }
            // 立即弹出确认的失败 toast（不再延迟等待）
            if (state === "expired" && el) {
                _toast("iLink 会话已过期，消息未送达。可点击红色感叹号重试，或点击顶部重新扫码。");
            } else if (el) {
                _toast("发送失败，可点击红色感叹号重试");
            }
        }

        // sent / delivered / failed / expired 终态后清理映射表
        if (state === "delivered" || state === "failed" || state === "expired" || state === "sent") {
            if (reqId) delete _state.pendingByReqId[reqId];
            if (rowId) delete _state.pendingByRowId[rowId];
            if (clientId) delete _state.pendingByClientId[clientId];
        }
    };

    // ── PR4: 处理 session_status 事件 ───────────────────────
    // 终态：state == "session_expired" / "reauthing" / "active" / "disconnected"
    // 用于显示 banner 和"重新扫码"按钮
    const _handleSessionStatus = function(s) {
        if (!s) return;
        var state = s.state || "";
        _state.sessionMessage = s.message || "";

        if (state === "session_expired") {
            _state.sessionTerminal = true;
            _state.reauthRunning = false;
            _renderSessionBanner(true, s.message || "iLink 会话已过期，请点击重新扫码", s.expired_users || []);
        } else if (state === "reauthing") {
            _state.sessionTerminal = false;
            _state.reauthRunning = true;
            _renderSessionBanner(false, s.message || "正在等待重新扫码...", []);
        } else if (state === "active") {
            _state.sessionTerminal = false;
            _state.reauthRunning = false;
            // FIX M4 (2026-07-20): active 时清掉 disconnected 标记，恢复发送
            _state.sessionDisconnected = false;
            _renderSessionBanner(false, "", []);
            // FIX 2026-07-15: reauth 成功后触发全量同步，刷新用户列表、聊天列表、会话状态
            //   避免重新扫码后回到主界面状态未更新
            if (typeof _fullSync === 'function') {
                _fullSync("session_active");
            }
        } else if (state === "disconnected") {
            _state.sessionTerminal = false;
            _state.reauthRunning = false;
            // FIX M4 (2026-07-20): disconnected 表示 iLink 后端连不上（与 expired 不同的瞬时态），
            //   阻断 _sendMsg 发送避免无效请求与配额浪费
            _state.sessionDisconnected = true;
            _renderSessionBanner(false, "", []);
        }
    };

    // ── PR4: 渲染会话终态 banner（含"重新扫码"按钮）──────────
    // 复用现有 conn-status-bar，根据 sessionTerminal 决定显示内容
    const _renderSessionBanner = function(isTerminal, msg, expiredUsers) {
        var bars = [document.getElementById("conn-status-bar-list"), document.getElementById("conn-status-bar-chat")];
        bars.forEach(function(bar) {
            if (!bar) return;
            // 终态：清空 + 重新填充（包含按钮）
            if (isTerminal) {
                bar.className = "conn-status-bar show error";
                bar.innerHTML = "";
                var text = document.createElement("span");
                text.className = "conn-status-text";
                text.textContent = msg || "iLink 会话已过期";
                bar.appendChild(text);
                if (expiredUsers && expiredUsers.length > 0) {
                    var sub = document.createElement("span");
                    sub.className = "conn-status-sub";
                    sub.textContent = "（" + expiredUsers.length + " 个账号）";
                    bar.appendChild(sub);
                }
                var btn = document.createElement("button");
                btn.className = "conn-status-action";
                btn.textContent = "重新扫码";
                btn.onclick = function() { _startReauth(); };
                bar.appendChild(btn);
            } else if (msg) {
                // 过渡态（reauthing）：保留 spinner 风格
                bar.className = "conn-status-bar show warn";
                bar.textContent = msg;
            } else {
                // 正常态：隐藏 banner（由 _updateConnStatus 决定其他状态）
                bar.className = "conn-status-bar";
                bar.innerHTML = "";
            }
        });
    };

    // ── PR4: 触发重新扫码 ──────────────────────────────────
    const _startReauth = function() {
        if (_state.reauthRunning) return;
        _state.reauthRunning = true;
        _api("reauth-start", {}).then(function(r) {
            if (r && (r.ok || r.success)) {
                _toast("请扫描二维码重新绑定");
                // 复用现有添加用户二维码弹窗
                _startAddUserPoll();
            } else {
                _state.reauthRunning = false;
                _toast("启动重新扫码失败: " + ((r && r.error) || "未知错误"));
            }
        }).catch(function(err) {
            _state.reauthRunning = false;
            _toast("重新扫码失败: " + (err.message || err));
        });
    };

    // ── PR3: 重发失败消息 ──────────────────────────────────
    const _resendFailed = function(el) {
        if (!el) return;
        var rowId = el.dataset.rowId;
        var text = "";
        var bubble = el.querySelector('.bubble');
        if (bubble) text = bubble.textContent || "";

        if (!rowId || rowId === "0" || rowId === "undefined") {
            // PR5: 同步发送路径无 row_id，直接重新发送文本
            // FIX H1 (2026-07-20): 媒体消息无 row_id 时不能用文本占位符重发——
            //   否则后端收到的 "[图片] xxx.jpg" 会被当作文字消息发出去。
            //   检测方式：气泡内存在 .bubble-media-* 子元素即视为媒体。
            //   这种情况只能让用户重新选择文件上传，无法在原地重试。
            if (bubble && bubble.querySelector('.bubble-media-file, .bubble-media-img, .bubble-media-video-thumb, .bubble-media-voice')) {
                _toast("媒体消息无法重发，请重新选择文件上传", 4000, "error");
                return;
            }
            _toast("正在重试发送...");
            // 移除旧的失败元素，避免重发后出现重复气泡
            el.remove();
            var input = document.getElementById("message-input");
            if (input) {
                input.value = text;
            }
            _sendMsg();
            return;
        }

        // 新版：调用后端 resend 端点，保留 row_id，重置 state 为 pending
        // FIX S18: 删除 `_state.reauthRunning = false` —— 与重发无关，是 _startReauth 路径的复制残留
        _api("outbound-resend", { row_id: Number(rowId) }).then(function(r) {
            if (r && (r.ok || r.success)) {
                _toast("已重新入队");
                // 把失败状态改回 sending，让 SSE 后续 send_ack 继续推进
                el.dataset.failedState = "";
                var statusEl = el.querySelector('.msg-send-status');
                if (statusEl) {
                    statusEl.className = "msg-send-status";
                    statusEl.innerHTML = '<div class="msg-send-loading"></div>';
                }
            } else {
                _toast("重试失败: " + ((r && r.error) || "未知错误"));
            }
        }).catch(function(err) {
            _toast("重试失败: " + (err.message || err));
        });
    };

    // ── PR5: F5 恢复未完成的出站消息 ─────────────────────────
    // 页面刷新后调用，恢复显示所有 send_state ∈ {pending, failed} 的出站消息
    // 状态条目出现在消息列表底部（按 created_at_ms 排序），用户可点击重发
    const _loadOutboundPending = async function(userId) {
        if (!userId) return;
        try {
            var data = await _get("outbound-pending?user=" + encodeURIComponent(userId));
            if (!data || !data.success || !data.messages) return;
            var msgs = data.messages;
            if (msgs.length === 0) return;
            // 仅在聊天视图才注入
            if (_state.view !== 'chat' || _state.currentUser !== userId) return;
            var ma = document.getElementById("messages-area");
            if (!ma) return;
            // FIX L11 (2026-07-20): DocumentFragment 批量 append，50+ 条从 N 次 reflow 降到 1 次。
            //   先清空 empty-state（fragment 模式下 _renderSendingMsg 不会清），再批量 append。
            var _empty = ma.querySelector(".empty-state");
            if (_empty) _empty.remove();
            var frag = document.createDocumentFragment();
            msgs.forEach(function(m) {
                // 避免重复注入（同时检查 data-msg-id 和 data-row-id，适配 _renderMsg 双 ID 设置）
                // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
                //   避免 m.row_id / m.req_id 含 "]" 等特殊字符逃逸 CSS 选择器。
                if (m.row_id && (_findByDataAttr('data-msg-id', m.row_id) || _findByDataAttr('data-row-id', m.row_id))) return;
                if (m.req_id && _findByDataAttr('data-sending-id', "pending_recover_" + m.req_id)) return;
                var pendingId = "pending_recover_" + (m.req_id || ("row_" + m.row_id));
                var pendingMsg = {
                    id: m.row_id || pendingId,
                    from: "me",
                    to: m.to_user_id || userId,
                    text: m.text || "",
                    time: new Date().toTimeString().slice(0, 8),
                    type: "out",
                    _sending: true,
                    _recovered: true,
                    _reqId: m.req_id || "",
                    _rowId: m.row_id || 0,
                };
                _renderSendingMsg(pendingMsg, pendingId, frag);
                // 注册映射，方便后续 send_ack 命中
                if (m.req_id) _state.pendingByReqId[m.req_id] = pendingId;
                if (m.row_id) _state.pendingByRowId[m.row_id] = pendingId;
                if (m.client_id) _state.pendingByClientId[m.client_id] = pendingId;
                // 如果后端状态是 failed，直接显示失败样式 + 绑定重试
                if (m.send_state === "failed" || m.send_state === "expired") {
                    // FIX P1-11 (2026-07-20): pendingId 含不可信的 m.req_id / m.row_id，
                    //   用 _findByDataAttr 避免选择器拼接注入。
                    var el = _findByDataAttr('data-sending-id', pendingId);
                    if (el) {
                        el.dataset.failedState = m.send_state;
                        el.dataset.reqId = m.req_id || "";
                        el.dataset.rowId = m.row_id || "";
                        var statusEl = el.querySelector('.msg-send-status');
                        if (statusEl) {
                            statusEl.className = "msg-send-status msg-send-fail";
                            statusEl.textContent = "!";
                            statusEl.style.cursor = "pointer";
                            statusEl.onclick = function(ev) {
                                ev.stopPropagation();
                                _resendFailed(el);
                            };
                        }
                    }
                }
            });
            // 一次性挂到 messages-area，触发 1 次 reflow
            ma.appendChild(frag);
            // 滚动到底部
            // FIX H3 (2026-07-20): 防御性二次校验 view，并改用 _isNearBottom 与 _renderMsg 保持一致，
            //   防止 view 切换后误滚 + 防止用户向上翻阅历史时被强制拽回底部
            if (_state.view === 'chat' && _state.currentUser === userId && typeof _isNearBottom === 'function' && _isNearBottom(ma)) {
                ma.scrollTop = ma.scrollHeight;
            }
        } catch (err) {
            console.warn("[outbound-pending] load error:", err);
        }
    };

    // ── PR4: 启动时主动拉取一次 session 状态 ────────────────
    // 用于页面加载时立即知道会话是否已过期（SSE 可能延迟）
    const _checkSessionStatus = async function() {
        try {
            var s = await _get("session-status");
            if (s) _handleSessionStatus(s);
        } catch (err) {
            console.warn("[session-status] check error:", err);
        }
    };

    // 暴露到全局以便其它流程触发
    _state._checkSessionStatus = _checkSessionStatus;
    _state._loadOutboundPending = _loadOutboundPending;
    document.addEventListener('visibilitychange', _onVisibilityChange);

    // ─── 网络恢复：浏览器检测到网络恢复时强制同步 ───
    window.addEventListener('online', function() {
        // FIX P0-5 (2026-07-20): 用 _state.loggedIn 替换 _state.token 真值判断，
        //   Cookie 迁移后 _state.token 恒为 ""，原守卫导致网络恢复后不重连。
        if (_state.loggedIn) {
            _fullSync("network_online");
            if (!_ws || _ws.readyState !== _WS_OPEN) {
                _startSSE();
            }
        }
    });
    
