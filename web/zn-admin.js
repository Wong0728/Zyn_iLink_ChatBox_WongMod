/* Zyn iLink ChatBox - Admin Panel Script */

// ─── Admin API helpers ──────────────────────────────
const _adminGet = function(path) {
  return new Promise(function(resolve, reject) {
    var xhr = new XMLHttpRequest();
    xhr.open("GET", "/api/admin/" + path, true);
    // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie 自动携带。
    xhr.timeout = 15000;
    var settled = false;
    var safeReject = function(err) {
      if (settled) return;
      settled = true;
      reject(err);
    };
    xhr.onload = function() {
      if (xhr.status === 401) { safeReject(new Error("401 Unauthorized")); return; }
      if (xhr.status >= 200 && xhr.status < 300) {
        try { resolve(JSON.parse(xhr.responseText)); }
        catch(e) { resolve({}); }
      } else {
        var detail = "";
        try { var body = JSON.parse(xhr.responseText); detail = body.error || body.detail || body.message || ""; } catch(e2) {}
        safeReject(new Error("HTTP " + xhr.status + (detail ? ": " + detail : "")));
      }
    };
    xhr.onerror = function() { safeReject(new Error("Network Error")); };
    xhr.ontimeout = function() { safeReject(new Error("请求超时")); };
    xhr.onabort = function() { safeReject(new Error("请求被取消")); };
    try { xhr.send(); } catch(err) { safeReject(err); }
  });
};

const _adminApi = function(path, body) {
  return new Promise(function(resolve, reject) {
    var xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/admin/" + path, true);
    xhr.setRequestHeader("Content-Type", "application/json");
    // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie 自动携带。
    xhr.timeout = 30000;
    var settled = false;
    var safeReject = function(err) {
      if (settled) return;
      settled = true;
      reject(err);
    };
    xhr.onload = function() {
      if (xhr.status === 401) { safeReject(new Error("401 Unauthorized")); return; }
      if (xhr.status >= 200 && xhr.status < 300) {
        try { resolve(JSON.parse(xhr.responseText)); }
        catch(e) { resolve({}); }
      } else {
        var detail = "";
        try { var b = JSON.parse(xhr.responseText); detail = b.error || b.detail || b.message || ""; } catch(e2) {}
        safeReject(new Error("HTTP " + xhr.status + (detail ? ": " + detail : "")));
      }
    };
    xhr.onerror = function() { safeReject(new Error("Network Error")); };
    xhr.ontimeout = function() { safeReject(new Error("请求超时")); };
    xhr.onabort = function() { safeReject(new Error("请求被取消")); };
    try { xhr.send(JSON.stringify(body || {})); } catch(err) { safeReject(err); }
  });
};

// ─── State ──────────────────────────────────────────
var _admin = {
  currentSection: "users",
  me: null,
};

// FIX A1+A2 (2026-07-20): 列表分页 + 搜索前端实现。
//   每个列表维护 rawData、search、shown（当前已显示条数）。
//   渲染时按 search 过滤后 slice(0, shown)，剩余条数提供"加载更多"按钮。
//   默认每页 20 条，避免 100+ 条一次性渲染卡顿。
var _listCache = {
  users:   { data: [], search: '', shown: 20, PAGE: 20, colspan: 12 },
  invites: { data: [], search: '', shown: 20, PAGE: 20, colspan: 8 },
  ipbans:  { data: [], search: '', shown: 20, PAGE: 20, colspan: 6 },
  audit:   { data: [], search: '', shown: 50, PAGE: 50, colspan: 5, dateFrom: '', dateTo: '' }
};

// 通用搜索过滤：在指定字段中匹配 search（不区分大小写）
var _matchSearch = function(row, keys, q) {
  if (!q) return true;
  var ql = q.toLowerCase();
  return keys.some(function(k) { return String(row[k] || '').toLowerCase().indexOf(ql) >= 0; });
};

// 通用加载更多按钮 HTML
var _loadMoreRowHtml = function(colspan, remaining) {
  return '<tr><td colspan="' + colspan + '" style="text-align:center;padding:10px;">' +
         '<button class="action-btn" data-action="load-more">加载更多（剩余 ' + remaining + ' 条）</button>' +
         '</td></tr>';
};

// ─── DOM helpers ───────────────────────────────────
var $ = function(id) { return document.getElementById(id); };

// FIX P1-12 (2026-07-20): _escapeHtml 同时转义引号（" 和 '），
//   既适用 HTML 内容上下文（<div>HERE</div>），也适用属性上下文（<div data-x="HERE">）。
//   原实现仅靠 textContent→innerHTML，不转义引号，含 " 或 ' 的恶意值可逃逸属性
//   （例如 username=x" onmouseover="alert(1) 拼入 data-user="..." 后可注入事件处理器）。
//   引号在 HTML 内容上下文中无需转义，但转义也不会破坏显示（浏览器正确解码 &quot; &#39;）。
//   统一转义引号可避免开发者忘记区分上下文而误用。
var _escapeHtml = function(s) {
  if (s == null) return "";
  var d = document.createElement("div");
  d.textContent = String(s);
  return d.innerHTML.replace(/"/g, '&quot;').replace(/'/g, '&#39;');
};

var _formatTime = function(t) {
  if (!t) return "-";
  try {
    var d = new Date(t);
    if (isNaN(d.getTime())) return String(t);
    var Y = d.getFullYear();
    var M = String(d.getMonth() + 1).padStart(2, "0");
    var D = String(d.getDate()).padStart(2, "0");
    var h = String(d.getHours()).padStart(2, "0");
    var m = String(d.getMinutes()).padStart(2, "0");
    return Y + "-" + M + "-" + D + " " + h + ":" + m;
  } catch(e) { return String(t); }
};

var _formatBytes = function(b) {
  if (b == null || b === 0) return "0 B";
  var units = ["B", "KB", "MB", "GB", "TB"];
  var i = 0;
  var v = Number(b);
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2) + " " + units[i];
};

var _botStateLabel = function(s) {
  if (!s) return '<span class="badge badge-disabled">无数据</span>';
  if (s.has_bot) {
    var label = s.session_state === "active" ? "正常" : (s.session_state === "session_expired" ? "已过期" : "离线");
    var cls = s.session_state === "active" ? "badge-active" : "badge-disabled";
    // L-18：contacts_total 强制数值化，防御异常数据注入 HTML
    var contacts = parseInt(s.contacts_total, 10);
    if (isNaN(contacts) || contacts < 0) contacts = 0;
    return '<span class="badge ' + cls + '">' + label + ' (' + contacts + ')</span>';
  }
  return '<span class="badge badge-disabled">未绑定</span>';
};

// ─── Section switching ─────────────────────────────
var _switchSection = function(name) {
  _admin.currentSection = name;

  // Update nav items
  document.querySelectorAll(".admin-nav-item").forEach(function(el) {
    el.classList.toggle("active", el.getAttribute("data-section") === name);
  });

  // Update sections
  document.querySelectorAll(".admin-section").forEach(function(el) {
    el.classList.toggle("active", el.id === "section-" + name);
  });

  // Update topbar title
  var titles = {
    users: "用户管理",
    invites: "邀请码",
    ipbans: "IP 封禁",
    tunnel: "内网穿透",
    settings: "系统设置",
    audit: "审计日志",
    stats: "系统统计",
    notification: "全局通知"
  };
  $("admin-topbar-title").textContent = titles[name] || name;

  // Close mobile nav
  $("admin-nav").classList.remove("open");
  $("admin-nav-overlay").classList.remove("show");

  // Load section data if not yet loaded
  _loadSection(name);
};

var _loadSection = function(name) {
  // F4 fix：离开 tunnel section 时清理轮询，避免后台继续打请求。
  if (name !== "tunnel") _stopTunnelPolling();
  switch (name) {
    case "users": _loadUsers(); break;
    case "invites": _loadInvites(); break;
    case "ipbans": _loadIpBans(); break;
    case "tunnel": _loadTunnel(); break;
    case "settings": _loadSettings(); break;
    case "audit": _loadAudit(); break;
    case "stats": _loadStats(); break;
    case "notification": break; // notification has no loader
  }
};

// ─── Users ─────────────────────────────────────────
var _loadUsers = function() {
  var tbody = $("users-tbody");
  tbody.innerHTML = '<tr><td colspan="12" style="text-align:center;color:var(--text-hint);padding:40px;">加载中...</td></tr>';

  _adminGet("users").then(function(res) {
    if (!res.success || !res.users) {
      tbody.innerHTML = '<tr><td colspan="12" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败</td></tr>';
      return;
    }
    _listCache.users.data = res.users || [];
    _botStatusCache = {};
    _renderUsers();
    // F8 fix：一次批量请求所有可见用户的 bot 状态，避免 N 次单用户轮询。
    _loadBotStatusBatch();
  }).catch(function(err) {
    tbody.innerHTML = '<tr><td colspan="12" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败: ' + _escapeHtml(err.message) + '</td></tr>';
  });
};

// FIX A1+A2: 用户列表渲染（搜索过滤 + 分页切片）
// FIX A9 (2026-07-20): 行首加 checkbox 支持批量操作
// 改造方案 §二+§三: 增加流量/机器人列 + 行展开配额编辑
// D2 fix：合并"机器人"和"机器人状态"为单列（badge 内嵌数字）。
var _botStatusCache = {}; // uid -> bot status data
var _expandedUser = null; // uid of expanded row

