// zn-site.js — 站点信息动态渲染模块
// DOM ready 时调用 /api/wasm/site-info，存到 window.__SITE_INFO，
// 触发 site-info-loaded 事件供各页监听更新品牌名。
// 各页面通过 data-site-name 属性声明式接入：
//   - 完整替换型（hero-title / nav-logo / footer-brand / 无前缀 <title>）：直接加 data-site-name
//   - 带前缀型（如 "管理面板 · <site_name>"）：不加 data-site-name，静态文本由 HTML 维护
(function () {
  function applySiteName(name) {
    if (!name) return;
    // 所有带 data-site-name 属性的元素替换为纯文本
    // （包括 <title data-site-name>，textContent 赋值会更新浏览器标签）
    document.querySelectorAll('[data-site-name]').forEach(function (el) {
      el.textContent = name;
    });
  }

  function loadSiteInfo() {
    fetch('/api/wasm/site-info', { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data && data.success && data.site_name) {
          window.__SITE_INFO = data;
          applySiteName(data.site_name);
          document.dispatchEvent(new CustomEvent('site-info-loaded', { detail: data }));
        }
      })
      .catch(function (e) {
        console.warn('[zn-site] 加载站点信息失败:', e);
      });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', loadSiteInfo);
  } else {
    loadSiteInfo();
  }
})();
