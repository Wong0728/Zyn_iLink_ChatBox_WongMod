/* Zyn iLink ChatBox - Web Interface Script */
    const _state = {
        // FIX H-7 (2026-07-18): session_token 不再嵌入 HTML。
        //   浏览器同源 XHR/fetch/WS 自动携带 HttpOnly Cookie，无需 JS 可读 token。
        //   此字段保留仅供 _state 内部逻辑判断"是否已登录"（登录后会被设为非空），
        //   不再用于构造 X-Session-Token 头或 WS URL query 参数。
        token: "",
        // FIX P0-5 (2026-07-20): 引入显式登录态布尔值。
        //   Cookie 迁移后 _state.token 恒为 ""，但 _state.token 真值判断
        //   仍被 12h 刷新、可见性同步、online 重连、扫码期 WS 预启动复用，
        //   导致这些功能全部失效。改用独立的 loggedIn 标志位由 /api/wasm/me
        //   响应设置，与 token 解耦。
        loggedIn: false,
        webUid: 0,  // Web 注册用户 uid（来自 /api/wasm/me），用于 /notify 通知按 target_uid 过滤
        apiBase: "",
        currentUser: null,
        lastMsgId: 0,
        pollInterval: null,
        displayedIds: {},  // { userId: Set<namespacedKey> } 按用户隔离；键已命名空间化，如 "id:1"、"row:2"、"client:..."，避免 messages.id 与 messages_v2.row_id 等不同序列互相碰撞
        users: [],
        selectedMessage: null,
        view: "init",
        nicknames: (function(){
            try { return JSON.parse(localStorage.getItem("zyn_nicknames") || "{}") || {}; }
            catch (e) {
                console.warn("[INIT] 损坏的 zyn_nicknames，已重置:", e);
                try { localStorage.removeItem("zyn_nicknames"); } catch(_) {}
                return {};
            }
        })(),
        lastMessages: {},
        _tempMsgId: 0,
        _loadingHistory: false,  // 正在加载历史时阻止并发轮询渲染
        connStatus: "unknown",   // unknown | connected | polling | disconnected
        pollHealth: {},          // 来自后端 /api/wasm/status 的 poll_health
        // FIX L13 (2026-07-20): WS RTT 测量与卡顿提示
        //   ping 发送时戳 + pong 收到时计算 RTT，维护最近 5 次的滑动平均
        //   _updateConnStatus 根据 avgRtt 显示"网络延迟较高"提示
        _pingSentAt: 0,          // 上一次 ping 发送时间戳（0=未发送/已收到 pong）
        _rttHistory: [],         // 最近 5 次 RTT（毫秒）
        _rttAvg: 0,              // 滑动平均 RTT（毫秒）
        webdavEnabled: false,    // 后端是否启用了 WebDAV
        trafficSaver: false,     // 省流量模式：true 时聊天界面不自动加载媒体
        webdavAuth: null,        // WebDAV Basic Auth token，前端直连时使用
        // ── PR3: 出站消息状态机追踪 ──
        // pendingByReqId: req_id(客户端生成) -> pending DOM 元素 data-sending-id
        // pendingByRowId: row_id(后端返回)   -> pending DOM 元素 data-sending-id
        // pendingByClientId: client_id       -> pending DOM 元素 data-sending-id
        pendingByReqId: {},
        pendingByRowId: {},
        pendingByClientId: {},
        // ── PR4: 会话终态 ──
        sessionTerminal: false,        // true 时显示"重新扫码"按钮
        sessionMessage: "",            // 终态提示文字
        reauthRunning: false,          // 用户已点击"重新扫码"
        // FIX M4 (2026-07-20): iLink session_status="disconnected" 的瞬时态标记。
        //   sessionTerminal=false（不需要重新扫码），但 iLink 不可用，阻断 _sendMsg 发送。
        sessionDisconnected: false,
        // ── 延迟失败 toast ──
        // HTTP send 返回非 success 时不立即弹"发送失败"，等 3s 看 SSE send_ack 结果
        _pendingFailureToast: {},      // reqId → setTimeout id
        // ── 历史加载期间消息缓存 ──
        // 当 _loadingHistory 为 true 时，到达的消息暂存于此，加载完成后处理
        _messageQueue: [],
        // FIX U1 (2026-07-20): 历史游标分页相关字段
        //   _historyOldestId：当前用户已加载最旧消息的 id（用于 before 游标）
        //   _historyHasMore：后端是否还有更早消息
        //   _loadingOlder：是否正在加载更早消息（防止 IntersectionObserver 重复触发）
        //   _historyObserver：IntersectionObserver 实例（监听顶部 sentinel）
        //   _historyObserverUser：observer 当前监听对应的 userId（切换用户时重建）
        _historyOldestId: null,
        _historyHasMore: false,
        _loadingOlder: false,
        _historyObserver: null,
        _historyObserverUser: null,
        // ── 快速连续发送队列 ──
        // 将用户快速触发的多条发送请求串行化，保证后端调用/响应按发送顺序完成，
        // 避免后发的消息先返回导致前端展示顺序错乱。
        _sendQueue: [],
        _sendQueueRunning: false,
        // FIX U3 (2026-07-20): 媒体上传 xhr 引用（reqId -> XMLHttpRequest），
        //   支持用户点击取消按钮调用 xhr.abort() 终止上传。
        _pendingUploads: {},
    };
    // FIX M12 (2026-07-20): 统一媒体类型映射。
    //   后端有时返回数字（media_type: 2/3/4/5），前端有时用字符串（"image"/"voice"），
    //   原代码每个分支都重复写 `mt === "image" || mt === 2 || ...` 12+ 次。
    //   ponytail: 用单一真值表 + helper 收敛，未来扩展只改一处。
    var _mediaTypeMap = {image: 2, video: 5, voice: 3, file: 4};
    var _matchesMediaType = function(mt, type) {
        if (mt === type || mt === _mediaTypeMap[type]) return true;
        return false;
    };
    var _isAnyMedia = function(mt) {
        return mt === "image" || mt === 2 || mt === "video" || mt === 5 ||
               mt === "voice" || mt === 3 || mt === "file"  || mt === 4;
    };
    // FIX M2/M5 (2026-07-20): 语音条波形高度统一工具函数。
    //   审计报告 M2/M5：原实现把确定性公式复制在 zn-render.js / zn-media.js / zn-connection.js
    //   共 5 处（_renderMsg、_renderSendingMsg、_loadSingleMedia、_handleMediaCacheUpdate、
    //   _loadCdnMedia），任何一处忘改种子或循环上限都会让同一消息渲染出不同波形。
    //   ponytail: 收敛到单一函数，未来调整波形只需改此处。
    //   公式与原 S65 一致：seed = id>0 ? id : (row_id>0 ? row_id : 0)；
    //   高度 = 6 + (seed + i*7 + dur*3) mod 14；最多 12 根。
    const _voiceBarHtml = function(msg, dur) {
        var seconds = dur || 1;
        var seed = (msg && typeof msg.id === 'number' && msg.id > 0) ? msg.id :
                   (msg && typeof msg.row_id === 'number' && msg.row_id > 0) ? msg.row_id : 0;
        var bars = "";
        var maxBars = Math.min(seconds, 12);
        for (var i = 0; i < maxBars; i++) {
            var h = 6 + ((seed + i * 7 + (seconds * 3)) % 14);
            bars += '<div class="bubble-media-voice-bar" style="height:' + h + 'px"></div>';
        }
        return bars;
    };