// F8 fix：批量拉取当前可见用户的 bot 状态，写缓存后局部刷新 cell。
var _loadBotStatusBatch = function() {
  var uids = [];
  var c = _listCache.users;
  var filtered = c.data.filter(function(u) {
    return _matchSearch(u, ['username', 'email', 'uid', 'id'], c.search);
  });
  filtered.slice(0, c.shown).forEach(function(u) {
    var uid = u.uid != null ? u.uid : u.id;
    if (!_botStatusCache[uid]) uids.push(uid);
  });
  if (uids.length === 0) return;
  _adminGet("bot-status-batch?uids=" + encodeURIComponent(uids.join(','))).then(function(res) {
    if (!res.success || !res.statuses) return;
    Object.keys(res.statuses).forEach(function(uidStr) {
      _botStatusCache[parseInt(uidStr)] = res.statuses[uidStr];
    });
    // 刷新已渲染的 bot-status cell
    var tbody = $("users-tbody");
    if (!tbody) return;
    Object.keys(res.statuses).forEach(function(uidStr) {
      var uid = parseInt(uidStr);
      var cell = tbody.querySelector('.bot-status-cell[data-uid="' + uid + '"]');
      if (cell) cell.innerHTML = _botStateLabel(res.statuses[uidStr]);
    });
  }).catch(function() { /* 静默：bot 状态非关键路径 */ });
};

var _renderUsers = function() {
  var tbody = $("users-tbody");
  var c = _listCache.users;
  var q = c.search;
  var filtered = c.data.filter(function(u) {
    return _matchSearch(u, ['username', 'email', 'uid', 'id'], q);
  });
  var slice = filtered.slice(0, c.shown);
  var html = "";
  slice.forEach(function(u) {
    var uid = u.uid != null ? u.uid : u.id;
    var roleBadge = "badge-" + (u.role === "owner" || u.role === "admin" ? u.role : "user");
    var statusBadge = u.enabled === false || u.disabled ? "badge-disabled" : "badge-active";
    var statusLabel = u.enabled === false || u.disabled ? "已禁用" : "正常";
    var disableLabel = u.enabled === false || u.disabled ? "启用" : "禁用";
    var disableAction = u.enabled === false || u.disabled ? "enable" : "disable";
    var usernameEsc = _escapeHtml(u.username);

    // 流量列
    var uploadStr = _formatBytes(u.used_upload_bytes) + " / " + _formatBytes(u.quota_upload_bytes);
    var downloadStr = _formatBytes(u.used_download_bytes) + " / " + _formatBytes(u.quota_download_bytes);
    var mediaStr = _formatBytes(u.used_media_bytes) + " / " + _formatBytes(u.quota_media_bytes);

    // 机器人状态（badge 已内嵌 contacts_total 数字，单列即可）
    var botStatus = _botStatusCache[uid];
    var botHtml = botStatus ? _botStateLabel(botStatus) : '<span class="badge badge-disabled">加载中...</span>';

    var isExpanded = _expandedUser === uid;
    var expandStyle = isExpanded ? '' : ' style="display:none;"';

    html += "<tr class='user-row' data-uid='" + uid + "' data-user='" + usernameEsc + "'>" +
      "<td style='text-align:center;'>" +
        (u.role === 'owner' ? '<span style="color:var(--text-hint);" title="owner 不可批量操作">—</span>'
                            : "<input type='checkbox' class='users-row-checkbox' data-user='" + usernameEsc + "'>") +
      "</td>" +
      "<td style='font-family:var(--font-mono,monospace);font-size:12px;color:var(--text-hint);'>" + uid + "</td>" +
      "<td><strong>" + usernameEsc + "</strong></td>" +
      "<td><span class='badge " + roleBadge + "'>" + _escapeHtml(u.role) + "</span></td>" +
      "<td><span class='badge " + statusBadge + "'>" + statusLabel + "</span></td>" +
      "<td style='color:var(--text-secondary);font-size:12px;'>" + _escapeHtml(u.email || "-") + "</td>" +
      "<td style='color:var(--text-hint);font-size:12px;'>" + _formatTime(u.created_at) + "</td>" +
      "<td style='font-size:12px;color:var(--text-secondary);'>" + uploadStr + "</td>" +
      "<td style='font-size:12px;color:var(--text-secondary);'>" + downloadStr + "</td>" +
      "<td style='font-size:12px;color:var(--text-secondary);'>" + mediaStr + "</td>" +
      "<td class='bot-status-cell' data-uid='" + uid + "'>" + botHtml + "</td>" +
      "<td style='white-space:nowrap;'>" +
        "<button class='action-btn' data-action='" + disableAction + "' data-user='" + usernameEsc + "' style='margin-right:4px;'>" + disableLabel + "</button>" +
        "<button class='action-btn danger' data-action='delete' data-user='" + usernameEsc + "'>删除</button>" +
      "</td>" +
      "</tr>" +
      // 展开行：配额详情 + inline edit
      "<tr class='user-expand-row' data-uid='" + uid + "'" + expandStyle + ">" +
        "<td colspan='12' style='padding:12px 14px;background:var(--bg-secondary);'>" +
          "<div style='font-size:13px;color:var(--text-secondary);display:flex;flex-wrap:wrap;gap:16px;align-items:center;'>" +
            "<span>上传: <span class='quota-val' data-uid='" + uid + "' data-field='upload_bytes'>" + _formatBytes(u.quota_upload_bytes) +
              "</span> <button class='kv-edit-btn' data-action='edit-quota' data-uid='" + uid + "' data-username='" + usernameEsc + "' data-field='upload_bytes'>编辑</button></span>" +
            "<span>下载: <span class='quota-val' data-uid='" + uid + "' data-field='download_bytes'>" + _formatBytes(u.quota_download_bytes) +
              "</span> <button class='kv-edit-btn' data-action='edit-quota' data-uid='" + uid + "' data-username='" + usernameEsc + "' data-field='download_bytes'>编辑</button></span>" +
            "<span>媒体: <span class='quota-val' data-uid='" + uid + "' data-field='media_bytes'>" + _formatBytes(u.quota_media_bytes) +
              "</span> <button class='kv-edit-btn' data-action='edit-quota' data-uid='" + uid + "' data-username='" + usernameEsc + "' data-field='media_bytes'>编辑</button></span>" +
            "<span>每日消息: <span class='quota-val' data-uid='" + uid + "' data-field='msg_per_day'>" + (u.quota_msg_per_day || 0) +
              "</span> <button class='kv-edit-btn' data-action='edit-quota' data-uid='" + uid + "' data-username='" + usernameEsc + "' data-field='msg_per_day'>编辑</button></span>" +
            "<span>媒体数量: <span class='quota-val' data-uid='" + uid + "' data-field='media_count'>" + (u.quota_media_count || 0) +
              "</span> <button class='kv-edit-btn' data-action='edit-quota' data-uid='" + uid + "' data-username='" + usernameEsc + "' data-field='media_count'>编辑</button></span>" +
          "</div>" +
        "</td>" +
      "</tr>";
  });
  var remaining = filtered.length - slice.length;
  if (remaining > 0) html += _loadMoreRowHtml(c.colspan, remaining);
  if (filtered.length === 0) {
    html = '<tr><td colspan="' + c.colspan + '" style="text-align:center;color:var(--text-hint);padding:40px;">' + (q ? '无匹配结果' : '暂无用户') + '</td></tr>';
  }
  tbody.innerHTML = html;
  var countEl = $("users-count");
  if (countEl) countEl.textContent = q ? '匹配 ' + filtered.length + ' / 共 ' + c.data.length + ' 条' : (c.data.length > 0 ? '共 ' + c.data.length + ' 条' : '');
  // FIX A9: 同步全选 checkbox 状态（搜索/翻页后重置）
  var selectAll = $('users-select-all');
  if (selectAll) selectAll.checked = false;
  // D5 fix：刷新"已选 N 项"提示
  _updateUsersSelectionCount();
};

// 行点击展开/收起
document.addEventListener("click", function(e) {
  var row = e.target.closest("tr.user-row");
  if (!row) return;
  // 忽略点击 checkbox / 按钮
  if (e.target.closest("input,button")) return;
  var uid = parseInt(row.getAttribute("data-uid"));
  var expandRow = document.querySelector('tr.user-expand-row[data-uid="' + uid + '"]');
  if (!expandRow) return;
  if (_expandedUser === uid) {
    expandRow.style.display = "none";
    _expandedUser = null;
  } else {
    // 收起其他展开行
    if (_expandedUser != null) {
      var prev = document.querySelector('tr.user-expand-row[data-uid="' + _expandedUser + '"]');
      if (prev) prev.style.display = "none";
    }
    expandRow.style.display = "";
    _expandedUser = uid;
  }
});

// D5 fix：刷新"已选 N 项"提示（监听 checkbox 变化 + 渲染后同步）。
var _updateUsersSelectionCount = function() {
  var el = $('users-selected-count');
  if (!el) return;
  var checked = document.querySelectorAll('.users-row-checkbox:checked');
  if (checked.length === 0) {
    el.style.display = 'none';
    el.textContent = '';
  } else {
    el.style.display = '';
    el.textContent = '已选 ' + checked.length + ' 项';
  }
};
document.addEventListener('change', function(e) {
  if (e.target && e.target.classList && e.target.classList.contains('users-row-checkbox')) {
    _updateUsersSelectionCount();
  }
});

// 配额 inline edit（D4 fix：用自定义 prompt modal 替代 window.prompt，统一风格）。
document.addEventListener("click", function(e) {
  var btn = e.target.closest("[data-action='edit-quota']");
  if (!btn) return;
  var uid = btn.getAttribute("data-uid");
  var username = btn.getAttribute("data-username");
  var field = btn.getAttribute("data-field");
  var span = document.querySelector('.quota-val[data-uid="' + uid + '"][data-field="' + field + '"]');
  if (!span) return;
  var current = span.textContent.trim();
  var labelMap = { upload_bytes: "上传(字节)", download_bytes: "下载(字节)", media_bytes: "媒体(字节)", msg_per_day: "每日消息数", media_count: "媒体数量" };
  _showPrompt("修改 " + (labelMap[field] || field) + "（输入数字）", current, function(newVal) {
    if (newVal == null || newVal.trim() === "") return;
    var num = parseInt(newVal.trim());
    if (isNaN(num) || num < 0) { _toast("请输入有效数字", 2000, "error"); return; }
    var body = { user: username };
    body[field] = num;
    _adminApi("user/quota", body).then(function(res) {
      if (res.success) {
        _toast("配额已更新", 2000);
        _botStatusCache = {}; // clear cache
        _loadUsers();
      } else {
        _toast(res.error || "更新失败", 3000, "error");
      }
    }).catch(function(err) {
      _toast(err.message, 3000, "error");
    });
  });
});

