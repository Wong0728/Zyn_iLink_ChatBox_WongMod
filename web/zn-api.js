
    // 安全更新 lastMsgId：只接受数字 id，避免 "srv_xxx"/"sending_xxx" 把 lastMsgId 污染成 NaN，
    // 进而 ?since=NaN 让后端 422、整个增量轮询失效。
    const _bumpLastMsgId = function(id) {
        var n = (typeof id === "number") ? id : Number(id);
        if (!Number.isFinite(n)) return;
        if (n > _state.lastMsgId) _state.lastMsgId = n;
    };
    

    // 全局 401 处理：session 过期时跳转到认证页面
    let _isReloadingFor401 = false;
    let _lastAuthReload = 0;
    const _handle401 = function() {
        if (_isReloadingFor401) return;
        // 避免频繁刷新：30 秒内不重复触发
        var now = Date.now();
        if (now - _lastAuthReload < 30000) return;
        _lastAuthReload = now;
        _isReloadingFor401 = true;
        // FIX P0-5 (2026-07-20): 立即清登录态，避免跳转期间 12h refresh /
        //   visibility / online 等守卫看到 loggedIn=true 重复触发请求。
        if (typeof _state !== "undefined") _state.loggedIn = false;
        console.warn("[AUTH] session 过期，跳转认证页面...");
        try { _toast("登录已过期，正在跳转登录页...", 2000, "error"); } catch(_) {}
        setTimeout(function() { window.location.href = "/auth"; }, 2000);
    };

    const _api = function(e, t) {
        return new Promise((function(r, n) {
            const o = new XMLHttpRequest();
            o.open("POST", "/api/wasm/" + e, true);
            o.setRequestHeader("Content-Type", "application/json");
            // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头。
            //   浏览器同源 XHR 自动携带 HttpOnly Cookie (session_token)，
            //   后端 extract_session_token 已支持 cookie 解析。
            //   保留 _state.token 字段以兼容老代码（登录响应会更新它），但不再用它做认证。
            // send 操作后端有 25s 超时保护（15s HTTP + 1 次重试），前端给 30s 余量；
            // send-media 涉及大文件上传+CDN 传输，需要更长超时
            o.timeout = (e === "send-media") ? 180000 : 30000;
            var _settled = false;
            var _safeReject = function(err) {
                if (_settled) return;
                _settled = true;
                n(err);
            };
            o.onload = function() {
                if (o.status === 401) { _handle401(); return _safeReject(new Error("401 Unauthorized")); }
                // Phase 3 (§7.3): 403 功能禁用——管理员关闭 upload/webdav/custom_webdav 时拒绝
                if (o.status === 403) {
                    var _fmsg = "操作被禁止";
                    try { var _fbody = JSON.parse(o.responseText); _fmsg = _fbody.message || _fbody.error || _fmsg; } catch(_) {}
                    try { _toast(_fmsg, 4000, "error"); } catch(_) {}
                    return _safeReject(new Error("HTTP 403: " + _fmsg));
                }
                // Phase 3: 413 配额超限 / 429 频率超限——解析 message 字段直接 toast（U9）
                if (o.status === 413 || o.status === 429) {
                    var _qmsg = "操作受限";
                    try { var _qbody = JSON.parse(o.responseText); _qmsg = _qbody.message || _qbody.error || _qmsg; } catch(_) {}
                    try { _toast(_qmsg, 4000, "error"); } catch(_) {}
                    return _safeReject(new Error("HTTP " + o.status + ": " + _qmsg));
                }
                if (o.status >= 200 && o.status < 300) {
                    try {
                        r(JSON.parse(o.responseText));
                    } catch(e) {
                        console.warn("[API] JSON parse error:", o.responseText.slice(0, 200));
                        r({});
                    }
                } else {
                    var detail = "";
                    try { var body = JSON.parse(o.responseText); detail = body.error || body.detail || ""; } catch(e2) {}
                    _safeReject(new Error("HTTP " + o.status + (detail ? ": " + detail : "")));
                }
            };
            o.onerror = function() { return _safeReject(new Error("Network Error")); };
            o.ontimeout = function() { return _safeReject(new Error("请求超时")); };
            o.onabort = function() { return _safeReject(new Error("请求被取消")); };
            try {
                o.send(JSON.stringify(t || {}));
            } catch (err) {
                _safeReject(err);
            }
        }));
    };

    const _get = function(e) {
        return new Promise((function(r, n) {
            const o = new XMLHttpRequest();
            o.open("GET", "/api/wasm/" + e, true);
            // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 Cookie 自动携带。
            o.timeout = 15000;
            var _settled = false;
            var _safeReject = function(err) {
                if (_settled) return;
                _settled = true;
                n(err);
            };
            o.onload = function() {
                if (o.status === 401) { _handle401(); return _safeReject(new Error("401 Unauthorized")); }
                // Phase 3 (§7.3): 403 功能禁用——管理员关闭 upload/webdav/custom_webdav 时拒绝
                if (o.status === 403) {
                    var _fmsg = "操作被禁止";
                    try { var _fbody = JSON.parse(o.responseText); _fmsg = _fbody.message || _fbody.error || _fmsg; } catch(_) {}
                    try { _toast(_fmsg, 4000, "error"); } catch(_) {}
                    return _safeReject(new Error("HTTP 403: " + _fmsg));
                }
                // Phase 3: 413 配额超限 / 429 频率超限——解析 message 字段直接 toast（U9）
                if (o.status === 413 || o.status === 429) {
                    var _qmsg = "操作受限";
                    try { var _qbody = JSON.parse(o.responseText); _qmsg = _qbody.message || _qbody.error || _qmsg; } catch(_) {}
                    try { _toast(_qmsg, 4000, "error"); } catch(_) {}
                    return _safeReject(new Error("HTTP " + o.status + ": " + _qmsg));
                }
                if (o.status >= 200 && o.status < 300) {
                    try {
                        r(JSON.parse(o.responseText));
                    } catch(e) {
                        r({});
                    }
                } else {
                    var detail = "";
                    try { var body = JSON.parse(o.responseText); detail = body.error || body.detail || ""; } catch(e2) {}
                    _safeReject(new Error("HTTP " + o.status + (detail ? ": " + detail : "")));
                }
            };
            o.onerror = function() { return _safeReject(new Error("Network Error")); };
            o.ontimeout = function() { return _safeReject(new Error("请求超时")); };
            o.onabort = function() { return _safeReject(new Error("请求被取消")); };
            try {
                o.send();
            } catch (err) {
                _safeReject(err);
            }
        }));
    };
    
