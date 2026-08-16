    var _escapeAttr = function(s) {
        return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/'/g, '&#39;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    };

    // FIX P1-11 (2026-07-20): 用 DOM 遍历替代 querySelector 字符串拼接，避免 CSS 选择器注入。
    //   原实现 document.querySelector('[data-msg-id="' + id + '"]') 中，含 "]" 或 "\""
    //   的恶意值可逃逸属性选择器（例如 id='abc"]div[onload="alert(1)' 可注入新选择器），
    //   服务器返回的 rowId / msgId / reqId 均不可信。
    //   本函数先用属性存在性选择器取出候选元素，再用 getAttribute 精确比较，
    //   完全避免把不可信值拼入选择器字符串。
    //   性能：querySelectorAll('[data-xxx]') 仅扫一遍 DOM，messages-area 元素数量有限，可接受。
    var _findByDataAttr = function(attr, value) {
        if (value === undefined || value === null || value === '') return null;
        var valStr = String(value);
        var els = document.querySelectorAll('[' + attr + ']');
        for (var i = 0; i < els.length; i++) {
            if (els[i].getAttribute(attr) === valStr) return els[i];
        }
        return null;
    };

    // FIX P1-12 (2026-07-20): _escape 仅转义 <、>、&（textContent 行为），
    //   不转义引号（" 和 '），仅适用于 HTML 内容上下文（如 <div>HERE</div>）。
    //   属性上下文（如 <div data-x="HERE">、<div title='HERE'>）必须用 _escapeAttr，
    //   否则含引号的恶意值可逃逸属性（例如 x' onmouseover='alert(1)）。
    const _escape = function(e) {
        const t = document.createElement("div");
        t.textContent = e;
        return t.innerHTML;
    };
    
    // type: 可选，'error' 显示红色 toast
    // FIX S34: 用模块级 _toastTimer 保存 setTimeout ID，
    //   触发新 toast 时 clearTimeout 旧定时器，避免新 toast 被旧定时器提前关闭
    // FIX U9 (2026-07-20): toast 时长自适应 + 点击关闭。
    //   - 长消息自适应时长：按字符数计算（每 30 字 +1s，最少 3s，最多 10s）
    //   - error 类型固定最少 5s（关键错误用户需要时间阅读）
    //   - 点击 toast 立即关闭
    var _toastTimer = null;
    var _toastDuration = function(msg, t, type) {
        if (t && t > 0) return t; // 调用方显式指定优先
        var charCount = String(msg || '').length;
        var base = type === 'error' ? 5000 : 3000;
        var extra = Math.ceil(charCount / 30) * 1000; // 每 30 字 +1s
        return Math.min(base + extra, 10000); // 上限 10s
    };
    const _toast = function(e, t, type) {
        const n = document.getElementById("toast");
        if (!n) return;
        // FIX L6 (2026-07-20): 相同文字不重启动画，只延长定时器。
        //   之前每次 _toast 都重设 textContent + show class，导致 L6 描述的 fallback → body
        //   解析两次 toast 造成 1-2 次闪烁。现已在动画中且文字相同时，仅刷新定时器。
        var isSameText = n.textContent === String(e) && n.classList.contains("show");
        if (!isSameText) {
            n.textContent = e;
            n.className = "toast";
            if (type === "error") n.classList.add("error");
            n.classList.add("show");
        } else {
            // 相同文字：同步 type class（info → error 或 error → info）
            if (type === "error" && !n.classList.contains("error")) {
                n.classList.add("error");
            } else if (type !== "error" && n.classList.contains("error")) {
                n.classList.remove("error");
            }
        }
        // FIX U9: 点击 toast 立即关闭
        n.onclick = function() {
            n.classList.remove("show");
            if (_toastTimer) { clearTimeout(_toastTimer); _toastTimer = null; }
        };
        if (_toastTimer) { clearTimeout(_toastTimer); _toastTimer = null; }
        var duration = _toastDuration(e, t, type);
        _toastTimer = setTimeout((function() { n.classList.remove("show"); _toastTimer = null; }), duration);
    };

    // ─── 连接状态条 ─────────────────────
    const _renderConnStatus = function(status, text, actionText, actionFn) {
        var bars = [document.getElementById("conn-status-bar-list"), document.getElementById("conn-status-bar-chat")];
        bars.forEach(function(bar) {
            if (!bar) return;
            bar.className = "conn-status-bar";
            if (!status || status === "connected" || status === "unknown") {
                bar.classList.remove("show");
                // FIX L9 (2026-07-20): 隐藏时清空 innerHTML，避免"重新连上 → 又断开"循环时
                //   上一次的警告文字/按钮短暂残留导致 1-2 帧闪烁。
                if (bar.innerHTML) bar.innerHTML = "";
                return;
            }
            bar.innerHTML = "";
            var textSpan = document.createElement("span");
            textSpan.className = "conn-status-text";
            textSpan.textContent = text || "";
            bar.appendChild(textSpan);
            if (actionText && actionFn) {
                var btn = document.createElement("button");
                btn.className = "conn-status-action";
                btn.textContent = actionText;
                btn.onclick = actionFn;
                bar.appendChild(btn);
            }
            bar.classList.add("show", status);
        });
    };

    // FIX L3 (2026-07-20): 自定义确认模态，替代 window.confirm()。
    //   问题：window.confirm() 在 WebView/微信内置浏览器/Tauri 等环境会被静默拦截或样式割裂，
    //   表现"明明点了确认却没反应"或根本不弹窗，导致用户管理/退出登录等关键操作失效。
    //   ponytail: 单一函数 + Promise 接口（onConfirm / onCancel 可选），不破坏同步流程的写法。
    var _confirmOverlay = null;
    const _showConfirm = function(message, onConfirm, onCancel, opts) {
        // 移除已有遮罩，避免叠加
        if (_confirmOverlay) {
            try { _confirmOverlay.remove(); } catch (e) {}
            _confirmOverlay = null;
        }
        var title = (opts && opts.title) || "请确认";
        var okText = (opts && opts.okText) || "确定";
        var cancelText = (opts && opts.cancelText) || "取消";
        var danger = !!(opts && opts.danger);

        var overlay = document.createElement("div");
        overlay.className = "confirm-modal-overlay";
        overlay.innerHTML =
            '<div class="confirm-modal" role="dialog" aria-modal="true" aria-labelledby="confirm-modal-title">' +
            '<div class="confirm-modal-title" id="confirm-modal-title">' + _escape(title) + '</div>' +
            '<div class="confirm-modal-body">' + _escape(message) + '</div>' +
            '<div class="confirm-modal-actions">' +
            '<button class="confirm-modal-cancel" type="button">' + _escape(cancelText) + '</button>' +
            '<button class="confirm-modal-ok" type="button">' + _escape(okText) + '</button>' +
            '</div></div>';
        _confirmOverlay = overlay;
        document.body.appendChild(overlay);
        var close = function(cb) {
            try { overlay.remove(); } catch (e) {}
            if (_confirmOverlay === overlay) _confirmOverlay = null;
            if (typeof cb === 'function') cb();
        };
        overlay.querySelector(".confirm-modal-ok").addEventListener("click", function() {
            close(onConfirm);
        });
        overlay.querySelector(".confirm-modal-cancel").addEventListener("click", function() {
            close(onCancel);
        });
        // 点遮罩关闭等同取消
        overlay.addEventListener("click", function(e) {
            if (e.target === overlay) close(onCancel);
        });
        if (danger) {
            var okBtn = overlay.querySelector(".confirm-modal-ok");
            if (okBtn) okBtn.classList.add("danger");
        }
    };

    // FIX D4 (2026-07-21): 自定义输入模态，替代 window.prompt()。
    //   原因同 _showConfirm：WebView/微信/Tauri 中 window.prompt 会被静默拦截，
    //   且与项目其他自定义 modal 风格割裂。
    //   接口：_showPrompt(message, defaultValue, onSubmit(value|null), opts)
    //   onSubmit 在用户点确定时收到输入值（trim 后），点取消/遮罩收到 null。
    var _promptOverlay = null;
    const _showPrompt = function(message, defaultValue, onSubmit, opts) {
        if (_promptOverlay) {
            try { _promptOverlay.remove(); } catch (e) {}
            _promptOverlay = null;
        }
        var title = (opts && opts.title) || "请输入";
        var okText = (opts && opts.okText) || "确定";
        var cancelText = (opts && opts.cancelText) || "取消";
        var placeholder = (opts && opts.placeholder) || "";

        var overlay = document.createElement("div");
        overlay.className = "confirm-modal-overlay";
        overlay.innerHTML =
            '<div class="confirm-modal" role="dialog" aria-modal="true">' +
            '<div class="confirm-modal-title">' + _escape(title) + '</div>' +
            '<div class="confirm-modal-body">' + _escape(message) + '</div>' +
            '<input type="text" class="confirm-modal-input" style="width:100%;padding:8px 12px;font-size:14px;border:1px solid var(--border-color,#ddd);border-radius:8px;background:var(--input-bg,#fff);color:var(--text-primary,#1c1c1e);outline:none;box-sizing:border-box;margin-bottom:16px;" placeholder="' + _escapeAttr(placeholder) + '">' +
            '<div class="confirm-modal-actions">' +
            '<button class="confirm-modal-cancel" type="button">' + _escape(cancelText) + '</button>' +
            '<button class="confirm-modal-ok" type="button">' + _escape(okText) + '</button>' +
            '</div></div>';
        _promptOverlay = overlay;
        document.body.appendChild(overlay);
        var input = overlay.querySelector(".confirm-modal-input");
        if (input) {
            input.value = defaultValue != null ? String(defaultValue) : '';
            // 异步聚焦，让浏览器先布局完成
            setTimeout(function() { input.focus(); input.select(); }, 0);
            // 回车提交
            input.addEventListener("keydown", function(e) {
                if (e.key === 'Enter') {
                    var v = input.value;
                    try { overlay.remove(); } catch (e2) {}
                    if (_promptOverlay === overlay) _promptOverlay = null;
                    if (typeof onSubmit === 'function') onSubmit(v);
                } else if (e.key === 'Escape') {
                    try { overlay.remove(); } catch (e2) {}
                    if (_promptOverlay === overlay) _promptOverlay = null;
                    if (typeof onSubmit === 'function') onSubmit(null);
                }
            });
        }
        var close = function(cb) {
            try { overlay.remove(); } catch (e) {}
            if (_promptOverlay === overlay) _promptOverlay = null;
            if (typeof cb === 'function') cb();
        };
        overlay.querySelector(".confirm-modal-ok").addEventListener("click", function() {
            var v = input ? input.value : '';
            close(function() { if (typeof onSubmit === 'function') onSubmit(v); });
        });
        overlay.querySelector(".confirm-modal-cancel").addEventListener("click", function() {
            close(function() { if (typeof onSubmit === 'function') onSubmit(null); });
        });
        overlay.addEventListener("click", function(e) {
            if (e.target === overlay) close(function() { if (typeof onSubmit === 'function') onSubmit(null); });
        });
    };

    const _updateConnStatus = function() {
        // 1. 判断前端 ↔ 服务器层
        var sseOk = _ws && _ws.readyState === WebSocket.OPEN;
        var isPolling = !!_state.pollInterval;
        // FIX 2026-07-19: "polling" 不再视为告警状态。
        //   设计上轮询就是消息获取的主路径（见 zn-connection.js 中 _startPoll 注释：
        //   "参考 Python 版，启动快速轮询作为主要消息获取方式。WS/SSE 仅作为通知触发立即轮询，
        //   确保消息始终能到达。之前只依赖 WS，WS 连接失败时消息完全丢失。"）。
        //   之前把轮询中显示成"连接不稳定"会让用户误以为系统故障，实际消息收发完全正常。
        //   现在只要轮询在跑（不管 WS 是否在线），都视为已连通；只在真正断开时才告警。
        var frontendStatus = sseOk ? "connected" : (isPolling ? "connected" : "disconnected");

        // SSE 正在连接中，polling 只是兜底，不算"不稳定"
        // 避免 SSE 建立期间误报"连接不稳定"
        // FIX S6: 原条件 `&& _es` 引用未定义变量（SSE 时代遗留），改用 _ws 状态判断
        if (!sseOk && isPolling && _esConnectingSince > 0 &&
            typeof _ws !== 'undefined' && _ws && _ws.readyState === WebSocket.CONNECTING) {
            frontendStatus = "connected";
        }

        // 2. 判断服务器 ↔ iLink 层
        var hasErrorAccount = false;
        var errorAccountName = "";
        var allExpired = true;
        var anyAccount = false;
        var pollHealth = _state.pollHealth || {};
        Object.keys(pollHealth).forEach(function(tokenShort) {
            var ph = pollHealth[tokenShort];
            anyAccount = true;
            if (ph.state !== "expired") allExpired = false;
            if (ph.state === "error" && ph.elapsed_since_success > 60) {
                hasErrorAccount = true;
                if (!errorAccountName) errorAccountName = tokenShort;
            }
        });

        // 3. 综合决策
        if (frontendStatus === "disconnected") {
            // 真正断开：WS 关闭 + 轮询也停了（极少见，仅在 _stopSSE 被显式调用时）
            // FIX U2 (2026-07-20): 断开状态显示手动重连按钮，用户可主动触发重连。
            _renderConnStatus("error", "与服务器断开连接，请检查网络", "重连", function() {
                if (typeof _startSSE === 'function') _startSSE();
            });
            _state.connStatus = "disconnected";
            return;
        }
        // connected / polling / connecting 都视为已连通，隐藏告警条。
        // 真正需要关注的是后端账号层异常（账号连接错误 / 全部过期）。
        if (hasErrorAccount) {
            _renderConnStatus("warn", "账号 " + errorAccountName + "… 连接异常，消息可能延迟");
            _state.connStatus = "account_error";
            return;
        }
        if (anyAccount && allExpired) {
            _renderConnStatus("error", "所有账号会话已过期，请重新扫码");
            _state.connStatus = "disconnected";
            return;
        }
        // FIX L13 (2026-07-20): RTT > 500ms 显示网络延迟提示（仅在 WS 连接时测量有效）
        //   - 500-1000ms：warn 黄色"网络延迟较高"
        //   - >1000ms：error 红色"网络延迟过高，消息可能延迟"
        //   仅在连续 3 次 ping/pong（约 75s）后开始判断，避免首次 ping 偶发抖动误报
        if (sseOk && _state._rttHistory && _state._rttHistory.length >= 3) {
            var avgRtt = _state._rttAvg || 0;
            if (avgRtt > 1000) {
                _renderConnStatus("error", "网络延迟过高（" + avgRtt + "ms），消息可能延迟");
                _state.connStatus = "laggy";
                return;
            }
            if (avgRtt > 500) {
                _renderConnStatus("warn", "网络延迟较高（" + avgRtt + "ms）");
                _state.connStatus = "laggy";
                return;
            }
        }
        // 一切正常：隐藏状态条
        _renderConnStatus("connected", "");
        _state.connStatus = "connected";
    };

    const _svgImage = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#999" stroke-width="1.5"><rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><path d="M21 15l-5-5L5 21"/></svg>';
    const _svgVideo = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#999" stroke-width="1.5"><polygon points="23 7 16 12 23 17 23 7"/><rect x="1" y="5" width="15" height="14" rx="2"/></svg>';
    const _svgFile = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#999" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>';
    const _svgVoice = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#999" stroke-width="1.5"><path d="M12 1a3 3 0 00-3 3v8a3 3 0 006 0V4a3 3 0 00-3-3z"/><path d="M19 10v2a7 7 0 01-14 0v-2"/><line x1="12" y1="19" x2="12" y2="23"/><line x1="8" y1="23" x2="16" y2="23"/></svg>';
    const _svgPlay = '<svg viewBox="0 0 24 24" width="36" height="36" fill="rgba(0,0,0,0.5)"><path d="M8 5v14l11-7z"/></svg>';

    // FIX M10 (2026-07-20): WebSocket 状态常量已由 zn-connection.js 在同 realm 全局
    //   作用域中声明（_WS_OPEN / _WS_CONNECTING），本文件不再重复声明，避免
    //   "Identifier '_WS_OPEN' has already been declared" SyntaxError 导致
    //   zn-connection.js 加载失败、_ws 未定义。原重复声明属于 dead code（本文件未使用）。



    const _formatFileSize = function(bytes) {
        if (!bytes || bytes <= 0) return "";
        if (bytes < 1024) return bytes + " B";
        if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KB";
        if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + " MB";
        return (bytes / (1024 * 1024 * 1024)).toFixed(1) + " GB";
    };

    const _loadingPlaceholder = function(mediaType, label) {
        var txt = label || (mediaType === "video" ? "视频加载中..."
            : mediaType === "voice" ? "语音加载中..."
            : mediaType === "file" ? "文件加载中..."
            : "图片加载中...");
        return '<div class="bubble-media-placeholder bubble-media-loading-inner">' +
            '<div class="bubble-media-spinner"></div>' +
            '<span>' + txt + '</span>' +
            '</div>';
    };

    const _refreshWebDavAuth = function() {
        // 后端返回 {ok, enabled, host, base_path}（非凭证），前端通过 /api/wasm/webdav-proxy 代理访问 WebDAV
        // 此函数仅用于检测 WebDAV 是否可用，不再获取直接访问凭证
        return fetch('/api/wasm/webdav-auth', {
            // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie。
            credentials: 'same-origin'
        }).then(function(r) { return r.json(); }).then(function(data) {
            if (data.ok && data.enabled) {
                _state.webdavAuth = { host: data.host, base_path: data.base_path };
            } else {
                _state.webdavAuth = null;
            }
        }).catch(function() {});
    };
    // FIX S69: 删除从未被调用的死代码 _fetchWebDavBlob（迁移到 WS 代理后未清理）