var _handleUserAction = function(e) {
  var btn = e.target.closest("button[data-action]");
  if (!btn) return;
  var action = btn.getAttribute("data-action");
  // FIX A1: 处理"加载更多"按钮
  if (action === "load-more") {
    // F5 fix：加载更多后自动滚动到新内容首行，避免用户手动找位置。
    var prevHeight = ($("users-tbody") || {}).scrollHeight || 0;
    _listCache.users.shown += _listCache.users.PAGE;
    _renderUsers();
    requestAnimationFrame(function() {
      var tbody = $("users-tbody");
      if (!tbody) return;
      // 找到新加载的第一行（上一页最后一行之后），滚动到可视区
      var firstNewRow = tbody.querySelectorAll("tr.user-row")[_listCache.users.shown - _listCache.users.PAGE - 1];
      // 上一页最后一行已显示，新行在它之后；这里直接滚动表格容器
      var wrap = tbody.closest('.admin-table-wrap') || tbody.parentElement;
      if (wrap && wrap.scrollHeight > prevHeight) {
        wrap.scrollTop = wrap.scrollTop + 200; // 小幅下滚让用户感知到新内容
      } else if (firstNewRow && firstNewRow.scrollIntoView) {
        firstNewRow.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
      }
    });
    return;
  }
  var username = btn.getAttribute("data-user");

  if (action === "delete") {
    // D6 fix：用 _showConfirm 替代 window.confirm，与项目其他模态一致。
    _showConfirm("确定要删除用户「" + username + "」吗？\n\n此操作不可撤销！", function() {
      _adminApi("user/delete", { user: username }).then(function(res) {
        if (res.success) { _toast("用户已删除", 2000); _loadUsers(); }
        else { _toast(res.error || "删除失败", 3000, "error"); }
      }).catch(function(err) { _toast(err.message, 3000, "error"); });
    }, null, { danger: true, okText: "删除" });
  } else if (action === "disable") {
    // FIX A5 (2026-07-20): 禁用操作添加确认对话框，避免误点击立即锁定用户。
    _showConfirm("确定要禁用用户「" + username + "」吗？\n\n禁用后该用户将无法登录，已建立的会话仍有效直至自然过期。", function() {
      _adminApi("user/disable", { user: username }).then(function(res) {
        if (res.success) { _toast("用户已禁用", 2000); _loadUsers(); }
        else { _toast(res.error || "禁用失败", 3000, "error"); }
      }).catch(function(err) { _toast(err.message, 3000, "error"); });
    }, null, { okText: "禁用" });
  } else if (action === "enable") {
    _adminApi("user/enable", { user: username }).then(function(res) {
      if (res.success) { _toast("用户已启用", 2000); _loadUsers(); }
      else { _toast(res.error || "启用失败", 3000, "error"); }
    }).catch(function(err) { _toast(err.message, 3000, "error"); });
  }
};

// ─── Invites ───────────────────────────────────────
var _loadInvites = function() {
  var tbody = $("invites-tbody");
  tbody.innerHTML = '<tr><td colspan="7" style="text-align:center;color:var(--text-hint);padding:40px;">加载中...</td></tr>';

  _adminGet("invites").then(function(res) {
    if (!res.success) {
      tbody.innerHTML = '<tr><td colspan="7" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败</td></tr>';
      return;
    }
    _listCache.invites.data = res.invites || [];
    _renderInvites();
  }).catch(function(err) {
    tbody.innerHTML = '<tr><td colspan="7" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败: ' + _escapeHtml(err.message) + '</td></tr>';
  });
};

// FIX A1+A2+A9: 邀请码列表渲染（搜索 + 分页 + 批量选择）
var _renderInvites = function() {
  var tbody = $("invites-tbody");
  var c = _listCache.invites;
  var q = c.search;
  var filtered = c.data.filter(function(inv) {
    return _matchSearch(inv, ['code', 'created_by', 'note'], q);
  });
  var slice = filtered.slice(0, c.shown);
  var html = "";
  slice.forEach(function(inv) {
    var expired = inv.expires_at && new Date(inv.expires_at) < new Date();
    var revoked = inv.revoked || inv.used;
    var statusLabel = revoked ? "已使用" : (expired ? "已过期" : "有效");
    var statusCls = revoked ? "badge-disabled" : (expired ? "badge-disabled" : "badge-active");
    var codeEsc = _escapeHtml(inv.code);

    html += "<tr>" +
      "<td style='text-align:center;'>" +
        (revoked ? '<span style="color:var(--text-hint);">—</span>'
                 : "<input type='checkbox' class='invites-row-checkbox' data-code='" + codeEsc + "'>") +
      "</td>" +
      "<td style='font-family:var(--font-mono,monospace);font-size:12px;'>" + codeEsc + "</td>" +
      "<td>" + _escapeHtml(inv.created_by || "-") + "</td>" +
      "<td style='font-size:12px;color:var(--text-hint);'>" + _formatTime(inv.created_at) + "</td>" +
      "<td style='font-size:12px;color:var(--text-hint);'>" + _formatTime(inv.expires_at) + "</td>" +
      "<td><span class='badge " + statusCls + "'>" + statusLabel + "</span></td>" +
      "<td style='font-size:12px;color:var(--text-secondary);'>" + _escapeHtml(inv.note || "-") + "</td>" +
      "<td>" +
        (revoked ? "-" : "<button class='action-btn danger' data-action='revoke-invite' data-code='" + codeEsc + "'>撤销</button>") +
      "</td>" +
      "</tr>";
  });
  var remaining = filtered.length - slice.length;
  if (remaining > 0) html += _loadMoreRowHtml(c.colspan, remaining);
  if (filtered.length === 0) {
    html = '<tr><td colspan="' + c.colspan + '" style="text-align:center;color:var(--text-hint);padding:40px;">' + (q ? '无匹配结果' : '暂无邀请码') + '</td></tr>';
  }
  tbody.innerHTML = html;
  var countEl = $("invites-count");
  if (countEl) countEl.textContent = q ? '匹配 ' + filtered.length + ' / 共 ' + c.data.length + ' 条' : (c.data.length > 0 ? '共 ' + c.data.length + ' 条' : '');
  // FIX A9: 同步全选 checkbox 状态
  var selectAll = $('invites-select-all');
  if (selectAll) selectAll.checked = false;
};

// ─── IP Bans ───────────────────────────────────────
var _loadIpBans = function() {
  var tbody = $("ipbans-tbody");
  tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:var(--text-hint);padding:40px;">加载中...</td></tr>';

  _adminGet("ip-bans").then(function(res) {
    if (!res.success) {
      tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败</td></tr>';
      return;
    }
    _listCache.ipbans.data = res.bans || [];
    _renderIpBans();
  }).catch(function(err) {
    tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败: ' + _escapeHtml(err.message) + '</td></tr>';
  });
};

// FIX A1+A2+A9: IP 封禁列表渲染（搜索 + 分页 + 批量选择）
var _renderIpBans = function() {
  var tbody = $("ipbans-tbody");
  var c = _listCache.ipbans;
  var q = c.search;
  var filtered = c.data.filter(function(b) {
    return _matchSearch(b, ['ip', 'reason'], q);
  });
  var slice = filtered.slice(0, c.shown);
  var html = "";
  slice.forEach(function(b) {
    var ipEsc = _escapeHtml(b.ip);
    html += "<tr>" +
      "<td style='text-align:center;'><input type='checkbox' class='ipbans-row-checkbox' data-ip='" + ipEsc + "'></td>" +
      "<td style='font-family:var(--font-mono,monospace);font-size:12px;'>" + ipEsc + "</td>" +
      "<td>" + _escapeHtml(b.reason || "-") + "</td>" +
      "<td style='font-size:12px;color:var(--text-hint);'>" + _formatTime(b.created_at) + "</td>" +
      "<td style='font-size:12px;color:var(--text-hint);'>" + _formatTime(b.expires_at) + "</td>" +
      "<td><button class='action-btn danger' data-action='unban' data-ip='" + ipEsc + "'>解封</button></td>" +
      "</tr>";
  });
  var remaining = filtered.length - slice.length;
  if (remaining > 0) html += _loadMoreRowHtml(c.colspan, remaining);
  if (filtered.length === 0) {
    html = '<tr><td colspan="' + c.colspan + '" style="text-align:center;color:var(--text-hint);padding:40px;">' + (q ? '无匹配结果' : '暂无封禁') + '</td></tr>';
  }
  tbody.innerHTML = html;
  var countEl = $("ipbans-count");
  if (countEl) countEl.textContent = q ? '匹配 ' + filtered.length + ' / 共 ' + c.data.length + ' 条' : (c.data.length > 0 ? '共 ' + c.data.length + ' 条' : '');
  // FIX A9: 同步全选 checkbox 状态
  var selectAll = $('ipbans-select-all');
  if (selectAll) selectAll.checked = false;
};

