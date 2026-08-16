    const _loadUsers = async function() {
        try {
            const e = await _get("users");
            if (e && e.users) {
                _state.users = e.users;
                if (_state.view === 'list') {
                    _renderChatList();
                    _loadChatListPreviews();
                }
            }
        } catch (err) {
            console.warn("[load-users] error:", err && err.message || err);
        }
    };

    const _renderChatList = function() {
        var container = document.getElementById("chat-list-items");
        if (!container) return;
        if (!_state.users || _state.users.length === 0) {
            // Phase 5 (U8): 空状态 CTA——新用户无聊天对象时，明确引导"添加微信好友开始对话"
            container.innerHTML = '<div class="chat-list-empty"><div class="chat-list-empty-icon"><svg viewBox="0 0 24 24" width="48" height="48" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 01-2 2H7l-4 4V5a2 2 0 012-2h14a2 2 0 012 2z"/></svg></div><div>暂无聊天</div><div class="chat-list-empty-hint" style="font-size:12px;color:var(--text-hint);margin:6px 0 14px;line-height:1.5;">扫码绑定微信后，点击下方按钮添加好友开始对话</div><button id="chat-list-add-user-btn" class="chat-list-add-user-btn">添加微信好友</button></div>';
            var addBtn = document.getElementById("chat-list-add-user-btn");
            if (addBtn) addBtn.addEventListener("click", _startAddUser);
            return;
        }
        var html = '';
        _state.users.forEach(function(userId) {
            var nickname = _state.nicknames[userId] || '';
            var displayName = nickname || userId;
            var lastMsg = _state.lastMessages[userId];
            var preview = '';
            var time = '';
            if (lastMsg) {
                if (lastMsg.media_type) {
                    var mediaLabels = {2: '[图片]', 3: '[语音]', 4: '[文件]', 5: '[视频]', 'image': '[图片]', 'voice': '[语音]', 'file': '[文件]', 'video': '[视频]'};
                    preview = mediaLabels[lastMsg.media_type] || lastMsg.text || '';
                } else {
                    preview = lastMsg.text || '';
                }
                time = lastMsg.time || '';
            }
            html += '<div class="chat-list-item-wrap" data-user-id="' + _escapeAttr(userId) + '">' +
                '<div class="chat-list-item" role="button" tabindex="0">' +
                '<div class="chat-list-item-avatar" style="background:' + _avatarColor(userId) + ';"><span class="chat-list-item-avatar-text">' + _avatarLetter(displayName) + '</span></div>' +
                '<div class="chat-list-item-content">' +
                '<div class="chat-list-item-name">' + _escape(displayName) + '</div>' +
                '<div class="chat-list-item-msg">' + _escape(preview) + '</div>' +
                '</div>' +
                '<div class="chat-list-item-time">' + _escape(time) + '</div>' +
                '</div></div>';
        });
        container.innerHTML = html;
        container.querySelectorAll('.chat-list-item').forEach(function(item) {
            item.addEventListener('click', function() {
                var wrap = item.closest('.chat-list-item-wrap');
                var userId = wrap && wrap.getAttribute('data-user-id');
                if (userId) _openChat(userId);
            });
        });
    };
    
    const _openChat = async function(userId) {
        if (!userId) return;
        // 切换到新会话前先设置加载标志，防止并发轮询渲染干扰
        // FIX S5: 用 try/finally 包裹，确保任何步骤抛错时 _loadingHistory 也能在 finally 复位，
        //   否则 _loadingHistory 永久为 true，后续新消息全部塞队列不渲染。
        _state._loadingHistory = true;
        // FIX U11 (2026-07-20): _openChat 加超时保护。
        //   原 switch-user 慢时会无限等待，用户盯着加载界面无超时无取消。
        //   现加 10s 超时，超时后 toast 提示并回退到列表页，避免用户卡死。
        var _openChatTimedOut = false;
        var _timeoutId = setTimeout(function() {
            _openChatTimedOut = true;
            _toast("加载超时，请检查网络后重试", 4000, "error");
            _state._loadingHistory = false;
            _backToChatList();
        }, 10000);
        try {
            _state.currentUser = userId;
            _state.view = 'chat';
            // 为当前用户创建独立的 displayedIds 集合（_loadHistory 中也会做，但提前创建避免窗口期）
            _state.displayedIds[userId] = new Set();
            var chatListPage = document.getElementById("chat-list-page");
            if (chatListPage) chatListPage.classList.remove("active");
            var userListPage = document.getElementById("user-list-page");
            if (userListPage) userListPage.classList.remove("active");
            var chatPage = document.getElementById("chat-page");
            if (chatPage) chatPage.classList.add("active");
            var tabbar = document.getElementById("bottom-tab-bar");
            if (tabbar) tabbar.classList.add("hidden");
            var title = document.getElementById("chat-header-title");
            if (title) {
                var nickname = _state.nicknames[userId] || '';
                title.textContent = nickname || userId;
            }
            var messagesArea = document.getElementById("messages-area");
            if (messagesArea) messagesArea.innerHTML = '<div class="empty-state"><div class="empty-state-icon"><svg viewBox="0 0 24 24" width="48" height="48" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"><path d="M12 2v4m0 12v4m-7.07-3.93l2.83-2.83m8.48-8.48l2.83-2.83M2 12h4m12 0h4M4.93 4.93l2.83 2.83m8.48 8.48l2.83 2.83"/></svg></div><div>正在加载历史消息...</div></div>';
            _state.lastMsgId = 0;
            await _api("switch-user", { user_id: userId });
            // 超时已触发则不再继续
            if (_openChatTimedOut) return;
            await _loadHistory(userId);
            if (_openChatTimedOut) return;
            // PR5: F5 后恢复未完成的出站消息（pending/failed）
            if (_state._loadOutboundPending) _state._loadOutboundPending(userId);
        } finally {
            clearTimeout(_timeoutId);
            _state._loadingHistory = false;
        }
    };

    const _backToChatList = function() {
        _state.view = 'list';
        _state.currentUser = null;
        _state.displayedIds = {};  // 重置为按用户隔离的空对象
        _state.lastMsgId = 0;
        // FIX S37: 同样复位 _loadingHistory，避免 _openChat 异常退出后状态残留
        _state._loadingHistory = false;
        var chatPage = document.getElementById("chat-page");
        if (chatPage) chatPage.classList.remove("active");
        var chatListPage = document.getElementById("chat-list-page");
        if (chatListPage) chatListPage.classList.add("active");
        var tabbar = document.getElementById("bottom-tab-bar");
        if (tabbar) tabbar.classList.remove("hidden");
        _setActiveTab('tab-list');
        _loadUsers();
        _loadChatListPreviews();
    };

    const _setActiveTab = function(tabId) {
        var tabs = document.querySelectorAll('.bottom-tab-item');
        tabs.forEach(function(t) { t.classList.remove('active'); });
        var tab = document.getElementById(tabId);
        if (tab) tab.classList.add('active');
    };

    const _switchToChatList = function() {
        _closeSettings();
        var chatPage = document.getElementById("chat-page");
        if (chatPage) chatPage.classList.remove("active");
        var userListPage = document.getElementById("user-list-page");
        if (userListPage) userListPage.classList.remove("active");
        var chatListPage = document.getElementById("chat-list-page");
        if (chatListPage) chatListPage.classList.add("active");
        var tabbar = document.getElementById("bottom-tab-bar");
        if (tabbar) tabbar.classList.remove("hidden");
        _setActiveTab('tab-list');
        _state.view = 'list';
        _loadChatListPreviews();
    };

    const _switchToUserList = function() {
        _closeSettings();
        var chatPage = document.getElementById("chat-page");
        if (chatPage) chatPage.classList.remove("active");
        var chatListPage = document.getElementById("chat-list-page");
        if (chatListPage) chatListPage.classList.remove("active");
        var userListPage = document.getElementById("user-list-page");
        if (userListPage) userListPage.classList.add("active");
        var tabbar = document.getElementById("bottom-tab-bar");
        if (tabbar) tabbar.classList.remove("hidden");
        _setActiveTab('tab-users');
        _state.view = 'users';
        _renderUserMgmtList();
    };

    const _switchToSettings = function() {
        _openSettings();
    };

    const _deleteUser = async function(userId) {
        if (!userId) return;
        try {
            var result = await _api("delete-user", { user_id: userId });
            if (result && result.success) {
                _toast("已删除");
                if (result.users) {
                    _state.users = result.users;
                } else {
                    _state.users = _state.users.filter(function(u) { return u !== userId; });
                }
                _renderChatList();
                _loadChatListPreviews();
                _renderUserMgmtList();
            } else {
                _toast((result && result.error) || "删除失败");
            }
        } catch(e) {
            _toast("删除失败");
        }
    };

    var _addUserPollTimer = null;
    // FIX S15: 后台等待用户到达的定时器也提升到模块级，便于在模态框关闭时清理
    var _addUserWaitTimer = null;

    const _startAddUser = async function() {
        var modal = document.getElementById("add-user-modal");
        var statusEl = document.getElementById("add-user-status");
        var qrEl = document.getElementById("add-user-qr");
        if (modal) modal.classList.add("show");
        if (statusEl) statusEl.textContent = "正在生成二维码...";
        if (qrEl) qrEl.innerHTML = '<div class="add-user-modal-spinner"></div>';
        
        try {
            var result = await _api("add-user-start", {});
            if (result.status === "already_running") {
                if (statusEl) statusEl.textContent = "已有进行中的添加操作，请等待...";
                _startAddUserPoll();
                return;
            }
            if (result.matrix) {
                _renderAddUserQR(result.matrix);
                if (statusEl) statusEl.textContent = "请使用微信扫码添加新用户";
                _startAddUserPoll();
            } else {
                if (statusEl) statusEl.textContent = "正在生成二维码...";
                _startAddUserPoll();
            }
        } catch(e) {
            if (statusEl) statusEl.textContent = "启动失败，请重试";
        }
    };

    const _startAddUserPoll = function() {
        if (_addUserPollTimer) clearInterval(_addUserPollTimer);
        _addUserPollTimer = setInterval(async function() {
            try {
                var data = await _get("add-user-status");
                var statusEl = document.getElementById("add-user-status");
                var qrEl = document.getElementById("add-user-qr");
                
                if (data.matrix && qrEl && qrEl.querySelector(".add-user-modal-spinner")) {
                    _renderAddUserQR(data.matrix);
                }
                
                var st = data.qrcode_status;
                if (st === "scaned" && statusEl) {
                    statusEl.textContent = "已扫码，请在手机上确认...";
                } else if (st === "done") {
                    if (statusEl) statusEl.textContent = "扫码成功！";
                    if (_addUserPollTimer) { clearInterval(_addUserPollTimer); _addUserPollTimer = null; }
                    // 立即加载一次用户列表
                    await _loadUsers();
                    _renderChatList();
                    _loadChatListPreviews();
                    // 关闭二维码模态框后，在主界面弹出红色 Toast 提示
                    _closeAddUserModal();
                    // FIX S35: 成功事件不应使用 error 红色 Toast
                    _toast("已添加用户，请先在手机端发送消息以建立连接", 5000, "info");
                    // 如果还没有用户，后台继续刷新等待消息到达（最多等 30s）
                    if (_state.users.length === 0) {
                        var waitCount = 0;
                        // FIX S15: 保存到模块级变量 _addUserWaitTimer，便于 _closeAddUserModal 清理
                        _addUserWaitTimer = setInterval(async function() {
                            waitCount++;
                            await _loadUsers();
                            _renderChatList();
                            _loadChatListPreviews();
                            if (_state.users.length > 0 || waitCount >= 15) {
                                clearInterval(_addUserWaitTimer);
                                _addUserWaitTimer = null;
                            }
                        }, 2000);
                    }
                } else if (st === "expired" || st === "timeout") {
                    if (statusEl) statusEl.textContent = "二维码已过期，请重新点击加号重试";
                    if (_addUserPollTimer) { clearInterval(_addUserPollTimer); _addUserPollTimer = null; }
                } else if (st === "error") {
                    if (statusEl) statusEl.textContent = "获取失败，请重试";
                    if (_addUserPollTimer) { clearInterval(_addUserPollTimer); _addUserPollTimer = null; }
                } else if (st === "waiting" && statusEl) {
                    statusEl.textContent = "请使用微信扫码添加新用户";
                }
            } catch(e) { console.warn("[add-user-poll]", e); }
        }, 2000);
    };

    const _renderAddUserQR = function(matrix) {
        var qrEl = document.getElementById("add-user-qr");
        if (!qrEl || !matrix) return;
        var rows = matrix.length;
        var cols = matrix[0].length;
        var cellSize = Math.max(6, Math.min(12, Math.floor(280 / cols)));
        var width = cols * cellSize + 40;
        var html = '<div class="qr-grid" style="grid-template-columns: repeat(' + cols + ', ' + cellSize + 'px); width: ' + width + 'px; max-width: 100%; overflow-x: auto; margin: 0 auto;">';
        for (var i = 0; i < rows; i++) {
            for (var j = 0; j < cols; j++) {
                html += '<div class="qr-cell ' + (matrix[i][j] === " " ? "white" : "") + '" style="width:' + cellSize + 'px;height:' + cellSize + 'px;"></div>';
            }
        }
        html += "</div>";
        qrEl.innerHTML = html;
    };

    const _closeAddUserModal = function() {
        var modal = document.getElementById("add-user-modal");
        if (modal) modal.classList.remove("show");
        if (_addUserPollTimer) { clearInterval(_addUserPollTimer); _addUserPollTimer = null; }
        // FIX S15: 同步清理后台等待用户到达的定时器，避免模态框关闭后仍继续轮询
        if (_addUserWaitTimer) { clearInterval(_addUserWaitTimer); _addUserWaitTimer = null; }
    };

    const _loadChatListPreviews = async function() {
        try {
            var data = await _get("chat-previews");
            if (data && data.previews) {
                for (var userId in data.previews) {
                    if (data.previews.hasOwnProperty(userId)) {
                        var p = data.previews[userId];
                        _state.lastMessages[userId] = {
                            text: p.text || '',
                            time: p.time || '',
                            media_type: p.media_type
                        };
                    }
                }
            }
        } catch(e) { console.warn("[preview]", e); }
        _renderChatList();
    };
    
    const _openNicknameModal = function() {
        if (!_state.currentUser) return;
        var modal = document.getElementById("nickname-modal");
        var input = document.getElementById("nickname-input");
        var userIdDiv = document.getElementById("nickname-modal-userid");
        if (!modal || !input) return;
        if (userIdDiv) userIdDiv.textContent = '用户ID: ' + _state.currentUser;
        input.value = _state.nicknames[_state.currentUser] || '';
        modal.classList.add("show");
        setTimeout(function() { input.focus(); }, 100);
    };
    
    const _closeNicknameModal = function() {
        var modal = document.getElementById("nickname-modal");
        if (modal) modal.classList.remove("show");
    };
    
    const _saveNickname = function() {
        if (!_state.currentUser) return;
        var input = document.getElementById("nickname-input");
        var nickname = input ? input.value.trim() : '';
        if (nickname) {
            _state.nicknames[_state.currentUser] = nickname;
        } else {
            delete _state.nicknames[_state.currentUser];
        }
        localStorage.setItem("zyn_nicknames", JSON.stringify(_state.nicknames));
        var title = document.getElementById("chat-header-title");
        if (title) title.textContent = nickname || _state.currentUser;
        _closeNicknameModal();
        _toast(nickname ? "备注名已保存" : "备注名已清除");
    };
    
    // ── Phase 4: 联系人头像颜色与首字母 ──
    var _avatarColors = ["#FF6B6B","#FFA94D","#FFD43B","#69DB7C","#38D9A9","#4DABF7","#748FFC","#DA77F2","#F783AC","#20C997"];
    var _avatarColorUsed = {};
    const _avatarLetter = function(name) {
        if (!name || name.length === 0) return '?';
        return name.charAt(0).toUpperCase();
    };
    // FIX L5 (2026-07-20): 用 userId hash 取色，替代全局计数器。
    //   原实现 _avatarColorIndex 累加，新增/删除用户时所有用户颜色位置都跟着 shift，
    //   视觉上"刚加的那个人被分配了新颜色，已有用户颜色没变但视觉错位"。
    //   现在对 userId 做 djb2 哈希后 mod 颜色表长度，与用户列表顺序完全解耦。
    //   同时保留 _avatarColorUsed 缓存避免每次重新计算（弱引用即可）。
    const _avatarColor = function(userId) {
        if (!userId) return _avatarColors[0];
        if (_avatarColorUsed[userId]) return _avatarColorUsed[userId];
        var hash = 5381;
        for (var i = 0; i < userId.length; i++) {
            hash = ((hash << 5) + hash) + userId.charCodeAt(i); // hash * 33 + c
            hash = hash & hash; // 转 32 位 int
        }
        var color = _avatarColors[Math.abs(hash) % _avatarColors.length];
        _avatarColorUsed[userId] = color;
        return color;
    };
    
