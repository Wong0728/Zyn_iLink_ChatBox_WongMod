    // FIX S13: 保存 session 刷新 interval 的 ID 到模块级变量，
    //   便于后续在登出/页面卸载时 clearInterval，避免内存泄漏与重复触发
    var _sessionRefreshTimer = null;

    const _init = function() {
        _initTheme();
        _initMobileViewport();
        _initEvents();
        _checkStatus();
        // Phase 5 (U7): 页面初始化时立即加载一次用量信息，使 chat-list-header 的用户名 pill
        //   在首次显示时就更新（不必等到用户打开设置面板才触发 _startQuotaPoll）。
        //   zn-settings.js 先于 zn-main.js 加载，_loadQuotaUsage 此处已可用。
        if (typeof _loadQuotaUsage === 'function') {
            try { _loadQuotaUsage(); } catch(e) { /* 静默失败：未登录或 token 无效时不影响主流程 */ }
        }
        // 每 12 小时刷新一次 session token，防止长期打开页面后 token 过期
        _sessionRefreshTimer = setInterval(function() {
            // FIX P0-5 (2026-07-20): 用 _state.loggedIn 替换 _state.token 真值判断。
            //   Cookie 迁移后 _state.token 恒为 ""，原守卫导致 12h refresh 永不触发，
            //   长期挂机用户在 cookie 过期前没有自动续期，被迫重新登录。
            if (!_state.loggedIn) return;
            _get("refresh-session").then(function(res) {
                // FIX P0-8 (2026-07-20): refresh-session 不再返回 session_token，
                //   仅靠 Set-Cookie 刷新浏览器 Cookie。判断 success 即可，
                //   并同步显式登录态布尔值。
                if (res && res.success) {
                    _state.loggedIn = true;
                }
            }).catch(function(err) {
                console.warn("[AUTH] session 刷新失败:", err.message || err);
            });
        }, 12 * 60 * 60 * 1000);
    };

    // ── 导出聊天记录 ──
    const _exportChatHistory = function() {
        if (!_state.currentUser) return;
        fetch('/api/wasm/export-history', {
            method: 'POST',
            // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie。
            headers: {'Content-Type': 'application/json'},
            body: JSON.stringify({user_id: _state.currentUser})
        }).then(function(r) {
            // Phase 5 (HIGH-6): 统一鉴权/配额错误处理（_checkFetchAuth 定义在 zn-media.js）
            //   - 401 → _handle401 跳转登录页
            //   - 403/413/429 → toast + 解析 body.message
            //   - 200 / 其他 → 让后续 .blob() 处理（_checkFetchAuth 直接返回 r）
            if (typeof _checkFetchAuth === 'function') {
                return _checkFetchAuth(r, "导出受限");
            }
            if (!r.ok) throw new Error('HTTP ' + r.status);
            return r;
        }).then(function(r) {
            return r.blob();
        }).then(function(blob) {
            var url = URL.createObjectURL(blob);
            var a = document.createElement('a');
            var now = new Date();
            var ts = now.getFullYear() +
                String(now.getMonth()+1).padStart(2,'0') +
                String(now.getDate()).padStart(2,'0') + '_' +
                String(now.getHours()).padStart(2,'0') +
                String(now.getMinutes()).padStart(2,'0') +
                String(now.getSeconds()).padStart(2,'0');
            a.href = url;
            a.download = _state.currentUser + '_chat_history_' + ts + '.html';
            document.body.appendChild(a);
            a.click();
            setTimeout(function() { URL.revokeObjectURL(url); try { document.body.removeChild(a); } catch(e) {} }, 1000);
        }).catch(function(err) {
            _toast("导出失败: " + (err.message || err));
        });
    };

    // ── 清空聊天记录（批量删除） ──
    const _clearChatHistory = async function(userId) {
        if (!userId) return;
        try {
            var result = await _api("clear-messages", { user_id: userId });
            if (result && result.success) {
                _toast("已清空聊天记录");
                _renderChatList();
                _loadChatListPreviews();
                _renderUserMgmtList();
            } else {
                _toast((result && result.error) || "清空失败");
            }
        } catch(e) {
            _toast("清空失败");
        }
    };

    // 认证已移至独立页面 /auth，chat 页面只负责聊天功能
    // 未认证用户会在页面加载时被前端脚本重定向到 /auth

    var _bootstrap = function() {
        _init();
    };
    
    window.ZynChat = {
        init: _bootstrap
    };
    
    // 页面加载完成后自动初始化
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', _bootstrap);
    } else {
        _bootstrap();
    }

    // 关闭/隐藏时关闭 EventSource，避免后台挂着无用连接
    window.addEventListener('pagehide', _stopSSE);
    window.addEventListener('pagehide', _stopMigratePoll);
    window.addEventListener('beforeunload', _stopSSE);

    // FIX 问题一 (2026-07-20): 暴露全局清理函数，供 zn-settings.js 的 _logoutAccount 调用。
    //   _sessionRefreshTimer 是模块级变量，无法跨文件直接访问。
    window._clearSessionRefreshTimer = function() {
        if (_sessionRefreshTimer) {
            clearInterval(_sessionRefreshTimer);
            _sessionRefreshTimer = null;
        }
    };