// ─── Settings ──────────────────────────────────────
var _loadSettings = function() {
  var container = $("settings-container");
  container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">加载中...</div>';

  _adminGet("settings").then(function(res) {
    if (!res.success || !res.settings) {
      container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">加载失败</div>';
      return;
    }
    var settings = res.settings;
    if (!Array.isArray(settings)) {
      // Maybe it's an object
      var arr = [];
      Object.keys(settings).forEach(function(k) { arr.push({ key: k, value: settings[k] }); });
      settings = arr;
    }
    if (settings.length === 0) {
      container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">暂无设置项</div>';
      return;
    }
    var SETTINGS_LABELS = {
      'site_name': '站点名称',
      'allow_open_registration': '开放注册',
      'allow_invite_registration': '邀请码注册',
      'terms_version': '守则版本',
      'terms_text': '守则内容',
      'terms.url': '使用守则链接',
      'docs.url': '文档链接',
      'admin.web_access': '管理面板访问策略',
      'default_quota_upload_bytes': '新用户上传配额(字节)',
      'default_quota_download_bytes': '新用户下载配额(字节)',
      'default_quota_media_bytes': '新用户媒体配额(字节)',
      'default_quota_media_count': '新用户媒体数量配额',
      'default_quota_msg_per_day': '新用户每日消息配额',
      'default_allow_upload': '新用户上传功能',
      'default_allow_webdav': '新用户 WebDAV 功能',
      'default_allow_custom_webdav': '新用户自定义 WebDAV'
    };
    var html = '<div class="kv-list">';
    settings.forEach(function(s) {
      html += '<div class="kv-row" data-key="' + _escapeHtml(s.key) + '">' +
        '<span class="kv-key" title="' + _escapeHtml(s.key) + '">' + _escapeHtml(SETTINGS_LABELS[s.key] || s.key) + '</span>' +
        '<span class="kv-value">' + _escapeHtml(s.value != null ? String(s.value) : "") + '</span>' +
        '<button class="kv-edit-btn" data-action="edit-setting" data-key="' + _escapeHtml(s.key) + '" data-value="' + _escapeHtml(s.value != null ? String(s.value) : "") + '">编辑</button>' +
        '</div>';
    });
    html += '</div>';
    container.innerHTML = html;
  }).catch(function(err) {
    container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">加载失败: ' + _escapeHtml(err.message) + '</div>';
  });
};

// ─── Tunnel (内网穿透) ──────────────────────────────
// F4 fix：合并 status + logs 为单端点轮询，减少一半请求；离开 tunnel section 时清空。
var _tunnelPollId = null;

var _loadTunnel = function() {
  _pollTunnelStatus();
};

var _stopTunnelPolling = function() {
  if (_tunnelPollId) { clearTimeout(_tunnelPollId); _tunnelPollId = null; }
};

var _setTunnelMsg = function(msg, type) {
  var el = $("tunnel-status-msg");
  if (!el) return;
  if (!msg) { el.style.display = "none"; el.textContent = ""; return; }
  el.style.display = "block";
  el.textContent = msg;
  el.className = "tunnel-status-msg" + (type ? " " + type : "");
};

var _pollTunnelStatus = function() {
  _adminGet("tunnel/status").then(function(res) {
    if (!res.success) { _setTunnelMsg("获取状态失败", "error"); return; }
    var t = res.tunnel;
    var dot = $("tunnel-dot");
    var statusText = $("tunnel-status-text");
    var infoArea = $("tunnel-info-area");
    var urlEl = $("tunnel-url");
    var subdomainEl = $("tunnel-subdomain");
    var pidEl = $("tunnel-pid");
    var startBtn = $("tunnel-start-btn");
    var stopBtn = $("tunnel-stop-btn");
    var portInput = $("tunnel-port");
    var remoteInput = $("tunnel-remote");
    var subInput = $("tunnel-subdomain-input");

    if (t.running) {
      dot.className = "tunnel-dot running";
      statusText.textContent = "运行中";
      if (infoArea) infoArea.style.display = "block";
      if (startBtn) startBtn.style.display = "none";
      if (stopBtn) stopBtn.style.display = "";
      if (portInput) portInput.disabled = true;
      if (remoteInput) remoteInput.disabled = true;
      if (subInput) subInput.disabled = true;
      // 填充运行时信息
      if (urlEl) urlEl.textContent = t.public_url || "获取中…";
      if (subdomainEl) subdomainEl.textContent = t.subdomain || "(随机)";
      if (pidEl) pidEl.textContent = t.pid != null ? String(t.pid) : "-";
    } else {
      var isError = t.state && t.state.indexOf("Error") !== -1;
      dot.className = "tunnel-dot " + (isError ? "error" : "stopped");
      statusText.textContent = isError ? "异常" : "已停止";
      if (infoArea) infoArea.style.display = "none";
      if (startBtn) startBtn.style.display = "";
      if (stopBtn) stopBtn.style.display = "none";
      if (portInput) portInput.disabled = false;
      if (remoteInput) remoteInput.disabled = false;
      if (subInput) subInput.disabled = false;
    }
    // 同步日志（合并端点后单次请求返回 status + logs）
    var logEl = $("tunnel-log");
    if (logEl && Array.isArray(res.logs)) {
      logEl.textContent = res.logs.join("\n") || "(暂无日志)";
      logEl.scrollTop = logEl.scrollHeight;
    }
    // 继续轮询（仅在仍处于 tunnel section 时）
    if (_tunnelPollId) clearTimeout(_tunnelPollId);
    _tunnelPollId = setTimeout(_pollTunnelStatus, 3000);
  }).catch(function(err) {
    _setTunnelMsg("获取状态失败: " + err.message, "error");
    if (_tunnelPollId) clearTimeout(_tunnelPollId);
    _tunnelPollId = setTimeout(_pollTunnelStatus, 5000);
  });
};

// ─── Audit Logs ────────────────────────────────────
var _loadAudit = function() {
  var tbody = $("audit-tbody");
  tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:var(--text-hint);padding:40px;">加载中...</td></tr>';

  _adminGet("audit").then(function(res) {
    if (!res.success) {
      tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败</td></tr>';
      return;
    }
    _listCache.audit.data = res.logs || [];
    _renderAudit();
  }).catch(function(err) {
    tbody.innerHTML = '<tr><td colspan="5" style="text-align:center;color:var(--text-hint);padding:40px;">加载失败: ' + _escapeHtml(err.message) + '</td></tr>';
  });
};

// FIX A1+A2+A3 (2026-07-20): 审计日志渲染（搜索 + 日期范围 + 分页 + 导出）
//   日期范围：dateFrom/dateTo 为 'YYYY-MM-DD' 字符串，按 created_at 字段过滤。
//   导出：CSV/JSON 文件下载（基于当前过滤后的全量数据，非当前页切片）。
var _filterAudit = function() {
  var c = _listCache.audit;
  var q = c.search;
  var fromTs = c.dateFrom ? new Date(c.dateFrom + 'T00:00:00').getTime() : null;
  var toTs = c.dateTo ? new Date(c.dateTo + 'T23:59:59.999').getTime() : null;
  return c.data.filter(function(log) {
    if (!_matchSearch(log, ['username', 'user', 'action', 'operation', 'ip'], q)) return false;
    var t = log.created_at || log.time;
    if (!t) return true;
    var ts = new Date(t).getTime();
    if (isNaN(ts)) return true;
    if (fromTs !== null && ts < fromTs) return false;
    if (toTs !== null && ts > toTs) return false;
    return true;
  });
};

var _renderAudit = function() {
  var tbody = $("audit-tbody");
  var c = _listCache.audit;
  var filtered = _filterAudit();
  var slice = filtered.slice(0, c.shown);
  var html = "";
  slice.forEach(function(log, i) {
    var detail = log.detail || log.message || log.action || "";
    // E1 fix：行可点击展开完整 detail。用 _showConfirm modal 显示完整内容，
    //   仅一个"关闭"按钮，与项目其他模态风格一致。
    //   原 .audit-msg title= 仅悬停可见，长内容被 ellipsis 截断不可读。
    var detailSafe = _escapeAttr(detail);
    html += "<tr class='audit-row' style='cursor:pointer;' data-detail='" + detailSafe + "'>" +
      "<td style='font-size:12px;color:var(--text-hint);white-space:nowrap;'>" + _formatTime(log.created_at || log.time) + "</td>" +
      "<td>" + _escapeHtml(log.username || log.user || "-") + "</td>" +
      "<td>" + _escapeHtml(log.action || log.operation || "-") + "</td>" +
      "<td class='audit-msg' title='" + _escapeHtml(detail) + "'>" + _escapeHtml(detail) + "</td>" +
      "<td style='font-size:12px;color:var(--text-hint);font-family:var(--font-mono,monospace);'>" + _escapeHtml(log.ip || "-") + "</td>" +
      "</tr>";
  });
  var remaining = filtered.length - slice.length;
  if (remaining > 0) html += _loadMoreRowHtml(c.colspan, remaining);
  if (filtered.length === 0) {
    var hasFilter = c.search || c.dateFrom || c.dateTo;
    html = '<tr><td colspan="' + c.colspan + '" style="text-align:center;color:var(--text-hint);padding:40px;">' + (hasFilter ? '无匹配结果' : '暂无审计日志') + '</td></tr>';
  }
  tbody.innerHTML = html;
  var countEl = $("audit-count");
  if (countEl) {
    var hasFilter = c.search || c.dateFrom || c.dateTo;
    countEl.textContent = hasFilter ? '匹配 ' + filtered.length + ' / 共 ' + c.data.length + ' 条' : (c.data.length > 0 ? '共 ' + c.data.length + ' 条' : '');
  }
};

