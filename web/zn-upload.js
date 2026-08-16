    const _toggleMediaPanel = function() {
        const panel = document.getElementById("media-panel");
        const btn = document.getElementById("plus-btn");
        if (!panel || !btn) return;
        if (panel.classList.contains("show")) {
            panel.classList.remove("show");
            btn.classList.remove("active");
        } else {
            panel.classList.add("show");
            btn.classList.add("active");
            const input = document.getElementById("message-input");
            if (input) input.blur();
        }
    };
    
    const _closeMediaPanel = function() {
        const panel = document.getElementById("media-panel");
        const btn = document.getElementById("plus-btn");
        if (panel) panel.classList.remove("show");
        if (btn) btn.classList.remove("active");
    };
    
    // ponytail: deleted 4 unused helpers (_showUploadProgress / _hideUploadProgress
    //   / _readFileAsBase64 / _readFileAsArrayBuffer) — replaced by inline FormData
    //   + XHR.upload.progress path in _sendMediaFile (HIGH-1 audit wave, 2026-07).

    const _generateThumbnail = function(file, maxWidth, maxHeight) {
        return new Promise(function(resolve) {
            // 超时保护：5 秒内未生成缩略图则跳过
            var settled = false;
            var timer = setTimeout(function() {
                if (!settled) { settled = true; resolve(""); }
            }, 5000);

            if (file.type && file.type.startsWith("image/")) {
                var img = new Image();
                var url = URL.createObjectURL(file);
                img.onload = function() {
                    if (settled) { URL.revokeObjectURL(url); return; }
                    var w = img.width, h = img.height;
                    var scale = Math.min(maxWidth / w, maxHeight / h, 1);
                    var cw = Math.round(w * scale), ch = Math.round(h * scale);
                    var canvas = document.createElement("canvas");
                    canvas.width = cw; canvas.height = ch;
                    var ctx = canvas.getContext("2d");
                    ctx.drawImage(img, 0, 0, cw, ch);
                    URL.revokeObjectURL(url);
                    var dataUrl = canvas.toDataURL("image/jpeg", 0.6);
                    settled = true; clearTimeout(timer); resolve(dataUrl);
                };
                img.onerror = function() { URL.revokeObjectURL(url); settled = true; clearTimeout(timer); resolve(""); };
                img.src = url;
            } else if (file.type && file.type.startsWith("video/")) {
                var video = document.createElement("video");
                var vurl = URL.createObjectURL(file);
                video.preload = "metadata";
                video.muted = true;
                video.onloadeddata = function() {
                    video.currentTime = Math.min(1, video.duration / 4);
                };
                video.onseeked = function() {
                    if (settled) { URL.revokeObjectURL(vurl); return; }
                    var w = video.videoWidth, h = video.videoHeight;
                    var scale = Math.min(maxWidth / w, maxHeight / h, 1);
                    var cw = Math.round(w * scale), ch = Math.round(h * scale);
                    var canvas = document.createElement("canvas");
                    canvas.width = cw; canvas.height = ch;
                    var ctx = canvas.getContext("2d");
                    ctx.drawImage(video, 0, 0, cw, ch);
                    URL.revokeObjectURL(vurl);
                    var dataUrl = canvas.toDataURL("image/jpeg", 0.6);
                    settled = true; clearTimeout(timer); resolve(dataUrl);
                };
                video.onerror = function() { URL.revokeObjectURL(vurl); settled = true; clearTimeout(timer); resolve(""); };
                video.src = vurl;
            } else {
                settled = true; clearTimeout(timer); resolve("");
            }
        });
    };

    const _sendMediaFile = async function(file, mediaType) {
        if (!_state.currentUser) {
            _toast("请先选择用户");
            return;
        }
        if (!file) return;

        var maxSize = 25 * 1024 * 1024;
        if (file.size > maxSize) {
            _toast("文件过大，最大支持 25MB");
            return;
        }

        // ── 图片格式快速校验 ──
        if (mediaType === "image") {
            var imgExt = (file.name.split('.').pop() || '').toLowerCase();
            var supportedExts = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp'];
            if (imgExt && supportedExts.indexOf(imgExt) === -1) {
                _toast("微信不支持 ." + imgExt + " 格式的图片，请使用 JPG/PNG/GIF/WebP/BMP 格式");
                return;
            }
        }

        _closeMediaPanel();

        var mediaTypeInt = {"image": 2, "voice": 3, "file": 4, "video": 5}[mediaType] || 4;
        var mediaTypeLabel = {"image": "图片", "voice": "语音", "file": "文件", "video": "视频"}[mediaType] || "文件";
        var thumbDataUrl = "";

        if (mediaType === "image") {
            thumbDataUrl = await _generateThumbnail(file, 200, 200);
        } else if (mediaType === "video") {
            thumbDataUrl = await _generateThumbnail(file, 200, 200);
        }

        // FIX: 生成 req_id，用于 send_ack 匹配（虽然当前后端媒体发送是同步的，但为未来异步化预留）
        var reqId = "req-media-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 10);

        var placeholderMsg = {
            from: 'me',
            to: _state.currentUser,
            text: '[' + mediaTypeLabel + '] ' + file.name,
            time: new Date().toTimeString().slice(0, 8),
            type: 'out',
            media_type: mediaTypeInt,
            media_data: thumbDataUrl,
            media_filename: file.name,
            _sending: true,
            _reqId: reqId,  // 关联 req_id
        };

        _state._tempMsgId = (_state._tempMsgId || 0) + 1;
        placeholderMsg.id = "sending_" + _state._tempMsgId;

        _renderSendingMsg(placeholderMsg);

        // FIX: 注册 pending 映射，让 _handleIncomingMessage 能精确匹配并移除占位元素
        _state.pendingByReqId[reqId] = placeholderMsg.id;

        // 超时兜底：180 秒后 API 还未返回则自动标记失败（大视频需要更长时间）
        var _timedOut = false;
        var _sendTimeoutId = setTimeout(function() {
            _timedOut = true;
            // 主动 abort xhr，避免 xhr 继续上传浪费带宽 + xhr.ontimeout 再触发一次错误 toast
            var _pendingXhr = _state._pendingUploads && _state._pendingUploads[reqId];
            if (_pendingXhr) { try { _pendingXhr.abort(); } catch(e) {} }
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 placeholderMsg.id 含 "]" 等特殊字符逃逸 CSS 选择器。
            var staleEl = _findByDataAttr('data-sending-id', placeholderMsg.id);
            if (staleEl) {
                var statusEl = staleEl.querySelector('.msg-send-status');
                if (statusEl) {
                    statusEl.className = 'msg-send-status msg-send-fail';
                    statusEl.textContent = '!';
                }
                staleEl.style.opacity = '0.5';
            }
            _toast("发送超时，请检查消息是否已送达");
            delete _state.pendingByReqId[reqId];
        }, 180000);

        // 更新发送状态文字（保留 spinner，进度文字单独显示）
        var _updateSendStatus = function(text) {
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 placeholderMsg.id 含 "]" 等特殊字符逃逸 CSS 选择器。
            var el = _findByDataAttr('data-sending-id', placeholderMsg.id);
            if (el) {
                var statusEl = el.querySelector('.msg-send-status');
                if (statusEl) {
                    statusEl.className = 'msg-send-status msg-send-uploading';
                    statusEl.innerHTML = '<span class="msg-send-loading"></span><span class="msg-send-progress-text">' + _escape(text) + '</span>';
                }
            }
        };

        // FIX U3 (2026-07-20): 初始化上传进度条 UI（进度条 + 百分比 + 取消按钮）
        //   替代纯文字"上传中 X%"，提供可视化进度条与中止入口。
        //   xhr 引用保存到 _state._pendingUploads[reqId]，取消按钮 click 触发 abort。
        var _initUploadProgress = function() {
            var el = _findByDataAttr('data-sending-id', placeholderMsg.id);
            if (!el) return;
            var statusEl = el.querySelector('.msg-send-status');
            if (!statusEl) return;
            statusEl.className = 'msg-send-status msg-send-uploading';
            statusEl.innerHTML =
                '<div class="upload-progress-wrap">' +
                    '<div class="upload-progress-bar"><div class="upload-progress-fill" style="width:0%"></div></div>' +
                    '<span class="upload-progress-text">0%</span>' +
                    '<button type="button" class="upload-cancel-btn" title="取消上传" aria-label="取消上传">✕</button>' +
                '</div>';
            var cancelBtn = statusEl.querySelector('.upload-cancel-btn');
            if (cancelBtn) {
                cancelBtn.addEventListener('click', function(ev) {
                    ev.stopPropagation();
                    var xhr = _state._pendingUploads[reqId];
                    if (xhr) {
                        try { xhr.abort(); } catch (e) {}
                    }
                });
            }
        };

        // FIX U3: 更新进度条百分比（progress 回调中调用）
        var _updateUploadProgress = function(pct) {
            var el = _findByDataAttr('data-sending-id', placeholderMsg.id);
            if (!el) return;
            var fill = el.querySelector('.upload-progress-fill');
            var text = el.querySelector('.upload-progress-text');
            if (fill) fill.style.width = pct + '%';
            if (text) text.textContent = pct >= 100 ? '发送中...' : (pct + '%');
        };

        try {
            var thumbnailData = "";

            if (mediaType === "image" || mediaType === "video") {
                try {
                    _updateSendStatus("缩略图中...");
                    var fullThumb = await _generateThumbnail(file, 300, 300);
                    if (fullThumb) {
                        thumbnailData = fullThumb.split(",")[1] || "";
                    }
                } catch(e) { console.warn("[thumb]", e); }
            }

            // 使用 FormData + multipart/form-data 上传，避免 base64 编码 33% 体积膨胀
            var formData = new FormData();
            formData.append("media_type", mediaType);
            formData.append("filename", file.name);
            formData.append("thumbnail", thumbnailData);
            formData.append("file", file, file.name);

            _initUploadProgress();

            // FIX S19: 在 await 之前记录当前会话，await 之后校验是否切换
            var _sentUserId = _state.currentUser;

            var result = await new Promise(function(resolve, reject) {
                var xhr = new XMLHttpRequest();
                xhr.open("POST", "/api/wasm/upload-media", true);
                // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie。
                xhr.timeout = 180000;

                // FIX U3 (2026-07-20): 保存 xhr 引用供取消按钮调用 abort()
                _state._pendingUploads[reqId] = xhr;

                // 上传进度回调
                xhr.upload.addEventListener("progress", function(ev) {
                    if (ev.lengthComputable) {
                        var pct = Math.round((ev.loaded / ev.total) * 100);
                        _updateUploadProgress(pct);
                    }
                });

                xhr.onload = function() {
                    if (xhr.status === 401) { _handle401(); reject(new Error("会话过期")); return; }
                    // Phase 5 (HIGH-6): 403/413/429 统一 toast（配额/限制/限流）
                    //   之前只走通用 else 分支 reject(new Error("HTTP 4xx"))，用户只看到"发送失败: HTTP 403"
                    //   无 toast、无后端 message 字段。现补 toast + 解析 body.message。
                    if (xhr.status === 403 || xhr.status === 413 || xhr.status === 429) {
                        var fallback = xhr.status === 403 ? "操作被拒绝"
                                     : xhr.status === 413 ? "文件过大"
                                     : "请求过于频繁，请稍后再试";
                        try { _toast(fallback, 4000, "error"); } catch(_) {}
                        try {
                            var body = JSON.parse(xhr.responseText);
                            if (body && (body.message || body.error)) {
                                try { _toast(body.message || body.error, 4000, "error"); } catch(_) {}
                            }
                        } catch(_) {}
                        reject(new Error("HTTP " + xhr.status + ": " + fallback));
                        return;
                    }
                    if (xhr.status >= 200 && xhr.status < 300) {
                        try { resolve(JSON.parse(xhr.responseText)); }
                        catch(e) { reject(new Error("响应解析失败")); }
                    } else {
                        var detail = "";
                        try { var body = JSON.parse(xhr.responseText); detail = body.error || body.detail || ""; } catch(e2) {}
                        reject(new Error("HTTP " + xhr.status + (detail ? ": " + detail : "")));
                    }
                };
                xhr.onerror = function() { reject(new Error("网络错误")); };
                xhr.ontimeout = function() { reject(new Error("上传超时")); };
                xhr.onabort = function() { reject(new Error("上传被取消")); };
                xhr.send(formData);
            });
            clearTimeout(_sendTimeoutId);
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 placeholderMsg.id 含 "]" 等特殊字符逃逸 CSS 选择器。
            var sendingEl = _findByDataAttr('data-sending-id', placeholderMsg.id);

            if (result && result.success && result.message) {
                var msg = result.message;
                if (!msg.id) {
                    // FIX L13 (2026-07-20): 用 media_cache_id 作稳定键替代 "srv_X" 字符串。
                    //   之前后端返回 id=0 时，前端生成 "srv_X" 字符串 id，后续 dedup 仅靠此字符串；
                    //   WS 推送的同条消息只有 media_cache_id，没有 "srv_X"，三路去重不命中 → 重复渲染。
                    //   现在 msg.id 留 0/undefined，依靠 _anyKeyDisplayed 自动检查 media_cache_id
                    //   作为稳定键，三路（REST 轮询/WS 推送/媒体缓存事件）都能命中同一键。
                    _state._tempMsgId = (_state._tempMsgId || 0) + 1;
                    // 注意：不再赋值 msg.id = "srv_X"，保留原始 id（0/undefined）由 dedup 用其他键匹配
                }
                // 先在占位元素上显示"已发送 ✓"状态，保留缩略图可见
                if (sendingEl) {
                    var statusEl = sendingEl.querySelector('.msg-send-status');
                    if (statusEl) {
                        statusEl.className = 'msg-send-status msg-send-sent';
                        statusEl.textContent = '✓';
                    }
                }
                // FIX: 清除 pending 映射，避免 WS 推送重复处理
                delete _state.pendingByReqId[reqId];
                // FIX U8 (2026-07-20): 移除 600ms 人为延迟。
                //   原延迟"让用户看到 ✓ 反馈"，但期间 WS 事件可创建重复渲染路径。
                //   去重逻辑（_anyKeyDisplayed）已能处理 WS 重复推送，无需用延迟规避。
                //   现在直接立即替换占位元素为真实消息，避免延迟感 + 简化渲染路径。
                var _msgToRender = msg;
                var _placeholderId = placeholderMsg.id;
                var _sendingElRef = sendingEl;
                // FIX S19: 校验会话是否在 await 期间切换
                if (_state.currentUser !== _sentUserId) {
                    // 会话已切换，不渲染
                } else {
                    // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
                    //   避免 _placeholderId 含 "]" 等特殊字符逃逸 CSS 选择器。
                    var curSendingEl = _sendingElRef && document.body.contains(_sendingElRef) ? _sendingElRef : _findByDataAttr('data-sending-id', _placeholderId);
                    if (curSendingEl) curSendingEl.remove();
                    var userForMedia = _state.currentUser;
                    if (userForMedia) {
                        if (!_state.displayedIds[userForMedia]) _state.displayedIds[userForMedia] = new Set();
                        // FIX 2026-07-16: 用统一去重辅助函数（id/row_id/client_id/req_id/media_cache_id）。
                        //   之前只检查 msg.id（后端返回 id=0 时被前端替换为 "srv_X" 字符串），
                        //   WS 事件（无 client_id/req_id/row_id）和 REST 轮询（DB 自增 id）
                        //   都无法匹配 "srv_X"，导致图片被渲染 3 次。
                        //   现在补 media_cache_id 作为稳定键，三路都能命中同一缓存键去重。
                        if (typeof _anyKeyDisplayed === 'function' && _anyKeyDisplayed(_state.displayedIds[userForMedia], _msgToRender)) {
                            if (typeof _addAllDedupKeys === 'function') _addAllDedupKeys(_state.displayedIds[userForMedia], _msgToRender);
                        } else {
                            _renderMsg(_msgToRender);
                            if (typeof _addAllDedupKeys === 'function') {
                                _addAllDedupKeys(_state.displayedIds[userForMedia], _msgToRender);
                            }
                        }
                    } else {
                        _renderMsg(_msgToRender);
                    }
                    if (typeof _msgToRender.id === "number") {
                        _bumpLastMsgId(_msgToRender.id);
                    }
                }
            } else {
                if (sendingEl) {
                    var statusEl = sendingEl.querySelector('.msg-send-status');
                    if (statusEl) {
                        statusEl.className = 'msg-send-status msg-send-fail';
                        statusEl.textContent = '!';
                    }
                }
                _toast((result && result.error) || "发送失败");
                delete _state.pendingByReqId[reqId];
            }
        } catch(e) {
            clearTimeout(_sendTimeoutId);
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 placeholderMsg.id 含 "]" 等特殊字符逃逸 CSS 选择器。
            var sendingEl2 = _findByDataAttr('data-sending-id', placeholderMsg.id);
            if (sendingEl2) {
                var statusEl2 = sendingEl2.querySelector('.msg-send-status');
                if (statusEl2) {
                    statusEl2.className = 'msg-send-status msg-send-fail';
                    statusEl2.textContent = '!';
                }
            }
            // FIX U3 (2026-07-20): 用户主动取消时使用友好提示，不显示"发送失败"
            //   超时 abort 也会走到这里，但超时 toast 已在 setTimeout 中显示，此处静默
            var isCancelled = (e && e.message === "上传被取消");
            if (isCancelled && _timedOut) {
                // 超时触发的 abort，toast 已在 setTimeout 中显示，不重复
            } else if (isCancelled) {
                _toast("已取消上传", 2000);
            } else {
                _toast("发送失败: " + (e.message || e));
            }
            delete _state.pendingByReqId[reqId];
        } finally {
            // FIX U3: 无论成功/失败/取消，清理 xhr 引用避免内存泄漏
            delete _state._pendingUploads[reqId];
        }
    };
    
    const _handlePhotoSelect = function(e) {
        var file = e.target.files && e.target.files[0];
        if (file) _sendMediaFile(file, "image");
        e.target.value = "";
    };
    
    const _handleVideoSelect = function(e) {
        var file = e.target.files && e.target.files[0];
        if (file) _sendMediaFile(file, "video");
        e.target.value = "";
    };
    
    const _handleFileSelect = function(e) {
        var file = e.target.files && e.target.files[0];
        if (file) _sendMediaFile(file, "file");
        e.target.value = "";
    };
    
