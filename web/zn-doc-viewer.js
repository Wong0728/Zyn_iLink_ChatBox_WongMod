// zn-doc-viewer.js — 通用文档/指南查看器(左侧抽屉式目录)
// 被 chat.html、terms.html、landing.html、auth.html 共享。
//
// 设计:
//   - 支持多种文档类型(type):内置 "terms"(使用守则)、"guide"(使用与管理指南),可扩展。
//   - 渲染 Markdown 后,自动扫描 h1/h2/h3 生成左侧目录抽屉,点击平滑滚动到对应章节。
//   - 抽屉在宽屏默认展开,窄屏(≤768px)默认折叠,可由顶部目录按钮切换。
//   - /terms 页面沿用本查看器,并保留"返回注册页/我已阅读"操作栏。
(function() {
    "use strict";

    // ── 内部工具 ──────────────────────────────────────
    function _escapeHtml(text) {
        var div = document.createElement("div");
        div.textContent = text;
        return div.innerHTML;
    }

    // 中文/英文标题转 slug,用作锚点 id。冲突时加序号。
    function _slugify(text) {
        var s = String(text).trim().toLowerCase();
        s = s.replace(/[\s_.]+/g, "-").replace(/[^\w\u4e00-\u9fa5-]/g, "");
        return s || "section";
    }

    // 极简 Markdown 渲染(保留原有实现,已工作良好)
    function _renderMarkdown(md) {
        var html = _escapeHtml(md);

        // 代码块
        html = html.replace(/```([\s\S]*?)```/g, function(_, code) {
            return "<pre><code>" + code.replace(/^\n|\n$/g, "") + "</code></pre>";
        });

        // 行内代码
        html = html.replace(/`([^`]+)`/g, "<code>$1</code>");

        // 标题(h1-h6)
        html = html.replace(/^###### (.*)$/gm, "<h6>$1</h6>");
        html = html.replace(/^##### (.*)$/gm, "<h5>$1</h5>");
        html = html.replace(/^#### (.*)$/gm, "<h4>$1</h4>");
        html = html.replace(/^### (.*)$/gm, "<h3>$1</h3>");
        html = html.replace(/^## (.*)$/gm, "<h2>$1</h2>");
        html = html.replace(/^# (.*)$/gm, "<h1>$1</h1>");

        // 引用
        html = html.replace(/^&gt; (.*)$/gm, "<blockquote>$1</blockquote>");

        // 无序列表
        html = html.replace(/^(\s*)[-*] (.*)$/gm, function(_, indent, text) {
            return "<li style=\"margin-left:" + (indent.length * 8) + "px\">" + text + "</li>";
        });

        // 有序列表
        html = html.replace(/^(\s*)\d+\. (.*)$/gm, function(_, indent, text) {
            return "<li style=\"margin-left:" + (indent.length * 8) + "px\">" + text + "</li>";
        });

        // 加粗 / 斜体
        html = html.replace(/\*\*\*(.*?)\*\*\*/g, "<strong><em>$1</em></strong>");
        html = html.replace(/\*\*(.*?)\*\*/g, "<strong>$1</strong>");
        html = html.replace(/\*(.*?)\*/g, "<em>$1</em>");

        // 段落
        var blocks = html.split(/\n\n+/);
        return blocks.map(function(block) {
            block = block.trim();
            if (!block) return "";
            if (/^<(h[1-6]|pre|blockquote|li)/.test(block)) return block;
            return "<p>" + block.replace(/\n/g, "<br>") + "</p>";
        }).join("\n");
    }

    // 扫描 contentEl 内的 h1/h2/h3,生成目录并填充 tocListEl。
    // 给每个标题加 id(若未有),返回 [{level, text, id}]。
    function _buildToc(contentEl, tocListEl) {
        if (!contentEl || !tocListEl) return [];
        var headings = contentEl.querySelectorAll("h1, h2, h3");
        var used = {};
        var items = [];
        var frag = document.createDocumentFragment();

        for (var i = 0; i < headings.length; i++) {
            var h = headings[i];
            var level = parseInt(h.tagName.charAt(1), 10);
            var text = (h.textContent || "").trim();
            if (!text) continue;

            // 生成唯一 id
            var id = h.id || _slugify(text);
            if (used[id]) { id = id + "-" + (++used[id]); }
            else { used[id] = 1; }
            h.id = id;

            items.push({ level: level, text: text, id: id });

            var a = document.createElement("a");
            a.href = "#" + id;
            a.className = "doc-viewer-toc-item toc-level-" + level;
            a.textContent = text;
            (function(anchor, targetId) {
                anchor.addEventListener("click", function(e) {
                    e.preventDefault();
                    var target = document.getElementById(targetId);
                    if (target) {
                        target.scrollIntoView({ behavior: "smooth", block: "start" });
                    }
                    // 窄屏点击后自动收起抽屉
                    if (window.matchMedia && window.matchMedia("(max-width: 768px)").matches) {
                        var toc = document.getElementById("doc-viewer-toc");
                        if (toc) toc.classList.remove("show");
                    }
                });
            })(a, id);
            frag.appendChild(a);
        }

        tocListEl.innerHTML = "";
        tocListEl.appendChild(frag);
        return items;
    }

    function _closeDocViewer() {
        var panel = document.getElementById("doc-viewer-panel");
        if (panel) panel.classList.remove("show");
        // 关闭时同步收起抽屉,避免下次打开残留
        var toc = document.getElementById("doc-viewer-toc");
        if (toc) toc.classList.remove("show");
    }

    function _buildTermsActions() {
        var bar = document.createElement("div");
        bar.className = "doc-viewer-actions";
        bar.innerHTML =
            '<button type="button" class="doc-viewer-btn secondary" id="doc-viewer-back-register">返回注册页</button>' +
            '<button type="button" class="doc-viewer-btn primary" id="doc-viewer-agree">我已阅读</button>';
        bar.querySelector("#doc-viewer-back-register").addEventListener("click", function() {
            window.location.href = "/auth?mode=register";
        });
        bar.querySelector("#doc-viewer-agree").addEventListener("click", function() {
            if (window.history.length > 1) {
                window.history.back();
            } else {
                window.location.href = "/auth?mode=register";
            }
        });
        return bar;
    }

    // ── 主入口 ────────────────────────────────────────
    // type: "terms" | "guide" | <自定义>(需后端提供 /api/wasm/<type>)
    // opts.showActions: 在 /terms 页面显示"返回注册页/我已阅读"操作栏
    window._openDocViewer = function(type, opts) {
        opts = opts || {};
        var panel = document.getElementById("doc-viewer-panel");
        var titleEl = document.getElementById("doc-viewer-title");
        var contentEl = document.getElementById("doc-viewer-content");
        var tocListEl = document.getElementById("doc-viewer-toc-list");
        if (!panel || !contentEl || !titleEl) return;

        // 清除旧的操作栏
        var oldBar = panel.querySelector(".doc-viewer-actions");
        if (oldBar) oldBar.remove();

        // 清空目录
        if (tocListEl) tocListEl.innerHTML = "";

        contentEl.className = "doc-viewer-content loading";
        contentEl.textContent = "正在加载...";
        titleEl.textContent = "文档";
        panel.classList.add("show");

        // 抽屉默认状态:宽屏展开,窄屏折叠
        var toc = document.getElementById("doc-viewer-toc");
        if (toc) {
            var narrow = window.matchMedia && window.matchMedia("(max-width: 768px)").matches;
            if (narrow) toc.classList.remove("show");
            else toc.classList.add("show");
        }

        var url = type === "terms" ? "/api/wasm/terms" : "/api/wasm/" + encodeURIComponent(type);

        fetch(url, { cache: "no-store" })
            .then(function(r) { return r.json(); })
            .then(function(data) {
                if (!data || !data.success) {
                    contentEl.className = "doc-viewer-content error";
                    contentEl.textContent = "加载失败,请稍后重试。";
                    return;
                }
                titleEl.textContent = data.title || (type === "terms" ? "使用守则" : "使用与管理指南");
                var text = data.text || "";
                if (!text && data.exists === false) {
                    contentEl.className = "doc-viewer-content error";
                    contentEl.textContent = data.message || "文档暂未配置。";
                    return;
                }
                contentEl.className = "doc-viewer-content";
                // 守则与指南统一用 Markdown 渲染(守则已改为 Markdown 格式,带 # 标题)
                contentEl.innerHTML = _renderMarkdown(text);

                // 生成左侧目录;无标题时隐藏抽屉与切换按钮
                var tocBtn = document.getElementById("doc-viewer-toc-btn");
                if (tocListEl) {
                    var items = _buildToc(contentEl, tocListEl);
                    if (toc) {
                        if (items.length === 0) {
                            toc.classList.add("no-toc");
                            if (tocBtn) tocBtn.style.display = "none";
                        } else {
                            toc.classList.remove("no-toc");
                            if (tocBtn) tocBtn.style.display = ""; // 恢复 CSS 默认(宽屏 none,窄屏 flex)
                        }
                    }
                }

                // 在 /terms 页面显示操作按钮
                if (type === "terms" && (opts.showActions || /^\/terms/.test(window.location.pathname))) {
                    var scroll = panel.querySelector(".doc-viewer-scroll");
                    if (scroll) scroll.appendChild(_buildTermsActions());
                }
            })
            .catch(function(err) {
                contentEl.className = "doc-viewer-content error";
                contentEl.textContent = "网络错误:" + (err.message || err);
            });
    };

    window._closeDocViewer = _closeDocViewer;

    // ── 事件绑定 ──────────────────────────────────────
    document.addEventListener("DOMContentLoaded", function() {
        var backBtn = document.getElementById("doc-viewer-back-btn");
        if (backBtn) backBtn.addEventListener("click", _closeDocViewer);

        // 目录抽屉切换按钮
        var tocBtn = document.getElementById("doc-viewer-toc-btn");
        if (tocBtn) {
            tocBtn.addEventListener("click", function(e) {
                e.stopPropagation();
                var toc = document.getElementById("doc-viewer-toc");
                if (toc) toc.classList.toggle("show");
            });
        }

        document.addEventListener("keydown", function(e) {
            if (e.key === "Escape") _closeDocViewer();
        });

        var panel = document.getElementById("doc-viewer-panel");
        if (panel) {
            panel.addEventListener("click", function(e) {
                if (e.target === panel) _closeDocViewer();
            });
        }
    });
})();