// FIX A3: 审计日志 CSV / JSON 导出（基于当前过滤后的全量数据）
var _exportAudit = function(format) {
  var filtered = _filterAudit();
  if (filtered.length === 0) {
    _toast("无数据可导出", 2000, "error");
    return;
  }
  var rows = filtered.map(function(log) {
    return {
      time: log.created_at || log.time || '',
      username: log.username || log.user || '',
      action: log.action || log.operation || '',
      detail: log.detail || log.message || log.action || '',
      ip: log.ip || ''
    };
  });
  var content, mime, ext;
  if (format === 'csv') {
    var escCsv = function(v) {
      var s = String(v == null ? '' : v);
      return /[",\n]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
    };
    var header = ['time', 'username', 'action', 'detail', 'ip'].join(',');
    content = header + '\n' + rows.map(function(r) {
      return [r.time, r.username, r.action, r.detail, r.ip].map(escCsv).join(',');
    }).join('\n');
    // 加 BOM 让 Excel 正确识别 UTF-8
    content = '\ufeff' + content;
    mime = 'text/csv;charset=utf-8';
    ext = 'csv';
  } else {
    content = JSON.stringify(rows, null, 2);
    mime = 'application/json;charset=utf-8';
    ext = 'json';
  }
  var blob = new Blob([content], { type: mime });
  var url = URL.createObjectURL(blob);
  var a = document.createElement('a');
  a.href = url;
  var ts = new Date();
  var pad = function(n) { return n < 10 ? '0' + n : '' + n; };
  var fname = 'audit-' + ts.getFullYear() + pad(ts.getMonth()+1) + pad(ts.getDate()) + '-' + pad(ts.getHours()) + pad(ts.getMinutes()) + '.' + ext;
  a.download = fname;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(function() { URL.revokeObjectURL(url); }, 1000);
  _toast("已导出 " + filtered.length + " 条到 " + fname, 3000);
};

// ─── Stats ─────────────────────────────────────────
var _loadStats = function() {
  var container = $("stats-container");
  container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">加载中...</div>';

  _adminGet("stats").then(function(res) {
    if (!res.success || !res.stats) {
      container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">加载失败</div>';
      return;
    }
    var stats = res.stats;
    var STATS_LABELS = {
      'users_total': '用户总数',
      'users_active': '活跃用户',
      'users_disabled': '禁用用户',
      'invites_total': '邀请码总数',
      'invites_active': '有效邀请码',
      'settings_count': '配置项数',
      'audit_recent': '近期审计',
      'mem_total_mb': '内存总量(MB)',
      'mem_used_mb': '内存已用(MB)',
      'cpu_usage_percent': 'CPU使用率(%)',
      'disk_total_mb': '磁盘总量(MB)',
      'disk_used_mb': '磁盘已用(MB)',
      'uptime_secs': '运行时间(秒)',
    };
    var html = '<div class="stat-cards">';
    Object.keys(stats).forEach(function(k) {
      var v = stats[k];
      html += '<div class="stat-card">' +
        '<div class="stat-card-label">' + _escapeHtml(STATS_LABELS[k] || k) + '</div>' +
        '<div class="stat-card-value">' + _escapeHtml(v != null ? v : 0) + '</div>' +
        '</div>';
    });
    html += '</div>';

    // FIX A13 (2026-07-20): Webhook 状态展示
    var wh = res.webhook;
    if (wh) {
      html += '<div style="margin-top:20px;padding:12px 16px;border:1px solid var(--border-color);border-radius:8px;background:var(--bg-card);">' +
              '<div style="font-size:14px;font-weight:600;margin-bottom:8px;">Webhook 出站推送</div>';
      if (wh.enabled) {
        html += '<div style="font-size:13px;line-height:1.8;">' +
                '<div>状态: <span class="badge badge-active">已启用</span></div>' +
                '<div>目标 URL 数: ' + _escapeHtml(wh.url_count) + '</div>' +
                '<div>HMAC 签名: ' + (wh.has_token ? '<span style="color:#27ae60;">已配置</span>' : '<span style="color:var(--text-hint);">未配置</span>') + '</div>' +
                '<div>距上次 SSRF 校验: ' + _escapeHtml(wh.secs_since_validate) + ' 秒</div>';
        if (wh.urls && wh.urls.length > 0) {
          html += '<div style="margin-top:4px;">目标列表:</div><ul style="margin:4px 0 0 0;padding-left:20px;font-size:12px;font-family:var(--font-mono,monospace);color:var(--text-secondary);">';
          wh.urls.forEach(function(u) {
            html += '<li style="word-break:break-all;">' + _escapeHtml(u) + '</li>';
          });
          html += '</ul>';
        }
        html += '</div>';
      } else {
        html += '<div style="font-size:13px;color:var(--text-hint);line-height:1.6;">' +
                '状态: <span class="badge badge-disabled">未配置</span><br>' +
                '配置方法：设置环境变量 <code style="background:var(--bg-secondary);padding:1px 4px;border-radius:3px;">ILINK_WEBHOOK_URLS</code>（逗号分隔多个 URL）和可选的 <code style="background:var(--bg-secondary);padding:1px 4px;border-radius:3px;">ILINK_WEBHOOK_TOKEN</code>（HMAC 签名密钥），重启服务生效。' +
                '</div>';
      }
      html += '</div>';
    }

    container.innerHTML = html;
  }).catch(function(err) {
    container.innerHTML = '<div style="text-align:center;color:var(--text-hint);padding:40px;">加载失败: ' + _escapeHtml(err.message) + '</div>';
  });
};

// ─── Event Handlers ────────────────────────────────

// Nav section switching
document.querySelectorAll(".admin-nav-item").forEach(function(el) {
  el.addEventListener("click", function() {
    _switchSection(el.getAttribute("data-section"));
  });
});

// Mobile nav toggle
$("admin-menu-toggle").addEventListener("click", function() {
  $("admin-nav").classList.toggle("open");
  $("admin-nav-overlay").classList.toggle("show");
});
$("admin-nav-overlay").addEventListener("click", function() {
  $("admin-nav").classList.remove("open");
  $("admin-nav-overlay").classList.remove("show");
});

// Create user
$("create-user-btn").addEventListener("click", function() {
  var username = $("create-user-username").value.trim();
  var password = $("create-user-password").value;
  var role = $("create-user-role").value;
  if (!username) { _toast("请输入用户名", 2000, "error"); return; }
  if (!password || password.length < 8) { _toast("密码至少 8 位", 2000, "error"); return; }

  var btn = this;
  btn.disabled = true;
  btn.textContent = "创建中...";

  _adminApi("user/create", { username: username, password: password, role: role }).then(function(res) {
    if (res.success) {
      _toast("用户创建成功", 2000);
      $("create-user-username").value = "";
      $("create-user-password").value = "";
      _loadUsers();
    } else {
      _toast(res.error || "创建失败", 3000, "error");
    }
  }).catch(function(err) {
    _toast(err.message, 3000, "error");
  }).finally(function() {
    btn.disabled = false;
    btn.textContent = "创建用户";
  });
});

// User table actions (delegated)
$("users-tbody").addEventListener("click", _handleUserAction);

// Create invite
$("create-invite-btn").addEventListener("click", function() {
  var days = parseInt($("invite-days").value, 10) || 7;
  var note = $("invite-note").value.trim();

  var btn = this;
  btn.disabled = true;
  btn.textContent = "生成中...";

  _adminApi("invite/create", { days: days, note: note || undefined }).then(function(res) {
    if (res.success) {
      _toast("邀请码已生成", 2000);
      $("invite-note").value = "";
      _loadInvites();
    } else {
      _toast(res.error || "生成失败", 3000, "error");
    }
  }).catch(function(err) {
    _toast(err.message, 3000, "error");
  }).finally(function() {
    btn.disabled = false;
    btn.textContent = "生成邀请码";
  });
});

// Invite actions (delegated)
$("invites-tbody").addEventListener("click", function(e) {
  var btn = e.target.closest("button[data-action]");
  if (!btn) return;
  var action = btn.getAttribute("data-action");
  // FIX A1: 处理"加载更多"按钮
  if (action === "load-more") {
    _listCache.invites.shown += _listCache.invites.PAGE;
    _renderInvites();
    return;
  }
  if (action === "revoke-invite") {
    var code = btn.getAttribute("data-code");
    // D6 fix：用 _showConfirm 替代 window.confirm，与项目其他模态一致。
    _showConfirm("确定要撤销邀请码「" + code + "」吗？", function() {
      _adminApi("invite/revoke", { code: code }).then(function(res) {
        if (res.success) { _toast("邀请码已撤销", 2000); _loadInvites(); }
        else { _toast(res.error || "撤销失败", 3000, "error"); }
      }).catch(function(err) { _toast(err.message, 3000, "error"); });
    }, null, { danger: true, okText: "撤销" });
  }
});

// Ban IP
$("ban-ip-btn").addEventListener("click", function() {
  var btn = this;
  var ip = $("ban-ip").value.trim();
  var reason = $("ban-reason").value.trim() || undefined;
  var days = parseInt($("ban-days").value, 10) || 7;
  if (!ip) { _toast("请输入 IP 地址", 2000, "error"); return; }

  // FIX A6 (2026-07-20): IP 封禁格式校验 + 危险值警告。
  //   原实现无校验，可封 0.0.0.0/127.0.0.1/内网网段锁死服务器或阻断合法用户。
  //   校验规则：支持 IPv4 / IPv6 / CIDR（/0 ~ /32 for IPv4，/0 ~ /128 for IPv6）。
  //   危险值：0.0.0.0/0、::/0、127.0.0.0/8、内网网段（10./172.16-31./192.168./fc00::/7）
  //   需二次确认。
  var ipValidation = _validateIpForBan(ip);
  if (!ipValidation.ok) {
    _toast(ipValidation.error, 3000, "error");
    return;
  }
  if (ipValidation.dangerous) {
    // D6 fix：用 _showConfirm 替代 window.confirm，danger 样式突出风险。
    _showConfirm("⚠ 危险封禁目标\n\n" + ipValidation.warning + "\n\n确定要继续封禁「" + ip + "」吗？", function() {
      _doBanIp(ip, reason, days, btn, true);
    }, null, { danger: true, okText: "继续封禁" });
    return;
  }
  _doBanIp(ip, reason, days, btn, false);
});

// FIX A6: 实际执行封禁的内部函数，由 ban-ip-btn click handler 在 confirm 通过后调用。
//   抽出以避免 confirm 回调嵌套过深。
var _doBanIp = function(ip, reason, days, btn, confirmDangerous) {
  btn.disabled = true;
  btn.textContent = "封禁中...";

  _adminApi("ip-ban", {
    ip: ip,
    reason: reason,
    days: days,
    confirm_dangerous: confirmDangerous === true
  }).then(function(res) {
    if (res.success) {
      _toast("IP 已封禁", 2000);
      $("ban-ip").value = "";
      $("ban-reason").value = "";
      _loadIpBans();
    } else {
      _toast(res.error || "封禁失败", 3000, "error");
    }
  }).catch(function(err) {
    _toast(err.message, 3000, "error");
  }).finally(function() {
    btn.disabled = false;
    btn.textContent = "封禁 IP";
  });
};

// FIX A6: IP/CIDR 校验 + 危险值检测。
//   返回 { ok: bool, error?: string, dangerous?: bool, warning?: string }
var _validateIpForBan = function(input) {
  // 拆分 CIDR 前缀
  var parts = input.split('/');
  if (parts.length > 2) {
    return { ok: false, error: "格式错误：仅允许一个 '/' 分隔符" };
  }
  var addr = parts[0];
  var prefix = parts.length === 2 ? parseInt(parts[1], 10) : null;

  // IPv4 正则
  var ipv4Re = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;
  // IPv6 正则（简化版：支持 :: 缩写、十六进制）
  var ipv6Re = /^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:))$/;

  var isIpv4 = ipv4Re.test(addr);
  var isIpv6 = ipv6Re.test(addr);

  if (!isIpv4 && !isIpv6) {
    return { ok: false, error: "IP 格式错误：需为合法 IPv4 或 IPv6 地址" };
  }

  // IPv4 段范围校验 + 危险值检测
  if (isIpv4) {
    var segs = addr.split('.').map(function(s) { return parseInt(s, 10); });
    if (segs.some(function(s) { return s < 0 || s > 255; })) {
      return { ok: false, error: "IPv4 段范围错误（0-255）" };
    }
    if (prefix !== null && (prefix < 0 || prefix > 32)) {
      return { ok: false, error: "IPv4 CIDR 前缀范围错误（0-32）" };
    }
    // 危险值检测
    var first = segs[0];
    var second = segs[1];
    var warnings = [];
    // 0.0.0.0/0 / 0.0.0.0
    if (first === 0) warnings.push("封禁 0.x.x.x 会阻断所有未明确允许的源 IP");
    // 127.0.0.0/8 本机回环
    if (first === 127) warnings.push("封禁 127.x.x.x 会阻断本机回环访问");
    // 10.0.0.0/8 / 172.16-31.x.x / 192.168.x.x 内网
    if (first === 10) warnings.push("封禁 10.x.x.x 会阻断内网客户端");
    if (first === 172 && second >= 16 && second <= 31) warnings.push("封禁 172.16-31.x.x 会阻断内网客户端");
    if (first === 192 && second === 168) warnings.push("封禁 192.168.x.x 会阻断内网客户端");
    // /0 前缀等价于封禁所有
    if (prefix === 0) warnings.push("/0 前缀等价于封禁所有 IPv4 地址");
    if (warnings.length > 0) {
      return { ok: true, dangerous: true, warning: warnings.join("\n") };
    }
  } else {
    // IPv6
    if (prefix !== null && (prefix < 0 || prefix > 128)) {
      return { ok: false, error: "IPv6 CIDR 前缀范围错误（0-128）" };
    }
    var lower = addr.toLowerCase();
    var warnings = [];
    if (lower === "::" || lower === "::0") warnings.push("封禁 :: 会阻断所有 IPv6 源 IP");
    if (lower === "::1") warnings.push("封禁 ::1 会阻断 IPv6 本机回环");
    if (lower.indexOf("fc") === 0 || lower.indexOf("fd") === 0) warnings.push("封禁 fc00::/7 会阻断 IPv6 内网客户端");
    if (prefix === 0) warnings.push("/0 前缀等价于封禁所有 IPv6 地址");
    if (warnings.length > 0) {
      return { ok: true, dangerous: true, warning: warnings.join("\n") };
    }
  }

  return { ok: true };
};

