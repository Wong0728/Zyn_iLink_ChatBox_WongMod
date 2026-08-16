    var _currentAudio = null;
    var _currentVoiceEl = null;

    // Phase 5 (HIGH-6): fetch 响应统一鉴权/配额错误处理。
    //   zn-media.js 中 6 处直接 fetch 绕过 zn-api.js 的 _api/_get XHR 封装,
    //   缺失 401→_handle401 / 403/413/429→toast 的统一处理。
    //   - 401: 触发 _handle401 跳转登录页
    //   - 403/413/429: 同步 fallback toast + 异步 clone body 解析更准确的 message
    //   - 其他 !ok: 不处理,让调用方的 !r.ok 检查走原通用错误路径
    //   返回 r 本身(200 时),供 .then 链继续 .json()/.blob()
    var _checkFetchAuth = function(r, fallbackMsg) {
        if (r.status === 401) {
            try { _handle401(); } catch(_) {}
            throw new Error("401 Unauthorized");
        }
        if (r.status === 403 || r.status === 413 || r.status === 429) {
            var msg = fallbackMsg || "操作受限";
            try { _toast(msg, 4000, "error"); } catch(_) {}
            // FIX L6 (2026-07-20): 异步解析 body 拿到更准确的 message（_toast 内部去重，
            //   相同文字不重启动画，避免 1-2 次闪烁）。
            try {
                r.clone().json().then(function(b) {
                    if (b && (b.message || b.error)) {
                        try { _toast(b.message || b.error, 4000, "error"); } catch(_) {}
                    }
                }).catch(function() {});
            } catch(_) {}
            throw new Error("HTTP " + r.status + ": " + msg);
        }
        return r;
    };

    const _handleMediaClick = function(msg, el) {
        var mt = msg.media_type;
        // 省流量模式下点击占位 → 强制加载单条媒体（不关闭全局开关）
        if (_state.trafficSaver && el && el.dataset && el.dataset.action === "load-media") {
            _loadSingleMedia(msg, el);
            return;
        }
        if (mt === "image" || mt === 2) {
            var imgEl = el ? el.querySelector('.bubble-media-img') : null;
            // FIX S39: 原 `!imgEl.src.endsWith('loading')` 永远为 true（src 是绝对 URL），
            //   改为检查父级 loading 标记，避免点击仍在加载的占位图触发预览
            var isLoading = imgEl ? (imgEl.dataset.loading === '1' ||
                (imgEl.closest('.bubble-media-loading') !== null)) : false;
            if (imgEl && imgEl.src && !isLoading) {
                window._previewImage(imgEl.src);
            } else if (msg.media_webdav_url) {
                window._previewImage(msg.media_webdav_url);
            } else if (msg.media_cache_id) {
                window._previewImage('/api/wasm/media/' + msg.media_cache_id + '?force=1');
            } else if (msg.media_data) {
                window._previewImage(msg.media_data);
            } else if (msg.media_cdn) {
                // FIX 2026-07-15: 参考 Python 版，未缓存的图片通过 CDN 下载后预览
                _toast("正在加载图片...");
                var cdnInfo = msg.media_cdn;
                try { cdnInfo = JSON.parse(msg.media_cdn); } catch(e) {}
                fetch('/api/wasm/download-media', {
                    method: 'POST',
                    headers: {'Content-Type': 'application/json'}, // FIX H-7 (2026-07-18): 依赖同源 HttpOnly Cookie,
                    body: JSON.stringify({
                        cdn_info: cdnInfo,
                        filename: msg.media_filename || 'image'
                    })
                }).then(function(r) {
                    _checkFetchAuth(r, "图片加载失败");
                    return r.json();
                }).then(function(result) {
                    if (result && result.success && result.cache_key) {
                        window._previewImage('/api/wasm/media/' + result.cache_key);
                    } else {
                        _toast("图片加载失败");
                    }
                }).catch(function() { _toast("图片加载失败"); });
            }
        } else if (_matchesMediaType(mt, "video")) {
            _playVideo(msg);
        } else if (_matchesMediaType(mt, "voice")) {
            _playVoice(msg);
        } else if (_matchesMediaType(mt, "file")) {
            _downloadMedia(msg, "file");
        }
    };

    // 省流量模式下点击单条媒体加载
    const _loadSingleMedia = function(msg, el) {
        var mt = msg.media_type;
        var cacheId = msg.media_cache_id || "";
        var cdn = msg.media_cdn || "";
        var webdavUrl = msg.media_webdav_url || "";
        if (!cacheId && !cdn && !webdavUrl) {
            _toast("媒体数据不可用");
            return;
        }
        // 移除 data-action 防止重复点击
        el.removeAttribute("data-action");
        el.innerHTML = '<div class="bubble-media-placeholder bubble-media-loading-inner"><div class="bubble-media-spinner"></div><span>加载中...</span></div>';
        if (mt === "image" || mt === 2) {
            if (webdavUrl) {
                el.className = "bubble-media-img-wrap bubble-media-loading";
                el.setAttribute("data-webdav-url", webdavUrl);
                el.setAttribute("data-media-type", "image");
                _loadWebDavMedia(el);
            } else if (cacheId) {
                var img = new Image();
                img.onload = function() {
                    el.className = "bubble-media-img-wrap";
                    // FIX S14: 附加 data-cache-key，便于 media_cache_update 在 WebDAV 模式下匹配
                    el.innerHTML = '<img class="bubble-media-img" data-cache-key="' + _escapeAttr(String(cacheId)) + '" src="' + _escapeAttr(img.src) + '" alt="图片" />';
                };
                img.onerror = function() {
                    el.innerHTML = '<div class="bubble-media-placeholder"><span>图片加载失败</span></div>';
                };
                img.src = '/api/wasm/media/' + cacheId + '?force=1';
            } else if (cdn) {
                el.className = "bubble-media-img-wrap bubble-media-loading";
                el.setAttribute("data-cdn", cdn);
                el.setAttribute("data-media-type", "image");
                window._loadCdnMedia(el, true);
            }
        } else if (_matchesMediaType(mt, "video")) {
            if (webdavUrl) {
                el.className = "bubble-media-img-wrap bubble-media-loading";
                el.setAttribute("data-webdav-url", webdavUrl);
                el.setAttribute("data-media-type", "video");
                _loadWebDavMedia(el);
            } else if (cacheId) {
                el.innerHTML = '<div class="bubble-media-video-thumb" data-cache-key="' + _escapeAttr(String(cacheId)) + '" data-action="play-video" data-video-src="/api/wasm/media/' + _escapeAttr(cacheId) + '?force=1"><video class="bubble-media-video-thumb-vid" src="/api/wasm/media/' + _escapeAttr(cacheId) + '?force=1" preload="metadata" muted playsinline></video><div class="bubble-media-play-btn">' + _svgPlay + '</div></div>';
            } else {
                el.innerHTML = '<div class="bubble-media-placeholder"><span>视频加载失败</span></div>';
            }
        } else if (_matchesMediaType(mt, "voice")) {
            if (webdavUrl) {
                el.className = "bubble-media-voice bubble-media-loading";
                el.setAttribute("data-webdav-url", webdavUrl);
                el.setAttribute("data-media-type", "voice");
                _loadWebDavMedia(el);
            } else if (cacheId) {
                var dur = msg.media_duration ? Math.ceil(msg.media_duration / 1000) : 1;
                // FIX M2/M5 (2026-07-20): 收敛到 _voiceBarHtml（zn-core.js），与 _renderMsg 共享。
                var bars = _voiceBarHtml(msg, dur);
                el.className = "bubble-media-voice";
                el.setAttribute("data-action", "play-voice");
                el.setAttribute("data-cache-id", cacheId);
                el.innerHTML = _svgVoice + '<div class="bubble-media-voice-bars">' + bars + '</div><div class="bubble-media-voice-dur">' + dur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div>';
            } else {
                el.innerHTML = '<div class="bubble-media-placeholder"><span>语音加载失败</span></div>';
            }
        } else if (_matchesMediaType(mt, "file")) {
            if (cacheId) {
                _downloadMedia(msg, "file");
            } else {
                _toast("文件数据不可用");
            }
        }
    };

    // FIX L2 (2026-07-20): 用 Object.create(null) 避免原型键冲突（__proto__/toString 等）。
    var _voicePlayFailed = Object.create(null);
    var _voiceProgressRaf = null;

    const _playVoice = function(msg) {
        if (!msg.media_cdn && !msg.media_cache_id && !msg.media_webdav_url) {
            _toast("语音数据不可用");
            return;
        }
        var msgId = msg.id;
        if (_voicePlayFailed[msgId]) {
            delete _voicePlayFailed[msgId];
        }
        if (_currentAudio) {
            _currentAudio.pause();
            // 释放音频缓冲，避免切换时旧音频仍在后台占用内存
            try { _currentAudio.removeAttribute("src"); _currentAudio.load(); } catch(_) {}
            _currentAudio = null;
            if (_currentVoiceEl) {
                _currentVoiceEl.classList.remove('voice-playing');
                var pf = _currentVoiceEl.querySelector('.bubble-media-voice-progress-fill');
                if (pf) pf.style.width = '0%';
                _currentVoiceEl = null;
            }
        }
        if (_voiceProgressRaf) {
            cancelAnimationFrame(_voiceProgressRaf);
            _voiceProgressRaf = null;
        }
        var voiceEl = null;
        if (msgId) {
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 msgId 含 "]" 等特殊字符逃逸 CSS 选择器；再手动排除 data-sending-id 元素。
            var msgRow = _findByDataAttr('data-msg-id', msgId);
            if (msgRow && msgRow.hasAttribute('data-sending-id')) msgRow = null;
            if (msgRow) voiceEl = msgRow.querySelector('.bubble-media-voice');
        }
        // 如果正在下载此语音，等待它完成而不是重复请求
        if (!msg.media_cache_id && voiceEl && voiceEl.classList.contains('bubble-media-loading')) {
            _toast("语音正在加载中，请稍候...");
            return;
        }
        var tryPlayAudio = function(audioUrl) {
            var audio = new Audio();
            var hasPlayed = false;
            var updateProgress = function() {
                if (!audio.duration || !voiceEl) return;
                var pct = (audio.currentTime / audio.duration) * 100;
                var fill = voiceEl.querySelector('.bubble-media-voice-progress-fill');
                if (fill) fill.style.width = pct + '%';
                var durEl = voiceEl.querySelector('.bubble-media-voice-dur');
                if (durEl && audio.duration) {
                    var remain = Math.ceil(audio.duration - audio.currentTime);
                    durEl.textContent = remain + '"';
                }
                if (!audio.paused && !audio.ended) {
                    _voiceProgressRaf = requestAnimationFrame(updateProgress);
                }
            };
            audio.addEventListener('canplaythrough', function() {
                hasPlayed = true;
                if (voiceEl) {
                    _currentVoiceEl = voiceEl;
                    voiceEl.classList.add('voice-playing');
                }
                audio.play().catch(function() {
                    _voicePlayFailed[msgId] = true;
                    if (voiceEl) voiceEl.classList.remove('voice-playing');
                    var pf = voiceEl ? voiceEl.querySelector('.bubble-media-voice-progress-fill') : null;
                    if (pf) pf.style.width = '0%';
                    _toast("语音播放失败，再次点击可下载");
                });
            });
            audio.addEventListener('error', function() {
                if (!hasPlayed) {
                    _voicePlayFailed[msgId] = true;
                    // FIX L2 (2026-07-20): 后端转码失败时返回原始 SILK/AMR + 真实 MIME，
                    //   浏览器无法解码会触发本事件。文案改为"语音转码失败"更准确，
                    //   区分"浏览器不支持"与"后端转码失败"两种场景。
                    _toast("语音转码失败或浏览器不支持此格式，再次点击可下载");
                }
            });
            audio.addEventListener('ended', function() {
                _currentAudio = null;
                if (_voiceProgressRaf) {
                    cancelAnimationFrame(_voiceProgressRaf);
                    _voiceProgressRaf = null;
                }
                if (_currentVoiceEl) {
                    _currentVoiceEl.classList.remove('voice-playing');
                    var pf = _currentVoiceEl.querySelector('.bubble-media-voice-progress-fill');
                    if (pf) pf.style.width = '0%';
                    var durEl = _currentVoiceEl.querySelector('.bubble-media-voice-dur');
                    if (durEl && msg.media_duration) durEl.textContent = Math.ceil(msg.media_duration / 1000) + '"';
                    _currentVoiceEl = null;
                }
            });
            audio.addEventListener('playing', function() {
                if (_voiceProgressRaf) cancelAnimationFrame(_voiceProgressRaf);
                _voiceProgressRaf = requestAnimationFrame(updateProgress);
            });
            audio.addEventListener('pause', function() {
                if (_voiceProgressRaf) {
                    cancelAnimationFrame(_voiceProgressRaf);
                    _voiceProgressRaf = null;
                }
            });
            _currentAudio = audio;
            audio.src = audioUrl;
            audio.load();
        };
        if (voiceEl && voiceEl.dataset.cacheId && voiceEl.dataset.cacheId.indexOf("blob:") === 0) {
            tryPlayAudio(voiceEl.dataset.cacheId);
            return;
        }
        if (msg.media_cache_id) {
            tryPlayAudio('/api/wasm/media/' + msg.media_cache_id + '?force=1');
            return;
        }
        _toast("正在加载语音...");
        var voiceAc = new AbortController();
        var voiceTimer = setTimeout(function() { try { voiceAc.abort(); } catch (e) {} }, 60000);
        fetch('/api/wasm/download-media', {
            method: 'POST',
            headers: {'Content-Type': 'application/json'}, // FIX H-7 (2026-07-18): 依赖同源 HttpOnly Cookie,
            body: JSON.stringify({cdn_info: (typeof msg.media_cdn === 'string' ? msg.media_cdn : JSON.stringify(msg.media_cdn)), filename: (msg.media_filename || "")}),
            signal: voiceAc.signal,
        }).then(function(r) {
            _checkFetchAuth(r, "语音加载失败");
            if (!r.ok) {
                return r.text().then(function(t) { throw new Error('HTTP ' + r.status + ': ' + t); });
            }
            return r.json();
        }).then(function(result) {
            clearTimeout(voiceTimer);
            if (result.success && result.cache_key) {
                tryPlayAudio(result.cache_key);
            } else {
                _toast("语音加载失败: " + (result.error || "未知错误"));
            }
        }).catch(function(err) {
            clearTimeout(voiceTimer);
            if (err && (err.name === "AbortError" || String(err).indexOf("aborted") >= 0)) {
                _toast("语音加载超时");
            } else {
                _toast("语音加载失败");
            }
        });
    };

    const _playVideo = function(msg) {
        if (!msg.media_cdn && !msg.media_cache_id && !msg.media_webdav_url) {
            _toast("视频数据不可用");
            return;
        }
        var tryPlayVideo = function(videoUrl) {
            window._previewVideo(videoUrl);
        };
        var videoThumb = null;
        if (msg.id) {
            // FIX P1-11 (2026-07-20): 用 _findByDataAttr 替代 querySelector 字符串拼接，
            //   避免 msg.id 含 "]" 等特殊字符逃逸 CSS 选择器；再手动排除 data-sending-id 元素。
            var msgRow = _findByDataAttr('data-msg-id', msg.id);
            if (msgRow && msgRow.hasAttribute('data-sending-id')) msgRow = null;
            if (msgRow) videoThumb = msgRow.querySelector('.bubble-media-video-thumb');
        }
        if (videoThumb && videoThumb.dataset.videoSrc) {
            tryPlayVideo(videoThumb.dataset.videoSrc);
            return;
        }
        if (msg.media_cache_id) {
            tryPlayVideo('/api/wasm/media/' + msg.media_cache_id + '?force=1');
            return;
        }
        // 统一走服务器代理（浏览器直连 CDN 不可靠）
        _fetchVideoViaServer(msg);
    };

    const _fetchVideoViaServer = function(msg) {
        _toast("正在加载视频...");
        var ac = new AbortController();
        var timer = setTimeout(function() { try { ac.abort(); } catch (e) {} }, 120000);
        fetch('/api/wasm/download-media', {
            method: 'POST',
            headers: {'Content-Type': 'application/json'}, // FIX H-7 (2026-07-18): 依赖同源 HttpOnly Cookie,
            body: JSON.stringify({cdn_info: (typeof msg.media_cdn === 'string' ? msg.media_cdn : JSON.stringify(msg.media_cdn)), filename: (msg.media_filename || "")}),
            signal: ac.signal,
        }).then(function(r) {
            _checkFetchAuth(r, "视频加载失败");
            if (!r.ok) {
                return r.text().then(function(t) { throw new Error('HTTP ' + r.status + ': ' + t); });
            }
            return r.json();
        }).then(function(result) {
            clearTimeout(timer);
            if (result.success && result.cache_key) {
                tryPlayVideo(result.cache_key);
            } else {
                _toast("视频加载失败: " + (result.error || "未知错误"));
            }
        }).catch(function(err) {
            clearTimeout(timer);
            if (err && (err.name === 'AbortError' || String(err).indexOf('aborted') >= 0)) {
                _toast("视频加载超时");
            } else {
                _toast("视频加载失败");
            }
        });
    };

    window._previewVideo = function(src) {
        var overlay = document.createElement('div');
        overlay.style.cssText = 'position:fixed;top:0;left:0;right:0;bottom:0;background:rgba(0,0,0,0.92);z-index:10002;display:flex;flex-direction:column;align-items:center;justify-content:center;cursor:default';
        var video = document.createElement('video');
        video.src = src;
        video.controls = true;
        video.autoplay = true;
        video.muted = true;
        video.playsInline = true;
        video.style.cssText = 'max-width:95%;max-height:85%;border-radius:8px;background:#000;outline:none';
        var closeBtn = document.createElement('div');
        closeBtn.style.cssText = 'position:absolute;top:16px;right:16px;width:36px;height:36px;border-radius:50%;background:rgba(255,255,255,0.2);display:flex;align-items:center;justify-content:center;cursor:pointer;font-size:20px;color:#fff;z-index:10003';
        closeBtn.innerHTML = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#fff" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>';
        var downloadBtn = document.createElement('div');
        downloadBtn.style.cssText = 'position:absolute;top:16px;right:60px;width:36px;height:36px;border-radius:50%;background:rgba(255,255,255,0.2);display:flex;align-items:center;justify-content:center;cursor:pointer;z-index:10003';
        downloadBtn.innerHTML = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#fff" stroke-width="2"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>';
        downloadBtn.addEventListener('click', function(ev) {
            ev.stopPropagation();
            var a = document.createElement('a');
            // FIX S67: 若 src 已含 query 参数，应追加 & 而非 ? 避免生成非法 URL
            var durl = src + (src.indexOf('?') >= 0 ? '&' : '?') + 'download=1';
            a.href = durl;
            a.download = 'video.mp4';
            document.body.appendChild(a);
            a.click();
            document.body.removeChild(a);
        });
        // FIX S70: 浏览器要求 autoplay 必须先 muted；提供独立"取消静音"按钮，
        //   用户点击后手动取消静音（也保留 controls 让用户用原生控件操作）
        var unmuteBtn = document.createElement('div');
        unmuteBtn.style.cssText = 'position:absolute;top:16px;right:104px;width:36px;height:36px;border-radius:50%;background:rgba(255,255,255,0.2);display:flex;align-items:center;justify-content:center;cursor:pointer;z-index:10003';
        unmuteBtn.title = '取消静音';
        unmuteBtn.innerHTML = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><line x1="23" y1="9" x2="17" y2="15"/><line x1="17" y1="9" x2="23" y2="15"/></svg>';
        unmuteBtn.addEventListener('click', function(ev) {
            ev.stopPropagation();
            video.muted = false;
            video.volume = 1;
            if (overlay.contains(unmuteBtn)) overlay.removeChild(unmuteBtn);
        });
        var closeOverlay = function() {
            video.pause();
            video.src = '';
            if (overlay.parentNode) document.body.removeChild(overlay);
        };
        closeBtn.addEventListener('click', function(ev) { ev.stopPropagation(); closeOverlay(); });
        overlay.addEventListener('click', function(ev) { if (ev.target === overlay) closeOverlay(); });
        overlay.appendChild(video);
        overlay.appendChild(closeBtn);
        overlay.appendChild(downloadBtn);
        overlay.appendChild(unmuteBtn);
        document.body.appendChild(overlay);
    };

    // ── 通用：用隐藏 <a> 触发浏览器下载（同步 click 之后给浏览器时间真正开始下载）──
    const _triggerAnchorDownload = function(href, filename) {
        var a = document.createElement('a');
        a.href = href;
        a.download = filename || "";
        a.rel = "noopener";
        a.style.display = "none";
        // 关键：必须先挂到 DOM 上再 click()，否则某些浏览器（尤其 Firefox）静默失败
        document.body.appendChild(a);
        try {
            a.click();
        } catch (err) {
            console.error("[DL] click() 异常:", err);
            try { document.body.removeChild(a); } catch (e2) {}
            _toast("下载失败: " + (err && err.message));
            return false;
        }
        // 延时移除 a：保证浏览器已开始下载流程
        setTimeout(function() {
            try { if (a.parentNode) a.parentNode.removeChild(a); } catch (e) {}
        }, 200);
        return true;
    };

    const _downloadDirectUrl = function(cacheId, filename) {
        try {
            // FIX P0-2 (2026-07-19): 移除 URL 中的 ?token= 拼接，依赖同源 HttpOnly Cookie 鉴权。
            //   原实现把 _state.token 拼到 URL，会暴露真实 token 到浏览器历史 / Referer / 反代日志。
            //   require_session 已豁免 /api/wasm/media/，同源 Cookie 自动携带即可。
            var downloadUrl = (cacheId.indexOf("blob:") === 0) ? cacheId : '/api/wasm/media/' + cacheId + '?download=1&force=1';
            if (_triggerAnchorDownload(downloadUrl, filename)) {
                _toast("正在接收: " + filename, 4000);
            }
        } catch (err) {
            console.error("[DL] _downloadDirectUrl 异常:", err);
            _toast("下载失败");
        }
    };

    const _downloadMedia = function(msg, mediaType) {
        if (!msg.media_cdn && !msg.media_cache_id && !msg.media_webdav_url) {
            _toast("媒体数据不可用");
            return;
        }
        var filename = msg.media_filename || (mediaType === "video" ? "video.mp4" : mediaType === "voice" ? "voice.silk" : "file.bin");
        if (msg.media_webdav_url) {
            // 代理 URL 是同源的，可以直接用 <a> 标签下载
            // FIX P0-2 (2026-07-19): 同样移除 ?token= 拼接，依赖同源 Cookie
            var downloadUrl = msg.media_webdav_url + '?download=1';
            if (_triggerAnchorDownload(downloadUrl, filename)) {
                _toast("正在接收: " + filename, 4000);
            }
            return;
        }
        if (msg.media_cache_id) {
            _downloadDirectUrl(msg.media_cache_id, filename);
            return;
        }
        _fetchDownloadViaServer(msg, filename);
    };

    const _fetchDownloadViaServer = function(msg, filename) {
        _toast("正在接收 " + filename + "（服务器中转）...", 5000);
        var dlAc = new AbortController();
        var dlTimer = setTimeout(function() { try { dlAc.abort(); } catch (e) {} }, 120000);
        fetch('/api/wasm/download-media', {
            method: 'POST',
            headers: {'Content-Type': 'application/json'}, // FIX H-7 (2026-07-18): 依赖同源 HttpOnly Cookie,
            body: JSON.stringify({cdn_info: (typeof msg.media_cdn === 'string' ? msg.media_cdn : JSON.stringify(msg.media_cdn)), filename: filename}),
            signal: dlAc.signal,
        }).then(function(r) {
            _checkFetchAuth(r, "下载失败");
            if (!r.ok) {
                return r.text().then(function(t) { throw new Error('HTTP ' + r.status); });
            }
            return r.json();
        }).then(function(result) {
            clearTimeout(dlTimer);
            if (result && result.success && result.cache_key) {
                // 缓存 cache_key 到 msg 对象，后续点击可直接下载
                msg.media_cache_id = result.cache_key;
                _downloadDirectUrl(result.cache_key, filename);
            } else {
                _toast("下载失败: " + ((result && result.error) || "未知错误"));
            }
        }).catch(function(err) {
            clearTimeout(dlTimer);
            if (err && (err.name === "AbortError" || String(err).indexOf("aborted") >= 0)) {
                _toast("下载超时，请重试");
            } else {
                console.error("[DL] 服务器下载异常:", err);
                _toast("下载失败: " + (err && err.message));
            }
        });
    };

    window._previewImage = function(src) {
        var overlay = document.createElement('div');
        overlay.style.cssText = 'position:fixed;top:0;left:0;right:0;bottom:0;background:rgba(0,0,0,0.9);z-index:10002;display:flex;align-items:center;justify-content:center;cursor:zoom-out';
        var img = document.createElement('img');
        img.src = src;
        img.style.cssText = 'max-width:95%;max-height:95%;object-fit:contain;border-radius:4px';
        overlay.appendChild(img);
        overlay.addEventListener('click', function() { if (overlay.parentNode) document.body.removeChild(overlay); });
        document.body.appendChild(overlay);
    };

    const _removeLoadingSpinner = function(el, cacheKey) {
        var row = el.closest('.msg-row');
        if (row) {
            var spinner = row.querySelector('.msg-send-loading');
            if (spinner) spinner.remove();
            if (cacheKey && row._msgData) {
                row._msgData.media_cache_id = cacheKey;
            }
        }
    };

    const _loadWebDavMedia = function(el) {
        var webdavUrl = el.dataset.webdavUrl || "";
        var mediaType = el.dataset.mediaType || "image";
        // FIX S14: 取出 cache_key（由 _renderMsg 在 webdav 模式下附加），加载完成后保留属性
        var ck = el.dataset.cacheKey || "";
        if (!webdavUrl) return;
        if (el.dataset.loading === "1") return;
        el.dataset.loading = "1";

        var failLabelFor = { image: "图片加载失败", video: "视频加载失败", voice: "语音加载失败", file: "文件加载失败" };

        // 代理 URL 是同源的，图片/视频可以直接设置 src，无需 fetch + blob
        if (mediaType === "image") {
            var img = document.createElement('img');
            img.className = 'bubble-media-img';
            img.alt = '图片';
            if (ck) img.setAttribute('data-cache-key', ck);
            img.onload = function() {
                // FIX H2 (2026-07-20): 元素可能已脱离 DOM
                if (!document.body.contains(el)) return;
                el.innerHTML = '';
                el.appendChild(img);
                el.classList.remove('bubble-media-loading');
                el.removeAttribute('data-loading');
            };
            img.onerror = function() {
                // FIX H2 (2026-07-20): 元素可能已脱离 DOM
                if (!document.body.contains(el)) return;
                el.removeAttribute("data-loading");
                el.classList.add("bubble-media-loading");
                el.innerHTML = '<div class="bubble-media-placeholder bubble-media-fail-inner"><span>' + (failLabelFor.image) + '</span><button type="button" class="bubble-media-retry">重试</button></div>';
                var btn = el.querySelector(".bubble-media-retry");
                if (btn) {
                    btn.addEventListener("click", function(ev) {
                        ev.stopPropagation();
                        el.removeAttribute("data-loading");
                        el.classList.add("bubble-media-loading");
                        _loadWebDavMedia(el);
                    });
                }
            };
            img.src = webdavUrl;
        } else if (mediaType === "video") {
            var ckAttr = ck ? (' data-cache-key="' + _escapeAttr(ck) + '"') : '';
            // FIX M8 (2026-07-20): 视频失败时提供重试入口（仿图片路径），
            //   原始实现只 innerHTML 替换为带 play-video 的 video，src 失败无任何重试按钮。
            //   现在附加 onerror 监听，失败时回退到 fail-inner + 重试按钮。
            el.innerHTML = '<div class="bubble-media-video-thumb"' + ckAttr + ' data-action="play-video" data-video-src="' + _escapeAttr(webdavUrl) + '"><video class="bubble-media-video-thumb-vid" src="' + _escapeAttr(webdavUrl) + '" preload="metadata" muted playsinline></video><div class="bubble-media-play-btn">' + _svgPlay + '</div></div>';
            var vidEl = el.querySelector('video');
            if (vidEl) {
                vidEl.onerror = function() {
                    if (!document.body.contains(el)) return;
                    el.innerHTML = '<div class="bubble-media-placeholder bubble-media-fail-inner"><span>' + (failLabelFor.video || "视频加载失败") + '</span><button type="button" class="bubble-media-retry">重试</button></div>';
                    var btn = el.querySelector(".bubble-media-retry");
                    if (btn) {
                        btn.addEventListener("click", function(ev) {
                            ev.stopPropagation();
                            el.classList.add("bubble-media-loading");
                            _loadWebDavMedia(el);
                        });
                    }
                };
            }
            el.classList.remove('bubble-media-loading');
            el.removeAttribute('data-loading');
        } else if (mediaType === "voice") {
            // 语音需要 fetch 为 blob 才能播放
            // L-18：不再以 innerHTML 字符串回读再拼接（二阶注入面），
            //   改为克隆原有节点，完成后以 DOM 节点方式放回。
            var durNode = el.querySelector('.bubble-media-voice-dur');
            var barsNode = el.querySelector('.bubble-media-voice-bars');
            var durClone = durNode ? durNode.cloneNode(true) : null;
            var barsClone = barsNode ? barsNode.cloneNode(true) : null;
            el.innerHTML = '<div class="bubble-media-placeholder bubble-media-loading-inner"><div class="bubble-media-spinner"></div><span>语音加载中...</span></div>';
            fetch(webdavUrl).then(function(r) {
                _checkFetchAuth(r, failLabelFor.voice || "语音加载失败");
                if (!r.ok) throw new Error("HTTP " + r.status);
                return r.blob();
            }).then(function(blob) {
                // FIX H2 (2026-07-20): 元素可能已被 _loadHistory 清空（用户切换会话），
                //   操作脱离 DOM 的元素虽不报错，但闭包持有引用会泄漏且写入无效。
                if (!document.body.contains(el)) return;
                var blobUrl = URL.createObjectURL(blob);
                // 先释放旧的 blob URL，避免泄漏
                var oldCache = el.getAttribute('data-cache-id');
                if (oldCache && oldCache.indexOf('blob:') === 0) {
                    try { URL.revokeObjectURL(oldCache); } catch(_) {}
                }
                el.classList.remove('bubble-media-loading');
                el.removeAttribute('data-webdav-url');
                el.removeAttribute('data-media-type');
                el.removeAttribute('data-loading');
                el.setAttribute('data-action', 'play-voice');
                el.setAttribute('data-cache-id', blobUrl);
                // L-18：骨架用 innerHTML 重建（纯静态字符串），回读内容以节点方式放回
                el.innerHTML = _svgVoice + '<div class="bubble-media-voice-bars"></div><div class="bubble-media-voice-dur"></div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div></div>';
                var barsTarget = el.querySelector('.bubble-media-voice-bars');
                var durTarget = el.querySelector('.bubble-media-voice-dur');
                if (barsClone && barsTarget) {
                    while (barsClone.firstChild) barsTarget.appendChild(barsClone.firstChild);
                }
                if (durTarget) {
                    // textContent 赋值是安全写入，不解析 HTML
                    durTarget.textContent = durClone ? durClone.textContent : "";
                }
            }).catch(function(err) {
                // FIX H2 (2026-07-20): 元素可能已脱离 DOM
                if (!document.body.contains(el)) return;
                el.removeAttribute("data-loading");
                el.classList.add("bubble-media-loading");
                el.innerHTML = '<div class="bubble-media-placeholder bubble-media-fail-inner"><span>' + (failLabelFor.voice) + '</span><button type="button" class="bubble-media-retry">重试</button></div>';
                var btn = el.querySelector(".bubble-media-retry");
                if (btn) {
                    btn.addEventListener("click", function(ev) {
                        ev.stopPropagation();
                        el.removeAttribute("data-loading");
                        el.classList.add("bubble-media-loading");
                        _loadWebDavMedia(el);
                    });
                }
            });
        } else {
            // 文件等：直接用代理 URL
            el.innerHTML = '<div class="bubble-media-placeholder"><span>加载完成</span></div>';
            el.classList.remove('bubble-media-loading');
            el.removeAttribute('data-loading');
        }
    };
    window._loadWebDavMedia = _loadWebDavMedia;

    window._loadCdnMedia = function(el, fromUserClick) {
        var cdn = el.dataset.cdn || "";
        var mediaType = el.dataset.mediaType || "image";
        if (!cdn) return;

        // 省流量模式：不自动触发下载（已在 _renderMsg 中占位）
        // fromUserClick=true 时表示用户主动点击加载，应绕过省流量检查
        if (!fromUserClick && _state.trafficSaver) {
            return;
        }

        // 文件类型不自动下载（由用户点击触发）
        if (mediaType === "file") return;

        // 防止对同一个元素重复触发
        if (el.dataset.loading === "1") return;
        el.dataset.loading = "1";

        // 从父级 msg-row 上取文件名（如果有），传入后端作为缓存文件名
        var row = el.closest('.msg-row');
        var filename = (row && row._msgData && row._msgData.media_filename) || "";

        var iconFor = {
            image: _svgImage,
            video: _svgVideo,
            file: _svgFile,
            voice: _svgVoice,
        };
        var labelFor = {
            image: "图片加载中...",
            video: "视频加载中...",
            file: "文件加载中...",
            voice: "语音加载中...",
        };
        var failLabelFor = {
            image: "图片加载失败",
            video: "视频加载失败",
            file: "文件加载失败",
            voice: "语音加载失败",
        };

        // 渲染明确的 loading 状态（带 spinner + 文案 + 取消按钮）
        var _renderLoading = function() {
            el.innerHTML =
                '<div class="bubble-media-placeholder bubble-media-loading-inner">' +
                '<div class="bubble-media-spinner"></div>' +
                '<span>' + (labelFor[mediaType] || "加载中...") + '</span>' +
                '</div>';
        };
        var _renderFail = function(msg) {
            el.innerHTML =
                '<div class="bubble-media-placeholder bubble-media-fail-inner">' +
                '<span>' + (msg || failLabelFor[mediaType] || "加载失败") + '</span>' +
                '<button type="button" class="bubble-media-retry">重试</button>' +
                '</div>';
            var btn = el.querySelector(".bubble-media-retry");
            if (btn) {
                btn.addEventListener("click", function(ev) {
                    ev.stopPropagation();
                    el.removeAttribute("data-loading");
                    el.classList.add("bubble-media-loading");
                    window._loadCdnMedia(el, true);
                });
            }
        };

        _renderLoading();

        // 所有媒体统一走服务器代理
        var serverTimeoutMs = (mediaType === "voice") ? 60000 : (mediaType === "video" ? 120000 : 30000);
        var ac2 = new AbortController();
        var timer2 = setTimeout(function() { try { ac2.abort(); } catch (e) {} }, serverTimeoutMs);
        fetch('/api/wasm/download-media', {
            method: 'POST',
            headers: {'Content-Type': 'application/json'}, // FIX H-7 (2026-07-18): 依赖同源 HttpOnly Cookie,
            body: JSON.stringify({cdn_info: (typeof cdn === 'string' ? cdn : JSON.stringify(cdn)), filename: filename, media_type: mediaType}),
            signal: ac2.signal,
        }).then(function(r) {
            _checkFetchAuth(r, failLabelFor[mediaType] || "加载失败");
            if (!r.ok) {
                return r.text().then(function(t) { throw new Error('HTTP ' + r.status + ': ' + (t || '').slice(0, 100)); });
            }
            return r.json();
        }).then(function(result) {
            clearTimeout(timer2);
            // FIX H2 (2026-07-20): 元素可能已脱离 DOM
            if (!document.body.contains(el)) return;
            el.removeAttribute("data-loading");
            // 如果 media_cache_update 事件已经先更新了 DOM（data-cdn 已移除），跳过重复更新
            if (!el.dataset.cdn && el.dataset.cacheId) return;
            if (result && result.success && result.cache_key) {
                var cacheUrl = '/api/wasm/media/' + result.cache_key;
                // 优先使用服务端直接返回的 base64 数据，避免二次请求
                var dataUrl = result.data ? ('data:' + (result.mime || 'image/jpeg') + ';base64,' + result.data) : null;
                if (mediaType === "video") {
                    el.innerHTML = '<div class="bubble-media-video-thumb" data-cache-key="' + _escapeAttr(String(result.cache_key)) + '" data-action="play-video" data-video-src="' + _escapeAttr(cacheUrl) + '"><video class="bubble-media-video-thumb-vid" src="' + _escapeAttr(cacheUrl) + '" preload="metadata" muted playsinline></video><div class="bubble-media-play-btn">' + _svgPlay + '</div></div>';
                } else if (mediaType === "file") {
                    el.classList.remove("bubble-media-loading");
                    el.removeAttribute("data-cdn");
                    el.removeAttribute("data-media-type");
                    el.dataset.cacheId = result.cache_key;
                    return _removeLoadingSpinner(el, result.cache_key);
                } else if (mediaType === "voice") {
                    el.classList.remove("bubble-media-loading");
                    el.removeAttribute("data-cdn");
                    el.removeAttribute("data-media-type");
                    el.dataset.action = 'play-voice';
                    el.dataset.cacheId = result.cache_key;
                    // 恢复语音 UI（_renderLoading 替换了 innerHTML，需还原语音条+时长+进度条）
                    var voiceRow = el.closest('.msg-row');
                    var voiceMd = voiceRow && voiceRow._msgData;
                    var voiceDur = (voiceMd && voiceMd.media_duration) ? Math.ceil(voiceMd.media_duration / 1000) : 1;
                    // FIX M2/M5 (2026-07-20): 收敛到 _voiceBarHtml（zn-core.js），与 _renderMsg 共享。
                    var voiceBars = _voiceBarHtml(voiceMd, voiceDur);
                    el.innerHTML = _svgVoice + '<div class="bubble-media-voice-bars">' + voiceBars + '</div><div class="bubble-media-voice-dur">' + voiceDur + '"</div><div class="bubble-media-voice-progress"><div class="bubble-media-voice-progress-fill"></div></div>';
                    return _removeLoadingSpinner(el, result.cache_key);
                } else {
                    el.innerHTML = '<img class="bubble-media-img" data-cache-key="' + _escapeAttr(String(result.cache_key)) + '" src="' + _escapeAttr(dataUrl || cacheUrl) + '" alt="图片" />';
                }
                el.classList.remove("bubble-media-loading");
                _removeLoadingSpinner(el, result.cache_key);
                // 下载完成后滚动到底部，确保加载期间新收到的消息可见
                var _ma = document.getElementById('messages-area');
                if (_ma) _ma.scrollTop = _ma.scrollHeight;
            } else {
                _renderFail((result && result.error) || (failLabelFor[mediaType] || "加载失败"));
                el.classList.remove("bubble-media-loading");
                _removeLoadingSpinner(el);
            }
        }).catch(function(err) {
            clearTimeout(timer2);
            // FIX H2 (2026-07-20): 元素可能已脱离 DOM
            if (!document.body.contains(el)) return;
            el.removeAttribute("data-loading");
            console.warn("[CDN] 下载失败 mediaType=" + mediaType + " error=" + (err && (err.message || err)));
            // 如果 media_cache_update 事件已经先更新了 DOM，不显示错误
            if (!el.dataset.cdn && el.dataset.cacheId) return;
            el.classList.remove("bubble-media-loading");
            if (err && (err.name === "AbortError" || String(err).indexOf("aborted") >= 0)) {
                _renderFail("加载超时");
            } else {
                _renderFail("网络错误");
            }
            _removeLoadingSpinner(el);
        });
    };
    
    // ── Phase 4: 图片点击放大 (Lightbox) ─────────────────
    document.addEventListener('click', function(e) {
        var target = e.target;
        if (target.classList.contains('bubble-media-img')) {
            var src = target.getAttribute('src');
            if (src) {
                var lb = document.getElementById('image-lightbox');
                var lbImg = document.getElementById('lightbox-img');
                if (lb && lbImg) {
                    lbImg.src = src;
                    lb.style.display = 'flex';
                }
            }
        }
    });
