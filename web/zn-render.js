    // FIX S64: 检查滚动条是否接近底部（容差 100px），
    //   用户向上翻阅历史时不强制滚动到底部，避免打断阅读
    const _isNearBottom = function(t) {
        if (!t) return true;
        return t.scrollTop + t.clientHeight >= t.scrollHeight - 100;
    };

    const _renderMsg = function(e) {
        const t = document.getElementById("messages-area");
        if (!t) return null;
        const n = t.querySelector(".empty-state");
        if (n) n.remove();
        // FIX S64: 渲染前记录是否在底部附近，渲染后只在原本接近底部时才强制滚动
        var wasNearBottom = _isNearBottom(t);
        // FIX L1/L14 (2026-07-20): 直接返回 o（_renderResult 始终 === o 冗余）
        const o = document.createElement("div");
        o.className = "msg-row " + (e.type === "out" ? "out" : "in");
        if (e.id) o.dataset.msgId = e.id;
        // FIX 2026-07-15: 同时设置 rowId，方便 _loadOutboundPending 去重
        if (e.row_id) o.dataset.rowId = e.row_id;
        var bubbleContent = "";
        var mt = e.media_type;

        // 省流量模式：媒体不自动加载，显示低流量信息占位
        // FIX M12 (2026-07-20): 复用 _isAnyMedia 单一真值表。
        var isMedia = _isAnyMedia(mt);
        var saverActive = !!_state.trafficSaver && isMedia;
        if (saverActive) {
            var fileName = e.media_filename || "";
            var fileSize = e.media_filesize ? _formatFileSize(e.media_filesize) : "";
            var dur = e.media_duration ? Math.ceil(e.media_duration / 1000) + '"' : "";
            // FIX M12 (2026-07-20): 复用 _matchesMediaType
            if (_matchesMediaType(mt, "file")) {
                // 文件：显示文件名+大小，几乎不耗流量
                bubbleContent = '<div class="bubble-media-file" data-action="load-media" data-cache-id="' + _escapeAttr(e.media_cache_id || "") + '">' +
                    '<div class="bubble-media-file-icon">' + _svgFile + '</div>' +
                    '<div class="bubble-media-file-info">' +
                    '<div class="bubble-media-file-name">' + _escape(fileName || "文件") + '</div>' +
                    '<div class="bubble-media-file-size">' + _escape(fileSize || "省流量模式") + '</div>' +
                    '</div></div>';
            } else if (_matchesMediaType(mt, "voice")) {
                bubbleContent = '<div class="bubble-media-voice" data-action="load-media" data-cache-id="' + _escapeAttr(e.media_cache_id || "") + '">' + _svgVoice +
                    '<div class="bubble-media-voice-bars" style="opacity:0.4">' +
                    '<div class="bubble-media-voice-bar" style="height:8px"></div><div class="bubble-media-voice-bar" style="height:12px"></div><div class="bubble-media-voice-bar" style="height:10px"></div>' +
                    '</div><div class="bubble-media-voice-dur">' + (dur || "语音") + '</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill" style="width:0%"></div></div></div>';
            } else {
                // 图片/视频：显示类型+文件名（低流量）
                var typeLabel = (mt === "image" || mt === 2) ? "图片" : "视频";
                bubbleContent = '<div class="bubble-media-placeholder bubble-media-saver-inner" data-action="load-media" data-cache-id="' + _escapeAttr(e.media_cache_id || "") + '">' +
                    '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 18 0 9 9 0 0 0-18 0z"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>' +
                    '<span>' + _escape(typeLabel + (fileName ? ' · ' + fileName : '')) + '</span>' +
                    '<span class="bubble-media-saver-tip">点击加载 · ' + _escape(fileSize || "省流量模式") + '</span>' +
                    '</div>';
            }
            o.innerHTML = '<div class="bubble ' + (e.type === "out" ? "out" : "in") + '">' + bubbleContent + '<div class="msg-time-row"><span class="msg-time">' + _escape(e.time || "") + '</span></div></div>';
            o._msgData = e;
            t.appendChild(o);
            // FIX S64: 仅在原本接近底部时才强制滚动到底部
            if (wasNearBottom) t.scrollTop = t.scrollHeight;
            // 省流量模式：绑定点击事件后返回（不继续走下面的媒体渲染）
            if (isMedia) {
                var _bubbleDiv = o.querySelector('.bubble');
                if (_bubbleDiv) {
                    _bubbleDiv.style.cursor = 'pointer';
                    _bubbleDiv.addEventListener('click', function(ev) {
                        ev.stopPropagation();
                        var loadEl = ev.target.closest ? ev.target.closest('[data-action="load-media"]') : null;
                        if (loadEl) {
                            _handleMediaClick(e, loadEl);
                        } else {
                            _handleMediaClick(e, _bubbleDiv);
                        }
                    });
                }
            }
            return o;
        }

        if (mt === "image" || mt === 2) {
            var imgSrc = e.media_data || "";
            var webdavUrl = e.media_webdav_url || "";
            var cacheSrc = (!webdavUrl && e.media_cache_id) ? '/api/wasm/media/' + e.media_cache_id : '';
            // FIX S14: 同时附加 data-cache-key 属性，供 media_cache_update 在 WebDAV 代理 URL 模式下匹配
            var cacheKeyAttr = e.media_cache_id ? ' data-cache-key="' + _escapeAttr(String(e.media_cache_id)) + '"' : '';
            if (webdavUrl) {
                // 代理 URL 是同源的，可以直接设置 src，浏览器原生处理加载
                bubbleContent = '<div class="bubble-media-img-wrap"' + cacheKeyAttr + '><img class="bubble-media-img" loading="lazy" src="' + _escapeAttr(webdavUrl) + '" alt="图片" /></div>';
            } else if (cacheSrc || imgSrc) {
                var cdnAttr = (e.media_cdn && !e.media_cache_id) ? ' data-cdn="' + _escapeAttr(e.media_cdn) + '"' : '';
                var displaySrc = imgSrc || cacheSrc;
                var loadAttr = cacheSrc && imgSrc ? ' data-hq-src="' + _escapeAttr(cacheSrc) + '"' : '';
                bubbleContent = '<div class="bubble-media-img-wrap"' + cdnAttr + loadAttr + cacheKeyAttr + '><img class="bubble-media-img" loading="lazy" src="' + _escapeAttr(displaySrc) + '" alt="图片" /></div>';
            } else if (e.media_cdn) {
                bubbleContent = '<div class="bubble-media-img-wrap bubble-media-loading" data-cdn="' + _escapeAttr(e.media_cdn) + '" data-media-type="image"' + cacheKeyAttr + '>' + _loadingPlaceholder("image") + '</div>';
            } else {
                bubbleContent = '<div class="bubble-media-placeholder">' + _svgImage + '<span>图片</span></div>';
            }
        } else if (_matchesMediaType(mt, "video")) {
            // FIX S14: 视频也附加 data-cache-key，供 media_cache_update 重载 src
            var videoCacheKeyAttr = e.media_cache_id ? ' data-cache-key="' + _escapeAttr(String(e.media_cache_id)) + '"' : '';
            if (e.media_webdav_url) {
                // 代理 URL 是同源的，可以直接设置 video src
                var videoSrc = e.media_webdav_url;
                bubbleContent = '<div class="bubble-media-img-wrap"' + videoCacheKeyAttr + '><div class="bubble-media-video-thumb" data-action="play-video" data-video-src="' + _escapeAttr(videoSrc) + '"><video class="bubble-media-video-thumb-vid" src="' + _escapeAttr(videoSrc) + '" preload="metadata" muted playsinline></video><div class="bubble-media-play-btn">' + _svgPlay + '</div></div></div>';
            } else if (e.media_cache_id) {
                var videoSrc = '/api/wasm/media/' + e.media_cache_id;
                bubbleContent = '<div class="bubble-media-img-wrap"' + videoCacheKeyAttr + '><div class="bubble-media-video-thumb" data-action="play-video" data-video-src="' + _escapeAttr(videoSrc) + '"><video class="bubble-media-video-thumb-vid" src="' + _escapeAttr(videoSrc) + '" preload="metadata" muted playsinline></video><div class="bubble-media-play-btn">' + _svgPlay + '</div></div></div>';
            } else if (e.media_data) {
                bubbleContent = '<div class="bubble-media-img-wrap"><div class="bubble-media-video-thumb" data-action="play-video"><img class="bubble-media-img" loading="lazy" src="' + _escapeAttr(e.media_data) + '" alt="视频" /><div class="bubble-media-play-btn">' + _svgPlay + '</div></div></div>';
            } else if (e.media_cdn) {
                bubbleContent = '<div class="bubble-media-img-wrap bubble-media-loading" data-cdn="' + _escapeAttr(e.media_cdn) + '" data-media-type="video"' + videoCacheKeyAttr + '>' + _loadingPlaceholder("video") + '</div>';
            } else {
                bubbleContent = '<div class="bubble-media-file"><div class="bubble-media-file-icon">' + _svgVideo + '</div><div class="bubble-media-file-info"><div class="bubble-media-file-name">' + _escape(e.media_filename || "视频") + '</div><div class="bubble-media-file-size">' + (e.media_duration ? (e.media_duration / 1000).toFixed(1) + "s" : "") + '</div></div></div>';
            }
        } else if (_matchesMediaType(mt, "file")) {
            // 文件：不自动下载 CDN，直接显示文件信息，点击时触发下载
            var fileIcon = _svgFile;
            var fileName = _escape(e.media_filename || "文件");
            bubbleContent = '<div class="bubble-media-file" data-cdn-file="1">' +
                '<div class="bubble-media-file-icon">' + fileIcon + '</div>' +
                '<div class="bubble-media-file-info">' +
                '<div class="bubble-media-file-name">' + fileName + '</div>' +
                '<div class="bubble-media-file-hint">点击下载</div>' +
                '</div></div>';
        } else if (_matchesMediaType(mt, "voice")) {
            var dur = e.media_duration ? Math.ceil(e.media_duration / 1000) : 1;
            // FIX M2/M5 (2026-07-20): 收敛到 _voiceBarHtml，与 _loadSingleMedia /
            //   _loadCdnMedia / _handleMediaCacheUpdate 共享同一公式，避免"同一消息
            //   在不同位置渲染出不同波形"。
            var bars = _voiceBarHtml(e, dur);
            if (e.media_webdav_url) {
                bubbleContent = '<div class="bubble-media-voice bubble-media-loading" data-webdav-url="' + _escapeAttr(e.media_webdav_url) + '" data-media-type="voice">' + _svgVoice + '<div class="bubble-media-voice-bars">' + bars + '</div><div class="bubble-media-voice-dur">' + dur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div></div>';
            } else if (e.media_cache_id) {
                bubbleContent = '<div class="bubble-media-voice" data-action="play-voice" data-cache-id="' + _escapeAttr(e.media_cache_id || "") + '">' + _svgVoice + '<div class="bubble-media-voice-bars">' + bars + '</div><div class="bubble-media-voice-dur">' + dur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div></div>';
            } else if (e.media_cdn) {
                bubbleContent = '<div class="bubble-media-voice bubble-media-loading" data-cdn="' + _escapeAttr(e.media_cdn) + '" data-media-type="voice">' + _svgVoice + '<div class="bubble-media-voice-bars">' + bars + '</div><div class="bubble-media-voice-dur">' + dur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div></div>';
            } else {
                bubbleContent = '<div class="bubble-media-voice">' + _svgVoice + '<div class="bubble-media-voice-bars">' + bars + '</div><div class="bubble-media-voice-dur">' + dur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div></div>';
            }
        } else {
            bubbleContent = '<div class="bubble-text">' + _escape(e.text || "") + '</div>';
        }
        // FIX 2026-07-15: 历史消息加载时检查 send_state，显示发送失败/过期标记
        var statusHtml = '';
        if (e.type === "out") {
            var ss = e.send_state || "";
            if (ss === "failed" || ss === "expired") {
                statusHtml = '<span class="msg-send-status msg-send-fail">!</span>';
            } else if (ss === "pending" || ss === "sending") {
                statusHtml = '<span class="msg-send-status"><div class="msg-send-loading"></div></span>';
            } else if (ss === "sent" || ss === "delivered") {
                // FIX 2026-07-16: 每个气泡只显示一个勾（用户反馈双勾易误解为重复消息）
                statusHtml = '<span class="msg-send-status msg-send-delivered">✓</span>';
            } else if (e.media_cdn && !e.media_cache_id && (mt !== "file" && mt !== 4)) {
                statusHtml = '<span class="msg-send-status msg-send-loading"></span>';
            }
        }
        o.innerHTML = '<div class="bubble ' + (e.type === "out" ? "out" : "in") + '">' + bubbleContent + '<div class="msg-time-row">' + statusHtml + '<span class="msg-time">' + _escape(e.time || "") + '</span></div></div>';
        o._msgData = e;
        t.appendChild(o);
        // FIX S64: 仅在原本接近底部时才强制滚动到底部
        if (wasNearBottom) t.scrollTop = t.scrollHeight;
        
        // FIX 2026-07-15: 对 send_state 为 failed/expired 的历史消息绑定重试点击
        if (e.type === "out" && (e.send_state === "failed" || e.send_state === "expired")) {
            o.dataset.failedState = e.send_state;
            if (e.req_id) o.dataset.reqId = e.req_id;
            if (e.row_id) o.dataset.rowId = e.row_id;
            var _failSpan = o.querySelector('.msg-send-fail');
            if (_failSpan) {
                _failSpan.style.cursor = 'pointer';
                _failSpan.onclick = function(ev) {
                    ev.stopPropagation();
                    _resendFailed(o);
                };
            }
        }

        // Return element for caller to attach additional data
        // (FIX S16/L1/L14: 直接 return o，不再保留 _renderResult 冗余变量)
        var loadingEl = o.querySelector('.bubble-media-loading');
        if (loadingEl) {
            var webdavUrl = loadingEl.dataset.webdavUrl;
            if (webdavUrl) {
                if (_state.trafficSaver) {
                    loadingEl.classList.add("bubble-media-saver");
                    loadingEl.innerHTML = '<div class="bubble-media-placeholder bubble-media-saver-inner">' +
                        '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 18 0 9 9 0 0 0-18 0z"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>' +
                        '<span>省流量模式已开启</span>' +
                        '<span class="bubble-media-saver-tip">点击加载 · WebDAV 代理</span>' +
                        '</div>';
                } else {
                    _loadWebDavMedia(loadingEl);
                }
            } else {
                // 省流量模式：不自动下载，显示占位 + 提示
                if (_state.trafficSaver) {
                    loadingEl.classList.add("bubble-media-saver");
                    loadingEl.innerHTML = '<div class="bubble-media-placeholder bubble-media-saver-inner">' +
                        '<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 18 0 9 9 0 0 0-18 0z"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>' +
                        '<span>省流量模式已开启</span>' +
                        '<span class="bubble-media-saver-tip">请到设置 → WebDAV 存储中关闭以加载</span>' +
                        '</div>';
                } else {
                    window._loadCdnMedia(loadingEl);
                }
            }
        }
        
        var hqWrap = o.querySelector('.bubble-media-img-wrap[data-hq-src]');
        if (hqWrap) {
            var hqImg = new Image();
            hqImg.onload = (function(wrap, src) {
                return function() {
                    var img = wrap.querySelector('.bubble-media-img');
                    if (img) img.src = src;
                };
            })(hqWrap, hqWrap.dataset.hqSrc);
            hqImg.src = hqWrap.dataset.hqSrc;
        }

        const bubbleDiv = o.querySelector('.bubble');
        if (bubbleDiv) {
            // FIX M12 (2026-07-20): 复用 _isAnyMedia
            var isMediaMsg = _isAnyMedia(mt);
            if (isMediaMsg) {
                bubbleDiv.style.cursor = 'pointer';
                bubbleDiv.addEventListener('click', (function(ev) {
                    ev.stopPropagation();
                    // 省流量模式下点击占位元素
                    var target = ev.target;
                    var loadEl = target.closest ? target.closest('[data-action="load-media"]') : null;
                    if (loadEl) {
                        _handleMediaClick(e, loadEl);
                    } else {
                        // 把 bubbleDiv 作为容器传给 handler，避免 null.querySelector
                        _handleMediaClick(e, bubbleDiv);
                    }
                }));
            }
        }
        return o;
    };

    const _renderSendingMsg = function(e, customSendingId, target) {
        // FIX L11 (2026-07-20): 允许传入 target（DocumentFragment 或 DOM 元素），
        //   批量恢复 pending 时挂到 fragment 上一次性 append，50+ 条从 50 次 reflow 降到 1 次。
        var t = target || document.getElementById("messages-area");
        if (!t) return;
        // FIX S64: 发送消息时也只在接近底部时才强制滚动
        var wasNearBottom = t.id ? _isNearBottom(t) : false;
        // FIX L11: 始终清空真实 messages-area 上的 empty-state（仅在挂在 DOM 上时）
        if (t.id) {
            var n = t.querySelector(".empty-state");
            if (n) n.remove();
        }
        const o = document.createElement("div");
        o.className = "msg-row out";
        // PR3/PR5: 支持自定义 pendingId（F5 恢复时用，避免与新发送冲突）
        var sendingId = customSendingId || e.id;
        o.dataset.sendingId = sendingId;
        if (e.id) o.dataset.msgId = e.id;
        // PR3: 把 req_id/row_id/client_id 存到 dataset，便于 _handleSendAck 兜底匹配
        if (e._reqId) o.dataset.reqId = e._reqId;
        if (e._rowId) o.dataset.rowId = e._rowId;
        if (e._clientId) o.dataset.clientId = e._clientId;
        // 记录目标用户，便于按用户精准移除 pending（避免 SSE 事件误删别的会话）
        var _tgt = e.to || _state.currentUser;
        if (_tgt) o.dataset.targetUser = _tgt;
        var bubbleContent = "";
        var mt = e.media_type;
        if (mt === 2 && e.media_data) {
            bubbleContent = '<div class="bubble-media-img-wrap"><img class="bubble-media-img" src="' + _escapeAttr(e.media_data) + '" alt="图片" /></div>';
        } else if (mt === 5 && e.media_data) {
            bubbleContent = '<div class="bubble-media-img-wrap"><div class="bubble-media-video-thumb"><img class="bubble-media-img" src="' + _escapeAttr(e.media_data) + '" alt="视频" /><div class="bubble-media-play-btn">' + _svgPlay + '</div></div></div>';
        } else if (mt === 3) {
            var dur = e.media_duration ? Math.ceil(e.media_duration / 1000) : 1;
            // FIX M2/M5 (2026-07-20): 收敛到 _voiceBarHtml 工具函数（zn-core.js）。
            var bars = _voiceBarHtml(e, dur);
            bubbleContent = '<div class="bubble-media-voice">' + _svgVoice + '<div class="bubble-media-voice-bars">' + bars + '</div><div class="bubble-media-voice-dur">' + dur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div></div>';
        } else if (mt === 4) {
            bubbleContent = '<div class="bubble-media-file"><div class="bubble-media-file-icon">' + _svgFile + '</div><div class="bubble-media-file-info"><div class="bubble-media-file-name">' + _escape(e.media_filename || "文件") + '</div></div></div>';
        } else {
            bubbleContent = '<div class="bubble-text">' + _escape(e.text || "") + '</div>';
        }
        o.innerHTML = '<div class="bubble out">' + bubbleContent + '<div class="msg-time-row"><span class="msg-send-status msg-send-loading"></span><span class="msg-time">' + _escape(e.time || "") + '</span></div></div>';
        t.appendChild(o);
        // FIX S64: 仅在原本接近底部时才强制滚动到底部（发送自己的消息默认会滚动，因为用户通常在底部）
        if (wasNearBottom) t.scrollTop = t.scrollHeight;
    };

    // ── 用真实消息替换 pending 元素，并保持原位置不变 ──
    // 解决快速连续发送时：后返回的响应先把 pending 删了并在末尾追加，导致顺序错乱。
    const _replacePendingWithRendered = function(pendingEl, msg) {
        if (!pendingEl) return _renderMsg(msg);
        var parent = pendingEl.parentNode;
        if (!parent) return _renderMsg(msg);
        var next = pendingEl.nextSibling;
        // FIX M1 (2026-07-20): 兜底复制 req_id/client_id/row_id 到 msg，
        //   让 _handleSendAck 即便 rowId 为 0 也能通过 clientId/reqId 找到真实元素。
        if (!msg.req_id && pendingEl.dataset.reqId) msg.req_id = pendingEl.dataset.reqId;
        if (!msg.client_id && pendingEl.dataset.clientId) msg.client_id = pendingEl.dataset.clientId;
        if (!msg.row_id && pendingEl.dataset.rowId) msg.row_id = pendingEl.dataset.rowId;
        pendingEl.remove();
        var realEl = _renderMsg(msg);
        if (realEl) {
            if (next) parent.insertBefore(realEl, next);
            else parent.appendChild(realEl);
        }
        return realEl;
    };