// IP bans actions (delegated)
$("ipbans-tbody").addEventListener("click", function(e) {
  var btn = e.target.closest("button[data-action]");
  if (!btn) return;
  var action = btn.getAttribute("data-action");
  // FIX A1: 处理"加载更多"按钮
  if (action === "load-more") {
    _listCache.ipbans.shown += _listCache.ipbans.PAGE;
    _renderIpBans();
    return;
  }
  if (action === "unban") {
    var ip = btn.getAttribute("data-ip");
    // D6 fix：用 _showConfirm 替代 window.confirm。
    _showConfirm("确定要解封 IP「" + ip + "」吗？", function() {
      _adminApi("ip-unban", { user: ip }).then(function(res) {
        if (res.success) { _toast("IP 已解封", 2000); _loadIpBans(); }
        else { _toast(res.error || "解封失败", 3000, "error"); }
      }).catch(function(err) { _toast(err.message, 3000, "error"); });
    });
  }
});

// Settings modal
var _currentEditKey = null;
// FIX A7 (2026-07-20): 缓存编辑前的原始值，用于保存时显示差异预览。
//   防止管理员误改设置（如把默认配额扩大 100 倍）后无确认直接生效。
var _currentEditOldValue = null;

// FIX A4 (2026-07-20): 按设置项类型渲染不同控件。
//   原实现所有值都用纯文本输入（type="text"），管理员容易输 "true" vs "1" 混淆。
//   现按 key 识别类型：
//   - boolean: 注册开关与 default_allow_*
//   - number: default_quota_*
//   - enum: admin.web_access
//   - text: 其他文本
var _getSettingControlType = function(key) {
  if (key === 'allow_open_registration' || key === 'allow_invite_registration' ||
      key.indexOf('default_allow_') === 0) {
    return 'boolean';
  }
  if (key.indexOf('default_quota_') === 0) {
    return 'number';
  }
  if (key === 'admin.web_access') {
    return 'enum';
  }
  return 'text';
};

var _BOOLEAN_TRUE_VALUES = ['on', 'true', '1', 'yes'];
var _isBooleanTrue = function(v) {
  return _BOOLEAN_TRUE_VALUES.indexOf(String(v || '').toLowerCase()) >= 0;
};

// C3 fix: 设置 key 中文说明映射表。
//   key 列表与 src/storage.rs is_supported_system_setting() 保持一致。
//   修改后端可写 key 时务必同步更新此表，否则管理员看到的"说明"字段为空。
var _SETTING_DESC_MAP = {
  // 站点
  "site_name": "站点显示名称，将出现在浏览器标题栏、登录页、通知邮件等位置。",
  "docs.url": "用户使用文档链接，登录页底部会显示。留空则隐藏入口。",
  // 注册与登录
  "allow_open_registration": "开放注册开关。开启后任何人都能直接注册账号，无需邀请码。建议保持关闭。",
  "allow_invite_registration": "邀请码注册开关。开启后管理员可生成邀请码让指定用户注册。",
  // 守则
  "terms_version": "守则版本号。修改 terms_text 后通常需要同步升版本号，触发已同意旧版用户重新确认。",
  "terms_text": "守则正文。注册/登录时显示给用户阅读并同意。支持纯文本，多行用 \\n。",
  "terms.url": "守则外链。若设置则用户看到的「同意守则」将跳转到此 URL，优先级高于 terms_text。",
  "admin.web_access": "管理面板访问策略：off=关闭、intranet=仅内网、open=允许所有已认证管理员访问。",
  // 系统默认值（新用户注册时套用）
  "default_quota_upload_bytes": "新注册用户的初始上传配额（字节）。",
  "default_quota_download_bytes": "新注册用户的初始下载配额（字节）。",
  "default_quota_media_bytes": "新注册用户的初始媒体存储配额（字节）。",
  "default_quota_msg_per_day": "新注册用户的初始每日消息数上限。",
  "default_quota_media_count": "新注册用户的初始媒体文件数量上限。",
  "default_allow_upload": "新注册用户是否默认允许上传文件。",
  "default_allow_webdav": "新注册用户是否默认允许使用 WebDAV。",
  "default_allow_custom_webdav": "新注册用户是否默认允许配置自定义 WebDAV。"
};

