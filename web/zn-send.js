    // FIX P0-4 (2026-07-20): 恢复 _sendMsg 完整实现。
    //   原文件仅一行 `const _sendMsg = function()`，无函数体，是语法错误。
    //   导致发送按钮和回车键均失效，且 JS 加载报错可能影响其他模块。
    //
    //   后端 api_send 是 PR5 同步发送路径：
    //     - 请求体：{text: string, req_id?: string}
    //     - to_user_id 由后端从 bot.get_current_user() 取，前端不传
    //     - 成功响应：{ok:true, success:true, message:{...}}
    //     - 失败响应：{ok:false, success:false, error:string, session_expired?:bool}
    //     - 成功后后端推 "message" 事件，前端 _handleIncomingMessage 自动替换 pending 元素
    //
    //   前端流程：
    //     1. 读取输入框文本，校验非空
    //     2. 生成 req_id（用于 ACK 匹配 + 后端日志关联）
    //     3. 立即清空输入框（避免重复点击）
    //     4. 创建 pending DOM 元素（_renderSendingMsg）
    //     5. 注册 pendingByReqId 映射
    //     6. 调用 _api("send", {text, req_id})
    //     7. 成功：保留 pending（spinner），等 WS/轮询拉到真实消息后自动替换；
    //        设置 30s 超时兜底，超时后用响应 message 立即渲染（防 WS 断线）
    //     8. 失败：把 pending 元素改为失败状态（红色感叹号 + 可重试）
    // FIX M6 (2026-07-20): 模块级串行化标志，_sendMsg 期间禁止再次触发。
    var _sendInProgress = false;
    const _sendMsg = function() {
        var input = document.getElementById("message-input");
        if (!input) return;
        // FIX M6 (2026-07-20): 模块级 _sendInProgress 守卫，串行化快速连按/连回车。
        //   之前 _sendMsg 并发触发可能导致两条 out 消息渲染顺序与实际发送顺序不一致。
        //   注意：消息文本仍按用户操作顺序入队（用 input.value 缓存），守卫仅阻止后端并发请求。
        if (_sendInProgress) {
            _toast("上一条消息正在发送，请稍候", 1500, "error");
            return;
        }
        var text = (input.value || "").trim();
        if (!text) return; // 空消息不发

        // 必须有当前聊天用户
        var toUser = _state.currentUser;
        if (!toUser) {
            _toast("请先选择一个聊天", 2000, "error");
            return;
        }

        // 会话终态时拒绝发送（避免无谓的失败请求）
        if (_state.sessionTerminal) {
            _toast("iLink 会话已过期，请重新扫码后再发送", 3000, "error");
            return;
        }

        // FIX M4 (2026-07-20): iLink session_status="disconnected" 时也阻断发送。
        //   disconnected 表示 iLink 后端连不上（sessionTerminal=false 但实际不可用），
        //   此时 _api("send") 会被后端拒，体验差且会扣减 msg_per_day 配额。
        //   session_disconnected 由 zn-events.js 的 _handleSessionStatus 维护。
        if (_state.sessionDisconnected) {
            _toast("iLink 暂时不可用，正在重连... 请稍候再试", 3000, "error");
            return;
        }

        _sendInProgress = true;

        // 生成 req_id（前端用于 ACK 匹配，后端用于日志关联）
        var reqId = "req-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 8);

        // 立即清空输入框（避免用户重复点发送导致同一文本多次入库）
        input.value = "";

        // 创建 pending DOM 元素
        var pendingId = "sending_" + Date.now() + "_" + Math.random().toString(36).slice(2, 8);
        var pendingMsg = {
            id: pendingId,
            from: "me",
            to: toUser,
            text: text,
            time: new Date().toTimeString().slice(0, 8),
            type: "out",
            _sending: true,
            _reqId: reqId,
        };
        _renderSendingMsg(pendingMsg, pendingId);

        // 注册 pending 映射，让 _handleIncomingMessage / _handleSendAck 能精确匹配
        _state.pendingByReqId[reqId] = pendingId;

        // 30s 超时兜底：WS 完全断开且轮询未拉到时，用响应 message 立即渲染
        var timeoutFired = false;
        var timeoutId = setTimeout(function() {
            timeoutFired = true;
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 pendingId 含 "]" 等特殊字符逃逸 CSS 选择器。
            var staleEl = _findByDataAttr('data-sending-id', pendingId);
            if (!staleEl) return; // 已被真实消息替换，无需处理
            // WS/轮询未拉到，保留 spinner 但提示用户检查网络
            _toast("消息已发送，等待服务器确认中...", 3000);
        }, 30000);

        // 调用后端 send 端点
        _api("send", { text: text, req_id: reqId }).then(function(r) {
            clearTimeout(timeoutId);
            if (r && (r.ok || r.success)) {
                // 同步发送成功：后端会推 "message" 事件，_handleIncomingMessage 自动替换 pending。
                // 这里只更新 pending 元素的 dataset（设置 client_id/row_id 便于 WS 事件匹配），
                // 不立即渲染（避免与 WS 事件重复）。
                var el = _findByDataAttr('data-sending-id', pendingId);
                if (el && r.message) {
                    // 把响应中的 row_id/client_id 写入 dataset，便于 WS message 事件去重匹配
                    var rowId = r.message.row_id || r.message.id;
                    var clientId = r.message.client_id;
                    if (rowId) { el.dataset.rowId = rowId; el.dataset.msgId = rowId; }
                    if (clientId) el.dataset.clientId = clientId;
                }
                // 如果 WS 已断开且 30s 兜底已触发，或 pending 元素仍在且 message 有完整数据，
                // 直接用响应 message 替换 pending（避免永久 spinner）
                if (timeoutFired || !_ws || _ws.readyState !== _WS_OPEN) {
                    if (el && r.message) {
                        // 删除 pending 映射，避免 _handleIncomingMessage 二次处理
                        delete _state.pendingByReqId[reqId];
                        // 用真实消息替换 pending
                        if (typeof _replacePendingWithRendered === 'function') {
                            _replacePendingWithRendered(el, r.message);
                        } else {
                            el.remove();
                            if (typeof _renderMsg === 'function') _renderMsg(r.message);
                        }
                    }
                }
                return;
            }
            // 失败：把 pending 元素改为失败状态
            var errMsg = (r && (r.error || r.message)) || "发送失败";
            var isExpired = r && r.session_expired;
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 pendingId 含 "]" 等特殊字符逃逸 CSS 选择器。
            var failEl = _findByDataAttr('data-sending-id', pendingId);
            if (failEl) {
                failEl.dataset.failedState = isExpired ? "expired" : "failed";
                failEl.dataset.reqId = reqId;
                var statusEl = failEl.querySelector('.msg-send-status');
                if (statusEl) {
                    statusEl.className = 'msg-send-status msg-send-fail';
                    statusEl.textContent = '!';
                    statusEl.style.cursor = 'pointer';
                    statusEl.onclick = function(ev) {
                        ev.stopPropagation();
                        _resendFailed(failEl);
                    };
                }
            }
            delete _state.pendingByReqId[reqId];
            if (isExpired) {
                _toast("iLink 会话已过期，消息未送达。可点击红色感叹号重试，或点击顶部重新扫码。", 4000, "error");
            } else {
                _toast("发送失败: " + errMsg, 3000, "error");
            }
        }).catch(function(err) {
            clearTimeout(timeoutId);
            // 网络错误：把 pending 元素改为失败状态
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 pendingId 含 "]" 等特殊字符逃逸 CSS 选择器。
            var failEl2 = _findByDataAttr('data-sending-id', pendingId);
            if (failEl2) {
                failEl2.dataset.failedState = "failed";
                failEl2.dataset.reqId = reqId;
                var statusEl2 = failEl2.querySelector('.msg-send-status');
                if (statusEl2) {
                    statusEl2.className = 'msg-send-status msg-send-fail';
                    statusEl2.textContent = '!';
                    statusEl2.style.cursor = 'pointer';
                    statusEl2.onclick = function(ev) {
                        ev.stopPropagation();
                        _resendFailed(failEl2);
                    };
                }
            }
            delete _state.pendingByReqId[reqId];
            _toast("发送失败: " + (err && err.message || err), 3000, "error");
        }).finally(function() {
            // FIX M6 (2026-07-20): 不论成功失败都释放守卫，让用户能发送下一条。
            _sendInProgress = false;
        });
    };
