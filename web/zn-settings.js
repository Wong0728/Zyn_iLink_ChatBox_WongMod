    const _openSettings = function() {
        const e = document.getElementById("settings-panel");
        if (e) {
            e.classList.add("show");
            _showSettingsPage('settings-main');
            _setActiveTab('tab-settings');
            _startQuotaPoll();
        }
    };
    
    const _closeSettings = function() {
        _stopMigratePoll();
        _stopQuotaPoll();
        const e = document.getElementById("settings-panel");
        if (e) e.classList.remove("show");
        if (_state.view === 'list') {
            _setActiveTab('tab-list');
        } else if (_state.view === 'users') {
            _setActiveTab('tab-users');
        }
    };

    // ── Phase 3 (U3): 存储用量配额进度条 ──
    // 30s 轮询 /api/wasm/me，渲染 5 维 used/quota 进度条（80% 黄、95% 红）。
    // quota==0 表示"不限"，不显示进度条只显示 used。
    var _quotaPollTimer = null;
    const _stopQuotaPoll = function() {
        if (_quotaPollTimer) { clearInterval(_quotaPollTimer); _quotaPollTimer = null; }
    };
    const _startQuotaPoll = function() {
        _stopQuotaPoll();
        _loadQuotaUsage();
        _quotaPollTimer = setInterval(_loadQuotaUsage, 30000);
    };
    const _loadQuotaUsage = function() {
        _get("me").then(function(res) {
            if (!res || !res.quota) return;
            // FIX P0-5 (2026-07-20): /api/wasm/me 200 + quota 字段说明 Cookie 有效，
            //   标记登录态。这是 _state.loggedIn 的主要设置点。
            _state.loggedIn = true;
            // 兜底：确保 webUid 被设置（chat.html 的 /api/wasm/me 探测在 zn-core.js 加载前执行，
            //   极罕见场景下 _state 未定义时 webUid 不会被设置。此处必然在 zn-core.js 之后执行）
            if (res.uid) _state.webUid = res.uid;
            // Phase 5 (U7): 在 chat-list-header 显示当前登录用户名 pill
            var pill = document.getElementById("current-user-pill");
            if (pill && res.username) {
                pill.textContent = res.username + (res.role === "owner" || res.role === "admin" ? " · " + res.role : "");
                pill.style.display = "inline-block";
            }
            var group = document.getElementById("quota-usage-group");
            var container = document.getElementById("quota-usage-container");
            var targetEl = document.getElementById("quota-storage-target");
            if (!group || !container) return;
            if (targetEl) {
                targetEl.textContent = res.storage_target === "own_webdav" ? "WebDAV 存储" : "服务器存储";
            }
            var dims = [
                { key: "upload_bytes",   label: "上传流量", unit: "bytes" },
                { key: "download_bytes", label: "下载流量", unit: "bytes" },
                { key: "media_bytes",    label: "媒体占用", unit: "bytes" },
                { key: "msg_per_day",    label: "今日消息", unit: "count" },
                { key: "media_count",    label: "媒体数量", unit: "count" },
            ];
            var html = "";
            dims.forEach(function(d) {
                var quota = (res.quota && res.quota[d.key]) || 0;
                var used = (res.used && res.used[d.key]) || 0;
                var pct = quota > 0 ? Math.min(100, Math.round(used / quota * 100)) : 0;
                var color = quota > 0 ? (pct >= 95 ? "#e74c3c" : (pct >= 80 ? "#f39c12" : "var(--accent)")) : "var(--accent)";
                var usedStr = d.unit === "bytes" ? _formatFileSize(used) : String(used);
                var quotaStr = quota > 0 ? (d.unit === "bytes" ? _formatFileSize(quota) : String(quota)) : "不限";
                var bar = quota > 0
                    ? '<div style="height:4px;background:var(--bg-input,#eee);border-radius:2px;overflow:hidden;margin-top:4px;">' +
                      '<div style="height:100%;width:' + pct + '%;background:' + color + ';transition:width .3s;"></div></div>'
                    : '';
                html += '<div style="margin-bottom:8px;">' +
                    '<div style="display:flex;justify-content:space-between;font-size:12px;color:var(--text-secondary);">' +
                    '<span>' + d.label + '</span>' +
                    '<span>' + usedStr + ' / ' + quotaStr + (quota > 0 ? ' (' + pct + '%)' : '') + '</span>' +
                    '</div>' + bar + '</div>';
            });
            container.innerHTML = html;
            group.style.display = "block";
        }).catch(function() {
            // 静默失败（未登录或无权限时不显示用量区）
            var group = document.getElementById("quota-usage-group");
            if (group) group.style.display = "none";
        });
    };
    
    const _showSettingsPage = function(pageId) {
        var pages = document.querySelectorAll('.settings-page');
        pages.forEach(function(p) { p.classList.remove('active'); p.classList.remove('settings-page-slide'); });
        var target = document.getElementById(pageId);
        if (target) {
            target.classList.add('active');
            if (pageId !== 'settings-main') {
                target.classList.add('settings-page-slide');
            }
        }
        var tabbar = document.getElementById("bottom-tab-bar");
        if (pageId === 'settings-main') {
            _stopMigratePoll();
            if (tabbar) tabbar.classList.remove("hidden");
        } else {
            if (tabbar) tabbar.classList.add("hidden");
        }
        if (pageId === 'settings-about') {
            _loadAbout();
        } else if (pageId === 'settings-webdav') {
            _loadWebDAVSettings();
        } else if (pageId === 'settings-account') {
            // iLink 绑定信息已移除（改造方案 §六）
        }
        else if (pageId === 'settings-user-mgmt') {
            _closeSettings();
            _switchToUserList();
        }
    };

    // ── Phase 5 (U10): 账号安全——修改密码 + 退出登录 ──
    const _setAccountStatusMsg = function(msg, type) {
        var el = document.getElementById("account-status-msg");
        if (!el) return;
        if (!msg) { el.style.display = "none"; el.textContent = ""; return; }
        el.style.display = "block";
        el.textContent = msg;
        el.style.color = type === "error" ? "#ff4d4f" : (type === "success" ? "#27ae60" : "var(--text-secondary)");
    };

    const _saveAccountPassword = function() {
        var oldPwd = (document.getElementById("account-old-password") || {}).value || "";
        var newPwd = (document.getElementById("account-new-password") || {}).value || "";
        var confirmPwd = (document.getElementById("account-confirm-password") || {}).value || "";
        if (!oldPwd) { _setAccountStatusMsg("请输入旧密码", "error"); return; }
        if (newPwd.length < 8) { _setAccountStatusMsg("新密码至少 8 位", "error"); return; }
        if (newPwd !== confirmPwd) { _setAccountStatusMsg("两次输入的新密码不一致", "error"); return; }
        _showConfirm(
            "修改成功后，当前及其他设备都将退出登录，需要使用新密码手动重新登录。确认继续？",
            function() { _submitAccountPassword(oldPwd, newPwd); },
            null,
            { okText: "确认修改", danger: true }
        );
    };

    const _submitAccountPassword = async function(oldPwd, newPwd) {
        _setAccountStatusMsg("正在修改...", "info");
        try {
            var res = await _api("set-password", {
                old_password: oldPwd,
                new_password: newPwd
            });
            if (res && res.success) {
                _setAccountStatusMsg("密码已修改，正在退出登录...", "success");
                _toast("密码已修改，请使用新密码重新登录");
                setTimeout(function() { window.location.href = "/auth"; }, 800);
            } else {
                _setAccountStatusMsg((res && res.error) || "修改失败", "error");
            }
        } catch (err) {
            _setAccountStatusMsg("修改失败：" + (err.message || err), "error");
        }
    };

    const _logoutAccount = function() {
        // FIX L3 (2026-07-20): 用 _showConfirm 替代 window.confirm，避免 WebView 拦截。
        _showConfirm("确认退出登录？", function() { _doLogout(); });
    };
    const _doLogout = async function() {
        // FIX P1-1 (2026-07-19): 支持登出时撤销全部"记住我"令牌。
        var revokeDevice = !!(document.getElementById("account-logout-revoke-device") || {}).checked;
        try {
            await _api("logout", { revoke_device: revokeDevice });
        } catch (e) { /* 忽略，强制跳转 */ }

        // 退出时如果撤销了 token，服务端已作废，无需额外清理
        // device_token 通过 HttpOnly Cookie 下发，JS 不可读写

        // FIX 问题一 (2026-07-20): 登出时清理所有定时器、观察者、WS 连接。
        //   避免：1) 多标签场景下定时器继续轮询已注销会话（401 风暴）
        //        2) IntersectionObserver 继续监听已卸载 DOM
        //        3) WS 心跳继续发送
        if (_quotaPollTimer) { clearInterval(_quotaPollTimer); _quotaPollTimer = null; }
        // FIX L8 (2026-07-20): 显式停掉迁移轮询，避免登出后 2s 一次请求触发 401 跳转。
        if (typeof _migratePollTimer !== 'undefined' && _migratePollTimer) {
            clearInterval(_migratePollTimer);
            _migratePollTimer = null;
        }
        if (typeof _stopQRPoll === 'function') { try { _stopQRPoll(); } catch(e) {} }
        if (typeof _stopSSE === 'function') { try { _stopSSE(); } catch(e) {} }
        if (_state._historyObserver) { try { _state._historyObserver.disconnect(); } catch(e) {} _state._historyObserver = null; }
        if (typeof window._clearSessionRefreshTimer === 'function') { try { window._clearSessionRefreshTimer(); } catch(e) {} }

        // 清除本地状态，跳转到认证页面
        _state.token = "";
        // FIX P0-5 (2026-07-20): 同步清登录态标志位
        _state.loggedIn = false;
        try { localStorage.removeItem("ilink-theme"); } catch(_) {}
        window.location.href = "/auth";
    };

    const _renderUserMgmtList = function() {
        var container = document.getElementById("user-mgmt-list");
        if (!container) return;
        if (!_state.users || _state.users.length === 0) {
            container.innerHTML = '<div style="text-align:center;padding:40px 20px;color:var(--text-hint);">暂无用户</div>';
            return;
        }
        var html = '';
        _state.users.forEach(function(userId) {
            var nickname = _state.nicknames[userId] || '';
            var displayName = nickname || userId;
            // Phase 5 (LOW-1): 属性上下文（data-user-id="..."）必须用 _escapeAttr
            //   而非 _escape（_escape 用 DOM textContent→innerHTML，只转义 < > &，
            //   不转义 " '，可被 userId 中的引号破坏属性边界导致 XSS）。
            //   文本内容（<div>...</div> 内）继续用 _escape。
            var uidAttr = _escapeAttr(userId);
            html += '<div class="user-mgmt-item" data-user-id="' + uidAttr + '">' +
                '<div class="user-mgmt-item-info">' +
                '<div class="user-mgmt-item-name">' + _escape(displayName) + '</div>' +
                '<div class="user-mgmt-item-id">' + _escape(userId) + '</div>' +
                '</div>' +
                '<div class="user-mgmt-actions">' +
                '<button class="user-mgmt-clear-btn" data-user-id="' + uidAttr + '">清空</button>' +
                '<button class="user-mgmt-delete-btn" data-user-id="' + uidAttr + '">删除</button>' +
                '</div>' +
                '</div>';
        });
        container.innerHTML = html;
        // FIX M13 (2026-07-20): 用事件委托替代 querySelectorAll().forEach(addEventListener)。
        //   原实现每渲染一次 N 个用户就挂 3N 个监听器，切换用户时 GC 压力陡增。
        //   现在仅在容器上挂一个委托监听器，O(1) 监听器，自动适配 innerHTML 重建。
        //   注意：仅在未挂过委托监听器时挂一次（防止 _renderUserList 多次调用时重复挂载）。
        if (!container._delegatedClick) {
            container._delegatedClick = true;
            container.addEventListener('click', function(e) {
                var t = e.target;
                var deleteBtn = t.closest && t.closest('.user-mgmt-delete-btn');
                if (deleteBtn) {
                    e.stopPropagation();
                    var uid = deleteBtn.getAttribute('data-user-id');
                    if (uid) _confirmAndDelete(uid);
                    return;
                }
                var clearBtn = t.closest && t.closest('.user-mgmt-clear-btn');
                if (clearBtn) {
                    e.stopPropagation();
                    var uid2 = clearBtn.getAttribute('data-user-id');
                    if (uid2) _confirmAndClear(uid2);
                    return;
                }
                var item = t.closest && t.closest('.user-mgmt-item');
                if (item) {
                    var uid3 = item.getAttribute('data-user-id');
                    if (uid3) _openChat(uid3);
                }
            });
        }
    };
    // FIX M13/L3 (2026-07-20): confirm() 包装函数，集中在这一处调用 _showConfirm 模态。
    //   ponytail: 用户管理 / 退出登录等所有"二次确认"统一走同一入口。
    var _confirmAndDelete = function(uid) {
        _showConfirm('确定删除用户 ' + uid + ' 及其所有聊天记录？', function() { _deleteUser(uid); });
    };
    var _confirmAndClear = function(uid) {
        _showConfirm('确定清空该用户的聊天记录？', function() { _clearChatHistory(uid); });
    };

    const _loadAbout = function() {
        const authorEl = document.getElementById("about-author");
        const versionEl = document.getElementById("about-version");
        const modifierEl = document.getElementById("about-modifier");
        // 从后端 API 获取版本号，避免硬编码
        _get("about").then(function(cfg) {
            if (authorEl) authorEl.textContent = (cfg && cfg.author) || "ZynSync";
            if (versionEl) versionEl.textContent = (cfg && cfg.version) || "3.0-wm1";
            if (modifierEl) modifierEl.textContent = (cfg && cfg.modifier) || "Mr.Wong";
        }).catch(function() {
            if (authorEl) authorEl.textContent = "ZynSync";
            if (versionEl) versionEl.textContent = "3.0-wm1";
            if (modifierEl) modifierEl.textContent = "Mr.Wong";
        });
    };
    
    // 二维码轮询计数器：防止无限循环
    var _qrPollCount = 0;
    var _QR_POLL_MAX = 60;        // 最多轮询 60 次（约 3 分钟）
    var _QR_POLL_INTERVAL = 3000; // 默认 3 秒间隔
    var _QR_FAST_INTERVAL = 1500; // 快速模式 1.5 秒（有二维码时）
    var _QR_SCANNED_INTERVAL = 500; // 已扫码后 0.5 秒轮询（等待确认）
    // FIX 2026-07-16: 二维码 expired 自动刷新会让 _qrPollCount 反复清零，
    //   _QR_POLL_MAX 永不触发。改用绝对时间超时兜底，避免轮询永不停止。
    var _qrPollStartTime = 0;
    var _QR_POLL_MAX_MS = 180000; // 3 分钟绝对超时
    // FIX 2026-07-16: QR 轮询激活标志，_stopQRPoll() 可让 _loadQR 提前退出。
    //   用于 WS status 事件确认 login_done 后主动停止 QR 轮询，避免与 _showChat 竞态。
    var _qrPollActive = true;
    const _stopQRPoll = function() {
        _qrPollActive = false;
    };

    const _checkStatus = async function() {
        var loginPage = document.getElementById("login-page");
        // FIX 2026-07-16: 提前启动 WS，让扫码成功的 status 事件能立即到达前端。
        //   之前 _startSSE 只在 _showChat 中调用，扫码前 WS 未建立，
        //   后端 broker.publish("status", {login_done:true}) 事件丢失，
        //   用户必须等 _loadQR 的 3s 轮询才能发现登录成功。
        // FIX P0-5 (2026-07-20): Cookie 迁移后 _state.token 恒为 ""，
        //   原来的 _state.token 真值判断导致扫码期间 WS 永远预启动失败。
        //   改用 _state.loggedIn（由 _loadQuotaUsage 设置）。
        // FIX U4 (2026-07-20): 移除 _state.loggedIn 守卫，无条件尝试 _startSSE。
        //   原守卫会因 _loadQuotaUsage 异步未完成而拦截 WS 启动，导致扫码期间
        //   收不到 qr_state 事件，U4 事件驱动退化回 1.5-3s 轮询。
        //   Cookie 无效时后端返 401，_startSSE 的 onclose 会指数退避重连，
        //   一旦 _loadQuotaUsage 完成（Cookie 已写入）下次重连即成功。
        if (typeof _startSSE === 'function') {
            _startSSE().catch(function(err) { console.warn("[ws] start error:", err && err.message || err); });
        }
        try {
            var e = await _get("status");
        } catch(err) {
            // 后端可能还在启动中，稍后重试
            if (loginPage) loginPage.style.display = "";
            var st = document.getElementById("status-text");
            if (st) st.textContent = "正在连接服务...";
            setTimeout(function() { _checkStatus().catch(function(er) { console.warn("[check-status] retry error:", er && er.message || er); }); }, 2000);
            return false;
        }
        // 同步 WebDAV / 省流量模式状态
        if (e) {
            _state.webdavEnabled = !!e.webdav_enabled;
            _state.trafficSaver = !!e.traffic_saver;
            if (_state.webdavEnabled) {
                _refreshWebDavAuth().catch(function() {});
            }
        }
        if (e && ((e.logged_in && e.login_done) || (e.users && e.users.length > 0))) {
            _showChat(e);
            _qrPollStartTime = 0; // 登录成功，重置绝对超时计时（下次扫码重新计时）
            // FIX S73: 同步重置 _qrPollCount，避免下次扫码时累计旧计数器触发误超时
            _qrPollCount = 0;
            return true;
        }
        // 未绑定 iLink（注册后无连接缓存）：显示登录页二维码，等待微信扫码。
        // 此前无论是否绑定都直接 _showChat，导致新用户永远看不到二维码。
        var hasBind = !!(e && e.bot_accounts && e.bot_accounts.length > 0);
        var isLoggedIn = !!(e && e.logged_in && e.login_done);
        if (e && !hasBind && !isLoggedIn) {
            if (loginPage) loginPage.style.display = "";
            var tabbar = document.getElementById("bottom-tab-bar");
            if (tabbar) tabbar.classList.add("hidden");
            _qrPollActive = true;
            _qrPollCount = 0;
            _qrPollStartTime = 0;
            _loadQR();
            return true;
        }
        // 已绑定账号但当前未登录（如会话过期）：仍进入聊天视图，
        //   由会话 banner 提示重新扫码，不阻塞界面。
        _showChat(e || {users: []});
        return true;
    };
    
    const _loadQR = async function() {
        if (!_qrPollActive) return;
        _qrPollCount++;

        // FIX 2026-07-16: 绝对超时兜底。二维码 expired 自动刷新会让
        //   _qrPollCount 反复清零，原计数器永不触发，故用绝对时间。
        if (_qrPollStartTime === 0) _qrPollStartTime = Date.now();
        if (Date.now() - _qrPollStartTime > _QR_POLL_MAX_MS) {
            var st = document.getElementById("status-text");
            if (st) st.innerHTML = '<div class="qr-tip" style="color:#e74c3c;">连接超时</div><div class="qr-subtip">请检查网络连接后刷新页面重试</div>';
            var qrEl = document.getElementById("qr-code");
            // FIX U10 (2026-07-20): 超时刷新由 <a href="javascript:..."> 改为 <button>，
            //   支持键盘聚焦/回车触发，且不受 CSP 对 inline event handler 的限制。
            if (qrEl) {
                qrEl.innerHTML = '<div class="qr-loading-spinner"></div><div style="text-align:center;color:var(--text-hint);margin-top:10px;">获取超时，<button class="qr-reload-btn">刷新页面</button>重试</div>';
                var btn = qrEl.querySelector(".qr-reload-btn");
                if (btn) btn.onclick = function() { location.reload(); };
            }
            _qrPollStartTime = 0;
            return;
        }

        // 超过最大轮询次数 → 显示错误
        if (_qrPollCount > _QR_POLL_MAX) {
            var st = document.getElementById("status-text");
            if (st) st.innerHTML = '<div class="qr-tip" style="color:#e74c3c;">连接超时</div><div class="qr-subtip">请检查网络连接后刷新页面重试</div>';
            var qrEl = document.getElementById("qr-code");
            // FIX U10: 同上，<button> 替代 <a href="javascript:...">。
            if (qrEl) {
                qrEl.innerHTML = '<div class="qr-loading-spinner"></div><div style="text-align:center;color:var(--text-hint);margin-top:10px;">获取超时，<button class="qr-reload-btn">刷新页面</button>重试</div>';
                var btn = qrEl.querySelector(".qr-reload-btn");
                if (btn) btn.onclick = function() { location.reload(); };
            }
            return;
        }

        var e;
        try {
            e = await _get("qrcode");
        } catch(err) {
            // 网络错误：等待后重试
            var st = document.getElementById("status-text");
            if (st) st.textContent = "网络错误，正在重试...";
            setTimeout(_loadQR, _QR_POLL_INTERVAL);
            return;
        }

        // FIX S14: await 期间 _stopQRPoll() 可能已将 _qrPollActive 置 false，
        //   必须重新检查，否则会继续渲染过期的二维码/触发跳转
        if (!_qrPollActive) return;

        // 已登录 → 跳转聊天
        if (e && (e.redirect_to_chat || e.login_done)) {
            _toast("检测到已连接，正在跳转...");
            try {
                var t = await _get("status");
                // FIX S14: status await 期间可能已被 _stopQRPoll 停止
                if (!_qrPollActive) return;
                if (t && ((t.logged_in && t.login_done) || (t.users && t.users.length > 0))) {
                    _showChat(t);
                    return;
                }
            } catch(err) {}
            // status 接口暂时不可用，等一下再试
            setTimeout(_loadQR, 1000);
            return;
        }

        // 有二维码 → 渲染
        if (e && e.matrix) {
            _renderQR(e.matrix);
            var state = e.state || "";
            var msg = e.message || "";
            var stEl = document.getElementById("status-text");

            // FIX 2026-07-16: 已扫码后用更短间隔轮询（500ms），
            //   快速检测 confirmed 状态，减少用户等待。
            var nextInterval = _QR_FAST_INTERVAL;
            if (state === "scanned") {
                if (stEl) stEl.innerHTML = '<div class="qr-tip" style="color:#27ae60;">已扫码</div><div class="qr-subtip">请在手机上确认连接...</div>';
                nextInterval = _QR_SCANNED_INTERVAL;
            } else if (state === "expired") {
                if (stEl) stEl.innerHTML = '<div class="qr-tip">二维码已过期</div><div class="qr-subtip">正在自动刷新...</div>';
            } else {
                if (stEl) stEl.innerHTML = '<div class="qr-tip">请使用微信扫码连接</div><div class="qr-subtip">打开手机微信 → 扫一扫 → 确认连接</div>';
            }

            // 继续轮询以检测扫码状态变化
            setTimeout(_loadQR, nextInterval);
            return;
        }

        // 没有二维码：根据后端状态显示不同消息
        var state = (e && e.state) || "";
        var msg = (e && e.message) || "正在获取二维码...";
        var stEl = document.getElementById("status-text");

        if (state === "error") {
            // 后端明确报错
            if (stEl) stEl.innerHTML = '<div class="qr-tip" style="color:#e74c3c;">获取失败</div><div class="qr-subtip">' + _escape(msg) + '</div>';
            // 错误时降低轮询频率
            setTimeout(_loadQR, 5000);
            return;
        }

        if (state === "fetching" || state === "idle") {
            if (stEl) stEl.textContent = msg;
            // 正在获取中，等待较短时间
            setTimeout(_loadQR, _QR_POLL_INTERVAL);
            return;
        }

        // 兜底：显示消息并继续轮询
        if (stEl) stEl.textContent = msg;
        setTimeout(_loadQR, _QR_POLL_INTERVAL);
    };
    
    const _renderQR = function(e) {
        const t = document.getElementById("qr-code");
        if (!t) return;
        const n = e.length;
        const o = e[0].length;
        const winW = window.innerWidth || screen.width;
        let r;
        if (winW < 768) {
            r = Math.min(winW * 0.85, 320);
        } else {
            r = Math.min(300, 350);
        }
        const s = Math.max(5, Math.min(10, Math.floor((r - 80) / o)));
        const a = o * s + 40;
        let c = '<div class="qr-grid" style="grid-template-columns: repeat(' + o + ', ' + s + 'px); width: ' + a + 'px; max-width: 100%; overflow-x: auto; margin: 0 auto;">';
        for (const row of e) {
            for (const cell of row) {
                c += '<div class="qr-cell ' + (cell === " " ? "white" : "") + '" style="width:' + s + 'px;height:' + s + 'px;"></div>';
            }
        }
        c += "</div>";
        t.innerHTML = c;
        // FIX L11 (2026-07-20): QR 码作为图像语义暴露给屏幕阅读器。
        t.setAttribute("role", "img");
        t.setAttribute("aria-label", "微信登录二维码，请使用微信扫一扫确认连接");
        const l = document.getElementById("qr-loading");
        if (l) l.style.display = "none";
        const d = document.getElementById("status-text");
        if (d) d.innerHTML = '<div class="qr-tip">请使用微信扫码连接</div><div class="qr-subtip">打开手机微信 → 扫一扫 → 确认连接</div>';
    };
    
    const _showChat = function(e) {
        // FIX 2026-07-16: 防止 QR 轮询与 WS status 事件竞态双触发 _showChat。
        //   仅在预聊天状态（init/login）允许进入聊天，首次进入后 view 变为 list，
        //   后续重复调用（来自 QR 轮询或 WS status 的二次触发）直接返回。
        if (_state.view !== 'init' && _state.view !== 'login') return;
        const t = document.getElementById("login-page");
        if (t) t.style.display = "none";
        _state.users = e.users || [];
        _state.view = 'list';
        const n = document.getElementById("chat-list-page");
        if (n) n.classList.add("active");
        var tabbar = document.getElementById("bottom-tab-bar");
        if (tabbar) tabbar.classList.remove("hidden");
        _renderChatList();
        _loadChatListPreviews();
        _startPoll();
        // FIX 2026-07-16: toast 只在「真正无用户」时显示，刷新页面不重复提示。
        //   之前无条件弹"请先在手机端发送消息"，但用户已有用户/消息时也被弹，
        //   造成误以为系统要求每次都必须先发消息。
        //   改为：仅当 e.users 为空（首次连接且未扫码）时才提示。
        // FIX S68: 改用 _state._loginToastShown 标志而非 sessionStorage，
        //   登出再登录（新会话）也应重新提示，sessionStorage 在浏览器标签存活期间不会清除。
        if (Array.isArray(e.users) && e.users.length === 0) {
            if (!_state._loginToastShown) {
                _toast("已连接，请在用户端（手机微信）发送一条消息以建立会话", 5000, "info");
                _state._loginToastShown = true;
            }
        }
    };

    const _initMobileViewport = function() {
        if (!window.visualViewport) return;
        var vv = window.visualViewport;
        // FIX M11 (2026-07-20): 用比例阈值替代固定 80px。
        //   iOS Safari 软键盘弹出时 window.innerHeight 也会变化（地址栏隐藏/恢复），
        //   固定 80px 阈值会误判。键盘弹起时 visualViewport.height 通常比 innerHeight 小 30% 以上。
        //   ponytail: 比例阈值比固定像素更鲁棒，键盘收起/弹出时正确切换。
        var _isKeyboardOpen = function() {
            if (!window.innerHeight) return false;
            return (window.innerHeight - vv.height) / window.innerHeight > 0.25;
        };
        var onResize = function() {
            if (_isKeyboardOpen()) {
                document.body.classList.add('keyboard-open');
                var chatContainer = document.querySelector('.chat-container.active');
                if (chatContainer) {
                    chatContainer.style.height = vv.height + 'px';
                }
                var chatPage = document.getElementById('chat-page');
                if (chatPage && chatPage.classList.contains('active')) {
                    chatPage.style.height = vv.height + 'px';
                }
                var settingsPanel = document.getElementById('settings-panel');
                if (settingsPanel && settingsPanel.classList.contains('show')) {
                    settingsPanel.style.height = vv.height + 'px';
                }
                var inputArea = document.querySelector('.chat-container.active .input-area');
                if (inputArea) {
                    inputArea.style.position = 'sticky';
                    inputArea.style.bottom = '0';
                }
                var messagesArea = document.getElementById('messages-area');
                if (messagesArea) {
                    messagesArea.scrollTop = messagesArea.scrollHeight;
                }
                var activeInput = document.querySelector('input:focus, textarea:focus');
                if (activeInput) {
                    setTimeout(function() {
                        activeInput.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
                    }, 80);
                }
            } else {
                document.body.classList.remove('keyboard-open');
                var chatContainers = document.querySelectorAll('.chat-container');
                chatContainers.forEach(function(c) { c.style.height = ''; });
                var chatP = document.getElementById('chat-page');
                if (chatP) chatP.style.height = '';
                var sp = document.getElementById('settings-panel');
                if (sp) sp.style.height = '';
                var inputAreas = document.querySelectorAll('.input-area');
                inputAreas.forEach(function(ia) {
                    ia.style.position = '';
                    ia.style.bottom = '';
                });
            }
        };
        vv.addEventListener('resize', onResize);
        vv.addEventListener('scroll', function() {
            // FIX M11 (2026-07-20): 与 onResize 同一比例阈值。
            if (_isKeyboardOpen()) {
                window.scrollTo(0, 0);
                document.documentElement.scrollTop = 0;
            }
        });
        window.addEventListener('orientationchange', function() {
            setTimeout(function() {
                document.body.classList.remove('keyboard-open');
                var chatContainers = document.querySelectorAll('.chat-container');
                chatContainers.forEach(function(c) { c.style.height = ''; });
                var chatP = document.getElementById('chat-page');
                if (chatP) chatP.style.height = '';
                var sp = document.getElementById('settings-panel');
                if (sp) sp.style.height = '';
                var inputAreas = document.querySelectorAll('.input-area');
                inputAreas.forEach(function(ia) {
                    ia.style.position = '';
                    ia.style.bottom = '';
                });
            }, 200);
        });
    };



    const _initEvents = function() {
        document.addEventListener("error", function(ev) {
            var img = ev.target;
            if (img.tagName === 'IMG' && img.classList.contains('bubble-media-img')) {
                var wrap = img.closest('.bubble-media-img-wrap');
                if (wrap && wrap.dataset.cdn && !wrap.classList.contains('bubble-media-loading') && img.src.indexOf('/api/wasm/media/') === -1) {
                    wrap.classList.add('bubble-media-loading');
                    wrap.innerHTML = '<div class="bubble-media-placeholder">' + _svgImage + '<span>图片</span></div>';
                    window._loadCdnMedia(wrap, true);
                }
                // FIX 2026-07-15: 图片 src 指向 /api/wasm/media/<cache_key> 但缓存未命中 (404)，
                //   后端预取可能还在进行中。显示 loading 占位，等 media_cache_update 事件到达后自动重载。
                if (img.src.indexOf('/api/wasm/media/') !== -1 && (!wrap || !wrap.classList.contains('bubble-media-loading'))) {
                    if (!img.dataset.retryCount) img.dataset.retryCount = '0';
                    var retryCount = parseInt(img.dataset.retryCount, 10) || 0;
                    if (retryCount < 3) {
                        img.dataset.retryCount = String(retryCount + 1);
                        // 延迟重试：3s, 6s, 9s（预取通常在 1-5s 内完成）
                        setTimeout(function() {
                            if (img.parentNode) {
                                img.src = img.src.split('&t=')[0].split('?t=')[0] + '?t=' + Date.now();
                            }
                        }, 3000 * (retryCount + 1));
                    }
                }
            }
        }, true);
        document.addEventListener("click", function(ev) {
            var thumb = ev.target.closest("[data-action='play-video']");
            if (thumb) {
                var videoSrc = thumb.dataset.videoSrc;
                if (videoSrc) {
                    window._previewVideo(videoSrc);
                } else {
                    var imgEl = thumb.querySelector('img');
                    if (imgEl && imgEl.src && imgEl.src.indexOf('/api/wasm/media/') !== -1) {
                        window._previewVideo(imgEl.src);
                    }
                }
                return;
            }
        });
        const e = document.getElementById("send-btn");
        if (e) e.addEventListener("click", _sendMsg);
        const t = document.getElementById("message-input");
        if (t) {
            t.addEventListener("keypress", function(e) { if (e.key === "Enter") { _closeMediaPanel(); _sendMsg(); } });
            t.addEventListener("focus", function() { _closeMediaPanel(); setTimeout(function() { var ma = document.getElementById('messages-area'); if (ma) ma.scrollTop = ma.scrollHeight; }, 100); });
        }
        const plusBtn = document.getElementById("plus-btn");
        if (plusBtn) plusBtn.addEventListener("click", _toggleMediaPanel);
        const photoOpt = document.getElementById("media-photo");
        if (photoOpt) photoOpt.addEventListener("click", function() { document.getElementById("file-photo").click(); });
        const cameraOpt = document.getElementById("media-camera");
        if (cameraOpt) cameraOpt.addEventListener("click", function() { document.getElementById("file-camera").click(); });
        const videoOpt = document.getElementById("media-video");
        if (videoOpt) videoOpt.addEventListener("click", function() { document.getElementById("file-video").click(); });
        const fileOpt = document.getElementById("media-file");
        if (fileOpt) fileOpt.addEventListener("click", function() { document.getElementById("file-doc").click(); });
        const filePhoto = document.getElementById("file-photo");
        if (filePhoto) filePhoto.addEventListener("change", _handlePhotoSelect);
        const fileCamera = document.getElementById("file-camera");
        if (fileCamera) fileCamera.addEventListener("change", _handlePhotoSelect);
        const fileVideo = document.getElementById("file-video");
        if (fileVideo) fileVideo.addEventListener("change", _handleVideoSelect);
        const fileVideoCap = document.getElementById("file-video-capture");
        if (fileVideoCap) fileVideoCap.addEventListener("change", _handleVideoSelect);
        const fileDoc = document.getElementById("file-doc");
        if (fileDoc) fileDoc.addEventListener("change", _handleFileSelect);
        const n = document.getElementById("user-select-btn");
        if (n) n.addEventListener("click", function() { const e = document.getElementById("user-dropdown"); if (e) e.classList.toggle("show"); });
        const chatListSettingsBtn = document.getElementById("chat-list-settings-btn");
        if (chatListSettingsBtn) chatListSettingsBtn.addEventListener("click", _openSettings);
        const tabList = document.getElementById("tab-list");
        if (tabList) tabList.addEventListener("click", _switchToChatList);
        const tabUsers = document.getElementById("tab-users");
        if (tabUsers) tabUsers.addEventListener("click", _switchToUserList);
        const tabSettings = document.getElementById("tab-settings");
        if (tabSettings) tabSettings.addEventListener("click", _switchToSettings);
        const userListAddBtn = document.getElementById("user-list-add-btn");
        if (userListAddBtn) userListAddBtn.addEventListener("click", _startAddUser);
        const addUserCloseBtn = document.getElementById("add-user-close-btn");
        if (addUserCloseBtn) addUserCloseBtn.addEventListener("click", _closeAddUserModal);
        const chatBackBtn = document.getElementById("chat-back-btn");
        if (chatBackBtn) chatBackBtn.addEventListener("click", _backToChatList);
        const chatMenuBtn = document.getElementById("chat-menu-btn");
        if (chatMenuBtn) chatMenuBtn.addEventListener("click", function(e) {
            e.stopPropagation();
            var dd = document.getElementById("chat-menu-dropdown");
            if (dd) dd.classList.toggle("show");
        });
        const chatMenuNickname = document.getElementById("chat-menu-nickname");
        if (chatMenuNickname) chatMenuNickname.addEventListener("click", function() {
            var dd = document.getElementById("chat-menu-dropdown");
            if (dd) dd.classList.remove("show");
            _openNicknameModal();
        });
        const chatMenuExport = document.getElementById("chat-menu-export");
        if (chatMenuExport) chatMenuExport.addEventListener("click", function() {
            var dd = document.getElementById("chat-menu-dropdown");
            if (dd) dd.classList.remove("show");
            _exportChatHistory();
        });
        const nicknameCancelBtn = document.getElementById("nickname-cancel-btn");
        if (nicknameCancelBtn) nicknameCancelBtn.addEventListener("click", _closeNicknameModal);
        const nicknameSaveBtn = document.getElementById("nickname-save-btn");
        if (nicknameSaveBtn) nicknameSaveBtn.addEventListener("click", _saveNickname);
        const nicknameInput = document.getElementById("nickname-input");
        if (nicknameInput) nicknameInput.addEventListener("keypress", function(e) { if (e.key === "Enter") _saveNickname(); });
        document.addEventListener("click", function(e) {
            const t = document.getElementById("user-dropdown");
            const n = document.getElementById("user-select-btn");
            const o = document.getElementById("settings-panel");
            const mediaPanel = document.getElementById("media-panel");
            const plusBtn = document.getElementById("plus-btn");
            const nicknameModal = document.getElementById("nickname-modal");
            if (t && !t.contains(e.target) && n && !n.contains(e.target)) {
                t.classList.remove("show");
            }
            if (o && o.classList.contains("show") && !o.contains(e.target)) {
                var tabbar = document.getElementById("bottom-tab-bar");
                if (!tabbar || !tabbar.contains(e.target)) {
                    _closeSettings();
                }
            }
            if (nicknameModal && nicknameModal.classList.contains("show") && e.target === nicknameModal) {
                _closeNicknameModal();
            }
            var addUserModal = document.getElementById("add-user-modal");
            if (addUserModal && addUserModal.classList.contains("show") && e.target === addUserModal) {
                _closeAddUserModal();
            }
            if (mediaPanel && mediaPanel.classList.contains("show") && !mediaPanel.contains(e.target) && plusBtn && !plusBtn.contains(e.target)) {
                _closeMediaPanel();
            }
            var chatMenuDropdown = document.getElementById("chat-menu-dropdown");
            var chatMenuBtnEl = document.getElementById("chat-menu-btn");
            if (chatMenuDropdown && chatMenuDropdown.classList.contains("show") && !chatMenuDropdown.contains(e.target) && chatMenuBtnEl && !chatMenuBtnEl.contains(e.target)) {
                chatMenuDropdown.classList.remove("show");
            }
        });
        const s = document.getElementById("settings-back-btn");
        if (s) s.addEventListener("click", _closeSettings);
        const themeItem = document.getElementById("settings-theme-item");
        if (themeItem) themeItem.addEventListener("click", _toggleTheme);
        const aboutItem = document.getElementById("settings-about-item");
        if (aboutItem) aboutItem.addEventListener("click", function() { _showSettingsPage('settings-about'); });

        const userMgmtItem = document.getElementById("settings-user-mgmt-item");
        if (userMgmtItem) userMgmtItem.addEventListener("click", function() { _switchToUserList(); });
        const aboutBackBtn = document.getElementById("about-back-btn");
        if (aboutBackBtn) aboutBackBtn.addEventListener("click", function() { _showSettingsPage('settings-main'); });

        // ── WebDAV 设置入口 ──
        const webdavItem = document.getElementById("settings-webdav-item");
        if (webdavItem) webdavItem.addEventListener("click", function() { _showSettingsPage('settings-webdav'); });
        const webdavBackBtn = document.getElementById("webdav-back-btn");
        if (webdavBackBtn) webdavBackBtn.addEventListener("click", function() { _showSettingsPage('settings-main'); });
        const webdavSaveBtn = document.getElementById("webdav-save-btn");
        if (webdavSaveBtn) webdavSaveBtn.addEventListener("click", _saveWebDAVSettings);
        const webdavTestBtn = document.getElementById("webdav-test-btn");
        if (webdavTestBtn) webdavTestBtn.addEventListener("click", _testWebDAVConnection);
        const webdavMigrateBtn = document.getElementById("webdav-migrate-btn");
        if (webdavMigrateBtn) webdavMigrateBtn.addEventListener("click", _migrateMediaToWebDAV);

        // ── Phase 5 (U10): 账号安全入口 ──
        const accountItem = document.getElementById("settings-account-item");
        if (accountItem) accountItem.addEventListener("click", function() { _showSettingsPage('settings-account'); });
        const accountBackBtn = document.getElementById("account-back-btn");
        if (accountBackBtn) accountBackBtn.addEventListener("click", function() { _showSettingsPage('settings-main'); });
        const accountSaveBtn = document.getElementById("account-save-btn");
        if (accountSaveBtn) accountSaveBtn.addEventListener("click", _saveAccountPassword);
        const accountLogoutBtn = document.getElementById("account-logout-btn");
        if (accountLogoutBtn) accountLogoutBtn.addEventListener("click", _logoutAccount);

        // iLink 重新扫码绑定
        const ilinkReauthBtn = document.getElementById("ilink-reauth-btn");
        if (ilinkReauthBtn) ilinkReauthBtn.addEventListener("click", _doReauthILink);
        // QR 页面上的切换账号链接
        const qrSwitchLink = document.getElementById("qr-switch-account-link");
        if (qrSwitchLink) qrSwitchLink.addEventListener("click", _doSwitchILinkAccount);
        const webdavSaverToggle = document.getElementById("webdav-traffic-saver");
        if (webdavSaverToggle) webdavSaverToggle.addEventListener("change", _onTrafficSaverChange);
        // 改造 3.2：配置字段改动 → 标记 dirty → 禁用测试按钮
        // 注意：traffic-saver 有自己的即时保存 handler，不纳入 dirty 检测
        var webdavDirtyIds = ["webdav-enabled", "webdav-url", "webdav-username", "webdav-password", "webdav-base-path", "webdav-auto-migrate"];
        webdavDirtyIds.forEach(function(id) {
            var el = document.getElementById(id);
            if (el) el.addEventListener("change", function() { _setWebDAVDirty(true); });
        });
        // Enable/disable sub-settings when WebDAV enabled toggle changes
        var webdavEnabledEl = document.getElementById("webdav-enabled");
        if (webdavEnabledEl) webdavEnabledEl.addEventListener("change", function() {
            _updateWebdavSubSettingsState(this.checked);
        });
        // 文本输入用 input 事件更实时
        ["webdav-url", "webdav-username", "webdav-password", "webdav-base-path"].forEach(function(id) {
            var el = document.getElementById(id);
            if (el) el.addEventListener("input", function() { _setWebDAVDirty(true); });
        });
    };

    // ── iLink 账号绑定管理 ───────────────────────────────────────
    var _loadILinkBindInfo = function() {
        _get("status").then(function(s) {
            if (!s) return;
            var botIdEl = document.getElementById("ilink-bot-id");
            var userIdEl = document.getElementById("ilink-user-id");
            var stateEl = document.getElementById("ilink-session-state");
            var sectionEl = document.getElementById("ilink-bind-section");
            if (!sectionEl) return;
            if (s.has_token) {
                if (botIdEl) botIdEl.textContent = (s.bot_accounts && s.bot_accounts.length > 0 && s.bot_accounts[0].bot_id) || "\u2014";
                if (userIdEl) userIdEl.textContent = (s.bot_accounts && s.bot_accounts.length > 0 && s.bot_accounts[0].user_id) || "\u2014";
                var stateMap = {"active":"已连接","session_expired":"已过期","reauthing":"重新绑定中","disconnected":"未连接"};
                if (stateEl) stateEl.textContent = stateMap[s.session_state] || s.session_state || "\u2014";
                if (s.session_state === "session_expired") {
                    if (stateEl) stateEl.style.color = "#e74c3c";
                } else if (s.session_state === "active") {
                    if (stateEl) stateEl.style.color = "#27ae60";
                } else {
                    if (stateEl) stateEl.style.color = "";
                }
            } else {
                if (botIdEl) botIdEl.textContent = "未绑定";
                if (userIdEl) userIdEl.textContent = "\u2014";
                if (stateEl) { stateEl.textContent = "未绑定"; stateEl.style.color = ""; }
            }
        }).catch(function() {
            var stateEl = document.getElementById("ilink-session-state");
            if (stateEl) stateEl.textContent = "加载失败";
        });
    };

    var _doReauthILink = function() {
        var msgEl = document.getElementById("ilink-reauth-msg");
        if (msgEl) { msgEl.style.display = "block"; msgEl.textContent = "正在启动重新绑定..."; msgEl.style.color = ""; }
        _api("reauth-start", {}).then(function(r) {
            if (r && r.ok) {
                if (msgEl) { msgEl.textContent = "请到登录页面扫描二维码"; msgEl.style.color = "#27ae60"; }
                _closeSettings();
                _state.view = 'login';
                var chatListPage = document.getElementById("chat-list-page");
                if (chatListPage) chatListPage.classList.remove("active");
                var chatPage = document.getElementById("chat-page");
                if (chatPage) chatPage.classList.remove("active");
                var loginPage = document.getElementById("login-page");
                if (loginPage) loginPage.style.display = "";
                var tabbar = document.getElementById("bottom-tab-bar");
                if (tabbar) tabbar.classList.add("hidden");
                _qrPollActive = true;
                _qrPollCount = 0;
                _qrPollStartTime = 0;
                _loadQR();
            } else {
                if (msgEl) { msgEl.textContent = (r && r.error) || "启动失败"; msgEl.style.color = "#e74c3c"; }
            }
        }).catch(function(e) {
            if (msgEl) { msgEl.textContent = (e && e.message) || "网络错误"; msgEl.style.color = "#e74c3c"; }
        });
    };

    var _doSwitchILinkAccount = function() {
        _doReauthILink();
    };
    
    const _updateThemeUI = function() {
        var label = document.getElementById('theme-label');
        var isDark = document.documentElement.getAttribute('data-theme') === 'dark';
        if (label) label.textContent = isDark ? '深色模式' : '浅色模式';
    };

    const _toggleTheme = function() {
        var isDark = document.documentElement.getAttribute('data-theme') === 'dark';
        if (isDark) {
            document.documentElement.removeAttribute('data-theme');
            // FIX M18 (2026-07-20): 主题存储键名统一为 'ilink-theme'（与 landing 页面一致）。
            //   原代码用 'theme' 键，与登出清理（line 169 removeItem('ilink-theme')）和
            //   landing.html/landing.js 的 'ilink-theme' 键不一致，导致登出后主题残留、
            //   主界面与 landing 页面主题状态不共享。
            localStorage.setItem('ilink-theme', 'light');
        } else {
            document.documentElement.setAttribute('data-theme', 'dark');
            localStorage.setItem('ilink-theme', 'dark');
        }
        _updateThemeUI();
    };

    const _initTheme = function() {
        var saved = localStorage.getItem('ilink-theme');
        if (saved === 'dark') {
            document.documentElement.setAttribute('data-theme', 'dark');
        } else {
            document.documentElement.removeAttribute('data-theme');
        }
        _updateThemeUI();
    };

    // ── WebDAV 设置 ────────────────────────────────────────────
    var _webdavLastSettings = null;
    // 改造 3.2：dirty 标记 — 表单有未保存改动时禁用测试按钮
    var _webdavDirty = false;

    const _setWebDAVDirty = function(dirty) {
        _webdavDirty = !!dirty;
        var btn = document.getElementById("webdav-test-btn");
        if (!btn) return;
        if (_webdavDirty) {
            btn.disabled = true;
            btn.textContent = "请先保存";
        } else {
            btn.disabled = false;
            btn.textContent = "测试连接";
        }
    };

    const _setWebDAVStatusMsg = function(text, kind) {
        var el = document.getElementById("webdav-status-msg");
        if (!el) return;
        el.textContent = text || "";
        el.className = "webdav-status-msg" + (kind ? (" " + kind) : "");
    };

    const _updateWebDAVSummary = function() {
        var summary = document.getElementById("webdav-summary");
        if (!summary) return;
        var bits = [];
        if (_state.webdavEnabled) bits.push("已启用");
        else bits.push("未启用");
        if (_state.trafficSaver) bits.push("省流量模式开启");
        summary.textContent = bits.join(" · ");
    };

    const _updateWebdavSubSettingsState = function(enabled) {
        var trafficSaver = document.getElementById('webdav-traffic-saver');
        var autoMigrate = document.getElementById('webdav-auto-migrate');
        if (trafficSaver) trafficSaver.disabled = !enabled;
        if (autoMigrate) autoMigrate.disabled = !enabled;
    };

    const _loadWebDAVSettings = async function() {
        _setWebDAVStatusMsg("");
        try {
            var res = await _get("webdav-settings");
            if (!res || !res.success) {
                _setWebDAVStatusMsg("加载配置失败：" + ((res && res.error) || "未知错误"), "error");
                return;
            }
            var s = res.settings || {};
            _webdavLastSettings = s;
            var enabled = document.getElementById("webdav-enabled");
            var url = document.getElementById("webdav-url");
            var username = document.getElementById("webdav-username");
            var password = document.getElementById("webdav-password");
            var basePath = document.getElementById("webdav-base-path");
            var saver = document.getElementById("webdav-traffic-saver");
            var autoMigrate = document.getElementById("webdav-auto-migrate");
            if (enabled) enabled.checked = !!s.enabled;
            _updateWebdavSubSettingsState(!!s.enabled);
            if (url) url.value = s.url || "";
            if (username) username.value = s.username || "";
            // 密码：后端已设置时返回 "********"（打码），未设置时返回 ""
            // type="password" 输入框会自动遮罩为圆点/星星，用户看到的是星星而非明文
            // 保存时空值 → 后端保持原密码不变；"********" → 后端识别为"不修改"
            if (password) password.value = s.password || "";
            if (password) password.placeholder = s.password ? "已设置，如需修改请清空后输入" : "请输入 WebDAV 密码";
            if (basePath) basePath.value = s.base_path || "/ilink-media";
            if (saver) saver.checked = !!s.traffic_saver;
            if (autoMigrate) autoMigrate.checked = !!s.auto_migrate_on_save;
            _state.webdavEnabled = !!s.enabled;
            _state.trafficSaver = !!s.traffic_saver;
            if (_state.webdavEnabled) {
                _refreshWebDavAuth().catch(function() {});
            }
            _updateWebDAVSummary();
            // 改造 3.2：配置已从 DB 加载，表单与 DB 一致 → 清除 dirty
            _setWebDAVDirty(false);
            // 检测迁移任务状态，有则恢复 UI 显示
            try {
                var st = await _get("webdav-migrate-status");
                if (st && st.success && st.state) {
                    var state = st.state;
                    if (state.running) {
                        // 仍在跑 → 恢复进度轮询
                        var btn = document.getElementById("webdav-migrate-btn");
                        if (btn) btn.disabled = true;
                        _showWebDAVProgress(true);
                        _stopMigratePoll();
                        _pollMigrateStatus();
                        _migratePollTimer = setInterval(_pollMigrateStatus, 2000);
                    } else if (state.finished_at && state.finished_at > 0) {
                        // 之前有完成的迁移（无论成功或失败）→ 一次性显示最终结果
                        // 避免刷新页面后状态栏残留"正在启动迁移任务..."等旧文本。
                        if (state.error) {
                            _setWebDAVStatusMsg("上次迁移失败：" + state.error, "error");
                        } else if (state.total > 0) {
                            _setWebDAVStatusMsg("上次迁移完成：" + _formatMigrateState(state), "success");
                        }
                        // 没有迁移过（finished_at=0）→ 保持空状态，不显示旧文本
                    }
                }
            } catch (e) { /* 忽略 */ }
        } catch (err) {
            _setWebDAVStatusMsg("加载配置失败：" + (err.message || err), "error");
        }
    };

    const _readWebDAVForm = function() {
        var enabled = document.getElementById("webdav-enabled");
        var url = document.getElementById("webdav-url");
        var username = document.getElementById("webdav-username");
        var password = document.getElementById("webdav-password");
        var basePath = document.getElementById("webdav-base-path");
        var saver = document.getElementById("webdav-traffic-saver");
        var autoMigrate = document.getElementById("webdav-auto-migrate");
        return {
            enabled: !!(enabled && enabled.checked),
            url: (url && url.value || "").trim(),
            username: (username && username.value || "").trim(),
            // 空字符串表示不修改密码：发给后端用 "********" 占位
            password: (password && password.value) || "",
            base_path: (basePath && basePath.value || "").trim() || "/ilink-media",
            traffic_saver: !!(saver && saver.checked),
            auto_migrate_on_save: !!(autoMigrate && autoMigrate.checked)
        };
    };

    const _saveWebDAVSettings = async function() {
        var data = _readWebDAVForm();
        if (data.enabled && !data.url) {
            _setWebDAVStatusMsg("启用前请填写服务地址", "error");
            return;
        }
        // Phase 5 (HIGH-7 / SSRF): WebDAV URL 协议白名单——拒绝 file:/ftp:/gopher:/data:/javascript:/等危险 scheme
        //   防止用户填入 file:///etc/passwd 或 http://169.254.169.254/ (云元数据 SSRF) 等
        //   后端 api_webdav_save 会再次校验,此处前端拦截早返回更友好的错误提示
        if (data.url) {
            var _urlLower = data.url.toLowerCase();
            var _schemeOk = (_urlLower.startsWith("http://") || _urlLower.startsWith("https://"));
            if (!_schemeOk) {
                _setWebDAVStatusMsg("服务地址必须以 http:// 或 https:// 开头", "error");
                return;
            }
            // 拒绝明显的内网/元数据地址(云元数据 SSRF 高危)
            //   注意:这里是尽力而为的客户端校验,真正严格的校验在后端(api_webdav_save)
            //   仅拦截最常见的元数据 IP,允许内网部署(企业内部 WebDAV 合法)
            var _ssrfBad = [
                "169.254.169.254",   // AWS/Azure/GCP 元数据
                "metadata.google.internal", // GCP 元数据
                "metadata.aliyun.com",      // 阿里云元数据
                "100.100.100.200"            // 阿里云元数据
            ];
            for (var i = 0; i < _ssrfBad.length; i++) {
                if (_urlLower.indexOf(_ssrfBad[i]) >= 0) {
                    _setWebDAVStatusMsg("服务地址包含禁止访问的元数据服务", "error");
                    return;
                }
            }
        }
        // Phase 3 (U5): 存储目标切换确认——检测 enabled 状态从当前生效值翻转
        //   服务器存储 ↔ WebDAV 存储是重大变更，需用户二次确认避免误操作
        var currentEnabled = !!_state.webdavEnabled;
        if (data.enabled !== currentEnabled) {
            var confirmMsg = data.enabled
                ? "切换到 WebDAV 存储后，新上传的媒体将保存到你的 WebDAV 服务器。\n\n若需同时迁移已有媒体，请勾选「保存后自动迁移」。\n\n确认切换到 WebDAV 存储？"
                : "切换回服务器存储后，新上传的媒体将保存到服务器本地。\n\n原 WebDAV 上的媒体仍可通过原链接访问，但不再更新。\n\n确认切换回服务器存储？";
            // FIX L3 (2026-07-20): 用 _showConfirm 替代 window.confirm
            _showConfirm(confirmMsg, function() { _applyWebDAVToggle(data); }, function() {
                _setWebDAVStatusMsg("已取消切换", "info");
            });
        } else {
            // 启用状态未变，仅修改字段（URL/用户名/密码/base_path 等）→ 直接保存
            _applyWebDAVToggle(data);
        }
    };
    const _applyWebDAVToggle = async function(data) {
        var payload = {
            enabled: data.enabled,
            url: data.url,
            username: data.username,
            password: data.password === "" ? "********" : data.password,
            base_path: data.base_path,
            traffic_saver: data.traffic_saver,
            auto_migrate_on_save: data.auto_migrate_on_save
        };
        _setWebDAVStatusMsg("保存中...", "info");
        try {
            var res = await _api("webdav-settings", payload);
            if (res && res.success) {
                _setWebDAVStatusMsg("已保存", "success");
                _toast("WebDAV 配置已保存");
                var s = res.settings || {};
                _webdavLastSettings = s;
                _state.webdavEnabled = !!s.enabled;
                _state.trafficSaver = !!s.traffic_saver;
                // 同步 auto_migrate 复选框（后端可能修正值）
                var autoMigrate = document.getElementById("webdav-auto-migrate");
                if (autoMigrate) autoMigrate.checked = !!s.auto_migrate_on_save;
                if (_state.webdavEnabled) {
                    _refreshWebDavAuth().catch(function() {});
                }
                _updateWebDAVSummary();
                // 改造 3.2：保存成功 → 表单与 DB 一致 → 清除 dirty，启用测试按钮
                _setWebDAVDirty(false);
                // 决策 B：若启用了 auto_migrate_on_save，后端会自动触发迁移，前端恢复进度轮询
                if (s.enabled && s.auto_migrate_on_save) {
                    _showWebDAVProgress(true);
                    _stopMigratePoll();
                    _pollMigrateStatus();
                    _migratePollTimer = setInterval(_pollMigrateStatus, 2000);
                    var mBtn = document.getElementById("webdav-migrate-btn");
                    if (mBtn) mBtn.disabled = true;
                }
            } else {
                _setWebDAVStatusMsg("保存失败：" + ((res && res.error) || "未知错误"), "error");
            }
        } catch (err) {
            _setWebDAVStatusMsg("保存失败：" + (err.message || err), "error");
        }
    };

    const _testWebDAVConnection = async function() {
        // 改造 3.2：dirty 时不应触发（按钮已 disabled），此处兜底拦截
        if (_webdavDirty) {
            _setWebDAVStatusMsg("表单有未保存的改动，请先保存再测试", "error");
            return;
        }
        _setWebDAVStatusMsg("测试连接中...", "info");
        try {
            // 后端从 DB 读取已保存的配置测试，不再传 form 字段
            var res = await _api("webdav-test", {});
            if (res && res.ok) {
                _setWebDAVStatusMsg("连接成功：" + (res.message || "OK"), "success");
            } else {
                _setWebDAVStatusMsg("连接失败：" + ((res && res.message) || "未知错误"), "error");
            }
        } catch (err) {
            _setWebDAVStatusMsg("测试失败：" + (err.message || err), "error");
        }
    };

    var _migratePollTimer = null;

    const _formatMigrateState = function(s) {
        if (!s) return "";
        var parts = [];
        parts.push("总计 " + (s.total || 0));
        parts.push("上传 " + (s.uploaded || 0));
        // 改造 3.3：显示覆盖计数（云端同名不同内容被覆盖的文件数）
        if (s.overwritten) parts.push("覆盖 " + s.overwritten);
        parts.push("跳过 " + (s.skipped || 0));
        parts.push("失败 " + (s.failed || 0));
        parts.push("释放本地 " + (s.deleted_local || 0));
        return parts.join(" · ");
    };

    // 改造 3.4：格式化字节速率
    const _formatBytes = function(bps) {
        if (!bps || bps <= 0) return "—";
        if (bps < 1024) return bps.toFixed(0) + " B/s";
        if (bps < 1024 * 1024) return (bps / 1024).toFixed(1) + " KB/s";
        if (bps < 1024 * 1024 * 1024) return (bps / 1024 / 1024).toFixed(1) + " MB/s";
        return (bps / 1024 / 1024 / 1024).toFixed(2) + " GB/s";
    };

    // 改造 3.4：格式化剩余时间
    const _formatEta = function(sec) {
        if (!sec || sec <= 0 || !isFinite(sec)) return "计算中…";
        if (sec < 60) return "剩余约 " + Math.ceil(sec) + "s";
        var m = Math.floor(sec / 60);
        var s = Math.ceil(sec % 60);
        return "剩余约 " + m + "m " + s + "s";
    };

    // 改造 3.4：显示/隐藏进度条容器
    const _showWebDAVProgress = function(show) {
        var wrap = document.getElementById("webdav-progress-wrap");
        if (wrap) wrap.style.display = show ? "" : "none";
    };

    // 改造 3.4：更新进度条 + 速率 + ETA
    const _updateWebDAVProgress = function(s) {
        var fill = document.getElementById("webdav-progress-fill");
        var text = document.getElementById("webdav-progress-text");
        var pct = 0;
        if (s.bytes_total && s.bytes_total > 0) {
            pct = Math.min(100, Math.round(s.bytes_done / s.bytes_total * 100));
        } else if (s.total && s.total > 0) {
            // 没有 bytes 字段时退回用文件数算进度
            pct = Math.min(100, Math.round((s.uploaded + s.skipped + s.failed) / s.total * 100));
        }
        if (fill) fill.style.width = pct + "%";
        if (text) {
            var bits = [pct + "%"];
            if (s.bytes_total && s.bytes_total > 0) {
                bits.push(_formatBytes(s.bytes_per_sec));
                bits.push(_formatEta(s.eta_seconds));
            }
            if (s.current) bits.push("当前: " + String(s.current).slice(0, 12) + "…");
            text.textContent = bits.join(" · ");
        }
    };

    const _stopMigratePoll = function() {
        if (_migratePollTimer) {
            clearInterval(_migratePollTimer);
            _migratePollTimer = null;
        }
    };

    const _pollMigrateStatus = function() {
        _get("webdav-migrate-status").then(function(res) {
            if (!res || !res.success) return;
            // FIX 2026-07-16: 后端返回 {success, state}，迁移状态在 res.state 中。
            // 之前直接用 res 作为 s，导致 running 等字段全为 undefined，
            // 状态栏永远卡在"正在启动迁移任务..."且进度条不动。
            var s = res.state || {};
            if (s.running) {
                _setWebDAVStatusMsg("迁移中 " + _formatMigrateState(s), "info");
                _showWebDAVProgress(true);
                _updateWebDAVProgress(s);
            } else {
                _stopMigratePoll();
                if (s.error) {
                    _setWebDAVStatusMsg("迁移失败：" + s.error, "error");
                    _toast("迁移失败");
                } else {
                    _setWebDAVStatusMsg("迁移完成：" + _formatMigrateState(s), "success");
                    _toast("迁移完成");
                }
                // 最终态：进度条拉满后 1.5s 隐藏
                _updateWebDAVProgress(s);
                var fill = document.getElementById("webdav-progress-fill");
                if (fill && !s.error) fill.style.width = "100%";
                setTimeout(function() { _showWebDAVProgress(false); }, 1500);
                var btn = document.getElementById("webdav-migrate-btn");
                if (btn) btn.disabled = false;
            }
        }).catch(function(err) {
            console.warn("[WebDAV] 进度查询失败:", err && err.message);
        });
    };

    const _migrateMediaToWebDAV = function() {
        // FIX L3 (2026-07-20): 用 _showConfirm 替代 window.confirm
        _showConfirm("将本地媒体批量迁移到 WebDAV，迁移成功后会删除本地副本。该过程可能较慢，期间请勿关闭页面。\n\n确定继续？", function() { _doMigrateMediaToWebDAV(); });
    };
    const _doMigrateMediaToWebDAV = async function() {
        _stopMigratePoll();
        var btn = document.getElementById("webdav-migrate-btn");
        if (btn) btn.disabled = true;
        _setWebDAVStatusMsg("正在启动迁移任务...", "info");
        _showWebDAVProgress(true);
        try {
            var res = await _api("webdav-migrate", {});
            if (!res || !res.success) {
                _setWebDAVStatusMsg("启动失败：" + ((res && res.error) || "未知错误"), "error");
                _showWebDAVProgress(false);
                if (btn) btn.disabled = false;
                return;
            }
            // 立即查询一次，然后周期轮询
            _pollMigrateStatus();
            _migratePollTimer = setInterval(_pollMigrateStatus, 2000);
        } catch (err) {
            _setWebDAVStatusMsg("启动失败：" + (err.message || err), "error");
            _showWebDAVProgress(false);
            if (btn) btn.disabled = false;
        }
    };

    const _onTrafficSaverChange = async function() {
        var saver = document.getElementById("webdav-traffic-saver");
        var checked = !!(saver && saver.checked);
        try {
            var res = await _api("webdav-traffic-saver", { traffic_saver: checked });
            if (res && res.success) {
                _state.trafficSaver = checked;
                _updateWebDAVSummary();
                _toast(checked ? "已开启省流量模式" : "已关闭省流量模式");
                // 重新拉一次历史让占位/真实媒体重新渲染
                if (_state.currentUser && typeof _loadHistory === "function") {
                    _loadHistory(_state.currentUser);
                }
            } else {
                _setWebDAVStatusMsg("切换失败：" + ((res && res.error) || "未知错误"), "error");
                if (saver) saver.checked = !checked; // 回滚
            }
        } catch (err) {
            _setWebDAVStatusMsg("切换失败：" + (err.message || err), "error");
            if (saver) saver.checked = !checked;
        }
    };