$("settings-container").addEventListener("click", function(e) {
  var btn = e.target.closest("button[data-action]");
  if (!btn) return;
  var action = btn.getAttribute("data-action");
  if (action === "edit-setting") {
    _currentEditKey = btn.getAttribute("data-key");
    var val = btn.getAttribute("data-value");
    // FIX A7 (2026-07-20): 缓存原始值，保存时显示差异预览。
    _currentEditOldValue = val;
    $("setting-edit-key").value = _currentEditKey;

    // C3 fix：填充该 key 的中文说明（若映射表中存在）。
    //   避免管理员面对技术 key 不知所措，必须回查文档。
    var descEl = $("setting-edit-desc");
    var descWrap = $("setting-edit-desc-wrap");
    var desc = _SETTING_DESC_MAP[_currentEditKey];
    if (desc && descEl && descWrap) {
      descEl.textContent = desc;
      descWrap.style.display = '';
    } else if (descWrap) {
      descWrap.style.display = 'none';
    }
    // FIX A4: 根据 key 类型动态渲染不同控件
    var controlType = _getSettingControlType(_currentEditKey);
    var valueContainer = $("setting-edit-value-container");
    if (!valueContainer) return;
    var html = '';
    if (controlType === 'boolean') {
      var checked = _isBooleanTrue(val) ? 'checked' : '';
      html = '<label style="display:flex;align-items:center;gap:8px;cursor:pointer;">' +
             '<input type="checkbox" id="setting-edit-value" ' + checked + ' style="width:18px;height:18px;">' +
             '<span id="setting-edit-bool-label">' + (_isBooleanTrue(val) ? '已开启 (on)' : '已关闭 (off)') + '</span>' +
             '</label>';
    } else if (controlType === 'number') {
      html = '<input type="number" id="setting-edit-value" value="' + _escapeHtml(val) + '" placeholder="输入数字（0 = 使用系统默认，-1 = 无限制）" style="width:100%;">';
    } else if (controlType === 'enum' && _currentEditKey === 'admin.web_access') {
      var opts = [['off', '关闭'], ['intranet', '仅内网'], ['open', '开放给已认证管理员']];
      var currentVal = String(val || 'intranet');
      html = '<select id="setting-edit-value" style="width:100%;">';
      opts.forEach(function(o) {
        html += '<option value="' + o[0] + '"' + (o[0] === currentVal ? ' selected' : '') + '>' + o[1] + '</option>';
      });
      html += '</select>';
    } else {
      html = '<input type="text" id="setting-edit-value" value="' + _escapeHtml(val) + '" placeholder="输入新值" style="width:100%;">';
    }
    valueContainer.innerHTML = html;
    // 布尔型：实时更新 label
    if (controlType === 'boolean') {
      var checkbox = $("setting-edit-value");
      var boolLabel = $("setting-edit-bool-label");
      if (checkbox && boolLabel) {
        checkbox.addEventListener('change', function() {
          boolLabel.textContent = this.checked ? '已开启 (on)' : '已关闭 (off)';
        });
      }
    }
    $("setting-modal").classList.add("show");
  }
});

$("setting-modal-cancel").addEventListener("click", function() {
  $("setting-modal").classList.remove("show");
  _currentEditKey = null;
  _currentEditOldValue = null;
});

$("setting-modal-save").addEventListener("click", function() {
  var key = _currentEditKey;
  if (!key) return;
  // FIX A4: 根据 controlType 提取值（checkbox 用 .checked，其他用 .value）
  var controlType = _getSettingControlType(key);
  var valueEl = $("setting-edit-value");
  var value;
  if (controlType === 'boolean') {
    value = valueEl && valueEl.checked ? 'on' : 'off';
  } else {
    value = (valueEl ? valueEl.value : '').trim();
    // 数值型校验
    if (controlType === 'number' && value !== '' && isNaN(parseInt(value, 10))) {
      _toast("请输入有效数字", 2000, "error");
      return;
    }
  }

  // FIX A7 (2026-07-20): 保存前显示原值 → 新值差异预览，要求管理员二次确认。
  //   原值与新值相同时直接退出（无变化），避免误触发后端写入。
  var oldVal = _currentEditOldValue != null ? String(_currentEditOldValue) : '(未设置)';
  var newVal = String(value);
  if (oldVal === newVal) {
    _toast("值未变化", 2000);
    $("setting-modal").classList.remove("show");
    _currentEditKey = null;
    _currentEditOldValue = null;
    return;
  }
  // 截断过长值避免 modal 溢出
  var truncOld = oldVal.length > 80 ? oldVal.substring(0, 77) + '...' : oldVal;
  var truncNew = newVal.length > 80 ? newVal.substring(0, 77) + '...' : newVal;
  var confirmMsg = "确认修改设置项？\n\n" +
                   "  键名: " + key + "\n" +
                   "  原值: " + truncOld + "\n" +
                   "  新值: " + truncNew + "\n\n" +
                   "点击「确定」保存，「取消」放弃修改。";
  // D6 fix：用 _showConfirm 替代 window.confirm，保存动作在回调里执行。
  _showConfirm(confirmMsg, function() {
    _doSaveSetting(key, value);
  });
});

// FIX A7: 实际执行保存的内部函数，由 confirm 回调触发。
//   抽出避免回调嵌套和重复的 disable/enable 逻辑分散。
var _doSaveSetting = function(key, value) {
  var btn = $("setting-modal-save");
  btn.disabled = true;
  btn.textContent = "保存中...";

  _adminApi("setting", { key: key, value: value }).then(function(res) {
    if (res.success) {
      _toast("设置已更新", 2000);
      $("setting-modal").classList.remove("show");
      _currentEditKey = null;
      _currentEditOldValue = null;
      _loadSettings();
    } else {
      _toast(res.error || "保存失败", 3000, "error");
    }
  }).catch(function(err) {
    _toast(err.message, 3000, "error");
  }).finally(function() {
    btn.disabled = false;
    btn.textContent = "保存";
  });
};

// Notification send/clear
$("notification-send-btn").addEventListener("click", function() {
  var message = $("notification-message").value.trim();
  var level = $("notification-level").value;
  var statusEl = $("notification-status");
  if (!message) { _toast("请输入通知内容", 2000, "error"); return; }
  var btn = this;
  btn.disabled = true;
  _adminApi("notification", { message: message, level: level }).then(function(res) {
    if (res.success) {
      _toast("通知已发送", 2000);
      if (statusEl) statusEl.textContent = "已发送: " + message;
    } else {
      _toast(res.error || "发送失败", 3000, "error");
    }
  }).catch(function(err) {
    _toast(err.message, 3000, "error");
  }).finally(function() {
    btn.disabled = false;
  });
});

$("notification-clear-btn").addEventListener("click", function() {
  var statusEl = $("notification-status");
  var btn = this;
  btn.disabled = true;
  _adminApi("notification", { message: "", level: "clear" }).then(function(res) {
    if (res.success) {
      _toast("通知已清除", 2000);
      if (statusEl) statusEl.textContent = "通知已清除";
      $("notification-message").value = "";
    } else {
      _toast(res.error || "清除失败", 3000, "error");
    }
  }).catch(function(err) {
    _toast(err.message, 3000, "error");
  }).finally(function() {
    btn.disabled = false;
  });
});

// ─── Tunnel event handlers ─────────────────────────
(function() {
  var startBtn = $("tunnel-start-btn");
  var stopBtn = $("tunnel-stop-btn");
  if (startBtn) startBtn.addEventListener("click", function() {
    var port = parseInt($("tunnel-port").value, 10) || 8888;
    var remote = parseInt($("tunnel-remote").value, 10) || 80;
    var subdomain = ($("tunnel-subdomain-input").value || "").trim();
    _setTunnelMsg("正在启动隧道...", "info");
    startBtn.disabled = true;
    _adminApi("tunnel/start", {port: port, remote: remote, subdomain: subdomain}).then(function(res) {
      if (res.success) {
        _setTunnelMsg("隧道启动成功", "success");
        // 立即刷新状态
        _pollTunnelStatus();
      } else {
        _setTunnelMsg(res.error || "启动失败", "error");
        startBtn.disabled = false;
      }
    }).catch(function(err) {
      _setTunnelMsg("启动失败: " + err.message, "error");
      startBtn.disabled = false;
    });
  });
  if (stopBtn) stopBtn.addEventListener("click", function() {
    _setTunnelMsg("正在停止隧道...", "info");
    stopBtn.disabled = true;
    _adminApi("tunnel/stop", {}).then(function(res) {
      if (res.success) {
        _setTunnelMsg("隧道已停止", "success");
        _pollTunnelStatus();
      } else {
        _setTunnelMsg(res.error || "停止失败", "error");
        stopBtn.disabled = false;
      }
    }).catch(function(err) {
      _setTunnelMsg("停止失败: " + err.message, "error");
      stopBtn.disabled = false;
    });
  });
})();

// ─── Init ──────────────────────────────────────────
// FIX A1+A2+A3 (2026-07-20): 列表搜索框 + 审计日志日期范围 + 导出按钮事件绑定。
//   使用 input 事件实时过滤（无防抖，列表数据量本地过滤性能足够）。
//   搜索时重置 shown=PAGE，避免搜索后仍停留在已加载更多页的状态。
var _bindListSearchEvents = function() {
  var bindSearch = function(inputId, cacheKey, renderFn) {
    var el = $(inputId);
    if (!el) return;
    el.addEventListener('input', function() {
      _listCache[cacheKey].search = el.value.trim();
      _listCache[cacheKey].shown = _listCache[cacheKey].PAGE;
      renderFn();
    });
  };
  bindSearch('users-search', 'users', _renderUsers);
  bindSearch('invites-search', 'invites', _renderInvites);
  bindSearch('ipbans-search', 'ipbans', _renderIpBans);
  bindSearch('audit-search', 'audit', _renderAudit);

  // 审计日志日期范围
  var dateFromEl = $('audit-date-from');
  var dateToEl = $('audit-date-to');
  if (dateFromEl) {
    dateFromEl.addEventListener('change', function() {
      _listCache.audit.dateFrom = dateFromEl.value;
      _listCache.audit.shown = _listCache.audit.PAGE;
      _renderAudit();
    });
  }
  if (dateToEl) {
    dateToEl.addEventListener('change', function() {
      _listCache.audit.dateTo = dateToEl.value;
      _listCache.audit.shown = _listCache.audit.PAGE;
      _renderAudit();
    });
  }

  // 审计日志导出按钮
  var exportCsvBtn = $('audit-export-csv');
  if (exportCsvBtn) exportCsvBtn.addEventListener('click', function() { _exportAudit('csv'); });
  var exportJsonBtn = $('audit-export-json');
  if (exportJsonBtn) exportJsonBtn.addEventListener('click', function() { _exportAudit('json'); });

  // audit-tbody 加载更多 + 行点击展开完整 detail（事件委托）
  var auditTbody = $('audit-tbody');
  if (auditTbody) {
    auditTbody.addEventListener('click', function(e) {
      // 加载更多按钮
      var btn = e.target.closest('button[data-action="load-more"]');
      if (btn) {
        _listCache.audit.shown += _listCache.audit.PAGE;
        _renderAudit();
        return;
      }
      // E1 fix：行点击展开完整 detail
      var row = e.target.closest('tr.audit-row');
      if (row) {
        var detail = row.getAttribute('data-detail') || '';
        // 复用 _showConfirm modal（仅一个"关闭"按钮，onConfirm=noop）
        _showConfirm(detail, function() {}, null, { title: '详情', okText: '关闭' });
      }
    });
  }

  // FIX A9: 用户列表全选 + 批量操作
  var selectAllEl = $('users-select-all');
  if (selectAllEl) {
    selectAllEl.addEventListener('change', function() {
      var checked = selectAllEl.checked;
      document.querySelectorAll('.users-row-checkbox').forEach(function(cb) { cb.checked = checked; });
    });
  }
  var bulkDisableBtn = $('users-bulk-disable');
  if (bulkDisableBtn) bulkDisableBtn.addEventListener('click', function() { _bulkUserAction('disable'); });
  var bulkEnableBtn = $('users-bulk-enable');
  if (bulkEnableBtn) bulkEnableBtn.addEventListener('click', function() { _bulkUserAction('enable'); });
  var bulkDeleteBtn = $('users-bulk-delete');
  if (bulkDeleteBtn) bulkDeleteBtn.addEventListener('click', function() { _bulkUserAction('delete'); });

  // FIX A9: 邀请码列表全选 + 批量撤销
  var invitesSelectAll = $('invites-select-all');
  if (invitesSelectAll) {
    invitesSelectAll.addEventListener('change', function() {
      var checked = invitesSelectAll.checked;
      document.querySelectorAll('.invites-row-checkbox').forEach(function(cb) { cb.checked = checked; });
    });
  }
  var bulkRevokeBtn = $('invites-bulk-revoke');
  if (bulkRevokeBtn) bulkRevokeBtn.addEventListener('click', function() { _bulkInviteRevoke(); });

  // FIX A9: IP 封禁列表全选 + 批量解封
  var ipbansSelectAll = $('ipbans-select-all');
  if (ipbansSelectAll) {
    ipbansSelectAll.addEventListener('change', function() {
      var checked = ipbansSelectAll.checked;
      document.querySelectorAll('.ipbans-row-checkbox').forEach(function(cb) { cb.checked = checked; });
    });
  }
  var bulkUnbanBtn = $('ipbans-bulk-unban');
  if (bulkUnbanBtn) bulkUnbanBtn.addEventListener('click', function() { _bulkIpUnban(); });
};

// FIX A9 + F6 + D6 (2026-07-21): 通用批量操作 helper。
//   原实现 _bulkUserAction / _bulkInviteRevoke / _bulkIpUnban 三份近乎一致的复制粘贴，
//   仅 selector / endpoint / reload 函数不同。统一抽取为 _bulkAction，
//   消除重复并避免后续修改（如再换 confirm→modal）需同步改三处。
//
//   F6 修复：原实现每 5 条触发一次进度 toast，因 idx 变化导致 isSameText 始终 false
//   重启动画，肉眼可见闪屏。改为只在开始显示一次"开始批量 X N 项"，
//   结束显示一次"批量完成：成功 X 失败 Y"，中间无进度更新。
//
//   D6 修复：confirm → _showConfirm，danger 标识由 opts.danger 传入。
//
// 参数：
//   opts.selector     选中行 checkbox 的 CSS 选择器
//   opts.attr         从 checkbox 读取的数据属性名（如 'data-user' / 'data-code' / 'data-ip'）
//   opts.emptyMsg     未选中任何项时的 toast 提示
//   opts.confirmMsg   返回确认 modal 文案（函数，接收 items 数组）
//   opts.endpoint     后端 API endpoint（如 'user/delete'）
//   opts.bodyKey      请求 body 中字段名（如 'user' / 'code'）
//   opts.actionLabel  动作中文标签（如 '删除' / '撤销' / '解封'），用于 toast
//   opts.danger       是否危险操作（true 时 modal 显示红色按钮）
//   opts.reload       完成后调用的刷新函数
var _bulkAction = function(opts) {
  var checkboxes = document.querySelectorAll(opts.selector + ':checked');
  var items = Array.prototype.map.call(checkboxes, function(cb) { return cb.getAttribute(opts.attr); });
  if (items.length === 0) {
    _toast(opts.emptyMsg, 2000, "error");
    return;
  }
  _showConfirm(opts.confirmMsg(items), function() {
    // F6 fix：开始时显示一次进度，结束再显示一次，中间无进度 toast 避免闪屏。
    _toast("开始批量" + opts.actionLabel + " " + items.length + " 项...", 2000);
    var idx = 0, success = 0, failed = 0;
    var next = function() {
      if (idx >= items.length) {
        var msg = "批量" + opts.actionLabel + "完成：成功 " + success + "，失败 " + failed;
        _toast(msg, failed > 0 ? 5000 : 2500, failed > 0 ? 'error' : undefined);
        opts.reload();
        return;
      }
      var item = items[idx++];
      var body = {};
      body[opts.bodyKey] = item;
      _adminApi(opts.endpoint, body).then(function(res) {
        if (res.success) success++; else failed++;
      }).catch(function() {
        failed++;
      }).then(function() { next(); });
    };
    next();
  }, null, { danger: !!opts.danger, okText: opts.actionLabel });
};

var _bulkUserAction = function(action) {
  var actionLabel = action === 'delete' ? '删除' : action === 'disable' ? '禁用' : '启用';
  _bulkAction({
    selector: '.users-row-checkbox',
    attr: 'data-user',
    emptyMsg: "请先勾选要" + actionLabel + "的用户",
    confirmMsg: function(users) {
      return "确定要批量" + actionLabel + "以下 " + users.length + " 个用户吗？\n\n" +
             (action === 'delete' ? '此操作不可撤销！\n' : '') +
             users.join(', ');
    },
    endpoint: action === 'delete' ? 'user/delete' : (action === 'disable' ? 'user/disable' : 'user/enable'),
    bodyKey: 'user',
    actionLabel: actionLabel,
    danger: action === 'delete',
    reload: _loadUsers
  });
};

var _bulkInviteRevoke = function() {
  _bulkAction({
    selector: '.invites-row-checkbox',
    attr: 'data-code',
    emptyMsg: "请先勾选要撤销的邀请码",
    confirmMsg: function(codes) {
      return "确定要批量撤销以下 " + codes.length + " 个邀请码吗？\n\n" + codes.join(', ');
    },
    endpoint: 'invite/revoke',
    bodyKey: 'code',
    actionLabel: '撤销',
    danger: true,
    reload: _loadInvites
  });
};

var _bulkIpUnban = function() {
  _bulkAction({
    selector: '.ipbans-row-checkbox',
    attr: 'data-ip',
    emptyMsg: "请先勾选要解封的 IP",
    confirmMsg: function(ips) {
      return "确定要批量解封以下 " + ips.length + " 个 IP 吗？\n\n" + ips.join(', ');
    },
    endpoint: 'ip-unban',
    bodyKey: 'user',
    actionLabel: '解封',
    reload: _loadIpBans
  });
};

var _initAdmin = function() {
  // FIX A1+A2+A3: 绑定列表搜索/导出事件
  _bindListSearchEvents();

  // FIX 问题三 (2026-07-20): 合并双重并发请求为单次。
  //   原实现同时发起两个请求：
  //     1) _adminGet("users") 成功则什么都不做，失败才走 fallback
  //     2) XHR /api/wasm/me 始终执行
  //   导致首次加载重复请求（第一个拉取用户列表被丢弃，第二个又触发 _loadSection 再拉一次）。
  //   修复：只用 /api/wasm/me 做权限校验，通过后再调 _loadSection。
  var xhr = new XMLHttpRequest();
  xhr.open("GET", "/api/wasm/me", true);
  // FIX H-7 (2026-07-18): 不再设置 X-Session-Token 头，依赖同源 HttpOnly Cookie 自动携带。
  xhr.timeout = 10000;
  xhr.onload = function() {
    if (xhr.status >= 200 && xhr.status < 300) {
      try {
        var me = JSON.parse(xhr.responseText);
        if (me.role !== "owner" && me.role !== "admin") {
          // D6 fix：用 _showConfirm 替代 window.alert（WebView 兼容），
          //   用户点确定后才跳回首页，避免 alert 被拦截后停在管理页空白态。
          _showConfirm("权限不足：需要管理员或所有者权限", function() {
            window.location.href = "/";
          }, null, { okText: "返回首页" });
          return;
        }
        _admin.me = me;
        $("admin-current-user").textContent = me.username || "";
        $("admin-current-role").textContent = me.role || "";
        _loadSection(_admin.currentSection);
      } catch(e) {
        window.location.href = "/";
      }
    } else {
      // FIX admin-auth-redirect + F3 (2026-07-21): 未登录时先 toast 提示再跳 /auth。
      //   旧实现直接跳 /auth，用户不知道发生了什么。F3 修复要求先 toast "会话过期"。
      //   _toast 内部用 setTimeout 异步显示，立即跳转会被中断，故用 setTimeout
      //   延迟 800ms 让 toast 可见后再跳转。
      _toast("会话已过期，正在跳转登录页...", 1500);
      setTimeout(function() { window.location.href = "/auth"; }, 800);
    }
  };
  xhr.onerror = function() {
    _toast("网络错误，正在跳转登录页...", 1500);
    setTimeout(function() { window.location.href = "/auth"; }, 800);
  };
  xhr.ontimeout = function() {
    _toast("请求超时，正在跳转登录页...", 1500);
    setTimeout(function() { window.location.href = "/auth"; }, 800);
  };
  xhr.send();
};

// Kick off when DOM is ready
if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", _initAdmin);
} else {
  _initAdmin();
}
